---
name: doc-cleanup
description: >-
  Systematic documentation audit and cleanup across the repository.
  Dispatches parallel agents to cross-reference docs against code, identify stale/outdated docs,
  archive completed design docs, fix inaccuracies, and optimize AGENTS.md / CLAUDE.md files.
  Use when docs may have drifted from code, after major refactors, or on a periodic schedule.
  Triggers on: "clean up docs", "audit documentation", "are our docs accurate", "doc sweep",
  "documentation cleanup", "check for stale docs".
---

# Documentation Cleanup

Systematic, agent-driven documentation audit that cross-references all documentation against actual code to find and fix staleness, archive completed work, and optimize `AGENTS.md` / `CLAUDE.md` files.

## When to Use

- After a major refactor or feature removal
- When `AGENTS.md` / `CLAUDE.md` feels bloated or inaccurate
- Periodic maintenance (monthly recommended)
- "clean up docs", "audit documentation", "are our docs accurate", "doc sweep"

## Scope

All markdown documentation across the repo:

- `AGENTS.md` and `CLAUDE.md` files (root + each component)
- `ARCHITECTURE.md` files
- Design docs (`**/docs/design/*.md`)
- Feature docs, specs, plans, READMEs
- API references, protocol specs, event specs

## Approach

### Phase 1: Discovery

Identify all documentation files:

```
Glob **/*.md
Glob **/AGENTS.md
Glob **/CLAUDE.md
Glob **/docs/**/*.md
Glob **/ARCHITECTURE*.md
```

Count and categorize:

- Living docs (`AGENTS.md`, `CLAUDE.md`, `ARCHITECTURE.md`, API refs) — must be accurate
- Design docs — check status field vs code reality
- Historical docs — should be clearly labeled or archived
- READMEs — verify against directory contents

### Phase 2: Parallel Investigation (Agent Swarm)

Dispatch 4+ agents in parallel using the `general-purpose` subagent type.

**Divide by top-level component.** Read `AGENTS.md` → Workspace Layout (or Repo Structure) to identify the major components of the repo. In a monorepo with services / libs / tools, dispatch one agent per component. In a flat repo, dispatch one agent per doc category (living docs, design docs, READMEs).

For each component-agent, instruct it to cross-reference that component's docs against its code:

- `AGENTS.md` / `CLAUDE.md` / `ARCHITECTURE.md` file trees, module structures, key files
- Public API docs vs actual API definitions (route handlers, proto messages, CLI entry points)
- Design docs under `docs/design/`: status field vs implementation reality
- Subdirectory READMEs vs actual directory contents
- Event / protocol specs vs actual emitted events

**Plus one Devil's Advocate agent** — independent quality challenge:

- Pick 3 random docs and verify accuracy against code
- Challenge: are "stale" docs actually planning docs that shouldn't be archived?
- Identify MISSING documentation gaps (code areas with no docs)
- Risk-assess the proposed changes

### Agent Prompt Template

Each agent prompt MUST include:

1. Specific doc files to review (list every file)
2. Method: read doc, verify claims against code, classify as CURRENT / STALE / ARCHIVE / OPTIMIZE
3. For STALE: note what specifically is wrong with file:line receipts
4. For ARCHIVE: note what feature/code was removed
5. Output format: summary table + detailed findings + recommendations for `AGENTS.md` / `CLAUDE.md`

### Classification Rules

| Classification | Criteria | Action |
|---------------|----------|--------|
| **CURRENT** | Doc matches code | No action |
| **STALE** | Doc has specific inaccuracies vs code | Fix the inaccuracies |
| **ARCHIVE** | Doc has explicit Removed/Deprecated/Superseded/Historical status | Move to `docs/archive/` with header |
| **OPTIMIZE** | Doc is accurate but can be improved (redundant, verbose, etc.) | Refactor |

**Do NOT archive Draft/Proposed docs** — these are planning documents with institutional value, even if unimplemented.

### Phase 3: Evidence Digest

Compile findings from all agents into:

1. Confirmed inaccuracies (multiple agents agree or code receipts provided)
2. Archive candidates (only docs with explicit completed/removed status)
3. `AGENTS.md` / `CLAUDE.md` optimization opportunities
4. Missing documentation gaps

### Phase 4: Implementation

Create a feature branch and implement changes:

**Archival process:**

1. Create `<component>/docs/archive/` directory if needed
2. `git mv` the file to the archive directory
3. Add header: `> **Archived**: This document describes completed work or historical context. Kept for reference.`
4. Do NOT delete — archive preserves institutional knowledge

**`AGENTS.md` / `CLAUDE.md` fixes:**

- Fix file references to deleted/renamed files
- Update counts (skill counts, event counts, etc.)
- Fix env var defaults that don't match code
- Fix pipeline / CI workflow paths
- Add missing entries to structure tables

**Staleness fixes:**

- Fix specific inaccuracies (endpoint names, class names, partition counts)
- Add staleness headers to docs that reference deleted code
- Update broken cross-references between docs

**Naming consistency:**

- Apply any pending naming sweeps (check `docs/naming-sweep-*.md`)

### Phase 5: Verification

Run quality checks on changed components:

```bash
# Verify no broken markdown links (basic check)
grep -r "](.*\.md)" --include="*.md" <changed_files> | grep -v node_modules

# Verify archived files are tracked by git
git status --short

# Verify key file references from AGENTS.md / CLAUDE.md still exist
# (agent should spot-check paths mentioned in the living docs)
```

## Archive Directory Convention

```
<component>/docs/archive/
  design/        # Completed/superseded design docs
  refactor/      # Historical refactor snapshots
  reports/       # Point-in-time analysis reports
  README.md      # Index of archived docs (optional)
```

Each archived file gets this header:

```markdown
> **Archived**: This document describes [completed work / a removed feature / a superseded approach]. Kept for historical reference.
```

## What NOT to Do

- Don't archive Draft/Proposed design docs (they contain valuable design rationale)
- Don't aggressively remove "redundant" content from `AGENTS.md` / `CLAUDE.md` (it may be intentional for standalone use)
- Don't restructure entire docs — make targeted fixes
- Don't update design doc status fields unless you've verified implementation against code
- Don't delete any documentation — always archive instead
