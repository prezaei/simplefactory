---
name: architecture-cleanup
description: |
  Adversarial codebase cleanup skill with 7-agent analysis (5 discovery + 2 adversarial challenge).
  Reviews architecture, design patterns, test coverage, production readiness, and regression history.
  Devil's Advocate kills false positives; Regression Risk Analyst maps blast radius.
  Creates dated refactor documents, commits them with PRs, creates GitHub issues, and orchestrates
  implementation with regression guards. Designed for continuous/scheduled execution at scale.
  Use when user asks to "review architecture", "cleanup codebase", "audit design patterns",
  "review technical debt", "check architecture health", "production readiness review",
  or "refactor component".
---

# Architecture Cleanup Skill

Perform comprehensive, recurring codebase health checks as the **Architecture Cleanup Orchestrator**. Analyze the codebase from architecture, design, and **production readiness** perspectives — identifying deficiencies that could affect a service handling **millions of requests at scale**.

Operate an **adversarial multi-agent pipeline**: five discovery agents find issues, two adversarial agents challenge those findings — killing false positives, assessing blast radius, and ensuring proposed fixes won't introduce regressions. Only findings that survive adversarial challenge enter the refactor plan.

## CRITICAL: Autonomous Execution

**This skill runs AUTONOMOUSLY through ALL phases without stopping for user input.**

- **DO NOT** ask the user clarifying questions during execution
- **DO NOT** wait for user approval between phases
- **DO NOT** deviate from the workflow — execute phases 1→2→3→4→5→6→7→8 in order
- **DO** proceed automatically from one phase to the next
- **DO** make reasonable decisions based on codebase analysis
- **DO** complete ALL phases in a single run

If you encounter ambiguity, make the best decision based on:
1. Codebase conventions and patterns
2. Production readiness requirements
3. Industry best practices

**Never stop mid-workflow to ask questions.** The goal is end-to-end automation.

## Activation

This skill activates when:
- User invokes `/architecture-cleanup`
- Phrases: "architecture review", "codebase cleanup", "design audit", "technical debt review", "architecture health check", "production readiness review"

## Prerequisites

This skill depends on the **`feature-dev` plugin** (Claude Code / Copilot):
- Phase 4 optionally launches a `feature-dev:code-reviewer` subagent (falls back gracefully if Copilot CLI is absent, but the subagent itself is assumed present).
- Phase 8 invokes `/feature-dev:feature-dev` per refactor item during implementation.

If the `feature-dev` plugin is not installed, phases 1–5 still run end-to-end, but phase 8 will fail. Install it or skip to phase 5 output and implement manually.

Optional: GitHub Copilot CLI (`npm install -g @github/copilot`) enables multi-model review in phase 4.

## Input Parameters

Parse from user input. **If not provided, use defaults - DO NOT ask.**

| Parameter | Options | Default | Description |
|-----------|---------|---------|-------------|
| **component** | Any top-level package/directory, or `all` | Auto-detect from repo structure | Which component(s) to analyze |
| **focus** | `full`, `architecture`, `patterns`, `tests`, `production` | `full` | Analysis scope |
| **depth** | `quick`, `standard`, `thorough` | `standard` | Analysis depth |

### Component Discovery

**At the start of execution, discover the repository structure automatically:**

1. Read the project's root configuration files (`package.json`, `pnpm-workspace.yaml`, `Cargo.toml`, `pyproject.toml`, `go.mod`, etc.) to understand the repo layout
2. Identify top-level components/packages (e.g., `packages/*`, `apps/*`, `services/*`, `src/`)
3. If the repo is a monorepo, list the available components
4. If the repo is a single project, treat the root as the single component

**If component not specified in user input**, default to the most significant production component (the one with the most source code, or the main application entry point).

**DO NOT ask the user** — proceed with the default or parsed value.

### Quality Check Commands

**Auto-detect the project's quality tooling by reading configuration files:**

| Config File | Likely Tooling |
|-------------|----------------|
| `package.json` | `pnpm verify`, `pnpm test`, `pnpm check`, `pnpm type-check` |
| `pyproject.toml` / `setup.cfg` | `ruff check .`, `mypy .`, `pytest` |
| `Cargo.toml` | `cargo clippy`, `cargo test` |
| `go.mod` | `go vet ./...`, `go test ./...` |
| `Makefile` / `Justfile` | Read for lint/test/check targets |

