# Ship It — Iteration Playbook for PR #{{PR_NUMBER}}

> **This file is generated during pre-flight. Replace all `{{PLACEHOLDER}}` values with actual PR details before writing to disk.**

**PR**: #{{PR_NUMBER}} | **Branch**: {{BRANCH}} -> {{BASE_BRANCH}}
**Repo**: {{REPO}} | **Max rounds**: {{MAX_ROUNDS}}
**Required checks**: {{REQUIRED_CHECKS}} (JSON array — see Case B below for behavior when empty)
**Copilot available**: {{COPILOT_AVAILABLE}} (true/false — filled by pre-flight from `state.copilot_available`; when true the Copilot-on-HEAD gate applies, see Step 3d and Step 4 Gate 5; when false the gate is skipped). Reviewer login for the `reviews` API check is the fixed `copilot-pull-request-reviewer[bot]`. GOTCHA: Copilot's inline review **comments** appear under login `"Copilot"` (not the bot slug) in the REST comments API.
**GHE hostname**: {{GHE_HOSTNAME}} (null for github.com; sub-agents should read `ghe_hostname` from state.json and pass `--hostname <value>` on every `gh api` call when non-null)
**State file**: `.ship-it/pr-{{PR_NUMBER}}/state.json`

---

## Setup

1. Read `.ship-it/pr-{{PR_NUMBER}}/state.json` — load current state
2. If `status` is not `in_progress`, exit immediately (already merged or aborted)
3. Increment `iteration` count in memory (write back at end)
4. If `iteration > max_rounds`:
   - Set `status: "aborted"`, write state
   - Cancel cron: `CronDelete(id=<cron_id from state>)`
   - Output the abort report (see Final Report below)
   - Cleanup workspace: `rm -rf .ship-it/pr-{{PR_NUMBER}}`
   - Exit

---

## Step 1: Gather State

Spawn **2 agents in parallel** — CI status and comment fetch happen simultaneously.

### Agent A — CI + Mergeable Status Agent

```
Agent(subagent_type="general-purpose", prompt="
You are checking CI and mergeable status for PR #{{PR_NUMBER}} on branch {{BRANCH}} in repo {{REPO}}.

FIRST: git fetch origin {{BRANCH}} && git checkout {{BRANCH}} && git pull && git branch --show-current

Step 1 — Get the HEAD commit SHA (this is the commit CI must have run against):
  HEAD_SHA=$(git rev-parse HEAD)
  echo 'HEAD SHA:' $HEAD_SHA

Step 2 — Get PR checks AND mergeable status:
  gh pr checks {{PR_NUMBER}}
  gh pr view {{PR_NUMBER}} --json mergeable,mergeStateStatus -q '{mergeable, mergeStateStatus}'

Capture `mergeable` — one of: MERGEABLE | CONFLICTING | UNKNOWN. UNKNOWN means GitHub hasn't finished computing — treat as 'pending' for this iteration and re-check next cycle.

Step 3 — Verify HEAD SHA matches what GitHub considers the PR head:
  PR_HEAD=$(gh pr view {{PR_NUMBER}} --json headRefOid -q .headRefOid)
  echo 'PR HEAD:' $PR_HEAD
If HEAD_SHA != PR_HEAD, report ci_status as 'pending' with details 'HEAD SHA mismatch — local HEAD does not match PR head on GitHub. Branch may have been updated.' Do NOT interpret any check results — they may be stale.

Step 4 — Get detailed check info (only if HEAD_SHA == PR_HEAD). Run:
  gh pr view {{PR_NUMBER}} --json statusCheckRollup --jq '.statusCheckRollup[] | [.name, .status, .conclusion, .detailsUrl] | @tsv'
This shows the checks for the PR's current head commit.

Classify each check using the required_checks list from state.json: {{REQUIRED_CHECKS}}.
- REQUIRED: the check's name is in required_checks.
- NON-BLOCKING: any check NOT in required_checks.

**If required_checks is EMPTY, no check is required.** Do NOT invert this to "all listed checks are required" — that assumption stalls Ship It on governance/async checks that may never fire. Empty required_checks means the repo has no branch protection on the base branch; admin-merge bypasses everything.

CI conclusion mapping:
- 'SUCCESS' or 'success' → pass
- 'NEUTRAL' or 'neutral' or 'skipped' → pass (path filters excluded all changed files — this is normal for docs-only or unrelated PRs)
- 'FAILURE' or 'failure' → fail
- 'CANCELLED' or 'cancelled' → fail
- '' or 'pending' or 'queued' or 'in_progress' → pending

CRITICAL RULES (when required_checks is non-empty):
- If a required check does NOT appear in the output AT ALL, report it as **pending** — NOT green. A missing check means CI has not started yet.
- ALL required checks must be green (SUCCESS) or skipped (NEUTRAL). Do NOT report all_green if only some required checks passed.
- Do NOT report all_green if any required check is absent from the output.
- Do NOT report pending if every required check is NEUTRAL — that means they correctly skipped and the PR is clear to merge.

CRITICAL RULES (when required_checks is empty):
- Always report all_green. No check is required, so there is nothing to wait for and nothing to "fail" on.
- missing_checks is always [] — nothing is missing because nothing is required.
- failures is always [] — even a FAILURE check is not a blocker in this mode. If the user wanted it enforced, they'd have branch protection or passed --required-checks.

Report as JSON:
{
  'ci_status': 'all_green' | 'has_failures' | 'pending',
  'mergeable': 'MERGEABLE' | 'CONFLICTING' | 'UNKNOWN',
  'head_sha': '<the HEAD SHA you checked>',
  'required_checks': [{'name': '...', 'status': 'pass|fail|pending', 'url': '...'}],
  'failures': ['check name and URL for each failure'],
  'missing_checks': ['names of required checks not found in output']
}
")
```

