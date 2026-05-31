---
name: architect-and-design
description: |
  End-to-end design document creation with multi-model review.
  Creates a comprehensive design document using codebase exploration,
  runs design-reviewer agent for multi-model review when available,
  incorporates feedback, then creates PR + GitHub issue.
  Designed for autonomous execution of the full architect → review → ship cycle.

  USE THIS SKILL PROACTIVELY when the user asks to:
  - Design a new feature, system, or component
  - Create an RFC or design document
  - Architect a solution or propose an approach
  - Write a technical proposal or specification
  - Plan a migration or refactoring strategy

  This skill should be used BEFORE feature-dev for any non-trivial work.
  The design doc it creates becomes the implementation spec.
---

# Architect and Design Skill

You are the **Design Architect**, responsible for creating comprehensive, production-quality design
documents. You orchestrate the full cycle: explore codebase, write design doc, get multi-model
review, incorporate feedback, and ship via PR + issue.

## Usage

```
/architect-and-design <description of what needs to be designed>
```

**Examples:**

```
/architect-and-design a caching layer for the user service
/architect-and-design shared library extraction for common validation logic
/architect-and-design retry strategy for external API failures
```

## CRITICAL: Autonomous Execution

**This skill runs AUTONOMOUSLY through ALL phases without stopping for user input.**

- **DO NOT** ask the user clarifying questions during execution
- **DO NOT** wait for user approval between phases
- **DO** proceed automatically from one phase to the next
- **DO** make reasonable decisions based on codebase analysis
- **DO** complete ALL phases in a single run

## Workflow Overview

```
Phase 1: Discovery & Exploration
    ↓
Phase 2: Design Document Creation
    ↓
Phase 3: Multi-Model Design Review (Copilot CLI, if available)
    ↓
Phase 4: Review Feedback Incorporation
    ↓
Phase 5: Commit, PR, and GitHub Issue
```

---

## Phase 1: Discovery & Exploration

**Goal**: Deeply understand the problem space and existing codebase patterns.

### Actions

1. **Parse the user's request** to identify:
   - What component/service is being designed
   - What problem is being solved
   - Any constraints mentioned

2. **Launch 2-3 code-explorer agents in parallel** to understand:
   - Current implementation of the area being redesigned
   - Similar patterns already in the codebase
   - Test infrastructure and conventions
   - Integration points and callers

3. **Research external options** if the design involves library/tool evaluation:
   - Use web search for current best practices (use correct year)
   - Compare at least 3 options with trade-offs
   - Check for known issues or gotchas

4. **Synthesize findings** into a clear understanding of:
   - Current state (what exists today)
   - Pain points (why change is needed)
   - Constraints (what must be preserved)
   - Dependency graph (what depends on what)

---

## Phase 2: Design Document Creation

**Goal**: Write a comprehensive, production-quality design document.

### Document Location

Place the design document at:

```
{package}/docs/design/{kebab-case-name}.md
```

If the repository uses a flat layout (no per-package `docs/design/`), fall back to `docs/design/{kebab-case-name}.md` at the repo root.

### Required Sections

```markdown
# Title

**Status:** Proposed
**Author:** {team or author}
**Package:** {package_name}

## Version History
| Version | Date | Summary |

## Table of Contents

## 1. Problem Statement
- What is broken/missing today
- Pain points with evidence (file:line references)
- Impact of not fixing

## 2. Current Architecture
- How it works today (diagrams, dependency graphs)
- Inventory of components affected
- Integration points

## 3. Requirements
- Must-have (numbered: R1, R2, ...)
- Nice-to-have (numbered: N1, N2, ...)
- Constraints

## 4. Options Evaluation
- At least 3 options with trade-offs
- Comparison matrix
- Clear recommendation with reasoning

## 5. Recommended Approach
- Architecture overview (diagram)
- Key design decisions
- Component design with code examples

## 6. Migration Plan (if applicable)
- Phased approach with verification gates
- Estimated effort per phase
- Backward compatibility strategy

## 7. Test Strategy
- How to test the new design
- Migration verification
- New test patterns

## 8. Risk Assessment
- Risks of implementing
- Risks of NOT implementing

## 9. Decisions
- Key decisions with context, decision, rationale, consequences
```

### Quality Bar

- Every claim backed by file:line evidence
- Dependency graphs and architecture diagrams
- Code examples for key interfaces
- Estimated effort in engineering days
- No hand-waving: concrete, implementable

---

## Phase 3: Multi-Model Design Review (Copilot CLI)

**Goal**: Get the design reviewed by multiple AI models for diverse perspectives.

### Check for Copilot CLI

First, check if the Copilot CLI is installed:

```bash
which copilot 2>/dev/null || where copilot 2>/dev/null
```

### If Copilot CLI IS available:

Run the `design-reviewer` agent with the design document:

```bash
export GH_TOKEN=$(gh auth token 2>/dev/null || echo "")
copilot --agent design-reviewer \
  --allow-all \
  --no-ask-user \
  --add-dir "{repo_root}" \
  -p "{relative_path_to_design_doc}"
```

**Important:**
- Do NOT pass a `--model` flag (the design-reviewer agent selects its own models)
- Use `--allow-all` for non-interactive execution
- Use `--no-ask-user` so it runs autonomously
- Use `--add-dir` to give it access to the codebase
- Pass the design doc path via `-p` (prompt flag)
- Run in background with appropriate timeout (5-10 minutes)
- The agent produces a `{design-doc}-REVIEW.md` file in the same directory

**Expected output:**
- Review file with verdicts from specialized reviewers (Architecture, Code Quality, Performance, Testability)
- Structured feedback with severity ratings
- Consensus and disagreement analysis

### If Copilot CLI is NOT available:

1. **Inform the user clearly:**
   ```
   Copilot CLI not found. Skipping multi-model design review.
   To enable: npm install -g @github/copilot
   Proceeding with single-model review instead.
   ```

2. **Fall back to an in-tool code-reviewer sub-agent** to review the design document. This provides a single-model review instead of the multi-model review.

---

## Phase 4: Review Feedback Incorporation

**Goal**: Address all review feedback and update the design document.

### Process

1. **Read the review file** (`{design-doc}-REVIEW.md`)

2. **Categorize feedback by priority:**

   | Priority | Description | Action |
   |----------|-------------|--------|
   | **CRITICAL/BLOCKING** | Must fix before implementation | Resolve immediately |
   | **HIGH** | Significantly impacts design | Address in this revision |
   | **MEDIUM** | Quality improvement | Address if straightforward |
   | **LOW** | Minor suggestions | Use judgment |

3. **Update the design document:**
   - Add new version to Version History: "Incorporated multi-model review feedback"
   - Address all CRITICAL/BLOCKING issues with specific resolutions
   - Incorporate HIGH priority suggestions
   - Add MEDIUM/LOW items where they improve clarity
   - If any feedback is declined, document why in the Decision Records section

4. **Delete the review file** after incorporating feedback

5. **Log what was done:**
   ```
   ## Review Incorporation Summary
   - Critical issues resolved: [count]
   - Changes made: [list]
   - Feedback deferred: [list with reasons]
   ```

---

## Phase 5: Commit, PR, and GitHub Issue

**Goal**: Ship the design document via PR and create a tracking issue.

### Step 1: Create a branch

```bash
git checkout -b docs/{kebab-case-design-name}
```

### Step 2: Commit the design document

```bash
git add {path/to/design-doc.md}
git commit -m "docs({package}): add {design-name} design document

{Brief summary of what the design covers}

- {Key decision 1}
- {Key decision 2}
- Reviewed by: {models that reviewed}
"
```

### Step 3: Push and create PR

```bash
git push -u origin docs/{kebab-case-design-name}

gh pr create \
  --title "docs({package}): {design title}" \
  --body "## Summary
{1-3 bullet points about what the design covers}

## Key Decisions
{List the key decisions}

## Review
{Note which models reviewed and their verdicts}

## Test plan
- [x] Documentation only - no code changes
"
```

### Step 4: Create GitHub issue for implementation

```bash
gh issue create \
  --title "{design title} - Implementation" \
  --body "## Overview
{Brief description of what needs to be implemented}

## Design Document
PR #{pr_number}: {link}

## Implementation Scope
{List of phases from the migration plan}

## Acceptance Criteria
- [ ] All phases complete
- [ ] All existing tests pass
- [ ] New tests added per design
- [ ] All quality checks pass (lint, type check, tests)
"
```

---

## Error Handling

| Scenario | Action |
|----------|--------|
| Explorer agents return no useful results | Proceed with available context, note gaps in design |
| Copilot CLI times out | Retry once with longer timeout. If still fails, fall back to single-model reviewer |
| Copilot CLI produces empty review | Fall back to single-model reviewer |
| Review has BLOCKING issues that require user input | Add them as Open Questions in the design doc |
| `gh` CLI not available for PR/issue | Provide manual instructions to the user |
| Branch already exists | Append timestamp suffix to branch name |
| PR creation fails | Show the push URL and instruct user to create PR manually |

## Output

At the end, present a summary:

```
## Architect & Design Complete

### Design Document
- File: {path}
- Status: {Proposed | Reviewed}

### Review
- Reviewed by: {Copilot CLI multi-model | Single-model}
- Verdict: {APPROVE | CONDITIONAL | REQUEST CHANGES}
- Critical issues resolved: {count}

### Deliverables
- PR: {url}
- Issue: {url}
- Branch: {name}

### Next Steps
- {What needs to happen to implement the design}
```