Store the discovered commands and use them throughout the workflow (Phases 6, 8).

### Handling `all` Components

When `component=all`, execute the **ENTIRE workflow separately for EACH component**:

```
for component in [discovered components]:
    Execute Phase 1-8 for {component}
    - Create refactor doc in docs/refactor/
    - Create separate PR for {component}
    - Create separate GitHub issue for {component}
    - Implement all items for {component}
```

**Each component gets:**
- Its own refactor document in its own docs folder
- Its own feature branch: `refactor/{component}-cleanup-YYYY-MM-DD`
- Its own PR
- Its own GitHub issue

**DO NOT** create a single combined document. Always create per-component artifacts.

---

## Workflow Overview

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  PHASE 1: DISCOVERY & ANALYSIS (5 Parallel Agents)                          │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌──────────────┐       │
│  │ Architecture │ │ Pattern      │ │ Test Coverage│ │ Production   │       │
│  │ Explorer     │ │ Analyzer     │ │ Auditor      │ │ Readiness    │       │
│  └──────┬───────┘ └──────┬───────┘ └──────┬───────┘ └──────┬───────┘       │
│         └────────────────┴────────────────┴────────────────┘               │
│                          ┌──────────────┐                                   │
│                          │ Regression   │                                   │
│                          │ History Scout│                                   │
│                          └──────┬───────┘                                   │
│                                 ▼                                           │
└─────────────────────────────────┬────────────────────────────────────────────┘
                                  │
┌─────────────────────────────────▼────────────────────────────────────────────┐
│  PHASE 2: ADVERSARIAL CHALLENGE (2 Parallel Agents)                         │
│  ┌────────────────────────┐  ┌────────────────────────┐                     │
│  │ Devil's Advocate        │  │ Regression Risk Analyst│                     │
│  │ Challenges all findings │  │ Maps blast radius      │                     │
│  └───────────┬─────────────┘  └───────────┬────────────┘                    │
│              └─────────────┬──────────────┘                                 │
│                            ▼                                                │
│            Consensus Resolution (filter findings)                           │
└─────────────────────────────────┬────────────────────────────────────────────┘
                                  │
┌─────────────────────────────────▼────────────────────────────────────────────┐
│  PHASE 3: SYNTHESIS                                                         │
│  Consolidate surviving findings → Confidence scoring → Refactor plan        │
│  Output: docs/refactor/YYYY-MM-DD-{component}-refactor.md                   │
└─────────────────────────────────┬────────────────────────────────────────────┘
                                  │
┌─────────────────────────────────▼────────────────────────────────────────────┐
│  PHASE 4: DESIGN REVIEW & FEEDBACK INCORPORATION                            │
│  design-reviewer / code-reviewer → Incorporate feedback → Finalize plan     │
└─────────────────────────────────┬────────────────────────────────────────────┘
                                  │
┌─────────────────────────────────▼────────────────────────────────────────────┐
│  PHASE 5: PR CREATION                                                       │
│  Create branch → Commit refactor doc → Push → Create PR into main           │
└─────────────────────────────────┬────────────────────────────────────────────┘
                                  │
┌─────────────────────────────────▼────────────────────────────────────────────┐
│  PHASE 6: ISSUE CREATION & QUESTION PRE-RESOLUTION                          │
│  Create GitHub issue → Run /feature-dev planning → Answer questions          │
│  → Update issue with Q&A → Issue ready for autonomous implementation        │
└─────────────────────────────────┬────────────────────────────────────────────┘
                                  │
┌─────────────────────────────────▼────────────────────────────────────────────┐
│  PHASE 7: STATUS CHECKPOINT (Automatic)                                     │
│  Output summary → Immediately proceed to Phase 8 (NO user input)            │
└─────────────────────────────────┬────────────────────────────────────────────┘
                                  │
┌─────────────────────────────────▼────────────────────────────────────────────┐
│  PHASE 8: IMPLEMENTATION                                                    │
│  Baseline test lock → Implement per item → Regression check after each      │
│  Invoke /feature-dev:feature-dev for each approved refactor item            │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## Incomplete Code Recognition