### Agent B — Comment Fetch Agent

```
Agent(subagent_type="general-purpose", prompt="
You are fetching review comments for PR #{{PR_NUMBER}} on branch {{BRANCH}} in repo {{REPO}}.

FIRST: git fetch origin {{BRANCH}} && git checkout {{BRANCH}} && git pull && git branch --show-current

Run these commands:
  gh pr view {{PR_NUMBER}} --json reviews,comments,reviewDecision
  gh api --paginate repos/{{REPO}}/pulls/{{PR_NUMBER}}/comments

CRITICAL — PAGINATE: the REST review-comments endpoint returns at most 30 comments per page. ALWAYS pass `--paginate` so you fetch ALL comments — a single unpaginated page silently drops comments past the first 30 and has caused a real miss (an untriaged Copilot comment left open at merge). This applies to every comment author, not just Copilot.

CRITICAL — COPILOT LOGIN: in the REST comments API, Copilot's inline review comments appear under author login "Copilot" (NOT the bot slug `copilot-pull-request-reviewer[bot]`, which is only the login on the reviews API). When detecting/triaging Copilot comments, match login "Copilot". Do NOT rely on the bot slug for comments.

ALSO check review-thread resolution so no unresolved thread is left open at merge (GraphQL — the REST comments API does not expose resolution state). Pass owner/repo/PR as GraphQL **variables** via `-F` — do NOT string-interpolate them into the query, and do NOT escape quotes inside the single-quoted query (escaped quotes are sent literally and cause a GraphQL parse error; `gh` does NOT expand `{owner}/{repo}` inside a GraphQL query the way it does for REST paths):
  OWNER=$(gh repo view --json owner -q .owner.login); REPO=$(gh repo view --json name -q .name)
  gh api graphql -F owner="$OWNER" -F repo="$REPO" -F pr={{PR_NUMBER}} -f query='
    query($owner:String!,$repo:String!,$pr:Int!){
      repository(owner:$owner,name:$repo){ pullRequest(number:$pr){
        reviewThreads(first:100){ pageInfo{ hasNextPage endCursor } nodes{ isResolved comments(first:1){ nodes{ databaseId author{ login } } } } } } } }'

(On GitHub Enterprise add `--hostname <ghe-host>` to the `gh api graphql` call too. The `gh repo view` calls auto-detect the host from the checked-out remote.)

CRITICAL — PAGINATE reviewThreads: `reviewThreads(first:100)` returns at most 100 threads. On a PR with >100 threads, later pages are SILENTLY DROPPED and an unresolved Copilot/human thread on a later page would be merged over. You MUST cover ALL threads: read `pageInfo { hasNextPage endCursor }` and, while `hasNextPage` is true, re-run the query passing the cursor as another variable, accumulating `nodes` across every page until `hasNextPage` is false:
  gh api graphql -F owner="$OWNER" -F repo="$REPO" -F pr={{PR_NUMBER}} -F cursor="<endCursor>" -f query='
    query($owner:String!,$repo:String!,$pr:Int!,$cursor:String!){
      repository(owner:$owner,name:$repo){ pullRequest(number:$pr){
        reviewThreads(first:100,after:$cursor){ pageInfo{ hasNextPage endCursor } nodes{ isResolved comments(first:1){ nodes{ databaseId author{ login } } } } } } } }'

Treat any thread with isResolved == false (especially one authored by "Copilot" or a human reviewer) as an unaddressed item — its root comment id should be surfaced as a NEW/untriaged comment so the decision gate routes it to triage. A still-open thread must NOT be merged over.

(Note: if the repo is on GitHub Enterprise rather than github.com, add `--hostname <ghe-host>` to every `gh api` call.)

Read the state file at .ship-it/pr-{{PR_NUMBER}}/state.json and extract the triaged_comment_ids array.

For each comment, capture: id, author login, author_association (MEMBER, COLLABORATOR, CONTRIBUTOR, NONE, or bot), body, path (file), line, created_at, in_reply_to_id.

Identify NEW comments: those whose id is NOT in triaged_comment_ids. Also surface the root comment of any UNRESOLVED review thread (from the GraphQL query) that is not already triaged.
Bot detection: if author login contains [bot] or author_association is 'NONE' and the account name suggests automation (github-actions, copilot, dependabot, etc.), mark as bot. The Copilot reviewer's comments use login "Copilot" — mark these as bot.

Report as JSON:
{
  'review_decision': 'APPROVED' | 'CHANGES_REQUESTED' | 'REVIEW_REQUIRED' | '',
  'new_comments': [{'id': N, 'author': '...', 'is_bot': true|false, 'body': '...', 'path': '...', 'line': N}],
  'total_comments': N,
  'new_count': N
}
")
```

