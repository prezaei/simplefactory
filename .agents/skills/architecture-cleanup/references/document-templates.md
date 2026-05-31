# Document Templates

All templates used by the architecture-cleanup skill for output artifacts.

---

## Refactor Document Template

Generate at: `docs/refactor/YYYY-MM-DD-{component}-refactor.md`

For monorepos, place in: `{component_path}/docs/refactor/YYYY-MM-DD-{component}-refactor.md`

**Directory Structure:**
```
docs/
└── refactor/
    ├── README.md                           # Documentation about refactor process
    ├── YYYY-MM-DD-{component}-refactor.md  # Current refactor plan
    └── archive/                            # Completed/superseded plans
        └── .gitkeep
```

Create these directories and README if they don't exist.

```markdown
# Architecture Refactor Plan: {Component}

**Generated:** YYYY-MM-DD
**Component:** {component}
**Status:** Draft
**Target Scale:** Millions of requests/day
**Adversarial Review:** Passed (N findings confirmed, M dismissed, K deferred)

## Executive Summary

[2-3 paragraph overview of component health, production readiness, and key findings.
Include: how many findings were proposed vs. how many survived adversarial challenge.]

## Architecture Overview

### Current State
```
[ASCII diagram of current architecture]
```

### Confirmed Issues (Survived Adversarial Challenge)

| ID | Priority | Category | Description | Confidence | Evidence | Regression Risk |
|----|----------|----------|-------------|------------|----------|-----------------|
| R001 | P0 | Concurrency | Potential deadlock in... | 85 | file:123 | Low — isolated code path |
| R002 | P0 | SPOF | No failover for... | 78 | file:45 | Medium — requires migration |
| R003 | P1 | Resource | Connection leak in... | 65 | file:89 | Low — additive change |

### Dismissed Findings (Killed by Adversarial Challenge)

| ID | Original Finding | Dismissed Because | Challenger |
|----|------------------|-------------------|------------|
| D001 | Race condition in X | Code path is single-threaded; async but not concurrent | Devil's Advocate |
| D002 | Missing circuit breaker on Y | Y is internal RPC with <1ms latency, circuit breaker adds complexity without value | Devil's Advocate |

### Deferred Findings (Too Risky Without Mitigation)

| ID | Finding | Regression Risk | Required Mitigation Before Fix |
|----|---------|-----------------|-------------------------------|
| DF001 | Refactor shared state in Z | High — 15 callers, 3 test files | Add integration tests for all callers first |

### DA Challenge Dispositions

| # | Finding ID | DA Challenge | Acceptance Criteria | Disposition | Evidence |
|---|-----------|-------------|-------------------|-------------|----------|
| 1 | R001 | [DA's challenge] | [What evidence satisfies DA] | REFUTED/ABSORBED/DEFERRED/UNRESOLVED | [Receipt] |

## Production Readiness Analysis

### Concurrency & Thread Safety

| Issue | Location | Severity | Confidence | Risk at Scale |
|-------|----------|----------|------------|---------------|
| Unprotected shared state | `file:123` | Critical | 85 | Data corruption under load |

### Scalability Assessment

| Component | Current Limit | Target | Gap | Bottleneck |
|-----------|---------------|--------|-----|------------|
| [Component] | [Current] | [Target] | [Gap] | [Bottleneck description] |

### Fault Tolerance

| Failure Mode | Current Behavior | Required Behavior | Gap |
|--------------|------------------|-------------------|-----|
| External API timeout | Hangs indefinitely | Circuit breaker + fallback | Missing circuit breaker |

### Resource Management

| Resource | Management | Issue | Fix |
|----------|------------|-------|-----|
| Network connections | Manual close | Leak on exception | Use proper cleanup patterns |

### Single Points of Failure

| SPOF | Impact | Mitigation Required |
|------|--------|---------------------|
| [Component] | [Impact description] | [Required mitigation] |

## Regression History (from Regression Scout)

### Known Fragile Areas

| Area | Past Issues | Pattern | Implication for This Refactor |
|------|-------------|---------|-------------------------------|
| [Area] | #123, #456 | Fix-refix chain | Extra caution required |

### Reverted/Failed Refactors

| PR/Commit | What Was Attempted | Why It Failed | Lesson |
|-----------|-------------------|---------------|--------|
| #789 | Refactored X | Broke Y due to Z | Must test Y when touching X |

## Design Pattern Analysis

### Patterns Found
| Pattern | Location | Usage | Assessment |
|---------|----------|-------|------------|
| [Pattern] | `path/file` | [Purpose] | [Assessment] |

### Missing Patterns
| Recommended Pattern | Where | Why |
|---------------------|-------|-----|
| Circuit Breaker | External API calls | Fault tolerance at scale |

### Anti-Patterns Detected
| Anti-Pattern | Location | Impact | Remediation | Confidence |
|--------------|----------|--------|-------------|------------|
| God Object | `path/file` | Maintainability | Split responsibilities | 75 |

### Large Files (Refactor Targets)

| File | Lines | Priority | Responsibilities | Suggested Split |
|------|-------|----------|------------------|-----------------|
| `path/large_file` | 1800 | **P0** | [Responsibilities] | [Split strategy] |

**Thresholds:** >1500 lines = P0, >800 lines = P1, >500 lines = P2

## Test Coverage Analysis

### Coverage Summary
| Component | Current | Target | Gap |
|-----------|---------|--------|-----|
| [Area] | [Current]% | 90% | [Gap]% |

### Critical Untested Paths
1. `file:function()` - [Why critical for production]

### Missing Test Types
- [ ] Load tests for capacity limits
- [ ] Chaos tests for failure injection
- [ ] Concurrency tests for race conditions

## Refactor Roadmap

### P0 — Critical Production Issues
- [ ] R001: [Description]
  - **Effort:** [S/M/L]
  - **Risk:** [Low/Medium/High] ([reason])
  - **Regression Mitigation:** [What to test after this change]
  - **Files:** `path/file`

### P1 — Scalability Improvements
- [ ] R003: [Description]
  - **Effort:** [S/M/L]
  - **Risk:** [Low/Medium/High]
  - **Regression Mitigation:** [What to test]
  - **Files:** `path/file`

### P2/P3 — Quality Improvements
- [ ] R004: [Description]
- [ ] R005: [Description]

## Deletion Manifest

**Code to Remove** (worth 2x additions):

| File/Code | Lines | Reason |
|-----------|-------|--------|
| `path/deprecated_file` | [N] | [Reason] |

## Dependencies

| Refactor | Depends On | Blocked By |
|----------|------------|------------|
| R003 | - | R001 |

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Breaking changes | Medium | High | Baseline test lock, incremental commits |
| Performance regression | Low | High | Load test before/after |
| Regression in adjacent code | Medium | High | Run full test suite after each item |

## Load Testing Recommendations

Before deploying refactors, validate with:

| Test | Tool | Target | Acceptance |
|------|------|--------|------------|
| Sustained load | k6/locust/artillery | [Target RPS] for 1 hour | No errors, p99 < 500ms |
| Spike test | [Tool] | 0 → [Peak] in 10s | Graceful degradation |
| Soak test | [Tool] | [Sustained RPS] for 24 hours | No memory leaks |
```