Before flagging any finding, agents MUST determine whether the code is **intentionally incomplete** — part of a phased build-out that hasn't finished yet. Flagging in-progress work as a defect is a false positive.

### Required checks

1. **Cross-reference design documents.** Search `docs/design/` and `{component_path}/docs/design/` for design docs mentioning the component. If a design describes phases, milestones, or a roadmap, compare the current code against it. Code that implements phase N but not phase N+1 is **not a finding** — it's planned work.

2. **Recognize stubs.** The following patterns indicate intentional placeholders, not defects:
   - `raise NotImplementedError`
   - `pass` in otherwise-empty method bodies
   - `TODO` / `FIXME` / `HACK` comments referencing a ticket or phase
   - Scaffolded proto services with no implementation
   - Empty or minimal test files for new modules
   - Feature flags or config guards that disable unfinished paths

3. **Check recent history.** If the component was scaffolded or substantially changed within the last 14 days (`git log --since='14 days ago' -- {component_path}`), treat missing functionality as likely in-progress rather than forgotten.

### Disposition

- If a finding matches an unfinished phase in a design doc → **Exclude** (not deferred — excluded entirely, it's not a finding)
- If a finding targets a stub with a linked ticket/TODO → **Exclude**
- If a finding targets recently scaffolded code with no design doc → **Defer** with note "likely in-progress, no design doc found"

**Include these checks in every agent prompt.** Agents that skip this protocol produce false positives that waste human review time.

## Universal Evidence Protocol

> Read `references/classification-rules.md` for the full evidence protocol, confidence calibration, cognitive hazards, discrimination tests, and anti-patterns. **All agents MUST internalize this protocol — include it in every agent prompt.**

Key rules (details in reference file):
- Every claim requires a receipt (`file:line`, issue number, commit SHA, etc.)
- No receipt after one challenge = claim **excluded** from the refactor plan
- Evidence hierarchy: Tool output > executable code paths > verifiable artifacts
- Only findings at confidence 50+ after adversarial challenge enter the refactor plan
- Counter-search requirement: search for evidence AGAINST before reporting
- Every agent report MUST end with an explicit ASSUMPTIONS section

## Knowledge Map

Sub-agents start fresh — they don't inherit context. Every agent prompt MUST include relevant parts of this map.

**CRITICAL: All agents MUST be spawned as `general-purpose` subagent_type.** This gives full tool access including MCP, Bash, and Git. Do not use `Explore` agents — they lack MCP access needed for comprehensive analysis. One role per subagent — never combine agents (combined agents lose adversarial independence).

**CRITICAL: Template Interpolation.** When dispatching sub-agents, Read the prompt file from `prompts/`, then replace `{INSERT KNOWLEDGE MAP FROM SKILL}` with the Knowledge Map section content below, `{component_path}` and `{component_name}` with actual values, and `{INSERT COMPILED EVIDENCE DIGEST FROM PHASE 1}` (Phase 2 agents only) with the compiled digest.

**Validation gate:** Before dispatching ANY sub-agent, scan the interpolated prompt for remaining `{INSERT` placeholders. If any are found, the interpolation is incomplete -- fix it before dispatching. Sending literal placeholder text deprives agents of architectural context and wastes the entire agent run.

```
Architecture & conventions:
- Root CLAUDE.md:           ./CLAUDE.md (monorepo structure, conventions, quality gates)
- Component CLAUDE.md:      ./<component>/CLAUDE.md (key files, config, testing)
- Architecture docs:        ./<component>/docs/ARCHITECTURE.md
- Design docs (component):  ./<component>/docs/design/*.md
- Design docs (repo-wide):  ./docs/design/*.md
  **READ DESIGN DOCS FIRST** — they define what is planned vs. built.
  Cross-reference before flagging incomplete code as a finding.

Quality tooling (auto-detected):
- Stored in orchestrator context from Input Parameters phase
- Typically: ruff check, mypy, pytest for Python; cargo clippy/test for Rust; etc.

Git history & issues:
- Recent changes:           git log --oneline -20
- Changes to file:          git log --oneline -10 <file>
- Issues (all states):      gh issue list --state all --search "<keywords>" --limit 20
- PRs (all states):         gh pr list --state all --search "<keywords>" --limit 20
- Reverted commits:         git log --oneline --grep="revert" -20

Past refactor work:
- Refactor docs:            docs/refactor/*.md
- Refactor archive:         docs/refactor/archive/*.md
- Progress tracking:        docs/refactor/PROGRESS.md

Past architecture reports:
- Reports:                  docs/reports/*.md (gap analyses, audits)
```

---

## Phase 1: Discovery & Analysis

Execute **FIVE** parallel agents to gather comprehensive codebase intelligence. Each agent has a distinct, non-overlapping responsibility.

### 1.1 Architecture Explorer

**Call using Agent tool:**
```
subagent_type: "general-purpose"
model: "opus"
prompt: [Read .agents/skills/architecture-cleanup/prompts/architecture-explorer.md, interpolate placeholders, use as prompt]
description: "Analyze: {component} architecture"
```

### 1.2 Pattern Analyzer

**Call using Agent tool:**
```
subagent_type: "general-purpose"
model: "opus"
prompt: [Read .agents/skills/architecture-cleanup/prompts/pattern-analyzer.md, interpolate placeholders, use as prompt]
description: "Analyze: {component} patterns"
```

### 1.3 Test Coverage Auditor

**Call using Agent tool:**
```
subagent_type: "general-purpose"
model: "sonnet"
prompt: [Read .agents/skills/architecture-cleanup/prompts/test-coverage-auditor.md, interpolate placeholders, use as prompt]
description: "Analyze: {component} test coverage"
```

### 1.4 Production Readiness Analyzer

**Call using Agent tool:**
```
subagent_type: "general-purpose"
model: "opus"
prompt: [Read .agents/skills/architecture-cleanup/prompts/production-readiness.md, interpolate placeholders, use as prompt]
description: "Analyze: {component} production readiness"
```

### 1.5 Regression History Scout

**Call using Agent tool:**
```
subagent_type: "general-purpose"
model: "opus"
prompt: [Read .agents/skills/architecture-cleanup/prompts/regression-history-scout.md, interpolate placeholders, use as prompt]
description: "Scout: {component} regression history"
```

**IMPORTANT**: Launch all FIVE agents in parallel for efficiency.

**→ PHASE 1 COMPLETE: Proceed immediately to Phase 2**

---

## Phase 2: Adversarial Challenge

After Phase 1 agents report, challenge their findings. The goal: **kill false positives, assess blast radius, and ensure proposed fixes won't introduce regressions.** Only findings that survive this phase enter the refactor plan.

### 2.0 Compile Evidence Digest

Before spawning adversarial agents, compile ALL Phase 1 findings into a single digest. The digest MUST include:

- **All findings** with their evidence (file:line refs, confidence scores)
- **All assumptions** from each agent's ASSUMPTIONS section
- **Proposed fixes** and their rationale
- **Affected files** and their test coverage (from Test Coverage Auditor)
- **Regression history** for affected areas (from Regression History Scout)
- **Any disagreements** between Phase 1 agents (e.g., Pattern Analyzer flags a pattern that Architecture Explorer considers intentional)

**Format the digest as a structured document** that adversarial agents can reference by finding ID (e.g., F001, F002).

### 2.1 Devil's Advocate

**Call using Agent tool:**
```
subagent_type: "general-purpose"
model: "opus"
prompt: [Read .agents/skills/architecture-cleanup/prompts/devils-advocate.md, interpolate placeholders, use as prompt]
description: "Challenge: {component} findings"
```

### 2.2 Regression Risk Analyst

**Call using Agent tool:**
```
subagent_type: "general-purpose"
model: "opus"
prompt: [Read .agents/skills/architecture-cleanup/prompts/regression-risk-analyst.md, interpolate placeholders, use as prompt]
description: "Regression risk: {component}"
```

**IMPORTANT**: Launch both agents in parallel.

### 2.3 Consensus Resolution & DA Challenge Resolution

> Read `references/classification-rules.md` for the full consensus resolution table, confidence score adjustment rules, DA Challenge Resolution Protocol (REFUTED/ABSORBED/DEFERRED/UNRESOLVED dispositions), precedence rules, DA obligation requirements, disposition tracking format, and the mandatory stress test before synthesis.

Key decision rules:
- Confidence >= 50 after challenge, no fatal regression risk → **Confirmed**
- Confidence < 50 after challenge, or DA killed with evidence → **Dismissed**
- Real issue but regression risk too high → **Deferred** with required mitigation
- Adversarial agents disagree → **Disputed** with both perspectives

DA challenges require explicit disposition (REFUTED/ABSORBED/DEFERRED/UNRESOLVED) with a receipt. Phase 2.4 dispositions override confidence-based outcomes. No disposition = finding enters as DISPUTED.

**→ PHASE 2 COMPLETE: Proceed immediately to Phase 3**

---

## Phase 3: Synthesis

After adversarial challenge, synthesize surviving findings into the refactor plan.

### 3.1 Consolidate Findings

Create a prioritized list of **confirmed and disputed findings** (those that survived Phase 2). **ALL confirmed issues must be addressed** — priority determines order, not exclusion.

> Read `references/classification-rules.md` for priority classification (P0-P3), large file thresholds, and scoring criteria.

### 3.2 Create Refactor Document

> Read `references/document-templates.md` for the full Refactor Document Template when generating the output file.

Generate the refactor plan at: `docs/refactor/YYYY-MM-DD-{component}-refactor.md`

For monorepos, place in: `{component_path}/docs/refactor/YYYY-MM-DD-{component}-refactor.md`

**→ PHASE 3 COMPLETE: Proceed immediately to Phase 4**

---

## Phase 4: Multi-Model Design Review & Feedback Incorporation

Submit the refactor document for comprehensive review using multiple AI models, then incorporate feedback.

### 4.1 Check for Copilot CLI

```bash
which copilot 2>/dev/null || where copilot 2>/dev/null
```

### 4.2 If Copilot CLI IS available:

Run the `design-reviewer` agent with the refactor document:

```bash
export GH_TOKEN=$(gh auth token 2>/dev/null || echo "")
copilot --agent design-reviewer \
  --allow-all \
  --no-ask-user \
  --add-dir "{repo_root}" \
  -p "{relative_path_to_refactor_doc}"
```

**Important:**
- Do NOT pass a `--model` flag (the design-reviewer agent selects its own models)
- Use `--allow-all` for non-interactive execution
- Use `--no-ask-user` so it runs autonomously
- Use `--add-dir` to give it access to the codebase
- Pass the refactor doc path via `-p` (prompt flag)
- Run in background with appropriate timeout (5-10 minutes)
- The agent produces a `{refactor-doc}-REVIEW.md` file in the same directory

### 4.3 If Copilot CLI is NOT available:

1. **Inform the user clearly:**
   ```
   Copilot CLI not found. Skipping multi-model design review.
   To enable: npm install -g @github/copilot
   Proceeding with Claude-only review instead.
   ```

2. **Fall back to Claude code-reviewer agent:**
   Launch a `feature-dev:code-reviewer` subagent (Agent tool) to review the refactor document.
   Focus areas: production readiness, concurrency, scalability, roadmap sequencing.

### 4.4 Incorporate Review Feedback

If the repo has a `/review-feedback` skill available, use it to incorporate the review. Otherwise:

1. Read the `-REVIEW.md` file produced by the design-reviewer
2. Categorize feedback by priority (BLOCKING, HIGH, MEDIUM, LOW)
3. Address BLOCKING/HIGH issues immediately in the refactor document
4. Add MEDIUM/LOW items where they improve clarity
5. Delete the `-REVIEW.md` file after incorporation
6. Log what was changed

**→ PHASE 4 COMPLETE: Proceed immediately to Phase 5**

---

## Phase 5: PR Creation

After the refactor document is finalized, commit it and create a PR into main.

> Read `references/document-templates.md` for the Commit Message Template and PR Body Template.

### 5.1 Create Feature Branch

```bash
git checkout main
git pull origin main
git checkout -b refactor/{component}-cleanup-YYYY-MM-DD
```

Branch naming: `refactor/{component}-cleanup-{date}` (e.g., `refactor/chat-cleanup-2026-02-10`)

### 5.2 Commit Refactor Document

Stage and commit the refactor document using the Commit Message Template from `references/document-templates.md`.

### 5.3 Push and Create PR

```bash
git push -u origin refactor/{component}-cleanup-YYYY-MM-DD
```

Create PR using `gh pr create` with the PR Body Template from `references/document-templates.md`.

**Store the PR number** for reference in the GitHub issue.

### 5.4 Verify PR Build Status

After pushing, check if CI passes. Look for build status using `gh pr checks` or equivalent CI tooling available in the repo. Fix any lint/type/test failures before proceeding.

**→ PHASE 5 COMPLETE: Proceed immediately to Phase 6**

---

## Phase 6–8: Implementation Pipeline

> Read `references/implementation-guide.md` for the full procedures for Phase 6 (Issue Creation & Question Pre-Resolution), Phase 7 (Status Checkpoint), and Phase 8 (Implementation with regression guards, feature-dev invocation, and completion reporting).

**Phase 6** creates a self-contained GitHub issue with pre-resolved Q&A for autonomous implementation. **Phase 7** outputs a status checkpoint (automatic — no user input). **Phase 8** runs baseline test lock, invokes `/feature-dev:feature-dev` per item in priority order, enforces regression guards after each change, and outputs the completion report.

> Also read `references/document-templates.md` for the GitHub Issue Template, Status Checkpoint Template, Completion Report Template, Final Summary Template, and Progress Tracking Template used in these phases.

---

## Agent Prompt Files

Agent prompts are stored in `prompts/` and loaded on demand at dispatch time via the Read tool. Each file is a self-contained prompt template with `{...}` placeholders for interpolation.

| Agent | Prompt File | Phase |
|-------|------------|-------|
| Architecture Explorer | `prompts/architecture-explorer.md` | 1.1 |
| Pattern Analyzer | `prompts/pattern-analyzer.md` | 1.2 |
| Test Coverage Auditor | `prompts/test-coverage-auditor.md` | 1.3 |
| Production Readiness Analyzer | `prompts/production-readiness.md` | 1.4 |
| Regression History Scout | `prompts/regression-history-scout.md` | 1.5 |
| Devil's Advocate | `prompts/devils-advocate.md` | 2.1 |
| Regression Risk Analyst | `prompts/regression-risk-analyst.md` | 2.2 |

**Dispatch pattern:** Read the prompt file → replace `{INSERT KNOWLEDGE MAP FROM SKILL}` with the Knowledge Map above → replace `{component_path}`, `{component_name}` with actual values → for Phase 2 agents, also replace `{INSERT COMPILED EVIDENCE DIGEST FROM PHASE 1}` → pass the interpolated text as the Agent tool's `prompt` parameter.

---

## Guidelines

### Workflow Execution

Execute Phase 1 -> 2 -> 3 -> 4 -> 5 -> 6 -> 7 -> 8 in strict order. Never stop mid-workflow to ask questions. Make autonomous decisions using codebase analysis. Only stop after Phase 8 completion or unrecoverable error.

### Adversarial Rigor

- Phase 2 is not optional. Every finding must survive adversarial challenge.
- False positives waste more time than they save -- better to dismiss 5 real issues than fix 5 non-issues.
- Proposed fixes must justify their blast radius. A fix that could break 15 callers needs pre-implementation tests for all 15.
- DA challenges require explicit disposition (REFUTED/ABSORBED/DEFERRED/UNRESOLVED with receipt). Adjusting a confidence score is not a disposition. No disposition = DISPUTED.
- Stress test before synthesis: verify KILL verdicts rest on definitive evidence.
- All agents MUST be `general-purpose` subagent_type. One role per subagent -- never combine.

### Regression Guards

- Baseline test lock before ANY implementation. Zero tolerance for regressions.
- High-risk findings need pre-implementation tests for affected callers.
- Each refactor item is a separate commit for easy revert.

### Execution Principles

- ALL confirmed issues must be addressed -- priority determines ORDER, not WHETHER to fix.
- Removing code is worth 2x adding code. Dead code is a P3 that must be addressed.
- Add concurrency tests before fixing race conditions.
- Escalate P0 issues immediately. Report adversarial results (survived vs killed).
- Deferred items require justification AND follow-up date.