---

## Step 2: Decision Gate

Once both agents complete, evaluate results. Take the **FIRST matching** action:

> **Where the Copilot-on-HEAD check lives:** rows 5 and 5b below reference "the Copilot-on-HEAD check". That check is computed by the exact-login / current-HEAD / submitted-state assertion in **[Step 3d](#step-3d-wait-for-copilot-review)** (wait path) and re-asserted at merge in **[Step 4 Gate 5](#gate-5--copilot-review-on-the-exact-current-head-only-when-copilot_available-is-true)**. Look there for the precise `gh api --paginate .../reviews` query and its pass condition.

| # | Condition | Action |
|---|-----------|--------|
| 1 | New comments exist (regardless of CI / mergeable state) | Go to **Step 3b: Triage Comments** |
| 2 | `mergeable == CONFLICTING` AND no new comments | Go to **Step 3c: Rebase onto base** |
| 3 | CI has failures AND no new comments | Go to **Step 3a: Fix CI** |
| 4 | `mergeable == UNKNOWN` AND no new comments | Log "Iteration {{N}}: mergeability still computing." Update state, exit. |
| 5 | CI green AND `mergeable == MERGEABLE` AND no new comments AND ( `{{COPILOT_AVAILABLE}}` is false OR the Copilot-on-HEAD check passes — see Step 3d ) | Go to **Step 4: Merge** |
| 5b | `{{COPILOT_AVAILABLE}}` is true AND CI green AND `mergeable == MERGEABLE` AND no new comments AND the Copilot-on-HEAD check does NOT yet pass | Go to **Step 3d: Wait for Copilot review** |
| 6 | CI pending AND `mergeable == MERGEABLE` AND no new comments | Log "Iteration {{N}}: CI still running, no new comments." Update state, exit. |

**Comments always take priority.** Reviewers post at any time — addressing feedback promptly shows respect for their time and prevents comment pile-up across iterations. Note that even when the Copilot gate applies (`{{COPILOT_AVAILABLE}}` is true), NEW comments (including any from the Copilot review itself, under login "Copilot") and any unresolved review thread are still triaged first via row 1 before any merge is considered.

**`CHANGES_REQUESTED` does NOT block merge.** If all reviewer comments have been triaged (addressed, rejected with rationale, or skipped), the PR is merge-ready regardless of `reviewDecision`. Reviewers often don't re-review promptly, and waiting burns iterations for no reason. Admin merge privileges (which the REST merge endpoint honors from the caller's token) bypass a `CHANGES_REQUESTED` review state. The real gate is: have we seen and addressed every comment?

---

## Step 3a: Fix CI Failures

Check `ci_fix_attempts` in state. If any failure type has been attempted 3+ times, set `status: "aborted"`, cancel cron, output report, cleanup workspace (`rm -rf .ship-it/pr-{{PR_NUMBER}}`), and exit.

Spawn a **CI Fix Agent**:

```
Agent(subagent_type="general-purpose", prompt="
You are fixing CI failures for PR #{{PR_NUMBER}} on branch {{BRANCH}} in repo {{REPO}}.

FIRST: git fetch origin {{BRANCH}} && git checkout {{BRANCH}} && git pull && git branch --show-current
Verify you are on branch {{BRANCH}} before making any changes.

1. Run `gh pr checks {{PR_NUMBER}}` — identify failing checks
2. For each failing check, follow its detailsUrl to the run (GitHub Actions: the URL is github.com/<repo>/actions/runs/<run_id>)
3. Fetch logs for the failing run:
   - List runs for this branch/PR: gh run list --branch {{BRANCH}} --limit 5
   - Read the failing run's logs: gh run view <run_id> --log-failed
4. Diagnose: lint error, type error, test failure, or infrastructure/flaky?

If INFRASTRUCTURE/FLAKY (not a code issue):
   5a. Requeue the failed run:
       gh run rerun <run_id> --failed
   Report: { 'result': 'FLAKY', 'requeued': true, 'run_id': '<id>', 'details': '...' }
   Do NOT push anything.

If CODE ISSUE:
5. Fix the code — only modify files that are already in this PR's diff scope
6. VERIFY before pushing (hill-climb guard) by running the repo's quality gates.
   Read AGENTS.md → 'PR Quality Checklist' for the exact commands this repo runs in CI.
   Run them locally. If any fails, DO NOT push.
7. If verification FAILS: revert your changes and report:
   { 'result': 'BLOCKED', 'details': 'Verification failed after fix — <what failed>' }
8. If verification PASSES: commit and push:
   git add <specific-files-only>
   git commit -m 'fix: address CI failure — <description>'
   git push
   Report: { 'result': 'FIXED', 'details': '...' }

IMPORTANT: Never modify files outside this PR's diff scope.
IMPORTANT: Never push without passing the repo's quality gates first.
")
```