---

## PR Body Template

Used in Phase 5.3 when creating the PR with `gh pr create`.

```markdown
## Summary

Architecture cleanup analysis for `{component}` component.
Analyzed by 5 discovery agents, challenged by 2 adversarial agents.

### Adversarial Review Results

| Phase | Count | Description |
|-------|-------|-------------|
| Findings Proposed | N | From 5 discovery agents |
| Confirmed | N | Survived adversarial challenge (confidence >= 50) |
| Dismissed | N | Killed by Devil's Advocate (false positives) |
| Deferred | N | Regression risk too high without mitigation |

### Confirmed Issues

| Priority | Count | Key Issues |
|----------|-------|------------|
| P0 Critical | N | [brief list] |
| P1 High | N | [brief list] |
| P2 Medium | N | [brief list] |
| P3 Low | N | [brief list] |

### Top Findings

1. **[Finding 1]** - Confidence: X - Brief description
2. **[Finding 2]** - Confidence: X - Brief description
3. **[Finding 3]** - Confidence: X - Brief description

### Refactor Document

See `docs/refactor/YYYY-MM-DD-{component}-refactor.md` for:
- Full architecture analysis with evidence chains
- Adversarial review log (what was dismissed and why)
- Regression risk assessment per finding
- Prioritized implementation roadmap

### Related Issue

Implementation tracked in: #{issue_number}

---

Generated by `/architecture-cleanup` skill (adversarial mode)
```

---

## Commit Message Template

Used in Phase 5.2.

```
docs({component}): architecture cleanup plan YYYY-MM-DD

Adversarial analysis (5 discovery + 2 challenge agents):
- Findings proposed: N
- Confirmed (survived challenge): N
- Dismissed (false positives killed): N
- Deferred (regression risk): N

Priority breakdown of confirmed issues:
- P0 Critical: N issues
- P1 High: N issues
- P2 Medium: N issues
- P3 Low: N issues

Key findings:
- [Top 3 findings summary]

See refactor document for full analysis, adversarial review log, and roadmap.

Co-Authored-By: Claude <noreply@anthropic.com>
```

---

## GitHub Issue Template

