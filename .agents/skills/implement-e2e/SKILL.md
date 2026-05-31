---
name: implement-e2e
argument-hint: describe the feature/change to implement end-to-end (or pass an existing PR/branch/issue ref)
description: |
  End-to-end feature delivery loop with the tightest possible quality gate: design → implement →
  a risk-scaled review gauntlet (/code-review → /adversarial-investigation → /simplify → /code-review →
  /adversarial-investigation) → open a PR + request Copilot review → react to EVERY Copilot comment in
  code (never defer) → push → re-request review while running /code-review again → hand to /ship-it with
  a Copilot-on-HEAD merge gate → merge only when CI is green, the PR is mergeable, and Copilot's review
  of the CURRENT HEAD is clean. Fixes are NEVER deferred; the loop re-reviews after every change so a fix
  can't smuggle in a new bug. Use when the user says "implement this end to end", "implement-e2e", "build
  this feature and don't let any bugs through", "full review gauntlet then ship", "run implement-e2e on
  this PR", or asks to build/ship a feature with the complete review-and-ship pipeline.
---

# Implement E2E

The tightest feature-delivery loop in the toolbox. It exists for one reason: **ship great code with no
bugs getting through.** Every stage either finds defects or proves there are none, and **no suggested
fix is ever deferred** — if a review or adversarial pass raises it, you fix it in code now (or prove it
is a false positive with evidence and rebut it inline). Changes get re-reviewed, because the most
common bug source in this loop is *the previous fix*.

```
   APPLICABILITY → DESIGN → IMPLEMENT → REVIEW GAUNTLET → PR + COPILOT-GATED SHIP IT
```

The two sub-skills `/code-review` and `/simplify` are bundled skills; `/adversarial-investigation`,
`/architect-and-design`, `feature-engineer`, and `/ship-it` are project skills. All are invoked by the
names used here.

## Operating rules (apply to EVERY phase)

1. **Never defer a fix.** Every finding from `/code-review`, `/adversarial-investigation`, `/simplify`,
   or Copilot is either (a) fixed in code this pass, or (b) refuted with a concrete receipt and an
   explicit written rationale, replied inline. "Later", "follow-up", "out of scope", "P-next" are not
   allowed for **a defect the loop surfaced**. The only carve-out is a *genuinely separate feature the
   user did not ask for* — that is a different PR; name it explicitly and get the user's nod rather than
   silently folding it in or silently dropping the finding. When unsure which side a finding is on, treat
   it as a defect and fix it.
