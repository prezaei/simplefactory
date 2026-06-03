#!/usr/bin/env bash
# Factory local live smoke (P7). Drives a throwaway GitHub repo through Factory's
# scenarios against the live stack and asserts each outcome. SCRIPTED MANUAL smoke —
# needs the live stack up, a real GitHub App installed on the repo, a real `claude`
# token, Docker, and `gh webhook forward` running. NOT a CI gate. See
# docs/runbooks/local-live-smoke.md.
#
# Scenarios (deterministically observable):
#   happy   issue labeled → manager triage → coder change-request → reviewer verdict
#   rework  "changes requested" review → a coder runs on the SAME CR (no duplicate CR)
#   crash   kill the worker mid-run → sweeper reclaims → the issue still produces a CR
#   config  push a .factory/config.yml change → it reloads and still drives the loop
#
# Usage:
#   FACTORY_SMOKE_REPO=owner/name ./scripts/smoke.sh                 # all scenarios
#   FACTORY_SMOKE_REPO=owner/name ./scripts/smoke.sh happy crash     # a subset
set -uo pipefail

# Require the target repo explicitly — never default to a real repo, so the script
# can't accidentally drive issues/labels/PRs against an unexpected repository.
REPO="${FACTORY_SMOKE_REPO:?set FACTORY_SMOKE_REPO=owner/name (the throwaway repo)}"
LABEL="${FACTORY_SMOKE_LABEL:-factory}"
DASHBOARD_URL="${DASHBOARD_URL:-http://localhost:8082}"
TOOLS_URL="${TOOLS_HEALTH_URL:-http://localhost:8081}"
WORKER_IMAGE="${FACTORY_WORKER_IMAGE:-factory-worker:local}"
TIMEOUT_SECS="${FACTORY_SMOKE_TIMEOUT:-1200}"   # generous: real claude runs
OWNER="${REPO%%/*}"; NAME="${REPO##*/}"
# One temp root for the whole run, removed on exit (no leaked per-run dirs).
TMPROOT="$(mktemp -d)"
WORKDIR="$TMPROOT/factory-smoke"

log()  { printf '\033[1;36m[smoke]\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m[ ok ]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[warn]\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m[fail]\033[0m %s\n' "$*" >&2; exit 1; }

# Results store as a temp file ("scenario status" per line) — works on macOS's stock
# bash 3.2, which has no associative arrays.
RESULTS_FILE="$TMPROOT/results"
trap 'rm -rf "$TMPROOT"' EXIT
set_result() { echo "$1 $2" >> "$RESULTS_FILE"; }
get_result() { awk -v s="$1" '$1==s{print $2}' "$RESULTS_FILE" | tail -1; }

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

# shellcheck disable=SC2317,SC2329  # invoked indirectly via wait_for's eval'd command string
work_items_json() { curl -fsS "$DASHBOARD_URL/api/repos/$OWNER/$NAME/work-items" 2>/dev/null || echo '[]'; }
# Prints "seen" iff a work item exists for issue #$1. A function so the jq filter runs in
# normal shell context — NOT embedded in a double-quoted string handed to wait_for's
# `eval`, where the jq program's own `"issue#"` literal would close the shell quote and
# expose `$n` to `set -u` ("unbound variable"). The subject is built here and passed as a
# jq --arg, so the filter contains no `"`.
# shellcheck disable=SC2317,SC2329  # invoked indirectly via wait_for's eval'd command string
has_issue_work_item() {
  work_items_json | jq -e --arg s "issue#$1" '[.[]|select(.subject_ref==$s)]|length>0' \
    >/dev/null && echo seen
}
# The CR a fresh issue produces. Match the coder's deterministic head branch
# (`factory/issue-N`) FIRST — a free-text `--search "$1"` only matches if the CR body
# happens to mention the number, so it silently misses CRs (and produced a false crash
# FAIL). Fall back to the text search for any non-standard branch naming.
# shellcheck disable=SC2317,SC2329  # invoked indirectly via wait_for's eval'd command string
pr_for_issue() {
  local url
  url="$(gh pr list -R "$REPO" --state all --head "factory/issue-$1" --json url -q '.[0].url' 2>/dev/null)"
  [ -z "$url" ] && url="$(gh pr list -R "$REPO" --state all --search "$1" --json url -q '.[0].url' 2>/dev/null)"
  printf '%s' "$url"
}

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
  if ! wait_for "a work item for the issue" "has_issue_work_item $issue" >/dev/null; then
    set_result happy FAIL; die "no work item enqueued for #$issue (webhook delivered? config loaded?)"
  fi
  ok "manager picked up the issue"
  cr="$(wait_for "the Coder to open a change request" "pr_for_issue '$issue'")" \
    || { set_result happy FAIL; die "no change request opened for #$issue"; }
  ok "change request: $cr"
  local crn="${cr##*/}"
  verdict="$(wait_for "the Reviewer's verdict" \
    "gh pr view '$crn' -R '$REPO' --json reviews -q '.reviews[-1].state' 2>/dev/null")" \
    || { set_result happy FAIL; die "no reviewer verdict on $cr"; }
  ok "reviewer verdict: $verdict"
  HAPPY_CR="$crn"
  set_result happy PASS
}