After agent completes:
- Update `ci_fix_attempts` in state (increment count for this failure type)
- Add iteration record to `history`
- Write state, exit iteration. Next cron fire checks the new CI run.

---

## Step 3b: Triage Review Comments

Spawn a **Comment Triage Agent** with the new comments:

```
Agent(subagent_type="general-purpose", prompt="
You are triaging PR review comments for PR #{{PR_NUMBER}} on branch {{BRANCH}} in repo {{REPO}}.

FIRST: git fetch origin {{BRANCH}} && git checkout {{BRANCH}} && git pull && git branch --show-current
Verify you are on branch {{BRANCH}} before reading or modifying any code.

(Note: if the repo is on GitHub Enterprise rather than github.com, add `--hostname <ghe-host>` to every `gh api` call below.)

## Author trust levels
- MEMBER / COLLABORATOR (human team member): HIGH trust. These people know the codebase. Assume their feedback is valid unless you can demonstrate otherwise with code evidence.
- Bot accounts (github-actions, copilot, dependabot, etc.): MEDIUM trust. Bots can hallucinate or reference stale code. Always verify against actual file contents.
- NONE / external: LOW trust. Verify everything against the code.

## For EACH new comment, determine:
1. Read the actual code at the referenced file:line ON THIS BRANCH
2. Is this a real bug, security issue, or correctness problem? -> IMPLEMENT
3. Is this a valid style/quality suggestion from a human reviewer? -> IMPLEMENT (respect team norms)
4. Is this a bot suggestion that is factually wrong or based on stale/wrong code? -> REJECT
5. Is this a human suggestion you believe is incorrect? -> REJECT_HUMAN (handle carefully)
6. Is this a trivial optional nit with no correctness impact? -> SKIP

## Actions per verdict:

IMPLEMENT:
- Fix the code (only files in this PR's diff scope)
- VERIFY before pushing by running the repo's quality gates (AGENTS.md → 'PR Quality Checklist')
- If verification fails, revert that specific fix, mark as BLOCKED

REJECT (bot comments):
- Reply with technical rationale:
  gh api repos/{{REPO}}/pulls/{{PR_NUMBER}}/comments/<id>/replies -f body='<rationale>'

REJECT_HUMAN (human reviewer comments):
- Reply respectfully with evidence-based rationale. Include a note that this is an automated response:
  gh api repos/{{REPO}}/pulls/{{PR_NUMBER}}/comments/<id>/replies -f body='<rationale>

  (Automated response — please let me know if you disagree and I will revisit.)'

SKIP:
- No action, no reply needed

## After all triage decisions:
If any fixes were made, commit and push ONCE:
  git add <specific-files-only>
  git commit -m 'fix: address review feedback — <summary of what was fixed>'
  git push

Reply to each IMPLEMENT comment that was fixed:
  gh api repos/{{REPO}}/pulls/{{PR_NUMBER}}/comments/<id>/replies -f body='Fixed — <description>'

## Output as JSON:
{
  'triaged': [
    {'id': N, 'verdict': 'IMPLEMENT|REJECT|REJECT_HUMAN|SKIP|BLOCKED', 'author': '...', 'is_bot': true|false, 'summary': '...'}
  ],
  'pushed': true|false,
  'human_rejections': [{'id': N, 'rationale': '...'}]
}

IMPORTANT: Never modify files outside this PR's diff scope.
IMPORTANT: Never push without passing the repo's quality gates first.

## Comments to triage:
<PASTE_NEW_COMMENTS_HERE>
")
```

When constructing this agent prompt, replace `<PASTE_NEW_COMMENTS_HERE>` with the actual new comments from Agent B's output, formatted as:
```
Comment #<id> by <author> (<association>) on <path>:<line>:
<body>
```

After agent completes:
- Add all triaged comment IDs to `triaged_comment_ids` in state
- Log any `REJECT_HUMAN` items in iteration history (these warrant user visibility)
- Add iteration record to `history`
- Write state, exit iteration

---

## Step 3c: Rebase onto Base

Base branch ({{BASE_BRANCH}}) has moved and the PR is now `CONFLICTING`. Attempt a rebase; auto-resolve only mechanical conflicts; escalate substantive ones to the user.

Check `rebase_attempts` in state. If already attempted 2+ times in this Ship It run, skip straight to escalation — repeated conflicts on the same branch are a signal that human attention is warranted.

Spawn a **Rebase Agent**:

```
Agent(subagent_type="general-purpose", prompt="
You are rebasing PR #{{PR_NUMBER}} (branch {{BRANCH}}) onto {{BASE_BRANCH}} in repo {{REPO}} because GitHub reports it as CONFLICTING.

FIRST:
  git fetch origin {{BRANCH}} {{BASE_BRANCH}}
  git checkout {{BRANCH}}
  git pull --ff-only origin {{BRANCH}}
  git branch --show-current   # must be {{BRANCH}}

Capture the pre-rebase HEAD so you can abort back to a known state:
  PRE_REBASE_SHA=$(git rev-parse HEAD)
  echo 'Pre-rebase HEAD:' $PRE_REBASE_SHA

Step 1 — Attempt the rebase:
  git rebase origin/{{BASE_BRANCH}}

If rebase exits 0 with no conflicts → go to Step 4 (verify + push).

Step 2 — Conflicts occurred. Classify them.

Inspect conflicted files:
  git status --porcelain | grep '^UU\\|^AA\\|^DU\\|^UD'

Classify each file into ONE of three buckets:

BUCKET A — REGENERABLE (accept base, regenerate deterministically):
  - uv.lock                                → git checkout --theirs uv.lock && uv lock
  - **/*_pb2*.py* (protobuf stubs)          → git checkout --theirs <file> && make protos
  - package-lock.json / yarn.lock / pnpm-lock.yaml → accept base + re-run the package manager's install
  - Any file listed in a .gitattributes 'merge=theirs' / generated marker

BUCKET B — ADDITIVE / NON-OVERLAPPING (union of both sides):
  The conflict hunks show both sides ADDED different content in the same region, with no line edited on both sides. Examples: new import in a sorted block, new entry in a list/dict/enum, new test alongside existing tests.
  - Resolve by keeping both sides' additions (remove conflict markers, keep union).
  - Re-run the repo's formatter/linter so ordering is canonical (ruff format / etc.).

BUCKET C — SUBSTANTIVE (requires human judgment):
  ANY of the following:
    1. A line was edited on BOTH sides with different semantic intent (not just reformat/imports).
    2. Either side's change depends on the other side's change to make sense.
    3. A test's expected behavior conflicts with a code change that alters that behavior.
    4. The resolution would require discarding or substantially rewriting either side's intent.

If ANY conflicted file falls in Bucket C, STOP:
  git rebase --abort
  # Capture the conflict summary for the user BEFORE abort wipes the worktree state:
  (in the pre-abort state, run `git diff` for each conflicted file and save excerpts)
Report:
  { 'result': 'ESCALATE',
    'reason': 'substantive_conflict',
    'pre_rebase_sha': '<PRE_REBASE_SHA>',
    'substantive_files': ['path1', 'path2', ...],
    'hunks': { 'path1': '<first ~50 lines of conflict diff>', ... },
    'base_commit_range': '<commits from base that introduced the conflicts, via git log origin/{{BASE_BRANCH}} ^$PRE_REBASE_SHA --oneline -- <file>>'
  }
Do NOT push anything.

Step 3 — Only Buckets A and B present: resolve them.
For each conflicted file, apply the bucket's resolution. Then:
  git add <resolved-files>
  git rebase --continue
Repeat until the rebase sequence is complete. If a new conflict appears mid-rebase, classify it the same way; Bucket C at any point → abort and escalate.

Step 4 — Verify BEFORE pushing (hill-climb guard):
Run the repo's quality gates (AGENTS.md → 'PR Quality Checklist'). If any gate fails:
  git rebase --abort || true
  # If the rebase already completed but verification failed, reset instead:
  git reset --hard $PRE_REBASE_SHA
Report:
  { 'result': 'ESCALATE',
    'reason': 'verification_failed_post_rebase',
    'pre_rebase_sha': '<PRE_REBASE_SHA>',
    'details': '<which gate failed, with the relevant output excerpt>' }
Do NOT push anything.

Step 5 — Verification passed. Force-push WITH LEASE (never plain --force):
  git push --force-with-lease origin {{BRANCH}}

If --force-with-lease is rejected (someone else pushed concurrently):
  Do NOT retry with --force. Report:
    { 'result': 'ESCALATE', 'reason': 'concurrent_push_detected', 'pre_rebase_sha': '<PRE_REBASE_SHA>' }

On successful push, report:
  { 'result': 'REBASED',
    'pre_rebase_sha': '<PRE_REBASE_SHA>',
    'new_head_sha': '<git rev-parse HEAD>',
    'resolved_files': { 'bucket_a': [...], 'bucket_b': [...] } }

IMPORTANT: Never modify files outside the conflict set.
IMPORTANT: Never force-push without --force-with-lease.
IMPORTANT: Never force-push to {{BASE_BRANCH}} or any protected branch — only to {{BRANCH}}.
")
```