2. **Green gate between every change.** After any code change, run the repo's full quality gates
   **exactly as `AGENTS.md` → PR Quality Checklist lists them** — copy the commands from AGENTS.md rather
   than paraphrasing here, so this skill can never drift from the repo's actual gate (in THIS repo, at the
   time of writing: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build` — but
   AGENTS.md wins if it has changed). Run the **FULL** test suite and confirm **ZERO failures** — read the
   actual failure count, do not just eyeball that some suites said "ok". If anything is red, fix it before
   moving on. (For a change with no executable code — see Applicability — the code gates are N/A; say so.)
3. **Hunt flakes, don't tolerate them.** If a test fails in the full run but passes in isolation, it is
   a real defect (usually shared-state/ordering/timing). Root-cause and fix it; re-run the full suite
   2–3× to confirm stability. A flake that "usually passes" is a bug that ships.
4. **A fix can introduce a bug.** That is why the gauntlet re-runs review after `/simplify` and why the
   ship loop re-runs `/code-review` after every Copilot round. Treat each fix as new code that must clear
   the same bar.
5. **Reply where the feedback lives.** For an *inline* review comment, reply on that comment's thread —
   never a new top-level PR comment:
   `gh api --method POST repos/{owner}/{repo}/pulls/<N>/comments/<comment_id>/replies -f "body=..."`.
   Copilot's *review-summary body* or a *PR-level (issue) comment* has no inline `comment_id` to reply
   on — for those, one reply on the review/PR is acceptable (it is not the forbidden "new comment instead
   of using the thread"). Be specific: name the commit and what changed.
6. **Telemetry + tests are part of "done."** Per `AGENTS.md` (do not weaken it): every new function,
   decision, and error path has info-level logging,
   and every behavior in the design has a test. New code without tests is incomplete.

## Applicability — scope the loop to the change (read FIRST)

Before running anything, classify the work so you neither skip needed rigor nor perform theater:

- **Already an open PR/branch for this work?** (the user pointed you at a PR, or a branch+PR exist) →
  Do NOT start at Phase 0 or open a duplicate PR. Enter at **Phase 2** (review gauntlet on the existing
  diff) and then **Phase 4** (Copilot-gated ship). Phases 0–1 and the PR-open in Phase 3 are already done.
- **Substantive code/logic change** → run the full pipeline.
- **Trivial or docs-only change with no executable code** (markdown, comments, pure-doc files) → the
  code-specific stages are **N/A by design, not skipped lazily**: `/code-review`
  still applies (it reviews prose/config too), but `/adversarial-investigation` (a multi-agent *production
  problem* diagnosis team — its own description says "Do NOT use for straightforward" work) and the
  cargo green gate have nothing to act on. State explicitly which stages you are declaring N/A and why.
  This is the "no theater" corollary of "no defer": don't fabricate tests for prose or spin up an
  adversarial team where there is no runtime to investigate. A **constant, a config value, or a
  CI-workflow file is NOT trivial** — it can change runtime behavior, tests, or CI, so it goes through the
  full gauntlet and green gate like any code.

When in doubt about whether a change is "substantive," run the full gauntlet — over-rigor is cheap, a
missed bug is not.

## Phase 0 — Design first

(Skip if entering on an existing PR.) Run **`/architect-and-design`** (or `feature-engineer` for larger
work) on the requested feature before writing code, per `AGENTS.md` ("Design first"). Produce: the files
to add/change, the data flow, the test contract (design → tests → code, in that order), and the telemetry
plan. For a trivial change, a short written plan is enough — but do not skip thinking about the test
contract.

## Phase 1 — Implement

(Skip if entering on an existing PR.) Write the code to fulfill the design. Tests define the contract
first, then code fulfills it. When the implementation is complete, run the **green gate** (rule 2). Do
not proceed until it is fully green.

## Phase 2 — The review gauntlet (fixed order)

Run these in order. After EACH one, fix **every** finding (rule 1), then re-run the green gate (rule 2)
before the next stage. Apply the Applicability scoping — for a docs/trivial change, declare the
code-only stages N/A with a one-line reason rather than running them as theater:

1. **`/code-review`** — fix all findings.
2. **`/adversarial-investigation`** — fix all confirmed issues; resolve every Devil's-Advocate
   challenge with evidence. (N/A for docs/trivial changes — there is no runtime/logic to diagnose.)
3. **`/simplify`** — apply the cleanups (the stage most likely to introduce a regression, which is why
   review runs again next).
4. **`/code-review`** — fix all findings (including anything `/simplify` disturbed).
5. **`/adversarial-investigation`** — final pre-PR pass; fix everything; reach consensus/verdict.
   (N/A for docs/trivial changes.)

If any stage produces a fix, the *next* stage reviews that fix too — that is the point. Do not shortcut
the order for a substantive change even if an earlier stage was clean.

## Phase 3 — Open the PR and request Copilot review

(Skip if entering on an existing PR; just ensure Copilot is requested.)

1. Create a feature branch off `main`, commit, and push. Commit/push only the work for this feature;
   follow `AGENTS.md` commit/PR conventions.
2. Open the PR with `gh` — clear title + body describing the change, the design, and the test coverage.
3. **Request Copilot's review:**
   `gh api --method POST repos/{owner}/{repo}/pulls/<N>/requested_reviewers -f "reviewers[]=copilot-pull-request-reviewer[bot]"`.
4. Record the current HEAD SHA — the Copilot gate is keyed to it.

## Phase 4 — Copilot-gated ship (delegate the wait-loop to /ship-it)

The Copilot review is **asynchronous and external** — do not busy-loop or block the turn waiting for it.
Hand the wait-iterate-merge loop to **`/ship-it`**, which runs on a recurring cron (re-checks every few
minutes and resumes). **`/ship-it` now enforces the Copilot-on-HEAD gate natively** (see the ship-it
skill): when Copilot is available on the repo it automatically requests the review and will **not** merge
until Copilot has *submitted* a review of the **current** HEAD with every inline comment triaged — so
invoke `/ship-it` to own the request-wait-merge loop. (If Copilot isn't installed on the repo, `/ship-it`
records that and proceeds without the gate rather than hanging.) `/ship-it`'s built-in detection counts
**any** submitted review state (it treats `state != "PENDING"` as a hit, which includes `DISMISSED`) and
triages only inline `comments`. Steps 1–3 below are a **stricter operator layer** you run each iteration
*on top of* `/ship-it`: they (a) count only *active* submitted states (`COMMENTED`/`APPROVED`/
`CHANGES_REQUESTED`, excluding `DISMISSED`), (b) also triage the Copilot **review-summary body**, and
(c) run `/code-review` on every fix — none of which `/ship-it` does itself. **One CI caveat:** `/ship-it`
gates CI on the repo's branch-protection-required checks by default; since `AGENTS.md` requires **all** CI
checks green (not only the required subset — see the exit condition), pass the full check set via
`/ship-it --required-checks <name,…>` whenever branch protection lists fewer. The EXTRA MERGE GATE below
is the native behavior `/ship-it` enforces; steps 1–3 tighten it:

> **EXTRA MERGE GATE — do not merge until BOTH:** (a) Copilot has submitted a review whose `commit_id`
> equals the **current** PR head SHA, and (b) every new inline comment from that review is triaged —
> fixed in code (with the full green gate, rule 2) or rebutted inline per rule 5. A review on a
> *superseded* commit does NOT satisfy the gate.

On each ship-it iteration (cron-fired), the loop must:

1. **Verify Copilot reviewed the current HEAD:**
   ```bash
   HEAD=$(gh pr view <N> --json headRefOid -q .headRefOid)
   gh api --paginate repos/{owner}/{repo}/pulls/<N>/reviews \
     --jq ".[] | select(.user.login==\"copilot-pull-request-reviewer[bot]\" and .commit_id==\"$HEAD\" and (.state==\"COMMENTED\" or .state==\"APPROVED\" or .state==\"CHANGES_REQUESTED\")) | .id" | wc -l   # >= 1
   # Match active submitted states explicitly — NOT `.state != "PENDING"`, which also admits
   # DISMISSED reviews (no longer an active signal), letting a dismissed review satisfy the gate.
   # --paginate (the reviews endpoint caps at 30/page) + count lines, NOT `[...] | length`
   # (with --paginate, --jq runs per page, so length is a per-page count, not the total).
   ```
   If `0`: log "waiting for Copilot review on <HEAD>" and exit the iteration (the cron retries). If still
   absent after ~2 iterations, or Copilot dropped from the requested-reviewers list, re-request it.
2. **Triage EVERY new piece of feedback from that review (rule 1)** — fix in code (then green gate) or
   rebut inline (rule 5). Never silently skip one. This means BOTH (a) every inline comment
   (`pulls/<N>/comments`, login `Copilot`) AND (b) the review's **summary body** (`reviews[].body`,
   login `copilot-pull-request-reviewer[bot]`) when non-empty — Copilot routinely puts findings only in
   the review-summary body with no inline comment, so a summary-only finding must be triaged too or it
   slips through the gate. `/ship-it` triages inline comments; the review-summary body is the fixer's
   responsibility (like the `/code-review` pass below) — read it each round and fix or rebut every point.
3. **When a Copilot comment is fixed in code, run `/code-review` on that fix before pushing** (rule 4 — a
   fix is new code; Copilot and `/code-review` each catch what the other misses, and rounds routinely
   catch defects *introduced by the previous fix*). NOTE: `/ship-it` does **not** run `/code-review`
   itself, so this pass is the fixer's responsibility — it's part of "fix it in code" (step 2), not a step
   `/ship-it` performs. (`/code-review` applies even to docs/trivial fixes — per Applicability it's
   `/adversarial-investigation` and the cargo green gate that are N/A for those, not `/code-review`.)
4. **Any pushed fix moves HEAD → the Copilot-on-HEAD gate RESETS:** re-request Copilot on the new HEAD
   and let the cron loop again.
5. **Merge** (squash, via the REST API so it is worktree-safe) only when the exit condition holds.

**Exit condition (all must hold for the current HEAD):** CI green for the exact HEAD SHA — **every** CI
check SUCCESS/NEUTRAL/SKIPPED, not only the branch-required subset (per `AGENTS.md` "all CI checks must
pass before merge"; if branch protection lists fewer, enforce the full set, e.g. via `/ship-it
--required-checks`) · PR `MERGEABLE` · Copilot reviewed the **current** HEAD and every comment
triaged · your own `/code-review` on the latest changes clean · no fix outstanding or deferred.

**Bound + escalation (don't loop forever):** cap the loop (use `/ship-it --max-rounds`, default ~20) and
escalate to the human if it isn't converging — e.g. Copilot keeps surfacing *new substantive* defects
with no sign of settling, the same finding recurs after a fix, CI fails the same way 3×, or a merge
conflict needs human judgment. "1–3 small hardening nits per round, trending down" is normal convergence;
"every round opens fresh substantive holes" means stop and surface it.

## Notes / lessons baked in

- **The green gate is the floor, not a formality.** A single skipped "0 failed" check is how a flake
  shipped once; read the count every time, and re-run to rule out flakes.
- **Each Copilot round tends to surface 1–3 real (often small, hardening) issues**, frequently in the
  prior fix. Convergence is normal but not instant — keep looping until a HEAD comes back clean, within
  the round bound above.
- **/ship-it enforces the Copilot-on-HEAD gate natively.** It requires CI green + threads/comments
  triaged + mergeable AND — when Copilot is available on the repo — a *submitted* Copilot review of the
  current HEAD before merging; a fix pushed mid-ship resets the gate to the new HEAD. So a fix can't merge
  un-reviewed. (On a repo without Copilot it proceeds without that gate rather than hanging.)
- **Migrations / irreversible artifacts:** prefer additive, checksum-safe changes (e.g., a new migration
  over editing an applied one) so the fix itself can't break existing state.
- **Scope discipline:** the "no defer" rule is about *defects the loop found*. A genuinely separate
  feature the user did not ask for is a different PR — name it explicitly and get the user's nod.