Used in Phase 6.1 with `gh issue create`. Labels: `refactor`, `architecture`, `automated`.

```markdown
## Overview

This issue tracks implementation of architecture improvements identified in the adversarial cleanup analysis.

**Refactor Plan:** `docs/refactor/YYYY-MM-DD-{component}-refactor.md`
**PR:** #{pr_number}
**Target:** 100% resolution of all confirmed issues
**Adversarial Review:** N findings confirmed, M dismissed, K deferred

## Agent Instructions

This issue is designed for **autonomous implementation** by Claude with `/feature-dev:feature-dev`.

### How to Work on This Issue

1. Read the refactor document at the path above
2. Implement issues in priority order: P0 → P1 → P2 → P3
3. For each item:
   - Run baseline tests FIRST (record what passes)
   - Create a focused commit
   - Run quality checks: {discovered quality commands}
   - Verify no regressions (all previously-passing tests still pass)
   - Update the refactor document (mark item complete)
4. Reference this issue in commits: `Fixes #{issue_number} (R00X)`
5. Push changes to the PR branch: `refactor/{component}-cleanup-YYYY-MM-DD`

### Implementation Constraints

- **Regression Guard:** Run full test suite before AND after each item. No regressions allowed.
- **Thread/Concurrency Safety:** All changes must be safe for concurrent execution
- **Backward Compatibility:** Maintain API compatibility unless explicitly approved
- **Testing:** Add/update tests for any behavior changes
- **No New Dependencies:** Avoid adding new packages without justification
- **Incremental:** Each refactor item should be a separate, reviewable commit
- **Blast Radius Awareness:** Check the "Regression Risk" column for each item

## Issue Summary

| Priority | Count | Items |
|----------|-------|-------|
| P0 Critical | N | R001, R002, ... |
| P1 High | N | R003, R004, ... |
| P2 Medium | N | R005, R006, ... |
| P3 Low | N | R007, R008, ... |

**Total:** N confirmed issues (M dismissed by adversarial review)

## Implementation Checklist

### P0 - Critical (Must Fix Immediately)
- [ ] **R001**: [Description] - `file` - Confidence: X - Regression Risk: [Low/Med/High]
- [ ] **R002**: [Description] - `file` - Confidence: X - Regression Risk: [Low/Med/High]

### P1 - High Priority
- [ ] **R003**: [Description] - `file` - Confidence: X - Regression Risk: [Low/Med/High]

### P2 - Medium Priority
- [ ] **R005**: [Description] - `file` - Confidence: X - Regression Risk: [Low/Med/High]

### P3 - Low Priority
- [ ] **R007**: [Description] - `file` - Confidence: X - Regression Risk: [Low/Med/High]

## Pre-Resolved Questions

The following questions were identified and answered during analysis to enable autonomous implementation:

### Architecture Questions

**Q1: [Question about architecture decision]**
> **A:** [Detailed answer with reasoning and code references]

### Implementation Questions

**Q2: [Question about specific refactor item]**
> **A:** [Answer with file paths and code patterns to follow]

### Testing Questions

**Q3: [Question about test coverage]**
> **A:** [Answer with test file locations and patterns]

## Context Files

Key files the implementing agent should understand:

| File | Purpose | Relevance |
|------|---------|-----------|
| `{path}` | [Purpose] | [Why needed for this refactor] |

## Acceptance Criteria

- [ ] All P0 issues resolved
- [ ] All P1 issues resolved
- [ ] All P2 issues resolved
- [ ] All P3 issues resolved
- [ ] No regressions (all previously-passing tests still pass)
- [ ] All quality checks pass ({discovered quality commands})
- [ ] Refactor document updated with completion status
- [ ] PR ready for review

---

Generated by `/architecture-cleanup` skill (adversarial mode)
```

---

## Status Checkpoint Template

Used in Phase 7 — output for user awareness, then immediately proceed to Phase 8.

```markdown
## Architecture Cleanup Ready: {component}

