# Implementation Guide

Detailed procedures for Phases 6, 7, and 8 of the architecture cleanup workflow, plus scheduling and continuous execution.

---

## Phase 6: Issue Creation & Question Pre-Resolution

Create a GitHub issue that enables autonomous agent implementation. The issue must be **self-contained** with all context needed for an agent to implement without asking clarifying questions.

### 6.1 Create Implementation Issue

Use `gh issue create` with the GitHub Issue Template from `references/document-templates.md`. Labels: `refactor`, `architecture`, `automated`.

**Store the issue number** and update the PR body with the issue reference.

### 6.2 Pre-Resolve Clarifying Questions

**CRITICAL:** Run `/feature-dev:feature-dev` in planning mode to identify questions an implementing agent would ask, then answer them yourself.

**Call using Skill tool:**

```
skill: "feature-dev:feature-dev"
args: "PLANNING MODE - DO NOT IMPLEMENT

Review the architecture refactor plan at docs/refactor/YYYY-MM-DD-{component}-refactor.md

Your task: Identify ALL clarifying questions you would need answered before implementing this refactor autonomously.

For each refactor item (R001, R002, etc.), list:
1. Questions about the current implementation
2. Questions about the target implementation
3. Questions about testing requirements
4. Questions about dependencies or side effects
5. Questions about regression risk mitigation

Format your response as:
## Questions for R001: [Title]
- Q1: [Question]
- Q2: [Question]

## Questions for R002: [Title]
- Q1: [Question]

... and so on.

DO NOT start implementing. Only output questions."
```

### 6.3 Answer Questions Using Codebase Analysis

For each question returned by `/feature-dev`:

1. **Search the codebase** to find the answer
2. **Read relevant files** to understand context
3. **Document the answer** with:
   - Specific file paths and line numbers
   - Code patterns to follow
   - Examples from existing code
   - Edge cases to consider

### 6.4 Update Issue with Q&A

After answering all questions, update the GitHub issue:

```bash
gh issue edit {issue_number} --body "$(cat <<'EOF'
[Updated issue body with filled-in Q&A section]
EOF
)"
```

### 6.5 Verify Issue Completeness

Before proceeding, verify the issue is self-contained:

**Checklist:**
- [ ] Refactor document path is correct and accessible
- [ ] PR number is linked
- [ ] All confirmed refactor items listed in checklist with confidence scores
- [ ] Pre-resolved questions section is populated
- [ ] Context files listed with purposes
- [ ] Implementation constraints clearly stated (including regression guard)
- [ ] Acceptance criteria defined

---

## Phase 7: Status Checkpoint (Automatic)

**This phase is automatic — DO NOT ask for user approval. Proceed directly to Phase 8.**

Output the Status Checkpoint Template from `references/document-templates.md` for the user's awareness, then immediately continue to Phase 8.

**IMPORTANT:** After outputting this status, immediately proceed to Phase 8. Do NOT wait for user input.

---

## Phase 8: Implementation

For each confirmed refactor item, invoke the feature-dev skill. Implementation is tracked via the GitHub issue created in Phase 6.

### 8.0 Baseline Test Lock

**Before ANY implementation, establish a regression baseline:**

1. Run ALL quality checks and capture the **full output** (do NOT truncate with `tail` or `head` — full output is required to detect individual test regressions):
   ```bash
   # Example for Python projects:
   ruff check {component}/ --ignore I001 2>&1
   mypy --config-file {component}/pyproject.toml {component}/ 2>&1
   pytest {component}/tests/unit/ -v --tb=short -q 2>&1
   ```

2. Record the **specific list of passing and failing tests** — this is the **regression baseline**. A summary count alone is insufficient: if a refactor fixes one test but breaks another, the total count stays the same but a regression exists.
3. Every implementation step MUST maintain or improve this baseline
4. If ANY test that previously passed now fails → **STOP and fix the regression** before proceeding

### 8.1 Implementation Strategy

**Goal: 100% resolution of ALL confirmed issues.**

Process refactors in dependency and priority order:
1. **P0 (Critical)** — Production safety: deadlocks, race conditions, security
2. **P1 (High)** — Scalability: SPOFs, resource leaks, architectural violations
3. **P2 (Medium)** — Reliability: circuit breakers, resource handling, test gaps
4. **P3 (Low)** — Quality: style, optimizations, documentation

