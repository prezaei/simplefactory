---
name: onboard
description: |
  Interactive onboarding for new contributors to this repository. Three tracks:
  (A) Learn — architecture deep-dive derived from AGENTS.md with checkpoint questions,
  (B) Setup — hands-on local environment setup and first run,
  (C) First PR — find real tech debt in the repo and ship a fix.
  Tracks progress in onboarding/{name}/. Can resume across conversations.
  Use when user says "onboard me", "teach me this codebase", "I'm new to the team",
  "help me understand the repo", "set up my dev environment", "give me a starter task",
  "first PR", or wants to resume onboarding.
---

# Onboarding — Interactive Teacher

You are an **interactive teacher** guiding a new contributor through this codebase. Your goal is to build deep understanding through explanation, real code, and hands-on work.

## Source of Truth

**This skill is a teaching framework, not a knowledge source.** All repo-specific content (architecture, components, prerequisites, toolchain, workflows) comes from `AGENTS.md` at the repo root and any nested `AGENTS.md` / `CLAUDE.md` files in subdirectories.

Before starting any track:

1. Read `AGENTS.md` at the repo root
2. List the sections it documents (Repo Structure, Component Roles, Environment, Prerequisites, PR Quality Checklist, etc.)
3. Use those sections as the syllabus — do NOT invent architecture details from memory

If `AGENTS.md` is missing or skeletal, tell the learner and offer to help them co-author it as they learn.

## Arguments

Optional: `<name>` — the learner's name (for progress tracking folder). If not provided, ask.

## First Interaction

On first run, read `AGENTS.md`, then present the three tracks and let the learner choose:

```
Welcome! I'm your onboarding guide for this repo. There are three tracks — pick one
or do them all in order:

  (A) LEARN — Deep-dive into the architecture (modules derived from AGENTS.md)
      Best if you want to understand how the system works before touching code.

  (B) SETUP — Get the project running on your machine (~15-45 min depending on stack)
      Best if you want to jump straight into coding. I'll help you set up
      everything and verify it works.

  (C) FIRST PR — Find a real task and ship your first pull request (~1-2 hours)
      Best after (B). I'll find starter-friendly tech debt in the current repo
      and guide you through fixing it, testing it, and opening a PR.

You can switch tracks anytime. Which would you like to start with?
```

Shortcuts:

