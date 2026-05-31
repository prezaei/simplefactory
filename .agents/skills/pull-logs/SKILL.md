---
name: pull-logs
description: |
  Pull and analyze logs from CI builds or production.

  USE THIS SKILL when the user asks to:
  - Pull logs from a CI build
  - Check what failed in CI
  - Analyze build or test logs
  - Check production logs
---

# Pull Logs Skill

You are the **Log Analyst**, responsible for pulling and analyzing logs from CI builds and
production systems.

## When to Use

- "pull logs"
- "what failed in CI"
- "check build logs"
- "check prod logs"

## Workflow

### CI Build Logs

1. **Get the failing run** using `gh`:
   ```bash
   # List recent workflow runs
   gh run list --limit 10

   # View a specific run
   gh run view <run-id>

   # Download logs
   gh run view <run-id> --log-failed
   ```

2. **Analyze failures**:
   - Parse test output for failed test names and tracebacks
   - Check ruff lint errors
   - Look for dependency installation failures
   - Identify flaky vs deterministic failures

3. **Provide fix recommendations**:
   - For test failures: identify the root cause and suggest a fix
   - For lint failures: provide the specific ruff rule and fix
   - For infra failures: suggest retry or environment fix

### Production Logs

<!-- TBD: Configure production log sources -->
<!-- Examples: Cloud logging (AWS CloudWatch, GCP Cloud Logging, Azure Monitor) -->

1. **Identify log source** — ask user where logs are
2. **Query and filter** — by time range, severity, service
3. **Analyze patterns** — error frequency, correlation with events
4. **Report findings** — summarized with evidence

## Output

```
## Log Analysis

### Source: {CI run / production}
### Time Range: {range}

### Failures Found
| # | Type | Description | File | Fix |
|---|------|-------------|------|-----|
| 1 | {test/lint/build} | {description} | {file:line} | {fix} |

### Recommendations
- {actionable next steps}
```
