# Artifact Formats Reference

Exact specifications for all structured artifacts produced during a long-run session. Agents must follow these formats precisely — consistency enables reliable handoffs between agents.

---

## feature-list.json

The central tracking artifact. Every feature is represented as a JSON object in an array. The Generator only modifies the `passes` field. No features may be deleted or have their descriptions altered.

### Schema

```json
[
  {
    "id": "string (kebab-case, unique)",
    "category": "string (logical grouping)",
    "priority": "P0 | P1 | P2",
    "description": "string (one sentence, user-facing behavior)",
    "steps": ["string (concrete test step)"],
    "passes": false,
    "sprint": "number (which sprint this belongs to)"
  }
]
```

### Rules

- `id` — kebab-case, globally unique (e.g., `auth-login`, `dashboard-charts`, `export-csv`)
- `priority` — P0 (must-have for MVP), P1 (important), P2 (nice-to-have)
- `steps` — ordered list of concrete actions to verify the feature works. Written from a user/tester perspective.
- `passes` — boolean. Starts `false`. Set to `true` only after the Generator has verified the feature works end-to-end. The Evaluator can reset to `false` if it finds the feature is actually broken.
- `sprint` — integer. Which sprint this feature is assigned to. Set by the Planner.

### Example

```json
[
  {
    "id": "auth-login",
    "category": "authentication",
    "priority": "P0",
    "description": "User can log in with email and password",
    "steps": [
      "Navigate to /login",
      "Enter valid email and password",
      "Submit the form",
      "Verify redirect to /dashboard",
      "Verify session cookie is set",
      "Verify user's name appears in the header"
    ],
    "passes": false,
    "sprint": 1
  },
  {
    "id": "auth-logout",
    "category": "authentication",
    "priority": "P0",
    "description": "User can log out and session is destroyed",
    "steps": [
      "Log in as a valid user",
      "Click the logout button",
      "Verify redirect to /login",
      "Verify session cookie is cleared",
      "Attempt to access /dashboard",
      "Verify redirect back to /login"
    ],
    "passes": false,
    "sprint": 1
  }
]
```

---

## progress.md

Running narrative of the build. Each agent appends to this file — never overwrites. The file serves as context for fresh agents starting a new sprint.

### Format

```markdown
# Build Progress

## Project
- **Task:** [Original user prompt]
- **Tier:** Standard | Extended
- **Total Features:** N
- **Total Sprints:** N

---

## Sprint 1: [Sprint Title]
**Started:** [timestamp]
**Features:** [list of feature IDs]

### Iteration 1
- Implemented: auth-login, auth-logout, auth-register
- Committed: `abc1234` — "feat: implement authentication flow"
- Issues found: login form doesn't handle empty email gracefully
- Self-eval: 3/4 features passing

### Evaluation 1
- Verdict: FAIL
- Scores: Functionality 7/10, Code Quality 8/10, Completeness 6/10, Robustness 5/10
- Key issues: auth-register missing email validation; login returns 500 on empty password

### Iteration 2
- Fixed: email validation on register, error handling on login
- Committed: `def5678` — "fix: add input validation to auth endpoints"
- Self-eval: 4/4 features passing

### Evaluation 2
- Verdict: PASS
- Scores: Functionality 9/10, Code Quality 8/10, Completeness 9/10, Robustness 7/10

**Completed:** [timestamp]
**Tag:** sprint-1-complete

---

## Sprint 2: [Sprint Title]
...
```

### Rules

- Never overwrite previous entries. Append only.
- Include commit hashes for traceability.
- Include evaluation scores for trend tracking.
- Keep entries concise but specific enough for a new agent to understand what happened.

---

## sprint-plan.md

Written by the Planner. Groups features into ordered sprints.

### Format

```markdown
# Sprint Plan

## Sprint 1: [Title — e.g., "Foundation & Authentication"]
**Goal:** [One sentence describing the sprint's purpose]
**Features:**
- auth-login (P0)
- auth-logout (P0)
- auth-register (P0)
- user-profile (P1)

**Dependencies:** None (foundational sprint)

---

## Sprint 2: [Title]
**Goal:** [One sentence]
**Features:**
- dashboard-overview (P0)
- dashboard-charts (P1)
- dashboard-filters (P1)

**Dependencies:** Sprint 1 (needs auth)

---
```

### Rules

- Sprints are ordered. Sprint N+1 may depend on Sprint N.
- Each sprint should contain 2-5 features.
- Foundational features (auth, data model, core CRUD) come first.
- P0 features are distributed across early sprints; P2 features go in later sprints.

---

## sprints/sprint-N-proposal.md

Written by the Generator before a sprint. Proposes what will be built and how it will be verified.

### Format

```markdown
# Sprint N Proposal: [Title]

## Features

### 1. [feature-id]: [Description]

**Approach:** [2-3 sentences on technical implementation]

**Acceptance Criteria:**
1. [Specific, testable criterion with expected behavior]
2. [Specific, testable criterion]
3. [Edge case criterion]
4. [Error handling criterion]

**Verification:**
- `curl -X POST http://localhost:3000/api/auth/login -d '{"email":"test@example.com","password":"pass123"}' | jq .status` → should return `"ok"`
- `curl -X POST http://localhost:3000/api/auth/login -d '{"email":"","password":""}' | jq .error` → should return `"Invalid credentials"`

### 2. [feature-id]: [Description]
...
```

---

## sprints/sprint-N-contract.md

Written by the Evaluator after reviewing the proposal. This is the binding agreement.

### Format

```markdown
# Sprint N Contract: [Title]

