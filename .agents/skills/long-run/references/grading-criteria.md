# Grading Criteria Reference

Domain-specific grading templates for the Evaluator agent. Select the template that best matches your task, or combine criteria from multiple templates.

## General Coding (Default)

Use for any coding task without a specific domain template.

### Functionality (Weight: High | Threshold: 8/10)

Does the feature work as specified end-to-end?

| Score | Description |
|-------|-------------|
| 1-3 | Core feature is broken or unimplemented; attempting the primary action fails |
| 4-5 | Happy path works but common scenarios fail (e.g., form submits but validation is missing) |
| 6-7 | Primary flows work; some secondary flows or integrations are broken |
| 8-9 | All specified behaviors work; only edge cases or rare scenarios have issues |
| 10 | Feature is bulletproof; handles edge cases gracefully beyond what was specified |

**Anti-patterns:**
- Feature appears to work in isolation but breaks when used with other features
- Happy path works but error states crash or hang
- Stub implementations that return hardcoded data instead of real logic

### Code Quality (Weight: Medium | Threshold: 7/10)

Is the code clean, maintainable, and well-structured?

| Score | Description |
|-------|-------------|
| 1-3 | Spaghetti code; no separation of concerns; copy-paste duplication; dead code everywhere |
| 4-5 | Functional but messy; inconsistent patterns; some dead code; error handling is ad-hoc |
| 6-7 | Reasonable structure; mostly consistent patterns; minor issues (unused imports, inconsistent naming) |
| 8-9 | Clean architecture; proper separation of concerns; consistent patterns; good error handling |
| 10 | Exemplary; well-documented where needed; defensive coding; follows framework best practices |

**Anti-patterns:**
- God functions/classes that do everything
- Error handling via empty catch blocks or silent failures
- Hardcoded values that should be configuration
- Mixing business logic with presentation/IO

### Completeness (Weight: High | Threshold: 8/10)

Are features fully implemented or are parts stubbed/faked?

| Score | Description |
|-------|-------------|
| 1-3 | Most features are stubs or TODOs; more scaffolding than implementation |
| 4-5 | Core logic exists but significant parts are incomplete (e.g., "TODO: implement validation") |
| 6-7 | Features work but some specified behaviors are missing or simplified |
| 8-9 | All specified behaviors are implemented; only minor polish items remain |
| 10 | Complete implementation with thoughtful additions beyond the spec |

**Anti-patterns:**
- `// TODO` comments in shipped code
- Functions that return hardcoded/mock data
- UI elements that are rendered but don't respond to interaction
- API endpoints defined but returning 501 Not Implemented

### Robustness (Weight: Low | Threshold: 6/10)

Does the code handle edge cases, invalid input, and error states?

| Score | Description |
|-------|-------------|
| 1-3 | No input validation; crashes on unexpected input; no error handling |
| 4-5 | Basic validation on primary inputs; crashes on less common edge cases |
| 6-7 | Validates most inputs; handles common error states; some edge cases unhandled |
| 8-9 | Comprehensive validation; graceful error handling; most edge cases covered |
| 10 | Defensive coding throughout; rate limiting; timeout handling; recovery from transient failures |

**Anti-patterns:**
- SQL injection or XSS vulnerabilities
- Unvalidated user input passed to system calls
- No timeout on external API calls
- Race conditions in concurrent access paths

---

## Frontend / UI

Use for tasks involving user-facing interfaces. Combine with General Coding criteria.

### Design Quality (Weight: High | Threshold: 7/10)

Does the interface feel like a coherent whole rather than a collection of parts?

| Score | Description |
|-------|-------------|
| 1-3 | Components look randomly assembled; no visual coherence; default browser styling |
| 4-5 | Functional but generic; looks like an unstyled component library demo |
| 6-7 | Coherent color/typography; some personality; a few rough edges in spacing/alignment |
| 8-9 | Strong visual identity; colors, typography, layout, and spacing work together to create a distinct mood |
| 10 | Museum-quality; the design has genuine creative choices that feel intentional and polished |