After agent completes:
- **If `result: REBASED`**: increment `rebase_attempts`, record `pre_rebase_sha` and `new_head_sha` in history, write state, exit iteration. Next cron fire re-checks CI against the new HEAD.
- **If `result: ESCALATE`**: set `status: "aborted"`, `abort_reason: "merge_conflict"`, record the agent's full report in state, cancel cron (`CronDelete(id=<cron_id>)`), output the Final Report (see below — include the escalation details so the user can resolve without re-running), cleanup workspace (`rm -rf .ship-it/pr-{{PR_NUMBER}}`), exit.

### Escalation message format

When the agent escalates, the Final Report's top line should be:

> **Ship It paused on PR #{{PR_NUMBER}} — merge conflict needs your attention.**

Followed by:
- `reason` (substantive_conflict | verification_failed_post_rebase | concurrent_push_detected)
- The conflicted file list and hunk excerpts (for substantive_conflict)
- Suggested next step: "resolve on branch {{BRANCH}}, push, then re-run `/ship-it {{PR_NUMBER}}`"

Do NOT spam the user with noise from mechanical conflicts — those were resolved silently. Only surface what actually needs their judgment.

---

## Step 3d: Wait for Copilot review

**Only reached when `{{COPILOT_AVAILABLE}}` is true.** When Copilot is unavailable (`false`) this step is dead and never routed to — behavior is exactly as before the Copilot gate existed.

The Copilot gate requires a review by `copilot-pull-request-reviewer[bot]` (the fixed reviews-API login) whose `commit_id` equals the EXACT current PR head SHA. A review on a superseded commit does NOT count — when the caller pushes a fix, HEAD moves and the gate naturally resets to the new HEAD.

Run the exact-login / current-HEAD check:

```bash
HEAD=$(gh pr view {{PR_NUMBER}} --json headRefOid -q .headRefOid)
gh api --paginate repos/{{REPO}}/pulls/{{PR_NUMBER}}/reviews \
  --jq ".[] | select(.user.login==\"copilot-pull-request-reviewer[bot]\" and .commit_id==\"$HEAD\" and .state != \"PENDING\") | .id" | wc -l   # one .id per matching review across ALL pages, then count lines — must be >= 1. (Do NOT use `[...] | length`: with --paginate, --jq runs PER PAGE, so length is a per-page count, not the total.)
```

On GitHub Enterprise, add `--hostname <host>` to the `gh api` calls, where `<host>` is `state.ghe_hostname` (non-null).

**PAGINATE** the reviews endpoint with `--paginate` — it returns at most 30 reviews per page, so on a PR with many reviews the Copilot review can fall onto a later page and be silently missed, breaking the gate. **Exclude PENDING reviews**: a `state == "PENDING"` review is an unsubmitted draft the bot has not yet posted — it must NOT satisfy the gate. Only submitted states count (COMMENTED, APPROVED, CHANGES_REQUESTED, DISMISSED).

- **If the count is >= 1**: a qualifying review exists for the current HEAD. The review's inline comments will surface as NEW comments via Step 1 / Agent B (remember: those comments carry login "Copilot", and Agent B paginates with `--paginate` + checks unresolved threads) and get triaged through row 1 of the decision gate (the existing comment-triage machinery). Once all of them are triaged and no Copilot thread is left unresolved, the next iteration routes to Step 4 (row 5). Do not merge from this step.
- **If the count is 0**: the reviewer has not yet reviewed the current HEAD. Then:
  1. Log: `"waiting for copilot-pull-request-reviewer[bot] review on <HEAD>"` (substitute the actual HEAD SHA).
  2. **Re-request the reviewer** if EITHER (a) it has been ~2 iterations since the review was requested with no qualifying review, OR (b) the reviewer has dropped from the PR's requested-reviewers list:
     ```bash
     # Check whether the reviewer is still in the requested list:
     gh pr view {{PR_NUMBER}} --json reviewRequests -q '.reviewRequests[].login'
     # If absent (or the ~2-iteration threshold is hit), re-request:
     gh api --method POST \
       repos/{{REPO}}/pulls/{{PR_NUMBER}}/requested_reviewers \
       -f "reviewers[]=copilot-pull-request-reviewer[bot]"
     ```
     On GitHub Enterprise, add `--hostname <host>` to the `gh api` call, where `<host>` is `state.ghe_hostname` (non-null).
     A pushed fix (HEAD moved) also warrants a re-request, since the prior review no longer applies to the new HEAD.
  3. Append an iteration record with `action: "waiting_copilot_review"` and the HEAD SHA in `details`. Update state, **exit the iteration** — the cron retries next cycle.

---

## Step 4: Merge

**All gates must pass. If ANY gate fails, exit iteration — next cron fire retries.** Gates 1–4 always apply; Gate 5 applies ONLY when `{{COPILOT_AVAILABLE}}` is true (when false — Copilot not installed/assignable on this repo — it is skipped entirely and behavior is unchanged).

### Gate 1 — CI green FOR THE HEAD COMMIT

**This is the most critical gate. DO NOT SKIP OR WEAKEN.**