**Do NOT skip lower priority items.** They often represent:
- Quick wins with high code quality impact
- Technical debt that compounds over time
- Issues that become P0/P1 under load

**Completion Criteria**: A cleanup cycle is complete only when ALL confirmed items are resolved or explicitly deferred with documented justification.

### 8.2 Invoke Feature Dev

For each refactor item, use the Skill tool. Reference the GitHub issue for full context:

```
skill: "feature-dev:feature-dev"
args: "Implement refactor R00X: [description]

GitHub Issue: #{issue_number}
PR Branch: refactor/{component}-cleanup-YYYY-MM-DD

Context:
- Component: {component}
- Refactor plan: docs/refactor/YYYY-MM-DD-{component}-refactor.md
- Priority: [P0/P1/P2/P3]
- Confidence: [score from adversarial review]
- Regression Risk: [Low/Medium/High from Regression Risk Analyst]
- Category: [Concurrency/SPOF/Resource/Pattern/etc.]
- Files affected: [list]

Requirements:
- [Specific requirements from refactor plan]

Regression Guard:
- Run full quality checks after implementation
- Compare test results against baseline (recorded in Phase 8.0)
- If any previously-passing test now fails, fix the regression before proceeding
- Check the 'Regression Risk' notes for this item in the refactor document

Production Constraints:
- Must be safe for concurrent execution
- Must handle failures gracefully
- Must not introduce new SPOFs
- Must maintain backward compatibility unless explicitly approved
- Update refactor document status when complete
- Reference issue in commit: Fixes #{issue_number} (R00X)"
```

**Alternative: Autonomous Pickup**

An agent can pick up the entire issue autonomously:

```
skill: "feature-dev:feature-dev"
args: "Implement GitHub Issue #{issue_number}

This is an architecture cleanup issue with pre-resolved questions and adversarial review.
Read the issue body for full context, implementation checklist, confidence scores, and Q&A.

CRITICAL: Run baseline tests FIRST, then implement all items in priority order (P0→P1→P2→P3).
After EACH item, verify no regressions against baseline.

Push commits to branch: refactor/{component}-cleanup-YYYY-MM-DD"
```

### 8.3 Post-Implementation Regression Check

After each refactor item:
1. Run quality checks: `{discovered quality commands}`
2. **Compare test results against Phase 8.0 baseline**
3. **If ANY regression detected:**
   - STOP implementing new items
   - Fix the regression immediately
   - Verify baseline is restored
   - Only then proceed to next item
4. Update the refactor document (mark item complete)
5. Update the GitHub issue checklist (check off completed item)
6. Create commit with message: `refactor({component}): R00X - [description]`
7. Push to PR branch

### 8.4 Final Summary

After all implementations complete, output the Completion Report Template and Final Summary Template from `references/document-templates.md`.

**IMPORTANT**: If any items remain unresolved, document clear justification and create follow-up tasks. The goal is always 100% resolution.

---

## Scheduling & Continuous Execution

This skill is designed for recurring execution. When run continuously:

### First Run
- Discover repo structure and component selection
- Full analysis (5 discovery + 2 adversarial agents)
- Create baseline refactor document
- Implement approved items with regression guards

### Subsequent Runs
1. Check for existing refactor document and GitHub issue for selected component
2. If exists and has incomplete items:
   - Check `PROGRESS.md` for issue number and PR branch
   - Check if code has changed (git diff against PR branch)
   - Resume implementation via: `/feature-dev:feature-dev Implement GitHub Issue #{issue_number}`
   - Or update analysis for significantly changed areas
3. If all items complete or document >7 days old:
   - Close the GitHub issue with completion summary
   - Merge or close the PR
   - Archive old document to `docs/refactor/archive/`
   - Start fresh analysis cycle

### Progress Tracking

Create/update `docs/refactor/PROGRESS.md` using the Progress Tracking Template from `references/document-templates.md`.

**Completion Policy:**
- A cycle is NOT complete until 100% of confirmed issues are resolved
- Incomplete cycles carry forward to the next session
- Deferred items require documented justification and follow-up date
- Recurring runs continue from where they left off