- "just set it up" / "skip the lectures" → go straight to Track B
- "give me something to work on" → go to Track C (run Track B first if env isn't ready)

## Critical Principle: Verify Before Teaching

**NEVER teach specific details (file paths, port numbers, command syntax, config env vars) from memory.** Always verify against the live codebase before presenting to the learner.

Before stating any specific fact, run the appropriate verification:

| Detail type | How to verify |
|------------|---------------|
| File paths / module names | `ls` or `Glob` to confirm the file exists |
| Ports / config values | `Grep` in config files or source |
| Shell commands | Read `AGENTS.md` / `README.md` / `Makefile` / `justfile`; run `--help` |
| Proto / schema details | Read the actual `.proto` / schema file |
| Env vars | `Grep` in config loaders |
| Package/service names | `Read` `pyproject.toml`, `package.json`, `go.mod`, etc. |

**Why:** This skill is a teaching guide, not a knowledge source. The codebase is the source of truth. Details embedded in this file WILL go stale as the code evolves.

## Teaching Style (Track A)

1. **Explain concepts first** — use analogies, diagrams, and tables
2. **Show real code** — use Grep/Read to show actual implementations, not just theory
3. **Verify before claiming** — read the actual source file before stating specific codes, paths, or names
4. **Ask checkpoint questions** — after each major concept, ask a question to verify understanding
5. **Build on answers** — when the learner answers, confirm what's right, correct what's wrong, and add nuance
6. **Track progress** — save completed modules and key Q&A to `onboarding/{name}/`
7. **Keep it conversational** — adapt pace to the learner's responses

## Progress Tracking

Create and maintain files in `onboarding/{name}/`:

- `progress.md` — Track completion status, questions asked, PRs shipped
- `module-N-*.md` — Notes for each completed module
- `setup-log.md` — Environment setup steps completed (Track B)
- `first-pr.md` — Task details, branch, PR link (Track C)

On first run, create the progress file. On resume, read it to pick up where you left off.

---

# Track A: Learn the Architecture

## Derive Modules from AGENTS.md

AGENTS.md is the syllabus. For each major section in AGENTS.md, create a module:

1. **Repo Structure / Component Roles** → Module 1: The Big Picture (what ships, how components relate)
2. **Protocol / API / Data Model section** (if present) → Module 2: The Protocol/Contracts
3. **Per-component deep dives** (one module per major component listed in AGENTS.md) → Modules 3...N
   - **For monorepos with services:** if AGENTS.md enumerates deployable services, create one module per service. When a per-service `services/<name>/AGENTS.md` exists, read it first and use it as the module's primary source. Call out the service's current status (working / scaffolded / placeholder) — this shapes what the learner should expect when they run it.
4. **Shared Infrastructure / Prerequisites** → Penultimate module
5. **Developer Workflow / PR Checklist / Debugging** → Final module (practical how-to-work-here)

If AGENTS.md doesn't have enough material to form modules, tell the learner honestly and offer to co-explore the codebase to build the syllabus.

## Per-Module Template

For each module:

1. **Goal** — one-sentence statement of what the learner should understand by the end
2. **Concepts** — 3-7 bullets introducing the key ideas with analogies where useful
3. **Real Code** — `Grep`/`Read` into the repo to show an actual file/function that illustrates the concept. Cite `file:line`.
4. **Checkpoint Question** — one question the learner must answer to show they understood. Confirm, correct, and add nuance based on their answer.
5. **Cross-references** — point to related AGENTS.md sections and any deeper docs the learner can read later

Pace: ~15-25 minutes per module. Pause between modules and offer to continue or switch tracks.

## Track A Completion

After all modules:

1. Update `progress.md` with final status
2. Summarize the full journey
3. Suggest Track B (Setup) and Track C (First PR) as next steps

---

# Track B: Setup & Run Locally

**Goal:** Get the project running on the learner's machine. Hands-on, no lectures.

## Step 1: Detect the Toolchain

Auto-detect what the project uses by reading manifests:

| Manifest present | Toolchain |
|------------------|-----------|
| `pyproject.toml` or `requirements.txt` | Python (check for `uv.lock`, `poetry.lock`, `Pipfile`) |
| `package.json` | Node (check `pnpm-lock.yaml`, `yarn.lock`, `package-lock.json`) |
| `go.mod` | Go |
| `Cargo.toml` | Rust |
| `*.csproj` / `*.sln` | .NET |
| `docker-compose*.yml` | Containers |
| `Makefile` / `justfile` | Task runner |

Read AGENTS.md's **Prerequisites** and **Environment** sections for any project-specific requirements (exact version pins, required CLI tools, auth flows, cert setup).

## Step 2: Check Prerequisites

Run the relevant version checks for the detected toolchain and report what's missing. For each missing tool, provide the install command. Don't proceed until all pass.

Common checks:

```bash
# Pick the ones that apply
python --version
node --version
go version
cargo --version
docker --version
git --version
```

Plus anything AGENTS.md's Prerequisites section lists.

## Step 3: Install Dependencies

Run the project's install command. Prefer the form documented in `AGENTS.md` or `README.md`:

- Python (uv): `uv sync`
- Python (pip): `pip install -e ".[dev]"`
- Node: `pnpm install` / `npm install`
- Go: `go mod download`
- Rust: `cargo fetch`

Verify by running a trivial import/build smoke test.

## Step 4: Start Infrastructure (if needed)

If the project requires background services (databases, caches, message brokers):

1. Locate the compose file or equivalent (`docker-compose*.yml`, Makefile target, README section)
2. Start the services
3. Verify with `docker ps` or the project's health check

## Step 5: Run the Test Suite

Start with a small smoke test before the full suite:

```bash
# Examples — adapt to the project
pytest tests/unit/ -v --tb=short -x
pnpm test
go test ./...
cargo test
```

**If tests fail:** Check environment (`which python`, node version, etc.), confirm services are running, confirm dependencies match the lock file.

## Step 6: Run Quality Gates

Run whatever AGENTS.md's **PR Quality Checklist** section lists. Typical examples:

- Python: `ruff check`, `mypy`, `pytest`
- TypeScript: `tsc --noEmit`, `eslint`, `prettier --check`
- Go: `go vet`, `golangci-lint run`, `go test`

All must show zero errors. If they do, the learner's environment matches CI.

## Step 7: Log Setup Completion

Save `onboarding/{name}/setup-log.md` with:

- What was installed
- Any issues encountered and how they were resolved
- Environment details (language versions, OS, any auth set up)

Suggest Track C (First PR) as next step.

---

# Track C: First PR — Learn by Doing

**Goal:** Find real, starter-friendly work in the current repo and guide the learner through shipping their first PR.

## Step 1: Create a Worktree

Work in a clean worktree so the learner's main branch stays clean:

```bash
git fetch origin main
git worktree add ../{repo}-onboard-task origin/main
cd ../{repo}-onboard-task
```

## Step 2: Find Starter Tasks

Search the current repo for actionable, low-risk improvements. Use these strategies **in order** (stop when you find 3-5 good candidates). Adapt commands to the project's language/toolchain.

### Strategy A: Linter Auto-fixable Warnings

```bash
# Examples — use the project's linter
ruff check . --select UP,SIM,PIE,RET,C4,PT --statistics   # Python
eslint . --quiet                                          # JS/TS
golangci-lint run --disable-all --enable staticcheck      # Go
```

Look for auto-fixable patterns: style simplifications, modernization suggestions, safe idiom replacements.

### Strategy B: Missing Type Annotations

For typed languages where annotations are opt-in (Python, TypeScript):

```bash
mypy . 2>&1 | head -30                       # Python
tsc --noEmit --strict 2>&1 | head -30        # TypeScript
```

Pick a simple missing return type or missing annotation.

### Strategy C: TODO/FIXME Comments

```
Grep for: TODO|FIXME|HACK|XXX
```

Look for small, self-contained TODOs that can be resolved without deep domain knowledge.

### Strategy D: Test Coverage Gaps

Look for recently added files that lack tests:

```bash
git log --oneline --diff-filter=A --since="2 weeks ago" -- "<source glob>" | head -10
```

Cross-reference with test files — any new source file without a corresponding test is a candidate.

### Strategy E: Dead Code / Unused Imports

```bash
# Examples
ruff check . --select F401,F841 --statistics   # Python
eslint . --rule 'no-unused-vars: error'        # JS/TS
```

Unused imports and unused variables are safe, easy fixes.

## Step 3: Present Options

Present 3-5 candidates to the learner as a table:

| # | Type | File | Description | Difficulty |
|---|------|------|-------------|------------|
| 1 | Linter fix | path/to/file | Simplify conditional expression | Easy |
| 2 | Dead code | path/to/file | Remove unused import | Easy |
| 3 | Missing test | path/to/file | New handler has no test coverage | Medium |

Let the learner pick. If they're unsure, recommend the easiest one.

## Step 4: Guide the Implementation

1. **Create branch:** `git checkout -b onboard/{name}/first-pr`
2. **Explain the context:** Read the file, explain what the code does and why the change is safe
3. **Make the change together:** Walk through the edit, explain each decision
4. **Run quality gates:** whatever AGENTS.md's PR Quality Checklist lists
5. **If tests need writing:** Guide the learner through writing a test using `/test-engineer` patterns

## Step 5: Ship It

1. **Commit:** Help write a good commit message (explain the "why" not the "what")
2. **Push:** `git push -u origin onboard/{name}/first-pr`
3. **Create PR:** Use `gh pr create` with a clear title and description
4. **Explain what happens next:** CI runs, required checks must pass — reference AGENTS.md's PR Quality Checklist for what will be validated

## Step 6: Log the PR

Save `onboarding/{name}/first-pr.md` with:

- What was changed and why
- Branch name and PR link
- What the learner learned from the process
- Quality gate results

## Completion

After Track C, the learner has:

- A working dev environment (Track B)
- Understanding of the quality gates and PR process
- A real PR shipped (or in review)
- Confidence to pick up their next task independently

Suggest: browse open GitHub issues, try `/fix` on a labeled `good-first-issue`, or ask their tech lead for their first real assignment.
