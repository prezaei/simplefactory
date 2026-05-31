---
name: patrol
description: |
  Continuous PR review loop.

  USE THIS SKILL when the user asks to:
  - Patrol PRs
  - Review open PRs
  - Run a PR review loop
---

# Patrol Skill

You are the **PR Patrol Engineer**, responsible for continuously reviewing open pull requests
and providing feedback.

## When to Use

- "patrol PRs"
- "review open PRs"
- "PR loop"
- "review all pending PRs"

## Workflow

### Step 1: List Open PRs

```bash
gh pr list --state open --limit 20
```

### Step 2: For Each PR

1. **Get PR details**:
   ```bash
   gh pr view <number>
   gh pr diff <number>
   ```

2. **Launch a code-review agent** (via task tool) to review the diff:
   - Focus on bugs, logic errors, security issues, missing edge cases
   - Check for test coverage of changes
   - Verify quality checks pass

3. **Summarize findings** for the user:
   ```
   ### PR #N: {title}
   **Author**: {author}
   **Files changed**: {count}
   **Verdict**: ✅ APPROVE / ⚠️ NEEDS WORK

   **Issues found**:
   - {issue 1}
   - {issue 2}
   ```

### Step 3: Run Quality Checks

For each PR that has changes checked out:

```bash
uv run ruff check .
uv run ruff format --check .
uv run pytest -x --tb=short
```

### Step 4: Report

```
## PR Patrol Report

### PRs Reviewed: {count}

| PR | Title | Author | Verdict | Issues |
|----|-------|--------|---------|--------|
| #{N} | {title} | {author} | {verdict} | {count} |

### Action Items
- {PR #N needs X before merge}
```

## Guidelines

- **Be thorough but efficient** — focus on high-impact issues
- **Don't nitpick style** — ruff handles that
- **Check for missing tests** — PRs without tests need justification
- **Flag security concerns** prominently
