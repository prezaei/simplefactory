---
name: ship-it
description: |
  Autonomous PR iteration loop: addresses review comments, waits for CI, iterates
  until green with no unresolved threads, then squash-merges (using admin merge privileges) and deletes the branch.
  Takes an existing PR number as input. Designed for autonomous execution after
  fix, feature-dev, or similar skills create a PR.
  Uses an agent swarm — all iteration work is delegated to agents so main context stays clean.
  Use when user says "ship it", "merge this PR", "land this PR", "address the review comments and merge",
  "iterate on PR #N until green", or "get this PR merged".
---

# Ship It

Print this header when the skill is first invoked:

```
      ·  *   .       *     ·        .     *
   .        ____                  ·
          /     \_____             *    .
    *    / SHIP IT    \_____          ·
        |___________________>==-  *
         \     /----         ·
    ·     \___/        ·   *      .
        *       .   ·        *
```

Autonomous PR shepherd that drives a pull request from "open with comments" to "merged." Uses an **agent swarm** pattern — main context does only pre-flight setup, then all iteration work runs in delegated agents with state persisted to disk.

**Runs AUTONOMOUSLY through ALL phases without stopping for user input.** Only stop if genuinely blocked (conflicting reviewer demands, max iterations exceeded, or a merge conflict that a rebase can't resolve mechanically — see [Merge Conflict Handling](#merge-conflict-handling)).

### State File I/O: Use Bash, Not Write/Edit

**CRITICAL**: All reads and writes to `.ship-it/pr-<N>/state.json` and `.ship-it/pr-<N>/playbook.md` MUST use the **Bash tool** (e.g., `cat`, `printf '%s' '...' > file`), NOT the Write or Edit tools. The Write/Edit tools render full diffs in the user's output, which pollutes the main context with noisy red/green state file updates every iteration. Bash writes are silent.

```bash
# GOOD — silent, no diff in output
printf '%s' '{"pr_number": 123, "iteration": 1, ...}' > .ship-it/pr-123/state.json

# BAD — shows full diff in user's terminal every time
# Write(file_path=".ship-it/pr-123/state.json", content="...")
```

This applies to the main context AND to all sub-agents (Lead Agent, CI Fix Agent, Comment Triage Agent, etc.).

## When to Use

- "ship it", "merge this PR", "land this PR"
- "address the review comments and merge"
- "iterate on PR #N until it's green"
- "get this PR merged"
- After `/fix`, `/feature-dev`, or similar skills create a PR

## When NOT to Use

- PR needs design-level rework (not just feedback fixes)
- PR targets a release branch — use manual merge

(Merge conflicts are no longer an exclusion — Ship It attempts a rebase and only escalates when conflicts need human judgment. See [Merge Conflict Handling](#merge-conflict-handling).)

## Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `<number>` | (none) | PR number. Auto-detects from current branch if omitted. |
| `--max-rounds N` | 20 | Maximum iteration rounds before giving up |
| `--required-checks <name>[,<name>...]` | (auto from branch protection) | Override the required-check list. Use on no-branch-protection repos to still enforce specific checks, or to override branch protection when you know a particular required check is broken. Pass an empty string (`--required-checks ""`) to explicitly require nothing. |

> **Copilot review is automatic, not a flag.** Whenever Copilot is available on the repo, Ship It always requests the Copilot reviewer and waits for its review on the current HEAD before merging (see [Phase 1](#phase-1-pre-flight-main-context) step 6 and [Step 3d / Gate 5 in the playbook](references/iteration-playbook-template.md)). On repos where Copilot is not installed/assignable, the gate is skipped automatically — Ship It never hangs waiting for a review that can't happen.

---

## Architecture: Agent Swarm

Main context does only pre-flight, then delegates everything to agents. State is persisted to `.ship-it/pr-<N>/state.json` so each cron-fired agent picks up where the last left off.

```
Main Context (lightweight — exits after setup)
  |-- Pre-flight validation
  |-- Write state.json + playbook.md
  |-- Create cron job (every 3 min)
  +-- Dispatch first iteration -> Lead Agent
                                      |
Lead Agent (each iteration)           |
  |-- Read state.json                 |
  |-- Spawn parallel sub-agents:      |
  |     |-- CI Status Agent           |
  |     +-- Comment Fetch Agent       |
  |-- Decision gate                   |
  |-- Spawn conditional sub-agents:   |
  |     |-- CI Fix Agent              |
  |     +-- Comment Triage Agent      |
  |-- Update state.json               |
  +-- If merge-ready -> execute merge  |
                                      |
Cron fires -> same Lead Agent flow ---+
```

---

## Phase 1: Pre-flight (Main Context)

Run these checks, then hand off to agents. Abort with a clear error if any fail.

1. **Parse arguments**: Extract PR number from args. Extract `--max-rounds N` (default 20). Extract `--required-checks` (comma-separated; explicit empty string = no checks required). (There is no Copilot flag — the Copilot review gate is automatic and gated on availability; see step 6.)
2. **Determine PR number**: from argument, or `gh pr view --json number -q .number`
3. **Validate PR state**:
   ```bash
   gh pr view <number> --json number,title,state,headRefName,baseRefName,mergeable
   ```
   Must be `OPEN`. If merged/closed, abort.
4. **Checkout PR branch**: `git fetch origin <headRefName> && git checkout <headRefName> && git pull`
5. **Get repo identifier and hostname**:
   ```bash
   gh repo view --json nameWithOwner -q .nameWithOwner
   # Determine hostname from the origin remote (required for `gh api` on GitHub Enterprise —
   # `gh api` does NOT auto-detect the host the way `gh pr/repo/issue` do).
   git remote get-url origin | sed -E 's#^(git@|https?://)##; s#[:/].*##'
   ```
   If the hostname is not `github.com`, pass `--hostname <host>` to every `gh api` call below AND record it in `state.json` as `ghe_hostname` so sub-agents route to the right host.

6. **Identify required checks** — the ONLY authoritative sources are (1) the `--required-checks` argument from step 1, or (2) the repo's branch protection config (classic branch protection **or** rulesets). Do NOT infer required checks from which checks happened to pass on prior merged PRs — that's a guess, not a requirement.

   **If `--required-checks` was passed in step 1**, use that list verbatim and skip the API calls below. An explicit empty string means no checks are required.

   Otherwise, query BOTH classic branch protection AND rulesets — modern GitHub repos (especially org-managed and GHE) often use rulesets instead of classic protection. Only the union of both is authoritative.

   **Step 6a — Classic branch protection:**

   ```bash
   # On GitHub Enterprise, add `--hostname <host>` (the value detected in step 5) to the `gh api` call.
   gh api repos/<owner>/<repo>/branches/<base_branch>/protection \
     --jq '.required_status_checks.contexts // []' 2>&1
   ```

   | Response | Meaning |
   |----------|---------|
   | JSON array (possibly empty) | Classic protection exists. Capture contexts. |
   | `"Branch not protected"` (HTTP 404) | Classic protection not configured. Continue to 6b. |
   | Generic `"Not Found"` (HTTP 404) | **You hit the wrong host** — likely github.com instead of GHE. Re-run with `--hostname`. |
   | 401/403 | Can't read classic protection. Continue to 6b; 6c reconciles. |

   **Step 6b — Rulesets (new GitHub mechanism):**

   ```bash
   # On GitHub Enterprise, add `--hostname <host>` (the value detected in step 5) to the `gh api` call.
   gh api repos/<owner>/<repo>/rules/branches/<base_branch> \
     --jq '[.[] | select(.type == "required_status_checks") | (.parameters.required_status_checks // []) | .[].context]' 2>&1
   ```

   Rulesets can be defined at the repository, organization, or enterprise level — this endpoint returns the effective union of all active rulesets for the branch.

   | Response | Meaning |
   |----------|---------|
   | JSON array (possibly empty) | Capture contexts from the `required_status_checks` rule type. |
   | HTTP 404 | No rulesets apply to this branch. |
   | 401/403 | Can't read rulesets. Continue to 6c; 6c reconciles. |

   **Step 6c — Combine:**

   - If BOTH 6a and 6b returned 401/403, stop and ask the user which checks are required (they can retry with `--required-checks <name1>,<name2>`). Do NOT silently treat a double denial as "no required checks".
   - Otherwise, union the contexts from the successful results (treat a 401/403 side as "no contributions"; a 404 side contributes an empty list). Deduplicate, for example:

     ```bash
     required_checks=$(jq -n --argjson a "$classic_checks" --argjson b "$ruleset_checks" '($a + $b) | unique')
     ```
   - If BOTH 6a and 6b returned empty (or 404), there is genuinely no required-check configuration → `required_checks: []`.

   **When `required_checks` is empty, CI is not gated:**
   - The merge gate is reviewer comments + an admin-privileged merge (via the REST endpoint). CI is informational.
   - Do NOT wait for any check — pending checks don't block, and neither do failing checks. The team's merge signal is human review, not CI. If they wanted CI gated, they'd have configured it.
   - Report this explicitly in the "Ship It started" message: "No required checks configured on `<base>` — CI is informational only. Will merge once all review comments are addressed."

   **Do NOT silently fall back to "all listed checks must pass" when `required_checks` is empty.** That fallback has caused Ship It to wait indefinitely on checks that were never required — e.g. enterprise governance checks that fire asynchronously, security scanners the team has accepted as noise, or checks that only appear on some PRs. Green on a prior PR ≠ required on this PR.

   **Escape hatch:** the user can invoke with `--required-checks <name1>,<name2>` (see Arguments) to override the resolved list — useful when both 6a and 6b are empty but specific checks should still be enforced.

   Record the resolved list in `state.json` as `required_checks`.

   **Always probe for Copilot availability and request the reviewer now** (no flag — this is default behavior). Attempt to request the fixed Copilot reviewer login during pre-flight so the review actually gets triggered, then record whether Copilot is usable on this repo:

   ```bash
   # On GitHub Enterprise, add `--hostname <host>` (the value detected in step 5) to the `gh api` call.
   gh api --method POST \
     repos/<owner>/<repo>/pulls/<number>/requested_reviewers \
     -f "reviewers[]=copilot-pull-request-reviewer[bot]"
   ```

   - **If it SUCCEEDS** (Copilot newly requested, already a requested reviewer, or has already reviewed) → set state `copilot_available: true`. The merge gate (playbook Step 3d / Gate 5) will wait for a Copilot review on the current HEAD.
   - **If it FAILS** (e.g. HTTP 422 — Copilot is not installed/assignable on this repo) → set state `copilot_available: false` and log: `"Copilot reviewer unavailable on this repo; proceeding without the Copilot gate"`. Do NOT block, do NOT abort — the gate is simply skipped for this run. This is the "if available" requirement: Ship It never hangs on a repo without Copilot.

   The reviewer login `copilot-pull-request-reviewer[bot]` is fixed (used for the `reviews` API check — query it with `gh api --paginate .../pulls/<N>/reviews` and require `state != "PENDING"` so a 30-per-page truncation or an unsubmitted draft never breaks the gate). Note the gotcha: inline review **comments** from Copilot appear under login `"Copilot"` (not the bot slug) in the REST comments API — see the playbook's comment-fetch step.
7. **Create workspace**: `mkdir -p .ship-it/pr-<number>`
8. **Write state file** via Bash (NOT Write tool — see State File I/O rule above):
   ```bash
   printf '%s' '{"pr_number": <number>, "repo": "<owner/repo>", "ghe_hostname": "<hostname or null>", "branch": "<headRefName>", "base_branch": "<baseRefName>", "required_checks": [<list from step 6>], "copilot_available": <true|false>, "max_rounds": <max_rounds>, "iteration": 0, "cron_id": null, "triaged_comment_ids": [], "ci_fix_attempts": {}, "rebase_attempts": 0, "status": "in_progress", "history": []}' > .ship-it/pr-<number>/state.json
   ```
   Set `ghe_hostname` to the value from step 5 if it is not `github.com`; otherwise `null`. Sub-agents read this and pass `--hostname` on every `gh api` call.
   **Unset values must be the JSON literal `null` (unquoted), never the string `"null"`.** The placeholders above are shown quoted (e.g. `"<hostname or null>"`) for readability — when a value is absent, emit bare `null` (e.g. `"ghe_hostname": null`) so `jq` reads it as null, not the four-character string `"null"`. Same rule for any other field whose value can be absent.
   Set `copilot_available` to the boolean resolved by the availability probe in step 6 (`true` if the reviewer request succeeded / Copilot is assignable, `false` if it failed — e.g. 422). When `false`, the Copilot merge gate is skipped and behavior matches the pre-Copilot version.
9. **Write iteration playbook** — `.ship-it/pr-<number>/playbook.md`:
   Read the template at `references/iteration-playbook-template.md` (relative to this skill directory) and fill in all `{{PLACEHOLDER}}` values with the actual PR details. Write the result to the playbook path. This playbook is the self-contained instruction set that each iteration agent executes. The `{{COPILOT_AVAILABLE}}` placeholder is filled from the `copilot_available` state field (`true`/`false`); when `false` the Copilot gate is skipped, preserving the old behavior.
10. **Create cron job**:
    ```
    CronCreate(
      cron="*/3 * * * *",
      recurring=true,
      prompt="You are the Ship It Lead Agent. Read and follow the iteration playbook at .ship-it/pr-<number>/playbook.md — it contains all instructions for one iteration cycle. Read .ship-it/pr-<number>/state.json for current state. Execute exactly one iteration, update the state file, and exit."
    )
    ```
    **Update `state.json`** with the returned cron job ID in the `cron_id` field.
11. **Dispatch first iteration immediately** — don't wait for the first cron fire:
    ```
    Agent(subagent_type="general-purpose", prompt="You are the Ship It Lead Agent. Read and follow the iteration playbook at .ship-it/pr-<number>/playbook.md — it contains all instructions for one iteration cycle. Read .ship-it/pr-<number>/state.json for current state. Execute exactly one iteration, update the state file, and exit.")
    ```
12. **Report to user**: "Ship It started for PR #N. Iterating every 3 minutes. I'll report back when it's merged or if something blocks." When `copilot_available` is `true`, add: "Copilot review requested — will wait for it on the current HEAD before merging." When `copilot_available` is `false`, add: "Copilot reviewer unavailable on this repo; proceeding without the Copilot gate."

**Main context is now done.** All further work happens in agents via the cron loop.

---

## Iteration Cycle Overview

Each iteration (first dispatch + every cron fire) follows the same playbook:

1. **Gather state in parallel** — spawn CI Status Agent + Comment Fetch Agent simultaneously
2. **Always check for new comments** — regardless of CI state, comments are triaged first because reviewers post at any time
3. **Decision gate** — route to the right fix agent or proceed to merge
4. **Verify before push** — every fix agent runs the repo's quality gates (from AGENTS.md) before pushing (hill-climb guard is built into each agent, not a separate step)
5. **Update state** — write triaged comment IDs, CI fix attempts, iteration history

The full iteration logic with all agent prompts lives in `references/iteration-playbook-template.md`. The playbook is written to disk during pre-flight so cron-fired agents can read it without needing the skill loaded.

---

## Key Design Decisions

### State persistence
Each cron fire is a new session with no memory of prior iterations. The `state.json` file bridges this gap — every iteration reads it first and writes it back after. The `triaged_comment_ids` array prevents re-triaging comments; `ci_fix_attempts` tracks retry counts per failure type.

### Comment-first priority
Comments are always checked and triaged before anything else, even when CI is still pending. Reviewers don't wait for CI to post feedback, so neither should we. This prevents wasted iterations where comments sit unaddressed.

### Human vs bot comment handling
The triage agent distinguishes between human reviewers (MEMBER/COLLABORATOR) and bots. Human reviewer suggestions get higher trust and more respectful handling. When the agent disagrees with a human reviewer, the reply is flagged as automated so the reviewer knows to push back if needed.

### Hill-climb guard
Verification (repo's quality gates — see `AGENTS.md` → PR Quality Checklist) is built into each fix agent's prompt — they verify BEFORE pushing, not after. If verification fails, they revert the specific fix and report BLOCKED rather than pushing broken code.

### Workspace cleanup
The `.ship-it/pr-<N>/` workspace is deleted after terminal states (merged or aborted). The final report is output to the conversation before cleanup, so nothing is lost. The `.ship-it/` directory should be gitignored to prevent accidental commits of ephemeral state.

---

## Merge Conflict Handling

PRs go stale while Ship It is running — base branch moves, `uv.lock` churns, proto stubs regenerate on other PRs. **Most conflicts are mechanical.** Ship It attempts a rebase and only escalates when a human judgment call is actually needed.

### When to run the rebase flow

The **Rebase Agent** (see the playbook) runs when either of these is true:
- `gh pr view --json mergeable` returns `CONFLICTING`
- A `git push` from a fix/triage agent fails with "non-fast-forward" (base moved under us)

### Decision rule

After `git rebase origin/<base_branch>`, classify each conflicted file:

| Class | Examples | Action |
|-------|----------|--------|
| **Regenerable** | `uv.lock`, `**/*_pb2*.py*` (protobuf stubs), `package-lock.json`, `yarn.lock` | Accept base side (`git checkout --theirs <file>`) then regenerate via the authoritative command (`uv lock`, `make protos`, `npm install`, etc.). Stage the regenerated artifact. |
| **Additive / non-overlapping** | New import line, new dict/list entry, new enum member, new test — both sides added different things in the same region | Take union of both sides. Re-run lint/format so ordering is deterministic. |
| **Substantive** | Both sides modified the same function body, the same config value, the same control flow | **STOP. Abort the rebase.** Escalate to the user. |

A conflict is **substantive** — i.e. requires user attention — if ANY of the following is true:
1. The same line was edited on both sides with different semantic intent (not just formatting/imports).
2. Either side's change depends on logic in the other side's change (resolving requires understanding both features).
3. A test's expected behavior conflicts with a code change that alters that behavior.
4. The resolution would require discarding or substantially rewriting either side's intent.

When in doubt, escalate. A false-escalation wastes a notification; a false-auto-resolve corrupts someone's work.

### On mechanical success

1. Resolve, `git add`, `git rebase --continue` until clean.
2. Run the repo's quality gates (AGENTS.md → PR Quality Checklist). If any fail, `git rebase --abort` and escalate — the rebase is no longer mechanical.
3. `git push --force-with-lease origin <branch>`. Never plain `--force`.
4. The next iteration re-checks CI against the new HEAD.

### On substantive conflict (escalation)

1. `git rebase --abort` — leave the branch in its pre-rebase state.
2. Set `status: "aborted"` with `abort_reason: "merge_conflict"` in state.
3. Cancel cron.
4. Write a Final Report that lists:
   - Each conflicted file and the conflicting commit(s) on both sides
   - The conflict hunks (from `git diff` during the abort) so the user can resolve without re-running
   - Suggested next step: "resolve manually, push, re-run `/ship-it <N>`"
5. Cleanup workspace.

---

## Error Handling

| Situation | Action |
|-----------|--------|
| PR not found | Abort with clear error |
| PR already merged | Cancel cron, report "already merged" |
| PR closed (not merged) | Abort — don't reopen |
| Same CI failure 3x | Abort — deeper issue, cancel cron |
| Merge conflicts — mechanical only (lockfiles, generated stubs, imports) | Rebase onto base, resolve, force-push-with-lease, continue |
| Merge conflicts — substantive (overlapping logic) | Abort rebase, escalate to user with conflict summary, cancel cron |
| Check conclusion is `ACTION_REQUIRED` | Do NOT treat as a single outcome. Fetch the check's `output.title` via `gh api repos/.../commits/<sha>/check-runs` and classify (see Guardrails) |
| All comments addressed but reviewer hasn't re-reviewed | Merge anyway — admin merge privileges (via the REST endpoint) bypass `CHANGES_REQUESTED` |
| New comments at merge time | Go back to triage, do NOT merge |
| `gh pr merge` fails with "'main' is already used by worktree" | Use the REST API directly (see [Executing the Merge](#executing-the-merge)). `gh pr merge` tries to checkout the base branch locally; that fails when this repo has sibling worktrees. |
| Merge fails with a permissions error | Report error — may lack admin rights on the branch |
| Rate limiting | Back off with 10s delays |

## Executing the Merge

`gh pr merge <N> --admin --squash --delete-branch` will fail in a secondary git worktree with:

```
failed to run git: fatal: 'main' is already used by worktree at '<other path>'
```

This is because `gh pr merge` tries to checkout and update the base branch locally. In a multi-worktree layout (common when iterating on multiple PRs in parallel), that always fails.

**Always use the REST API** — it does the merge entirely server-side with no local git manipulation:

```bash
# On GitHub Enterprise, add `--hostname <host>` to the `gh api` call, where `<host>` is `state.ghe_hostname` (non-null).
gh api --method PUT \
  repos/<owner>/<repo>/pulls/<N>/merge \
  -f merge_method=squash
# Response: {"sha": "...", "merged": true, "message": "..."}
```

`--admin` equivalents are not needed — the REST endpoint honors admin privileges from the caller's token automatically.

Branch deletion is typically handled by the merge (GitHub auto-deletes on squash-merge when configured). If the PR's repo doesn't auto-delete, explicitly:

```bash
# On GitHub Enterprise, add `--hostname <host>` to the `gh api` call, where `<host>` is `state.ghe_hostname` (non-null).
gh api --method DELETE \
  repos/<owner>/<repo>/git/refs/heads/<branch>
```

A 422 "Reference does not exist" response from the DELETE means the branch was already deleted — treat as success.

## Guardrails

- **Never force-push to `main` or any protected/release branch.** The ONLY time force-push is allowed is the rebase flow on the PR's own feature branch, and it MUST use `--force-with-lease` so a concurrent reviewer push is never clobbered. See [Merge Conflict Handling](#merge-conflict-handling).
- **NEVER merge unless CI is green for the HEAD commit** — every required check (from `state.required_checks`) must show SUCCESS, NEUTRAL, or SKIPPED for the exact HEAD SHA. If a required check is absent, pending, or shows results for a stale commit, DO NOT merge — exit the iteration and wait. This is the single most important guardrail.
- **When `copilot_available` is true** (the default on Copilot-enabled repos), NEVER merge unless `copilot-pull-request-reviewer[bot]` has *submitted* a review (`state != "PENDING"` — a PENDING draft does not count) whose `commit_id` equals the exact current HEAD SHA AND every new comment from that review is triaged. PAGINATE both APIs with `--paginate` (the reviews API AND the comments API each cap at 30/page — without pagination the Copilot review/comment can be on a later page and silently missed) and check review-thread resolution via GraphQL (loop the `reviewThreads` cursor to cover ALL threads) — see the playbook. The Copilot-on-HEAD check is computed in the playbook's [Step 3d](references/iteration-playbook-template.md) (wait-check) and [Step 4 Gate 5](references/iteration-playbook-template.md) (merge gate). A review on a superseded commit does NOT count; a pushed fix resets the gate to the new HEAD (re-request the reviewer). When Copilot is unavailable on the repo (`copilot_available: false`), this gate is skipped and behavior is unchanged.
- **`ACTION_REQUIRED` is a bucket code, not a diagnosis.** Never interpret it in isolation. The same conclusion covers at least three different real states: (1) the check was *skipped because the branch has merge conflicts with the base* (title: `"Skipped due to merge conflicts"`), (2) the check is *waiting for a human to approve the pipeline run* (ADO PR pipelines on GHE do this for external contributors), (3) the check is a *compliance gate asking for a human click* (e.g. GHE `GitOps/GitHubPop` with title `"Proof of presence"`). **Always** fetch `output.title` and `output.summary` from the Check Runs API before classifying:
  ```bash
  # On GitHub Enterprise, add `--hostname <host>` to the `gh api` call, where `<host>` is `state.ghe_hostname` (non-null).
  gh api repos/<owner>/<repo>/commits/<head_sha>/check-runs \
    --jq '.check_runs[] | select(.name=="<name>") | {conclusion, title: .output.title, summary: .output.summary}'
  ```
  Classify by title:
  - `"Skipped due to merge conflicts"` → treat as **merge conflict** → run the rebase flow.
  - `"Proof of presence"` or similar human-click compliance gate → non-blocking if not in `required_checks`; otherwise escalate to user.
  - Pipeline-approval gate (e.g. "Awaiting approval") → escalate to user; do NOT retry autonomously.
  - Anything else → treat as **failure** and surface the title/summary verbatim so the CI Fix Agent has the real error.

  The cost of treating `ACTION_REQUIRED` as a single "waiting" state is catastrophic: the cron polls forever while the PR is actually blocked on a conflict or a decision the user can fix in seconds.
- **Requeue CI on infrastructure/flaky failures** — when a CI failure is diagnosed as infrastructure/flaky (not a code issue), requeue the failed run instead of just waiting. On GitHub Actions: `gh run rerun <run-id> --failed`. Stale red CI won't auto-retry.
- **Merge when all comments are addressed** — `CHANGES_REQUESTED` is not a blocker if every comment has been triaged (addressed, rejected with rationale, or skipped). Reviewers often don't re-review promptly; admin merge privileges (via the REST merge endpoint) bypass this GitHub status.
- **Never merge without a fresh comment re-check** — always re-fetch right before merge.
- **Never modify files outside the PR's diff scope.**
- **Never delete or rewrite git history** — only add new commits.
- **Never blindly implement review suggestions** — triage first.
- **Respect human reviewers** — auto-rejections of human comments are flagged as automated.
- **Verify before push** — the repo's quality gates (per `AGENTS.md` → PR Quality Checklist) must pass before ANY push.
- **Maximum rounds enforced** (default 20) — prevents infinite loops.
- **Always reply to comments** — every fix or rejection gets a reply.
