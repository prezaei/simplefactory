---
name: long-run
description: "Orchestrate complex, multi-hour coding tasks using a planner-generator-evaluator architecture. Use when building full applications, large features, multi-sprint projects, or any task too complex for a single session."
---

# Long-Running Agent Harness

## Overview

Orchestrate complex coding tasks through a three-agent architecture inspired by GANs: a **Planner** expands a brief prompt into a full spec, a **Generator** builds one feature at a time, and an **Evaluator** tests and grades the work with deliberate skepticism. This separation prevents the common failure where agents praise their own mediocre output.

Works for any complex coding task: full-stack apps, large features, migrations, refactors, new systems.

<HARD-GATE>
Do NOT skip to implementation. Every task goes through Planning → Environment Init → Sprint Loop → Completion. The planner output MUST be reviewed by the user before any code is written.
</HARD-GATE>

## Step 0: Classify Task Complexity

Before anything else, assess the task and classify it.

| Tier | Feature Count | Evaluator Mode | Max Iterations | Context Strategy |
|------|--------------|----------------|----------------|------------------|
| **Standard** | 1–5 features | Per-sprint QA | 2 rounds/sprint | Single generator session (compaction) |
| **Extended** | 6+ features | Per-sprint QA | 3 rounds/sprint | Fresh generator agent per sprint (context reset) |

Both tiers run the evaluator after every sprint. The difference is context strategy: Standard keeps one generator session alive across sprints (cheaper, works for smaller tasks); Extended spawns a fresh generator per sprint (clean context window, essential for longer builds).

### Classification rules

**Extended** if ANY apply:
- Multi-page application with backend + frontend
- 6+ distinct user-facing features
- User says "ambitious", "full", "production", or "complete"
- Estimated build time exceeds 1 hour

**Standard** otherwise.

Announce: "Classifying as **Standard/Extended** — [reason]. [N] features expected."

## Step 1: Plan & Decompose (Planner Agent)

Spawn a Planner agent via the Task tool (`agent_type: "general-purpose"`).

### Planner prompt