```bash
# Step 1: Get the exact HEAD commit SHA
HEAD_SHA=$(git rev-parse HEAD)
echo "HEAD SHA: $HEAD_SHA"

# Step 2: Get the PR's head SHA as GitHub sees it
PR_HEAD=$(gh pr view {{PR_NUMBER}} --json headRefOid -q .headRefOid)
echo "PR HEAD: $PR_HEAD"

# Step 3: Verify they match — if not, the PR has a push that GitHub hasn't processed yet
# If they don't match, exit iteration — wait for GitHub to catch up.

# Step 4: Get checks
gh pr checks {{PR_NUMBER}}
```

**Verification rules depend on whether `state.required_checks` is populated:**

**Case A — `required_checks` is non-empty (branch protection defines required checks):**
1. `HEAD_SHA` must match `PR_HEAD`. If they differ, exit iteration (GitHub hasn't registered the latest push yet).
2. Every required check (from `state.required_checks`) must appear in the output. If a required check is **absent**, exit iteration — CI hasn't started yet.
3. Every required check must show as **passed (SUCCESS)** or **skipped (NEUTRAL)**.
4. Do NOT merge if any required check is pending, in_progress, queued, or absent.
5. Do NOT merge with only some required checks green and others still pending/running.

A NEUTRAL/skipped conclusion means the check's path filters excluded all changed files — this is valid for docs-only or unrelated-scope PRs and counts as passing.

**Case B — `required_checks` is empty (no branch protection on `{{BASE_BRANCH}}`):**

When nothing is required, **Gate 1 is a no-op — it always passes**. Do NOT wait for checks; do NOT investigate failing checks; do NOT treat any check as a blocker. If the user wanted these enforced, they'd configure branch protection.

1. `HEAD_SHA` must match `PR_HEAD`. If they differ, exit iteration.
2. Gate 1 passes unconditionally. Proceed to Gate 2 (comment re-check).

Rationale: the frequent failure mode this skill has hit is "sitting there when nothing is blocking" — waiting on governance checks that never fire, or investigating check failures that the repo has accepted as noise (e.g. a security scanner that fails on every PR). If there's no branch protection, the team's merge signal is **reviewer comments + admin merge**, not CI. Respect that.

Exception — if the user wants specific checks enforced on a no-branch-protection repo, they can pass `--required-checks <name1>,<name2>` to ship-it at invocation; that list becomes `required_checks` and Case A applies.

**If Gate 1 fails for ANY reason, exit iteration. Do NOT proceed to Gate 2.**

### Gate 2 — Fresh comment re-check
```bash
gh pr view {{PR_NUMBER}} --json reviews,comments,reviewDecision
gh api --paginate repos/{{REPO}}/pulls/{{PR_NUMBER}}/comments
```
Re-fetch ALL comments right now (the REST comments endpoint caps at 30/page — `--paginate` is REQUIRED so a reviewer comment past the first page is not silently merged over). CI can take many minutes — reviewers often post during that window. If there are ANY comments whose IDs are not in `triaged_comment_ids`, do NOT merge. Go back to Step 3b to triage them first.

### Gate 3 — All comments addressed
Verify that every comment ID from the PR is present in `triaged_comment_ids`. If there are untriaged comments, go back to Step 3b. `reviewDecision: CHANGES_REQUESTED` is NOT a blocker — reviewers often don't re-review promptly, and admin merge privileges (via the REST merge endpoint) bypass this GitHub status. The real safety check is Gate 2 (fresh comment re-check).

### Gate 4 — PR still open and mergeable
```bash
gh pr view {{PR_NUMBER}} --json state,mergeable
```

### Gate 5 — Copilot review on the EXACT current HEAD (only when `{{COPILOT_AVAILABLE}}` is true)

**Skip this gate entirely when `{{COPILOT_AVAILABLE}}` is false (Copilot not installed/assignable on this repo) — Step 4 then behaves exactly as before.**

When true, run the exact-login / current-HEAD assertion and ABORT the merge (exit iteration) if it returns 0:

```bash
HEAD=$(gh pr view {{PR_NUMBER}} --json headRefOid -q .headRefOid)
COUNT=$(gh api --paginate repos/{{REPO}}/pulls/{{PR_NUMBER}}/reviews \
  --jq ".[] | select(.user.login==\"copilot-pull-request-reviewer[bot]\" and .commit_id==\"$HEAD\" and .state != \"PENDING\") | .id" | wc -l | tr -d ' ')
# count lines (one .id per matching review) across ALL pages — NOT `[...] | length`, which with --paginate is computed per page (wrong total on >30-review PRs).
# --paginate: the reviews endpoint caps at 30/page — without it the Copilot review can be on a later page and be missed.
# .state != "PENDING": skip unsubmitted draft reviews; only submitted reviews (COMMENTED/APPROVED/CHANGES_REQUESTED/DISMISSED) satisfy the gate.
# COUNT must be >= 1. If 0, the reviewer has not reviewed the current HEAD —
# go to Step 3d (log "waiting for copilot-pull-request-reviewer[bot] review on $HEAD", re-request per the rule),
# update state, and EXIT the iteration. Do NOT merge.
```

On GitHub Enterprise, add `--hostname <host>` to the `gh api` calls, where `<host>` is `state.ghe_hostname` (non-null).

Additionally, every NEW inline comment from that HEAD review must already be triaged (its id present in `triaged_comment_ids`) AND no Copilot review thread may be left unresolved. Gate 2 (fresh comment re-check) enforces this — but ONLY if it paginated and matched the right login: re-fetch with `gh api --paginate` (comments cap at 30/page) and match Copilot comments by login "Copilot" (the reviews API uses the bot slug; the comments API uses "Copilot"). Also re-run the GraphQL `reviewThreads { isResolved }` check (the variable-based `-F owner/-F repo/-F pr` query from Step 1 / Agent B — never string-interpolated or quote-escaped), looping the `pageInfo { hasNextPage, endCursor }` cursor until exhausted so ALL threads are covered (>100 threads spill onto later pages) — any unresolved thread routes back to Step 3b. If any comment from the qualifying review is untriaged or any thread is unresolved, do NOT merge; triage it first. A review on a SUPERSEDED commit does not satisfy this gate; a pushed fix resets it to the new HEAD.

### Execute Merge

Only after all applicable gates pass, merge via the REST API (NOT `gh pr merge`, which checks out the base branch locally and fails in a multi-worktree layout — see SKILL.md → [Executing the Merge](../SKILL.md#executing-the-merge)). The REST endpoint merges entirely server-side and honors the caller's admin privileges automatically:
```bash
# On GitHub Enterprise, add `--hostname <host>` to the `gh api` call, where `<host>` is `state.ghe_hostname` (non-null).
gh api --method PUT repos/{{REPO}}/pulls/{{PR_NUMBER}}/merge -f merge_method=squash
# Response: {"sha": "...", "merged": true, "message": "..."}
```
Branch deletion is usually handled by the merge (GitHub auto-deletes on squash-merge when the repo is configured for it). If the repo does NOT auto-delete, delete the branch explicitly:
```bash
# On GitHub Enterprise, add `--hostname <host>` to the `gh api` call, where `<host>` is `state.ghe_hostname` (non-null).
gh api --method DELETE repos/{{REPO}}/git/refs/heads/{{BRANCH}}
```
A 422 "Reference does not exist" response from the DELETE means the branch was already deleted — treat as success.

### Post-Merge

1. Cancel cron: `CronDelete(id=<cron_id from state>)`
2. Verify: `gh pr view {{PR_NUMBER}} --json state,mergedAt`
3. Update state: set `status: "merged"`, write state
4. Output the Final Report
5. **Cleanup workspace**: `rm -rf .ship-it/pr-{{PR_NUMBER}}`

---

## Final Report

Write this to `.ship-it/pr-{{PR_NUMBER}}/report.md` AND output to conversation:

```
## Ship It Report — PR #{{PR_NUMBER}}

### Result: MERGED / ABORTED

### Iterations: N of {{MAX_ROUNDS}}
| Iter | CI Status | Comments Fixed | Comments Rejected | Human Rejections | Commits |
|------|-----------|---------------|-------------------|-----------------|---------|

### Comment Triage Summary
- Implemented: N (valid suggestions fixed)
- Rejected (bot): N (replied with rationale)
- Rejected (human): N (replied with rationale, flagged as automated)
- Skipped: N (trivial nits)
- Blocked: N (fix would break verification)

### CI Fixes
- Iter N: <description>

### Merge
- Merged at: <timestamp>
- Branch deleted: {{BRANCH}}
- Cron job cancelled: <cron_id>
```

---

## State File Update Protocol

**IMPORTANT**: All state file reads and writes MUST use the **Bash tool** (`cat` to read, `printf '%s' '...' > file` to write), NOT the Read/Write/Edit tools. Write/Edit tools render diffs in the user's terminal, polluting the main output with noisy state updates every iteration.

At the END of every iteration, update `.ship-it/pr-{{PR_NUMBER}}/state.json`:

1. Read current state (`cat .ship-it/pr-{{PR_NUMBER}}/state.json`)
2. Update `iteration` count
3. Append to `history`:
   ```json
   {
     "iteration": N,
     "action": "triage_comments|fix_ci|rebase|merge|waiting_ci|waiting_review|waiting_copilot_review|waiting_mergeable",
     "details": "...",
     "comments_fixed": 0,
     "comments_rejected": 0,
     "human_rejections": 0,
     "ci_fixed": false,
     "rebased": false,
     "pre_rebase_sha": null,
     "new_head_sha": null
   }
   ```
4. Update `triaged_comment_ids` (append, never remove)
5. Update `ci_fix_attempts` if CI was fixed
6. Increment `rebase_attempts` if a rebase was attempted (whether REBASED or ESCALATE)
7. Update `status` if terminal (merged/aborted); set `abort_reason` when aborting (`merge_conflict`, `max_rounds`, `ci_failure_repeat`, etc.)
8. Write state back to disk