**Anti-patterns:**
- Purple gradients over white cards (telltale AI-generated pattern)
- Default component library styling with no customization
- Inconsistent spacing (some elements use the grid, others don't)
- More than 3 typefaces

### Originality (Weight: High | Threshold: 6/10)

Are there deliberate creative choices, or is this template-driven?

| Score | Description |
|-------|-------------|
| 1-3 | Pure template/library defaults; indistinguishable from a Bootstrap demo |
| 4-5 | Minor customization (colors swapped) but layout and patterns are stock |
| 6-7 | Some custom decisions in layout or interaction; a human designer would notice intentional choices |
| 8-9 | Distinctive approach; creative layout, animation, or interaction choices that feel purposeful |
| 10 | Genuinely surprising; approaches that a human designer would study |

**Anti-patterns:**
- Centered hero with subtitle, three feature cards below, footer (the "startup template")
- Stock gradients as backgrounds
- Unmodified icon libraries
- Generic stock imagery

### Craft (Weight: Medium | Threshold: 7/10)

Typography hierarchy, spacing consistency, color harmony, contrast ratios.

| Score | Description |
|-------|-------------|
| 1-3 | Broken fundamentals: overlapping text, unreadable contrast, broken responsive layout |
| 4-5 | Readable but sloppy: inconsistent margins, misaligned elements, poor contrast in places |
| 6-7 | Solid fundamentals; consistent spacing; proper heading hierarchy; minor alignment issues |
| 8-9 | Polished; pixel-consistent spacing; strong typography hierarchy; proper contrast ratios |
| 10 | Flawless execution; every detail is intentional; passes WCAG AA accessibility |

### Usability (Weight: Medium | Threshold: 7/10)

Can users understand and complete tasks without guessing?

| Score | Description |
|-------|-------------|
| 1-3 | Users cannot figure out how to perform primary actions; navigation is broken |
| 4-5 | Primary actions are findable but workflows require trial-and-error |
| 6-7 | Clear primary actions; some secondary flows require discovery |
| 8-9 | Intuitive workflows; clear feedback on actions; loading and error states handled |
| 10 | Delightful UX; anticipates user needs; progressive disclosure; smooth transitions |

---

## API / Backend

Use for tasks involving APIs, services, or data processing.

### Correctness (Weight: High | Threshold: 8/10)

Do endpoints return correct data and status codes per the spec?

| Score | Description |
|-------|-------------|
| 1-3 | Endpoints return wrong data or wrong status codes; business logic errors |
| 4-5 | Happy path returns correct data; error cases return wrong status codes or generic 500s |
| 6-7 | Most responses are correct; some edge cases return unexpected results |
| 8-9 | All specified behaviors return correct data and status codes; proper error responses |
| 10 | Correct in all cases including rare edge cases; proper content negotiation |

### Performance (Weight: Medium | Threshold: 6/10)

Are there obvious performance issues under normal load?

| Score | Description |
|-------|-------------|
| 1-3 | N+1 queries; no pagination; full table scans on every request |
| 4-5 | Basic pagination exists; some N+1 queries remain; no indexing strategy |
| 6-7 | Reasonable query patterns; pagination works; basic indexes in place |
| 8-9 | Efficient queries; proper indexing; connection pooling; response caching where appropriate |
| 10 | Optimized for the expected access patterns; batch operations; rate limiting |

### Security (Weight: High | Threshold: 7/10)

Are there obvious security vulnerabilities?

| Score | Description |
|-------|-------------|
| 1-3 | SQL injection; plaintext passwords; no authentication on protected routes |
| 4-5 | Auth exists but has bypass paths; input not sanitized; secrets in code |
| 6-7 | Auth works; input sanitized; secrets in env vars; some CORS/CSRF gaps |
| 8-9 | Proper auth/authz; parameterized queries; CORS configured; rate limiting; input validation |
| 10 | Defense in depth; audit logging; proper secret management; security headers |

### API Design (Weight: Low | Threshold: 6/10)

Is the API intuitive and consistent?

| Score | Description |
|-------|-------------|
| 1-3 | Inconsistent naming; wrong HTTP methods; no error format; no versioning |
| 4-5 | Mostly RESTful; inconsistent error formats; some naming issues |
| 6-7 | Consistent patterns; proper HTTP methods; standardized error format |
| 8-9 | Clean REST design; HATEOAS hints; good documentation; consistent pagination |
| 10 | Exemplary API design; versioned; comprehensive docs; SDKs could be auto-generated |

---

## Data Pipeline

Use for ETL, data processing, or analytics tasks.

### Accuracy (Weight: High | Threshold: 9/10)

Does the pipeline produce correct output?

| Score | Description |
|-------|-------------|
| 1-3 | Output data is wrong; joins are incorrect; aggregations produce wrong totals |
| 4-5 | Simple cases work; complex transformations produce incorrect results |
| 6-7 | Most transformations are correct; some edge cases in data types or null handling |
| 8-9 | All specified transformations produce correct output; handles nulls and edge cases |
| 10 | Provably correct; includes validation checks that verify output against invariants |

### Error Handling (Weight: High | Threshold: 7/10)

What happens when input data is malformed or processing fails?

| Score | Description |
|-------|-------------|
| 1-3 | Pipeline crashes on any unexpected input; no recovery mechanism |
| 4-5 | Handles some bad input; crashes or silently drops data on others |
| 6-7 | Logs bad records; dead-letter queue or error output; partial failure doesn't crash the whole pipeline |
| 8-9 | Comprehensive error handling; retry logic; alerting on failure; recoverable checkpoints |
| 10 | Production-grade resilience; circuit breakers; exactly-once processing where possible |

### Idempotency (Weight: Medium | Threshold: 7/10)

Can the pipeline be safely re-run without producing duplicates or corruption?

| Score | Description |
|-------|-------------|
| 1-3 | Re-running produces duplicates or corrupts existing data |
| 4-5 | Some operations are idempotent; others produce side effects on re-run |
| 6-7 | Pipeline can be re-run safely with manual cleanup |
| 8-9 | Fully idempotent; re-running produces identical output; uses upserts or dedup |
| 10 | Idempotent with delta processing; only processes what changed |

---

## How to Use These Criteria

1. **Select the template** that matches your task. For full-stack work, combine General Coding + Frontend/UI + API/Backend.
2. **Customize thresholds** if needed. Lower them for prototypes, raise them for production code.
3. **The evaluator grades each dimension independently.** A high score in one area does not compensate for a failing score in another.
4. **Threshold means minimum acceptable.** Below threshold = sprint fails, regardless of other scores.
5. **Provide these criteria to both the Generator and Evaluator.** The Generator uses them as quality targets; the Evaluator uses them as a grading rubric.