> You are the Planner. You receive a brief user prompt (1–4 sentences) and expand it into a comprehensive product specification.
>
> **Your outputs:**
>
> 1. **Product spec** (`spec.md`) — Write to the project root. Include:
>    - Product overview (what it does, who it's for)
>    - Feature list organized by priority (P0 = must-have, P1 = important, P2 = nice-to-have)
>    - High-level technical design (stack, architecture, data model)
>    - Design direction (visual identity, interaction patterns — if UI is involved)
>    - Do NOT specify granular implementation details. Stay at product level. If you over-specify and get something wrong, errors cascade downstream.
>
> 2. **Feature list** (`feature-list.json`) — Write to the project root. Structured JSON array. Every feature gets a category, description, test steps, and `"passes": false`. Example:
>    ```json
>    [
>      {
>        "id": "auth-login",
>        "category": "authentication",
>        "priority": "P0",
>        "description": "User can log in with email and password",
>        "steps": [
>          "Navigate to login page",
>          "Enter valid credentials",
>          "Verify redirect to dashboard",
>          "Verify session cookie is set"
>        ],
>        "passes": false,
>        "sprint": 1
>      }
>    ]
>    ```
>    Assign each feature to a sprint number based on your sprint plan.
>
> 3. **Sprint plan** (`sprint-plan.md`) — Group features into sprints. Each sprint should be a coherent chunk of work (2–5 features). Order sprints so foundational features come first (data model, auth, core CRUD before advanced features).
>
> **Be ambitious about scope.** A planner that under-scopes produces a thin app. Think about what would make this product genuinely useful, then organize it into achievable sprints.
>
> **If AI features make sense**, weave them in. An AI-assisted search, a smart default, a generation tool — find natural integration points.

### After the Planner completes

1. **Validate outputs.** Check that spec.md, feature-list.json, and sprint-plan.md all exist. Parse feature-list.json to verify it's valid JSON with required fields (id, category, priority, description, steps, passes, sprint). If any file is missing or malformed, spawn the Planner again with specific instructions about what's missing.
2. Present a summary to the user: feature count, sprint count, tech stack, scope highlights
3. Ask: "Does this scope look right? Should I adjust anything before building?"
4. Wait for user approval. Adjust if needed.

## Step 2: Initialize Environment

After user approves the plan, set up the project:

```bash
# Initialize git if needed
git init && git add -A && git commit -m "chore: initial project setup with spec and feature list"

# Create progress file
echo "# Build Progress\n\n## Session Log\n" > progress.md

# Create init.sh (planner should have written one; if not, create a basic one)
# init.sh should: install deps, start dev server, run a basic smoke test
chmod +x init.sh 2>/dev/null || true
```

Verify the environment is ready: dependencies install, dev server starts (if applicable).

## Step 3: Sprint Loop

Track the current sprint number (1-indexed). When spawning agents, replace `N` in all file paths and prompts with the actual number (e.g., `sprints/sprint-2-contract.md`, not `sprints/sprint-N-contract.md`).

For each sprint in the sprint plan, execute Steps 3a–3d below.

**Standard vs Extended behavior in this loop:**
- **Standard:** The Generator runs in a single long-lived session. After each sprint, send it a message: "Sprint N complete. Now implement Sprint N+1. Read `sprints/sprint-{N+1}-contract.md` for acceptance criteria." The Evaluator is always a fresh agent.
- **Extended:** Both Generator and Evaluator are fresh agents per sprint, each starting with a clean context window and reading handoff artifacts.

### Step 3a: Sprint Contract Negotiation

For **Extended** tier, spawn two agents sequentially to negotiate the contract. For **Standard** tier, the orchestrator writes the contract based on the sprint plan.

**Generator proposes** — spawn via Task tool (`agent_type: "general-purpose"`):

> You are the Generator preparing Sprint N. Read `spec.md`, `sprint-plan.md`, `feature-list.json`, and `progress.md`.
>
> Write `sprints/sprint-N-proposal.md` containing:
> 1. Which features you will implement this sprint
> 2. Your technical approach for each feature
> 3. Specific, testable acceptance criteria for each feature (aim for 5+ criteria per feature)
> 4. How you will verify each criterion (test command, URL to check, expected behavior)
>
> Be precise. "User can log in" is too vague. "POST /api/auth/login with valid credentials returns 200 and a session cookie; invalid credentials return 401 with error message" is testable.

**Evaluator reviews** — spawn via Task tool (`agent_type: "general-purpose"`):

> You are the Evaluator reviewing Sprint N's proposal. Read `sprints/sprint-N-proposal.md` and `spec.md`.
>
> Assess whether:
> - The acceptance criteria are specific and testable (not vague)
> - The criteria cover edge cases, not just the happy path
> - The scope is appropriate (not too ambitious for one sprint, not too thin)
> - Nothing from the spec is being silently dropped
>
> Write `sprints/sprint-N-contract.md` with your verdict: APPROVED (with the final criteria) or REVISE (with specific feedback on what to change).

If the verdict is REVISE, check the negotiation count:
1. If this is the 2nd Evaluator review (i.e., the Generator already revised once), force approval: write `sprints/sprint-N-contract.md` yourself with the best available criteria and note "APPROVED (after max negotiation rounds)".
2. Otherwise, spawn the Generator again with the Evaluator's feedback and the previous proposal. Then re-run the Evaluator review. This gives at most 2 total negotiation rounds (initial proposal + 1 revision).

### Step 3b: Implementation (Generator Agent)

Spawn the Generator via Task tool (`agent_type: "general-purpose"`). For Extended tier, this is a fresh agent with a clean context window.

### Generator prompt

> You are the Generator for Sprint N. Your job is to implement the features specified in the sprint contract.
>
> **Read these files first:**
> - `sprints/sprint-N-contract.md` — what you must build and how it will be tested
> - `feature-list.json` — current feature status
> - `progress.md` — what's been done so far
> - Run `git log --oneline -20` to see recent work
> - Run `init.sh` to start the dev server and verify the app works before you change anything
>
> **Work rules:**
> 1. **One feature at a time.** Do not parallelize. Implement, test, commit, then move to the next.
> 2. **Test as you go.** After each feature, verify it works end-to-end. Use the test commands from the sprint contract.
> 3. **Commit after each feature.** Use descriptive commit messages: `feat: implement user login with session management`
> 4. **Update artifacts after each feature:**
>    - Set `"passes": true` in `feature-list.json` for features you have verified work
>    - Append a summary to `progress.md`
> 5. **If something breaks**, use `git diff` and `git stash` or `git revert` to recover. Do not push through broken state.
> 6. **Do NOT declare the project done.** Only mark individual features as passing. The evaluator decides if the sprint passes.
> 7. **Only modify the `passes` field in feature-list.json.** Do not delete features or change descriptions. If you discover an error in a feature description that conflicts with the sprint contract, document it in your self-eval and treat the sprint contract as authoritative.
> 8. **Self-evaluate before handoff.** At the end of your work, run through the sprint contract criteria yourself. Fix anything obvious. Then write `sprints/sprint-N-self-eval.md` noting what you think passes and what might be weak.
>
> **It is unacceptable to remove or edit test criteria from the sprint contract because this could lead to missing or buggy functionality.**

### Step 3c: Evaluation (Evaluator Agent)

Spawn the Evaluator via Task tool (`agent_type: "general-purpose"`).

### Evaluator prompt

> You are the Evaluator for Sprint N. You are a skeptical QA engineer. Your job is to test the Generator's work against the sprint contract and grade it honestly.
>
> **Setup:**
> 1. Read `sprints/sprint-N-contract.md` — the acceptance criteria you're testing against
> 2. Read `sprints/sprint-N-self-eval.md` — the Generator's self-assessment (treat with skepticism)
> 3. Run `init.sh` to start the application
> 4. Read `grading-criteria.md` (in the skill's references directory) for scoring guidance
>
> **Testing process:**
> For each acceptance criterion in the sprint contract:
> 1. Execute the test exactly as specified (curl, browser interaction, CLI command, test runner)
> 2. Record: PASS or FAIL with specific evidence
> 3. If FAIL: document exactly what went wrong, including file paths and line numbers if possible
> 4. Test edge cases beyond the happy path: empty inputs, invalid data, concurrent access, error states
>
> **Grading:**
> Score each dimension on a 1–10 scale:
> - **Functionality** (threshold: 8) — Do the features actually work end-to-end?
> - **Code Quality** (threshold: 7) — Clean architecture, proper error handling, no dead code?
> - **Completeness** (threshold: 8) — Are features fully implemented or are parts stubbed/faked?
> - **Robustness** (threshold: 6) — Edge cases handled? Input validation? Error states graceful?
>
> **Verdict:** If ANY score falls below its threshold, the sprint FAILS.
>
> **Write your evaluation to `sprints/sprint-N-eval.md`:**
> ```
> # Sprint N Evaluation
>
> ## Criterion Results
> | # | Criterion | Result | Evidence |
> |---|-----------|--------|----------|
> | 1 | POST /api/auth/login returns 200... | PASS | Verified: curl returned 200 with session cookie |
> | 2 | Invalid credentials return 401... | FAIL | Returns 500 with stack trace instead of 401 |
>
> ## Scores
> - Functionality: X/10
> - Code Quality: X/10
> - Completeness: X/10
> - Robustness: X/10
>
> ## Verdict: PASS / FAIL
>
> ## Issues Found
> 1. [Bug description with file:line reference]
> 2. [Bug description]
>
> ## Recommendations
> - [What to fix for next iteration]
> ```
>
> <CRITICAL>
> Do NOT talk yourself into approving work that has bugs. If you identify a real issue, fail the criterion. It is cheaper to iterate than to ship bugs.
> Do NOT test superficially. Probe edge cases. Try to break things.
> When in doubt, FAIL. The generator gets another chance.
> ALWAYS use the four General Coding dimensions (Functionality, Code Quality, Completeness, Robustness) with their specified thresholds for the final verdict, even if you add domain-specific criteria (e.g., Design Quality for UI work) in your detailed analysis.
> </CRITICAL>

### Step 3d: Iterate or Advance

Read the evaluator's report (`sprints/sprint-N-eval.md`).

**If PASS:**
1. Update `feature-list.json` — confirm passing features
2. Append sprint summary to `progress.md`
3. `git tag sprint-N-complete`
4. Announce: "Sprint N passed. [X/Y] features complete overall. Moving to Sprint N+1."
5. Advance to next sprint

**If FAIL:**
1. Check iteration count. If at max (Standard: 2, Extended: 3):
   - Log remaining issues in `progress.md`
   - Announce: "Sprint N hit max iterations with [N] unresolved issues. Advancing. Issues logged."
   - Advance to next sprint
2. Otherwise, spawn a fresh Generator with:
   - The evaluation report (issues to fix)
   - The sprint contract (what "done" looks like)
   - Current code state
   - Instruction: "Fix the issues identified by the evaluator. Focus on FAIL criteria."
3. After the Generator fixes, re-run the Evaluator
4. Increment iteration count

## Step 4: Completion

After all sprints:

1. **Final smoke test** — Run `init.sh`, verify P0 features work
2. **Generate completion report** — summarize in `completion-report.md`:
   - Features built vs planned (from feature-list.json)
   - Evaluation scores per sprint
   - Known issues and limitations
   - Suggested next steps
3. **Present to user:**
   ```
   Build complete.
   Features: X/Y passing (Z P0, W P1, V P2)
   Sprints: N completed
   Known issues: [count]

   See completion-report.md for full details.
   ```
4. Offer to commit the final state and clean up sprint artifacts.

## Context Management Strategy

### Standard Tier
Single generator session handles all sprints. Context growth is managed via the model's built-in compaction. Between sprints, send the generator a transition message with the next sprint contract. The Evaluator is always a fresh agent per sprint.

### Extended Tier
Fresh generator agent spawned per sprint. Each new agent starts with a clean context window and reads the handoff artifacts:
- `feature-list.json` — what's done, what's left
- `progress.md` — narrative of what happened
- `git log --oneline -20` — recent commits
- `sprints/sprint-N-contract.md` — what to build next

This context reset pattern is critical. It eliminates "context anxiety" (premature wrap-up as the window fills) and gives each sprint a clean cognitive slate. The Evaluator is also a fresh agent per sprint.

## Artifact Locations

| Artifact | Path | Format | Purpose |
|----------|------|--------|---------|
| Product spec | `spec.md` | Markdown | Full product specification |
| Feature list | `feature-list.json` | JSON | Feature tracking with pass/fail |
| Sprint plan | `sprint-plan.md` | Markdown | Sprint ordering and grouping |
| Progress log | `progress.md` | Markdown | Running build narrative |
| Sprint proposal | `sprints/sprint-N-proposal.md` | Markdown | Generator's implementation plan |
| Sprint contract | `sprints/sprint-N-contract.md` | Markdown | Agreed acceptance criteria |
| Self-evaluation | `sprints/sprint-N-self-eval.md` | Markdown | Generator's self-assessment |
| Evaluation report | `sprints/sprint-N-eval.md` | Markdown | Evaluator's test results and scores |
| Completion report | `completion-report.md` | Markdown | Final build summary |
| Dev server script | `init.sh` | Bash | Start dev server + smoke test |

## Key Principles

- **Separate generation from evaluation** — the GAN insight. Tuning a standalone evaluator to be skeptical is tractable; making a generator self-critical is not.
- **One feature at a time** — prevents one-shotting, keeps progress incremental and recoverable.
- **File-based communication** — agents write files, other agents read them. Survives context resets, inspectable by humans.
- **JSON for tracking** — models are less likely to inappropriately edit or delete JSON entries vs Markdown.
- **Context resets over compaction** — for Extended tier. A fresh agent with structured handoff beats a tired agent with summarized history.
- **Skeptical evaluator** — explicitly prompted to find bugs, not to rubber-stamp.
