---
name: implement-e2e
argument-hint: describe the feature/change to implement end-to-end (or pass a phase/issue ref)
description: |
  End-to-end feature delivery loop with the tightest possible quality gate: design → implement →
  a fixed multi-pass review gauntlet (/code-review → /adversarial-investigation → /simplify →
  /code-review → /adversarial-investigation) → open a PR + request Copilot review → react to EVERY
  Copilot comment in code (never defer) → push → re-request review while running /code-review again →
  repeat until CI is green, the PR is mergeable, and Copilot's review of the CURRENT HEAD is clean →
  /ship-it. Fixes are NEVER deferred; the loop re-reviews after every change so a fix can't smuggle in
  a new bug. Use when the user says "implement this end to end", "implement-e2e", "build this feature
  and don't let any bugs through", "full review gauntlet then ship", or asks to build a feature with the
  complete review-and-ship pipeline.
---

# Implement E2E

The tightest feature-delivery loop in the toolbox. It exists for one reason: **ship great code with no
bugs getting through.** Every stage either finds defects or proves there are none, and **no suggested
fix is ever deferred** — if a review or adversarial pass raises it, you fix it in code now (or prove it
is a false positive with evidence and rebut it inline). Changes get re-reviewed, because the most
common bug source in this loop is *the previous fix*.

```
      ┌─────────────────────────────────────────────────────────────────────┐
      │  DESIGN → IMPLEMENT → REVIEW GAUNTLET → PR+COPILOT LOOP → SHIP IT     │
      └─────────────────────────────────────────────────────────────────────┘
```

## Operating rules (apply to EVERY phase)

1. **Never defer a fix.** Every finding from `/code-review`, `/adversarial-investigation`, `/simplify`,
   or Copilot is either (a) fixed in code this pass, or (b) refuted with a concrete receipt and an
   explicit written rationale. "Later", "follow-up", "out of scope", "P5" are not allowed for a defect
   the loop surfaced — only for genuinely separate features the user did not ask for (say so explicitly).
2. **Green gate between every phase.** After any code change, run the repo's full quality gates
   (`AGENTS.md` → PR Quality Checklist: `cargo fmt --check`, `cargo clippy --locked -- -D warnings`,
   `cargo test --locked --all`). Run the **FULL** test suite and confirm **ZERO failures** — read the
   actual failure count, do not just eyeball that some suites said "ok". If anything is red, fix it
   before moving on.
3. **Hunt flakes, don't tolerate them.** If a test fails in the full run but passes in isolation, it is
   a real defect (usually shared-state/ordering/timing). Root-cause and fix it; re-run the full suite
   2–3× to confirm stability. A flake that "usually passes" is a bug that ships.
4. **A fix can introduce a bug.** That is why the gauntlet re-runs review after `/simplify` and why the
   PR loop re-runs `/code-review` after every Copilot round. Treat each fix as new code that must clear
   the same bar.
5. **Reply to review threads inline**, on the comment itself — never a new top-level PR comment. Be
   specific: name the commit and what changed.
6. **Telemetry + tests are part of "done."** Per `AGENTS.md`: every new decision/error path is logged,
   and every behavior in the design has a test. New code without tests is incomplete.

## Phase 0 — Design first

Run **`/architect-and-design`** (or `feature-engineer` for larger work) on the requested feature before
writing code, per `AGENTS.md` ("Design first"). Produce: the files to add/change, the data flow, the
test contract (design → tests → code, in that order), and the telemetry plan. For a trivial change, a
short written plan is enough — but do not skip thinking about the test contract.

## Phase 1 — Implement

Write the code to fulfill the design. Tests define the contract first, then code fulfills it. When the
implementation is complete, run the **green gate** (rule 2). Do not proceed until it is fully green.

## Phase 2 — The review gauntlet (fixed order, no skips)

Run these in exactly this order. After EACH one, fix **every** finding (rule 1), then re-run the green
gate (rule 2) before the next stage:

1. **`/code-review`** — fix all findings.
2. **`/adversarial-investigation`** — fix all confirmed issues; resolve every Devil's-Advocate
   challenge with evidence.
3. **`/simplify`** — apply the cleanups (this is the stage most likely to introduce a regression, which
   is why review runs again next).
