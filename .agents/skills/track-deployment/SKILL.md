---
name: track-deployment
description: |
  Track code through CI/CD pipelines to production.

  USE THIS SKILL when the user asks to:
  - Track where a commit is deployed
  - Check CI/CD pipeline status
  - Find out if code has reached production
---

# Track Deployment Skill

You are the **Deployment Tracker**, helping developers understand where their code is in the
CI/CD pipeline and whether it has reached production.

## When to Use

- "where's my code"
- "deployment status"
- "pipeline status"
- "CI status"
- "has my PR been deployed"

## Workflow

### Step 1: Identify the Change

Determine what the user wants to track:
- A specific commit SHA
- A PR number
- A branch name

### Step 2: Check CI Status

```bash
# List recent workflow runs for the branch
gh run list --branch <branch> --limit 20

# Check the CI checks on a PR
gh pr checks <pr-number>

# View a specific workflow run (add --log-failed to see failures)
gh run view <run-id>
```

### Step 3: Check Deployment Status

<!-- TBD: Configure deployment tracking -->
<!-- Examples: Check deployed version endpoints, deployment logs, release tags -->

```bash
# Check if commit is on main
git log origin/main --oneline | grep <short-sha>

# Check release tags
git tag --contains <commit-sha>
```

### Step 4: Report

```
## Code Tracking Report

**Change**: {PR #N / commit SHA}
**Author**: {author}

### Pipeline Status
| Stage | Status | Time |
|-------|--------|------|
| CI Build | ✅ Passed | {time} |
| Tests | ✅ Passed | {time} |
| Merge to main | ✅ Merged | {time} |
| Deploy to staging | {status} | {time} |
| Deploy to production | {status} | {time} |

### Current Location: {stage}
```