### Adversarial Review Summary
- **Findings Proposed:** N (from 5 discovery agents)
- **Confirmed:** N (survived adversarial challenge)
- **Dismissed:** M (false positives killed by Devil's Advocate)
- **Deferred:** K (regression risk too high)

### Artifacts Created
- **PR:** #{pr_number} - [link]
- **Issue:** #{issue_number} - [link]
- **Refactor Document:** docs/refactor/YYYY-MM-DD-{component}-refactor.md

### Confirmed Issues by Priority
| Priority | Count | Key Items |
|----------|-------|-----------|
| P0 Critical | N | [summary] |
| P1 High | N | [summary] |
| P2 Medium | N | [summary] |
| P3 Low | N | [summary] |

### Top 5 Production Risks (Confirmed)
1. **[Risk 1]** - Confidence: X - [Impact at scale]
2. **[Risk 2]** - Confidence: X - [Impact at scale]
3. **[Risk 3]** - Confidence: X - [Impact at scale]
4. **[Risk 4]** - Confidence: X - [Impact at scale]
5. **[Risk 5]** - Confidence: X - [Impact at scale]

### Key Dismissals (False Positives Killed)
1. **[Dismissed 1]** - [Why it was wrong]
2. **[Dismissed 2]** - [Why it was wrong]

### Pre-Resolved Questions
The GitHub issue includes N pre-answered questions.

---
**Proceeding to Phase 8: Implementation...**
```

---

## Completion Report Template

Used in Phase 8.4 after all implementations complete.

```markdown
## Cleanup Cycle Complete: {component}

**Date:** YYYY-MM-DD
**Total Confirmed Issues:** X
**Resolved:** X (100%)
**Deferred:** 0 (or list with justification)
**Regressions Encountered:** N (all fixed before proceeding)

### Adversarial Review Impact
- **Findings Proposed:** N (from discovery)
- **Confirmed:** N (entered implementation)
- **Dismissed:** M (saved wasted effort on false positives)
- **False Positive Rate:** M/N (X%)

### Issues Resolved by Priority
| Priority | Count | Examples |
|----------|-------|----------|
| P0 | N | R001, R002 |
| P1 | N | R003, R004 |
| P2 | N | R005 |
| P3 | N | R006, R007 |

### Key Improvements
- [Production readiness improvement]
- [Architecture improvement]
- [Test coverage improvement]

### Regression Guard Results
- Baseline tests: [N] passing
- Final tests: [N] passing (no regressions)
- Quality checks: PASS

### Deferred Items (if any)
| ID | Reason | Follow-up |
|----|--------|-----------|
| (none - 100% completion target) |

### Metrics Delta
| Metric | Before | After | Change |
|--------|--------|-------|--------|
| Concurrency issues | X | 0 | -X |
| Test coverage | X% | Y% | +Z% |

### Next Cycle Recommendations
- [Any new issues discovered during implementation]
- [Suggested focus areas for next review]
```

---

## Final Summary Template

Output after Phase 8 completion:

```markdown
## ARCHITECTURE CLEANUP COMPLETE

**Component:** {component}
**Date:** YYYY-MM-DD
**Total Confirmed Issues:** N
**Resolved:** N (100%)
**False Positives Killed:** M (saved effort)

### Artifacts
- **PR:** #{pr_number} - [link]
- **Issue:** #{issue_number} - [link] (CLOSED)
- **Commits:** N refactor commits

### Summary
All confirmed issues have been implemented with zero regressions.
Adversarial review dismissed M false positives that would have been wasted effort.
The PR is ready for review.

---
Architecture cleanup cycle completed successfully.
```

---

## Progress Tracking Template

Used in `docs/refactor/PROGRESS.md` for scheduling and continuous execution.

```markdown
# Architecture Cleanup Progress

**Target: 100% resolution of all confirmed issues per cycle**

## Current Cycles

### {component}
**Started:** YYYY-MM-DD
**Plan:** docs/refactor/YYYY-MM-DD-{component}-refactor.md
**PR:** #123 - [link]
**Issue:** #124 - [link]
**Branch:** refactor/{component}-cleanup-YYYY-MM-DD
**Status:** In Progress
**Progress:** 12/15 (80%) — NOT COMPLETE

**Adversarial Review:**
- Proposed: 20 | Confirmed: 15 | Dismissed: 4 | Deferred: 1

| Priority | Total | Done | Remaining |
|----------|-------|------|-----------|
| P0 | 2 | 2 | 0 |
| P1 | 5 | 4 | 1 |
| P2 | 4 | 3 | 1 |
| P3 | 4 | 3 | 1 |

**Remaining Items:**
- [ ] R008 (P1): [Description]
- [ ] R011 (P2): [Description]
- [ ] R014 (P3): [Description]

**To Resume:**
```
/feature-dev:feature-dev Implement GitHub Issue #124
```

## Metrics
| Component | Metric | Before | After | Delta |
|-----------|--------|--------|-------|-------|
| {component} | Concurrency Issues | 5 | 0 | -5 |
| {component} | Test Coverage | 75% | 90% | +15% |
| {component} | False Positives Killed | 4 | — | -4 (saved effort) |

## History (100% completion required)
| Date | Component | Plan | PR | Issue | Proposed | Confirmed | Completed | % |
|------|-----------|------|-----|-------|----------|-----------|-----------|---|

## Deferred Items Log
| Date | Component | ID | Reason | Follow-up Date |
|------|-----------|-----|--------|----------------|
| (empty - no deferrals) |
```