4. **`/code-review`** — fix all findings (including anything `/simplify` disturbed).
5. **`/adversarial-investigation`** — final pre-PR pass; fix everything; reach consensus/verdict.

If any stage produces a fix, the *next* stage reviews that fix too — that is the point. Do not shortcut
the order even if an earlier stage was clean.

## Phase 3 — Open the PR and request Copilot review

1. Create a feature branch (off `main`) if not already on one, commit, and push. (Commit/push only the
   work for this feature; follow `AGENTS.md` commit/PR conventions.)
2. Open the PR with `gh` — clear title + body describing the change, the design, and the test coverage.
3. **Request Copilot's review** on the PR:
   `gh api --method POST repos/{owner}/{repo}/pulls/<N>/requested_reviewers -f "reviewers[]=copilot-pull-request-reviewer[bot]"`.
4. Record the current HEAD SHA — the Copilot gate is keyed to it.

## Phase 4 — The Copilot loop (the tight part)

Loop until the exit condition holds. Each iteration:

1. **Wait for Copilot to review the CURRENT HEAD.** Verify a Copilot review whose `commit_id` equals the
   current head SHA exists:
   ```bash
   HEAD=$(gh pr view <N> --json headRefOid -q .headRefOid)
   gh api repos/{owner}/{repo}/pulls/<N>/reviews \
     --jq "[.[] | select((.user.login|test(\"[Cc]opilot\")) and .commit_id==\"$HEAD\")] | length"
   ```
   If it is `0`, Copilot has not reviewed this HEAD yet — wait and re-check. If it has not appeared after
   a couple of checks (or Copilot dropped out of the requested-reviewers list), re-request it. A review
   on a *superseded* commit does NOT satisfy the gate.
2. **Triage EVERY new inline comment from that review (rule 1):**
   - Fix it in code, then run the **green gate** (rule 2) — full suite, zero failures.
   - OR, only if it is genuinely wrong, reply with a concrete rebuttal. Never silently skip one.
   - **Reply on the comment thread itself** (rule 5):
     `gh api --method POST repos/{owner}/{repo}/pulls/<N>/comments/<comment_id>/replies -f "body=..."`.
3. **In parallel, run `/code-review` again** on the new changes you just made. Fix everything it finds
   (no deferral). This is the second reviewer that keeps the loop tight — Copilot and `/code-review`
   each catch what the other misses, and several rounds have caught defects *introduced by the previous
   fix*.
4. **If you pushed any fix, the HEAD changed → the Copilot-on-HEAD gate RESETS.** Push, then re-request
   Copilot review on the new HEAD and loop back to step 1. Expect the loop to run several rounds; each
   push invites a fresh review, and that is the cost of letting no bug through.

**Exit condition (all must hold for the current HEAD):**
- CI is green for the exact HEAD SHA (every required check SUCCESS/NEUTRAL/SKIPPED).
- The PR is `MERGEABLE`.
- Copilot has reviewed the **current** HEAD and **every** comment from that review is triaged (fixed or
  rebutted inline).
- Your own `/code-review` on the latest changes is clean.
- No fix is outstanding or deferred.

## Phase 5 — Ship it

When the exit condition holds, run **`/ship-it`** for the PR. It drives the final CI-green +
mergeable + comments-resolved checks and merges (squash). Keep the wait-for-Copilot gate intact: do not
merge a HEAD that Copilot has not reviewed clean.

## Notes / lessons baked in

- **The green gate is the floor, not a formality.** A single skipped "0 failed" check is how a flake
  shipped once; read the count every time, and re-run to rule out flakes.
- **Each Copilot round tends to surface 1–3 real (often small, hardening) issues**, frequently in the
  prior fix. Convergence is normal but not instant — keep looping until a HEAD comes back clean.
- **Migrations / irreversible artifacts:** prefer additive, checksum-safe changes (e.g., a new migration
  over editing an applied one) so the fix itself can't break existing state.
- **Scope discipline:** the "no defer" rule is about *defects the loop found*. A genuinely separate
  feature the user did not ask for is a different PR — name it explicitly and get the user's nod rather
  than silently folding it in.
