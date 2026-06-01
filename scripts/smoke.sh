#!/usr/bin/env bash
# Factory local live smoke (P7). Drives a throwaway GitHub repo through Factory's
# scenarios against the live stack and asserts each outcome. SCRIPTED MANUAL smoke —
# needs the live stack up, a real GitHub App installed on the repo, a real `claude`
# token, Docker, and `gh webhook forward` running. NOT a CI gate. See
# docs/runbooks/local-live-smoke.md.
#
# Scenarios (deterministically observable):
#   happy   issue labeled → manager triage → coder change-request → reviewer verdict
#   rework  human "changes requested" review on the CR → coder reworks → CR head advances
#   crash   kill the worker mid-run → sweeper reclaims → re-dispatch resumes → CR produced
#   config  push a .factory/config.yml change → it reloads and still drives the loop
#
# Usage:
#   FACTORY_SMOKE_REPO=owner/name ./scripts/smoke.sh                 # all scenarios
#   FACTORY_SMOKE_REPO=owner/name ./scripts/smoke.sh happy crash     # a subset
set -uo pipefail

REPO="${FACTORY_SMOKE_REPO:-prezaei/factory-smoke}"
LABEL="${FACTORY_SMOKE_LABEL:-factory}"
DASHBOARD_URL="${DASHBOARD_URL:-http://localhost:8082}"
TOOLS_URL="${TOOLS_HEALTH_URL:-http://localhost:8081}"
WORKER_IMAGE="${FACTORY_WORKER_IMAGE:-factory-worker:local}"
TIMEOUT_SECS="${FACTORY_SMOKE_TIMEOUT:-1200}"   # generous: real claude runs
OWNER="${REPO%%/*}"; NAME="${REPO##*/}"
WORKDIR="$(mktemp -d)/factory-smoke"

log()  { printf '\033[1;36m[smoke]\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m[ ok ]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[warn]\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m[fail]\033[0m %s\n' "$*" >&2; exit 1; }

declare -A RESULT  # scenario -> PASS/FAIL/SKIP

# Poll a shell snippet until it prints non-empty (success) or time out (return 1).
wait_for() {
  local desc="$1"; shift
  local deadline=$(( $(date +%s) + TIMEOUT_SECS ))
  log "waiting for: $desc (≤${TIMEOUT_SECS}s)"
  while :; do
    local out; out="$(eval "$*" 2>/dev/null || true)"
    [[ -n "$out" ]] && { printf '%s' "$out"; return 0; }
    [[ $(date +%s) -ge $deadline ]] && return 1
    sleep 5
  done
}

work_items_json() { curl -fsS "$DASHBOARD_URL/api/repos/$OWNER/$NAME/work_items" 2>/dev/null || echo '[]'; }
pr_for_issue()    { gh pr list -R "$REPO" --state all --search "$1" --json url -q '.[0].url' 2>/dev/null || true; }
pr_head()         { gh pr view "$1" -R "$REPO" --json headRefOid -q .headRefOid 2>/dev/null || true; }

open_labelled_issue() {
  local url num
  url="$(gh issue create -R "$REPO" --title "$1" --body "$2")"
  num="${url##*/}"
  gh issue edit "$num" -R "$REPO" --add-label "$LABEL" >/dev/null
  echo "$num"
}

# ---- preflight ----
preflight() {
  for c in gh jq curl docker; do command -v "$c" >/dev/null || die "$c not found"; done
  gh auth status >/dev/null 2>&1 || die "gh not authenticated"
  gh repo view "$REPO" >/dev/null 2>&1 || die "cannot see repo $REPO"
  curl -fsS "$DASHBOARD_URL/healthz" >/dev/null 2>&1 || die "dashboard down at $DASHBOARD_URL (stack up?)"
  curl -fsS "$TOOLS_URL/healthz" >/dev/null 2>&1 || die "tools down at $TOOLS_URL"
  gh label list -R "$REPO" --search "$LABEL" --json name -q '.[].name' | grep -qx "$LABEL" \
    || die "label '$LABEL' missing on $REPO"
  ok "preflight passed (repo=$REPO label=$LABEL)"
}

