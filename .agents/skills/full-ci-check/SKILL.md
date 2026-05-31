---
name: full-ci-check
description: |
  Run complete CI validation locally, iterate to green.

  USE THIS SKILL when the user asks to:
  - Run CI locally
  - Check if code is ready to push
  - Run all quality checks
  - Verify everything passes
---

# Full CI Check Skill

You are the **CI Check Engineer**, responsible for running the complete CI validation suite
locally and iterating until everything passes.

## When to Use

- "run CI"
- "full ci check"
- "is it green"
- "ready to push"
- "check everything"

## Workflow

### Step 1: Detect Affected Components

Determine which workspace members have changes:

```bash
# Check what's changed vs main
git --no-pager diff origin/main --name-only
```

### Step 2: Run Full Quality Suite

```bash
# Sync dependencies
uv sync --all-packages

# Lint all code
uv run ruff check .

# Check formatting
uv run ruff format --check .

# Run all tests
uv run pytest -v --tb=short
```

### Step 3: Fix Failures and Iterate

For each failure:

1. **Lint failures**: Fix the specific ruff violation
2. **Format failures**: Run `uv run ruff format .`
3. **Test failures**: Investigate and fix the root cause
4. **Dependency issues**: Run `uv sync --all-packages` to update

After fixing, re-run the failing check to validate, then re-run the full suite.

**Iterate up to 3 times.** If still failing after 3 iterations, report the remaining issues.

### Step 4: Report

```
## CI Check Results

### Quality Checks
- ✅/❌ Ruff lint
- ✅/❌ Ruff format
- ✅/❌ Pytest ({N} passed, {N} failed)

### Status: {READY TO PUSH / NEEDS WORK}
{Details of any remaining issues}
```

## Guardrails

- **Never skip checks** — all must pass
- **Fix related issues** — if fixing one thing breaks another, fix both
- **Report honestly** — don't hide failures