**Status:** APPROVED | REVISE

## Agreed Acceptance Criteria

### [feature-id]: [Description]
1. ✅ [Criterion from proposal — approved as-is]
2. ✏️ [Criterion, revised] — Original: "[original text]" → Revised because [reason]
3. ➕ [Added criterion] — Added because [reason: edge case, security, etc.]

### [feature-id]: [Description]
...

## Revision Notes (if REVISE)
- [What needs to change and why]

## Verification Commands
[Consolidated list of all test commands for this sprint]
```

---

## sprints/sprint-N-self-eval.md

Written by the Generator after completing a sprint, before the Evaluator runs.

### Format

```markdown
# Sprint N Self-Evaluation

## Feature Status
| Feature | Status | Confidence | Notes |
|---------|--------|------------|-------|
| auth-login | Passing | High | All criteria verified |
| auth-register | Passing | Medium | Email validation works but didn't test unicode |
| auth-logout | Passing | High | Session cleanup verified |

## Known Weaknesses
- [Anything the Generator suspects might fail evaluation]
- [Areas where shortcuts were taken]

## Test Evidence
- [Commands run and their output, or descriptions of manual testing performed]
```

---

## sprints/sprint-N-eval.md

Written by the Evaluator after testing the sprint.

### Format

```markdown
# Sprint N Evaluation

## Criterion Results

| # | Feature | Criterion | Result | Evidence |
|---|---------|-----------|--------|----------|
| 1 | auth-login | POST /api/auth/login with valid creds returns 200 | PASS | curl returned 200, session cookie set |
| 2 | auth-login | Invalid credentials return 401 with error message | FAIL | Returns 500 with stack trace; `auth.py:47` catches ValueError but raises unhandled TypeError |
| 3 | auth-register | POST /api/auth/register creates new user | PASS | User created, verified in DB |

## Scores

| Dimension | Score | Threshold | Status |
|-----------|-------|-----------|--------|
| Functionality | 7/10 | 8 | ❌ BELOW |
| Code Quality | 8/10 | 7 | ✅ ABOVE |
| Completeness | 8/10 | 8 | ✅ MEETS |
| Robustness | 5/10 | 6 | ❌ BELOW |

## Verdict: FAIL

**Reason:** Functionality (7/10) and Robustness (5/10) below thresholds.

## Issues Found

1. **auth-login error handling** — `auth.py:47` catches `ValueError` but the password check raises `TypeError` when password is None. Fix: add null check before password comparison.
2. **auth-register duplicate email** — No unique constraint on email column. Inserting a duplicate email causes 500 instead of 409 Conflict. Fix: add unique constraint to migration; catch IntegrityError in handler.
3. **CORS not configured** — Frontend requests from localhost:5173 are blocked. Fix: add CORS middleware with allowed origins.

## Recommendations

- Priority fixes: Issues #1 and #2 (functionality blockers)
- Also fix: Issue #3 (required for frontend integration)
- Consider: adding request validation middleware to catch malformed JSON before it reaches handlers
```

---

## completion-report.md

Generated after all sprints complete.

### Format

```markdown
# Completion Report

## Summary
- **Task:** [Original prompt]
- **Duration:** [Total time across all sprints]
- **Sprints Completed:** N/M
- **Features:** X/Y passing

## Feature Status

| Feature | Priority | Sprint | Status |
|---------|----------|--------|--------|
| auth-login | P0 | 1 | ✅ Passing |
| auth-register | P0 | 1 | ✅ Passing |
| dashboard-charts | P1 | 2 | ❌ Failing |
| export-csv | P2 | 3 | ⏭️ Skipped |

## Sprint History

| Sprint | Iterations | Final Verdict | Functionality | Code Quality | Completeness | Robustness |
|--------|-----------|---------------|---------------|-------------|-------------|------------|
| 1 | 2 | PASS | 9 | 8 | 9 | 7 |
| 2 | 3 | PASS (with issues) | 8 | 7 | 7 | 6 |

## Known Issues
1. [Issue description — sprint where it was identified]
2. [Issue description]

## Next Steps
- [Suggested improvements or features to build next]
- [Technical debt to address]
```

---

## init.sh

Created during environment initialization. Must be idempotent (safe to run multiple times).

### Template

```bash
#!/bin/bash
set -e

echo "=== Installing dependencies ==="
# Detect and install based on project type
if [ -f "package.json" ]; then
    npm install --quiet
elif [ -f "requirements.txt" ]; then
    pip install -r requirements.txt --quiet
elif [ -f "go.mod" ]; then
    go mod download
fi

echo "=== Starting dev server ==="
# Start in background; adapt to project type
if [ -f "package.json" ]; then
    npm run dev &
elif [ -f "manage.py" ]; then
    python manage.py runserver &
fi

DEV_PID=$!
sleep 5

echo "=== Smoke test ==="
# Basic check that the server responds
if curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/ | grep -q "200\|301\|302"; then
    echo "✅ Dev server is responding"
else
    echo "❌ Dev server is not responding"
    kill $DEV_PID 2>/dev/null
    exit 1
fi

echo "=== Environment ready ==="
echo "Dev server PID: $DEV_PID"
```

### Rules

- Must be idempotent — running twice doesn't break anything
- Should install dependencies, start the dev server, and run a basic smoke test
- Exit code 0 = ready; exit code 1 = something is broken
- The Planner writes the initial version; the Generator can update it as the stack evolves
