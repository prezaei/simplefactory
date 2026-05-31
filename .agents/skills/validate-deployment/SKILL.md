---
name: validate-deployment
description: |
  Post-deployment validation and smoke testing.

  USE THIS SKILL when the user asks to:
  - Validate a deployment
  - Run post-deploy checks
  - Smoke test a release
  - Check deployment health
---

# Validate Deployment Skill

You are the **Deployment Validator**, responsible for verifying that a deployment is healthy
and functioning correctly after release.

## When to Use

- "validate deployment"
- "check deployment health"
- "smoke test the release"
- "did the deployment break anything"

## Workflow

```
Phase 1: Health Check
    ↓
Phase 2: Smoke Tests
    ↓
Phase 3: Regression Detection
    ↓
Phase 4: Report
```

### Phase 1: Health Check

<!-- TBD: Configure health check endpoints per service -->

1. **Check service health endpoints**:
   ```bash
   # TBD: curl health endpoints
   curl -sf http://localhost:8000/health
   ```
2. **Verify all expected services are running**
3. **Check for error spikes in logs** (first 5 minutes post-deploy)

### Phase 2: Smoke Tests

<!-- TBD: Define core smoke test scenarios -->

1. **Run critical path tests** — the most important user workflows
2. **Verify API responses** match expected schemas
3. **Check data integrity** — reads/writes working correctly

### Phase 3: Regression Detection

1. **Compare with pre-deploy baseline** (if available):
   - Response times (p50, p95, p99)
   - Error rates
   - Resource usage (CPU, memory)
2. **Check for new error patterns** not present before deploy
3. **Verify no degradation** in key metrics

### Phase 4: Report

```
## Deployment Validation Report

**Environment**: {env}
**Deployed at**: {timestamp}
**Version**: {version/commit}

### Health Checks
| Service | Status | Latency |
|---------|--------|---------|
| {name} | ✅ Healthy / ❌ Unhealthy | {ms} |

### Smoke Tests
| Test | Result |
|------|--------|
| {scenario} | ✅ Pass / ❌ Fail |

### Regressions
- {None detected / list of regressions}

### Verdict: ✅ HEALTHY / ⚠️ DEGRADED / ❌ FAILING
```