# ---- scenario: happy path ----
scenario_happy() {
  log "=== happy: issue → manager → coder CR → reviewer verdict ==="
  local issue cr verdict
  issue="$(open_labelled_issue "[smoke] add a hello function" \
    "Factory smoke. Add a small documented \`hello()\` function and a test for it.")"
  ok "opened + labelled issue #$issue"
  if ! wait_for "a work item for the issue" \
      "work_items_json | jq -e --arg n '$issue' '[.[]|select((.subject_ref//\"\")|test(\$n))]|length>0' >/dev/null && echo seen" >/dev/null; then
    RESULT[happy]=FAIL; die "no work item enqueued for #$issue (webhook delivered? config loaded?)"
  fi
  ok "manager picked up the issue"
  cr="$(wait_for "the Coder to open a change request" "pr_for_issue '$issue'")" \
    || { RESULT[happy]=FAIL; die "no change request opened for #$issue"; }
  ok "change request: $cr"
  local crn="${cr##*/}"
  verdict="$(wait_for "the Reviewer's verdict" \
    "gh pr view '$crn' -R '$REPO' --json reviews -q '.reviews[-1].state' 2>/dev/null")" \
    || { RESULT[happy]=FAIL; die "no reviewer verdict on $cr"; }
  ok "reviewer verdict: $verdict"
  HAPPY_CR="$crn"; HAPPY_ISSUE="$issue"
  RESULT[happy]=PASS
}

# ---- scenario: rework loop (human requests changes → coder updates the CR) ----
scenario_rework() {
  log "=== rework: changes-requested → coder reworks → CR head advances ==="
  local crn="${HAPPY_CR:-}"
  if [[ -z "$crn" ]]; then warn "rework needs a CR from 'happy'; skipping"; RESULT[rework]=SKIP; return; fi
  local before after
  before="$(pr_head "$crn")"
  gh pr review "$crn" -R "$REPO" --request-changes \
    --body "Smoke: please also add a doc-comment example to hello()." >/dev/null \
    || { RESULT[rework]=FAIL; warn "could not submit changes-requested review"; return; }
  ok "submitted changes-requested review on CR #$crn (head was ${before:0:7})"
  after="$(wait_for "the Coder to push a rework commit" \
    "h=\$(pr_head '$crn'); [[ -n \"\$h\" && \"\$h\" != '$before' ]] && echo \"\$h\"")" \
    || { RESULT[rework]=FAIL; warn "CR head did not advance after changes-requested"; return; }
  ok "CR head advanced ${before:0:7} → ${after:0:7} (coder reworked)"
  RESULT[rework]=PASS
}

# ---- scenario: crash-resume ----
scenario_crash() {
  log "=== crash: kill worker mid-run → sweeper → re-dispatch resumes → CR ==="
  local issue wid resumed cr
  issue="$(open_labelled_issue "[smoke] add a goodbye function" \
    "Factory crash-resume smoke. Add a documented \`goodbye()\` function and a test.")"
  ok "opened + labelled issue #$issue"
  wid="$(wait_for "a worker container to start" \
    "docker ps --filter ancestor=$WORKER_IMAGE --format '{{.ID}}' | head -1")" \
    || { RESULT[crash]=FAIL; warn "no worker container appeared"; return; }
  log "killing worker $wid mid-run"
  docker kill "$wid" >/dev/null
  ok "worker killed; sweeper should reclaim the lapsed lease"
  resumed="$(wait_for "the re-dispatched worker container" \
    "docker ps --filter ancestor=$WORKER_IMAGE --format '{{.ID}}' | grep -v '^$wid' | head -1")" \
    || { RESULT[crash]=FAIL; warn "no re-dispatched worker after kill"; return; }
  ok "re-dispatched worker: $resumed"
  cr="$(wait_for "the resumed run to open its CR" "pr_for_issue '$issue'")" \
    || { RESULT[crash]=FAIL; warn "resumed run produced no CR"; return; }
  ok "crash-resume produced CR: $cr"
  RESULT[crash]=PASS
}

# ---- scenario: config reload on push ----
scenario_config() {
  log "=== config: push .factory/config.yml change → reloads + still drives the loop ==="
  rm -rf "$WORKDIR"; gh repo clone "$REPO" "$WORKDIR" >/dev/null 2>&1 || { RESULT[config]=FAIL; warn "clone failed"; return; }
  ( cd "$WORKDIR"
    printf "\n# smoke config reload %s\n" "$(date +%s)" >> .factory/config.yml
    git -c user.email=smoke@local -c user.name=smoke commit -aqm "smoke: config reload" && git push -q
  ) || { RESULT[config]=FAIL; warn "config push failed"; return; }
  ok "pushed config change"
  # The reload is observable: a freshly labelled issue still enqueues a work item.
  local issue
  issue="$(open_labelled_issue "[smoke] config reload probe" "Probe that config reloaded; no action needed.")"
  if wait_for "a work item after the config reload" \
      "work_items_json | jq -e --arg n '$issue' '[.[]|select((.subject_ref//\"\")|test(\$n))]|length>0' >/dev/null && echo seen" >/dev/null; then
    ok "config reloaded and still drives intake"
    RESULT[config]=PASS
  else
    RESULT[config]=FAIL; warn "no work item after config reload"
  fi
}

# ---- main ----
preflight
SCENARIOS=("$@")
[[ ${#SCENARIOS[@]} -eq 0 ]] && SCENARIOS=(config happy rework crash)
for s in "${SCENARIOS[@]}"; do
  case "$s" in
    config) scenario_config ;;
    happy)  scenario_happy ;;
    rework) scenario_rework ;;
    crash)  scenario_crash ;;
    *) die "unknown scenario: $s (use: config happy rework crash)" ;;
  esac
done

echo; log "==================== RESULTS ===================="
fail=0
for s in "${SCENARIOS[@]}"; do
  r="${RESULT[$s]:-?}"
  case "$r" in
    PASS) printf '  \033[1;32m%-8s PASS\033[0m\n' "$s" ;;
    SKIP) printf '  \033[1;33m%-8s SKIP\033[0m\n' "$s" ;;
    *)    printf '  \033[1;31m%-8s %s\033[0m\n' "$s" "$r"; fail=1 ;;
  esac
done
log "transcripts → Azurite 'transcripts' container; checkpoints → 'checkpoints'."
[[ $fail -eq 0 ]] && { ok "SMOKE PASSED"; exit 0; } || { die "SMOKE had failures"; }