# ---- scenario: rework loop (human requests changes → coder reworks the SAME CR) ----
# We assert the rework MECHANISM deterministically: a changes-requested review enqueues a
# coder that runs on the CR AND no NEW CR is opened (the coder pushes the CR's existing
# branch, not a `factory/cr-N` branch). We do NOT require the CR head to advance — whether
# the rework produces a new commit depends on claude judging a change is needed, which is
# non-deterministic; the loop wiring + "same CR" guarantee are the contract under test.
scenario_rework() {
  log "=== rework: changes-requested → coder runs on the SAME CR (no new CR) ==="
  local crn="${HAPPY_CR:-}"
  if [[ -z "$crn" ]]; then warn "rework needs a CR from 'happy'; skipping"; set_result rework SKIP; return; fi
  gh pr review "$crn" -R "$REPO" --request-changes \
    --body "Smoke: please tighten the implementation / add a doc-comment example." >/dev/null \
    || { set_result rework FAIL; warn "could not submit changes-requested review"; return; }
  ok "submitted changes-requested review on CR #$crn"
  # A coder work item appears for cr#N (the rework loop enqueued + ran a coder on the CR).
  if ! wait_for "a rework coder to run on cr#$crn" \
      "work_items_json | jq -e --arg n 'cr#$crn' '[.[]|select(.kind==\"coder\" and .subject_ref==\$n)]|length>0' >/dev/null && echo seen" >/dev/null; then
    set_result rework FAIL; warn "no rework coder ran on cr#$crn"; return
  fi
  ok "a coder ran on cr#$crn (rework loop fired)"
  # The fix: the rework coder pushes the CR's EXISTING branch, so NO new factory/cr-N CR.
  if gh pr list -R "$REPO" --state all --json headRefName -q '.[].headRefName' 2>/dev/null \
      | grep -qx "factory/cr-$crn"; then
    set_result rework FAIL; warn "a duplicate CR on factory/cr-$crn was opened (rework didn't reuse the CR branch)"; return
  fi
  ok "no duplicate CR opened — the rework targeted the CR's own branch"
  set_result rework PASS
}

# ---- scenario: crash-resume ----
# Resilience contract: KILLING the worker mid-run does not lose the work — the
# coordinator's sweeper reclaims the lapsed lease and the dispatcher re-dispatches, so the
# issue STILL produces a change request. Run this scenario ALONE (no other scenarios
# concurrently) so the worker we kill is this issue's. Reclaim latency = the lease TTL
# (set FACTORY_LEASE_TTL_SECS low on the dispatcher for a fast smoke).
scenario_crash() {
  log "=== crash: kill the worker mid-run → sweeper reclaims → issue still produces a CR ==="
  local issue wid cr
  issue="$(open_labelled_issue "[smoke] add a goodbye function" \
    "Factory crash-resume smoke. Add a documented \`goodbye()\` function and a test.")"
  ok "opened + labelled issue #$issue"
  wid="$(wait_for "a worker container to start" \
    "docker ps --filter ancestor=$WORKER_IMAGE --format '{{.ID}}' | head -1")" \
    || { set_result crash FAIL; warn "no worker container appeared"; return; }
  docker kill "$wid" >/dev/null 2>&1 || true
  ok "killed worker $wid mid-run; the sweeper must reclaim the lapsed lease + re-dispatch"
  # The recovery proof: despite the kill, the issue still ends up with a change request.
  cr="$(wait_for "issue #$issue to produce a CR despite the kill (reclaim+re-dispatch)" \
    "pr_for_issue '$issue'")" \
    || { set_result crash FAIL; warn "issue #$issue produced no CR after the kill (crash-resume failed)"; return; }
  ok "crash-resume recovered: issue #$issue → $cr"
  set_result crash PASS
}

# ---- scenario: config reload on push ----
scenario_config() {
  log "=== config: push .factory/config.yml change → reloads + still drives the loop ==="
  rm -rf "$WORKDIR"; gh repo clone "$REPO" "$WORKDIR" >/dev/null 2>&1 || { set_result config FAIL; warn "clone failed"; return; }
  ( cd "$WORKDIR"
    printf "\n# smoke config reload %s\n" "$(date +%s)" >> .factory/config.yml
    git -c user.email=smoke@local -c user.name=smoke commit -aqm "smoke: config reload" && git push -q
  ) || { set_result config FAIL; warn "config push failed"; return; }
  ok "pushed config change"
  # The reload is observable: a freshly labelled issue still enqueues a work item.
  local issue
  issue="$(open_labelled_issue "[smoke] config reload probe" "Probe that config reloaded; no action needed.")"
  if wait_for "a work item after the config reload" "has_issue_work_item $issue" >/dev/null; then
    ok "config reloaded and still drives intake"
    set_result config PASS
  else
    set_result config FAIL; warn "no work item after config reload"
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
  r="$(get_result "$s")"; r="${r:-?}"
  case "$r" in
    PASS) printf '  \033[1;32m%-8s PASS\033[0m\n' "$s" ;;
    SKIP) printf '  \033[1;33m%-8s SKIP\033[0m\n' "$s" ;;
    *)    printf '  \033[1;31m%-8s %s\033[0m\n' "$s" "$r"; fail=1 ;;
  esac
done
log "transcripts → Azurite 'transcripts' container; checkpoints → 'checkpoints'."
if [[ $fail -eq 0 ]]; then ok "SMOKE PASSED"; exit 0; else die "SMOKE had failures"; fi
