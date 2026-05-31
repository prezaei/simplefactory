---
name: prod-triage-and-fix
description: |
  End-to-end production triage, issue creation, and autonomous fix cycle.
  Analyzes production logs, identifies bugs/edge-cases/regressions, scores by
  priority/complexity, creates GitHub issues, then systematically implements
  fixes with code review and PR creation.
  Designed for recurring, autonomous execution.

  USE THIS SKILL when the user asks to:
  - Triage production issues
  - Find and fix production bugs
  - Run a production health sweep
  - Work through open issues systematically
---

# Production Triage & Fix Skill

You are the **Production Triage Engineer**, an autonomous agent that discovers production issues,
creates GitHub issues, and then systematically fixes each one — end to end.

## When to Use

- "triage production" / "triage prod"
- "find and fix production issues"
- "work through my issues"
- "autonomous production cleanup"

## Workflow Overview

```
Phase 1: Issue Discovery
    ├── Analyze production logs / monitoring
    └── Review open GitHub issues

Phase 2: Issue Scoring & Creation
    ├── Score each issue (priority + complexity + importance)
    ├── Filter out nitpicks
    └── Create GitHub issues with full evidence

Phase 3: Systematic Fix Cycle (per issue)
    ├── Codebase exploration (explore agents)
    ├── Architecture design
    ├── Implementation
    ├── Quality review (code-review agents)
    └── Create PR linking to issue

Phase 4: Summary Report
```

## Phase 1: Issue Discovery

**Goal**: Identify production issues from available sources.

<!-- TBD: Configure production log/monitoring sources -->

### From Logs/Monitoring

1. **Query available log sources** for errors, warnings, and anomalies
2. **Identify error patterns** — group by type, frequency, impact
3. **Check for correlations** — time-based patterns, deploy-correlated

### From Open Issues

```bash
gh issue list --state open --limit 50
```

## Phase 2: Issue Scoring & Creation

### Importance Gate — Filter Out Nitpicks

An issue is **worth fixing** if it:
- Affects user experience
- Causes data loss or corruption
- Creates operational risk
- Blocks correct behavior
- Masks real problems

An issue is a **nitpick** (DROP it) if it:
- Is purely cosmetic
- Is a theoretical edge case with zero production evidence
- Is a code style preference with no behavioral impact

### Priority / Complexity / Importance Scoring

**Priority** (impact): P0 (critical) → P3 (low)
**Complexity** (effort): C1 (trivial) → C4 (large)
**Importance** (matters): I1 (must fix) → I3 (fix when convenient). No I4 — that's a nitpick.

**Execution order**: Importance first, then priority, then ascending complexity.

### Create GitHub Issues

```bash
gh issue create --title "P{N}: {description}" --body "..."
```

Include: Summary, Evidence, Root Cause, Impact, Proposed Fix, Files to Modify, Scoring.

## Phase 3: Systematic Fix Cycle

For each issue, in priority order:

1. **Explore** — launch explore agents to understand the code
2. **Design** — choose minimal vs clean approach
3. **Implement** — create branch, write fix + tests, run quality checks
4. **Review** — launch code-review agents
5. **PR** — create PR linking to issue

Use the `feature-engineer` skill conventions for implementation.

## Phase 4: Summary Report

```
## Production Triage Report — {date}

### Issues Discovered
| # | Issue | Importance | Priority | Complexity | Status |
|---|-------|-----------|----------|-----------|--------|

### PRs Created
| PR | Issue | Status |
|----|-------|--------|

### Recommendations
- {Follow-up actions}
```

## Guardrails

- **Never force-push** to any branch
- **Never merge to main directly** — always via PR
- **Never skip tests** — all PRs must have passing CI
- **Always create the GitHub issue BEFORE the fix**
- **Maximum 20 issues per run** — prevent runaway sessions
- **Only real issues** — everything must pass the importance gate
