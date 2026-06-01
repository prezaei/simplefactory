#!/usr/bin/env bash
# Factory local live smoke (P7). Drives ONE labelled issue through the full loop
# against a throwaway GitHub repo, then exercises crash-resume by killing the worker
# container mid-run. This is a SCRIPTED MANUAL smoke — it needs the live stack up, a
# real GitHub App installed on the repo, a real `claude` token, Docker, and
# `gh webhook forward` running. NOT a CI gate. See docs/runbooks/local-live-smoke.md.
#
# Usage:
#   FACTORY_SMOKE_REPO=owner/name ./scripts/smoke.sh
#   FACTORY_SMOKE_REPO=owner/name ./scripts/smoke.sh --skip-crash   # happy path only
set -euo pipefail

REPO="${FACTORY_SMOKE_REPO:-prezaei/factory-smoke}"
LABEL="${FACTORY_SMOKE_LABEL:-factory}"
DASHBOARD_URL="${DASHBOARD_URL:-http://localhost:8082}"
TOOLS_URL="${TOOLS_HEALTH_URL:-http://localhost:8081}"
WORKER_IMAGE="${FACTORY_WORKER_IMAGE:-factory-worker:local}"
TIMEOUT_SECS="${FACTORY_SMOKE_TIMEOUT:-900}"   # per wait, generous: real claude runs
SKIP_CRASH=0
[[ "${1:-}" == "--skip-crash" ]] && SKIP_CRASH=1

OWNER="${REPO%%/*}"
NAME="${REPO##*/}"

log()  { printf '\033[1;36m[smoke]\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m[ ok ]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[fail]\033[0m %s\n' "$*" >&2; exit 1; }

# Poll `cmd` (a shell snippet) until it prints non-empty, or time out.
wait_for() {
  local desc="$1"; shift
  local deadline=$(( $(date +%s) + TIMEOUT_SECS ))
  log "waiting for: $desc (timeout ${TIMEOUT_SECS}s)"
  while :; do
    local out; out="$(eval "$*" 2>/dev/null || true)"
    if [[ -n "$out" ]]; then printf '%s' "$out"; return 0; fi
    [[ $(date +%s) -ge $deadline ]] && fail "timed out waiting for: $desc"
    sleep 5
  done
}

# ---- preflight ----
command -v gh >/dev/null   || fail "gh CLI not found"
command -v jq >/dev/null   || fail "jq not found"
command -v curl >/dev/null || fail "curl not found"
gh auth status >/dev/null 2>&1 || fail "gh not authenticated (run: gh auth login)"
gh repo view "$REPO" >/dev/null 2>&1 || fail "cannot see repo $REPO (exists? gh access?)"
curl -fsS "$DASHBOARD_URL/healthz" >/dev/null 2>&1 || fail "dashboard not up at $DASHBOARD_URL (is the live stack running?)"
curl -fsS "$TOOLS_URL/healthz"     >/dev/null 2>&1 || fail "tools not up at $TOOLS_URL"
gh label list -R "$REPO" --search "$LABEL" --json name -q '.[].name' | grep -qx "$LABEL" \
  || fail "label '$LABEL' missing on $REPO (create it: gh label create $LABEL -R $REPO)"
ok "preflight passed (repo=$REPO label=$LABEL)"

work_items_json() { curl -fsS "$DASHBOARD_URL/api/repos/$OWNER/$NAME/work_items" 2>/dev/null || echo '[]'; }

# Create an issue and apply the opt-in label (this is what intake's webhook triggers on).
open_labelled_issue() {
  local title="$1" body="$2"
  local url; url="$(gh issue create -R "$REPO" --title "$title" --body "$body")"
  local num="${url##*/}"
  gh issue edit "$num" -R "$REPO" --add-label "$LABEL" >/dev/null
  echo "$num"
}

# Find a PR (the Coder's change request) whose body/branch references the issue number.
pr_for_issue() {
  local issue="$1"
  gh pr list -R "$REPO" --state all --search "$issue" --json number,url \
    -q '.[0].url' 2>/dev/null || true
}

# =====================================================================
# PASS 1 — happy path: issue → manager triage → coder CR → reviewer verdict
# =====================================================================
log "=== PASS 1: happy path ==="
ISSUE1="$(open_labelled_issue \
  "[smoke] add a hello function" \
  "Factory smoke test. Please add a small documented \`hello()\` function and a test for it.")"
ok "opened + labelled issue #$ISSUE1"

# The intake webhook should enqueue a manager work item promptly.
wait_for "a work item to appear for the issue" \
  "work_items_json | jq -e --arg n '$ISSUE1' '[.[] | select((.subject_ref // \"\") | test(\$n))] | length>0' >/dev/null && echo seen" >/dev/null
ok "work item enqueued (manager picked up the issue)"

CR_URL="$(wait_for "the Coder to open a change request (PR)" "pr_for_issue '$ISSUE1'")"
ok "change request opened: $CR_URL"

CR_NUM="${CR_URL##*/}"
VERDICT="$(wait_for "the Reviewer to submit a verdict" \
  "gh pr view '$CR_NUM' -R '$REPO' --json reviews -q '.reviews[-1].state' 2>/dev/null")"
ok "reviewer verdict: $VERDICT"

log "transcripts persisted to Azurite (container: transcripts). Inspect with the"
log "Azure CLI / Storage Explorer against the FACTORY_BLOB_CONNECTION_STRING endpoint."
ok "PASS 1 complete — CR=$CR_URL verdict=$VERDICT"

# =====================================================================
# PASS 2 — crash-resume: kill the worker mid-run → sweeper reclaims →
#          re-dispatch restores the checkpoint and `claude --resume`s
# =====================================================================
if [[ "$SKIP_CRASH" == "1" ]]; then
  log "--skip-crash given; skipping crash-resume drill"
  ok "SMOKE PASSED (happy path only)"; exit 0
fi

log "=== PASS 2: crash-resume drill ==="
ISSUE2="$(open_labelled_issue \
  "[smoke] add a goodbye function" \
  "Factory crash-resume smoke. Add a documented \`goodbye()\` function and a test.")"
ok "opened + labelled issue #$ISSUE2"

# Wait for a worker container to be running, then kill it to simulate a crash.
WORKER_ID="$(wait_for "a worker container to start" \
  "docker ps --filter ancestor=$WORKER_IMAGE --format '{{.ID}}' | head -1")"
log "killing worker container $WORKER_ID mid-run (simulated crash)"
docker kill "$WORKER_ID" >/dev/null
ok "worker killed; its lease will lapse and the coordinator's sweeper should reclaim it"

# A NEW worker container (different id) must come up for the re-dispatch.
RESUMED_ID="$(wait_for "the re-dispatched (resumed) worker container" \
  "docker ps --filter ancestor=$WORKER_IMAGE --format '{{.ID}}' | grep -v '^$WORKER_ID' | head -1")"
ok "re-dispatched worker container started: $RESUMED_ID"

# The resumed run should still produce the change request despite the kill.
CR2_URL="$(wait_for "the resumed run to open its change request" "pr_for_issue '$ISSUE2'")"
ok "crash-resume produced change request: $CR2_URL"

ok "SMOKE PASSED — happy path + crash-resume both produced change requests"
