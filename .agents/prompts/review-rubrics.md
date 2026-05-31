# Review Rubrics

## Hard Rules

### Always Check

- All public functions have doc comments explaining purpose and panics/errors.
- Error types use `thiserror` with meaningful context messages.
- No `unwrap()` or `expect()` in non-test code without a comment explaining why it's safe.
- New code paths have corresponding tests — unit tests for isolated logic, integration tests for subsystem behavior.
- Every enum variant is tested through every entry point that accepts it.
- Test names match what the test actually asserts (name/body mismatch is a bug).
- Async functions that do I/O have timeouts or cancellation support.
- Every transaction and unit of work is logged — inbound/outbound requests, events, state transitions, decisions, errors.
- Sensitive data (tokens, keys) is never logged — even at debug level.

### Never Allow

- `unsafe` blocks without a `// SAFETY:` comment explaining the invariant.
- Hardcoded secrets, tokens, or connection strings.
- `panic!()` in library code (only in tests or truly unrecoverable situations).
- Blocking calls (`.block_on()`, `std::thread::sleep()`) inside async contexts.
- Circular dependencies between workspace crates.

## Repo-Specific Patterns

- Error types implement `Display` and include enough context to diagnose without a debugger.
- Database/storage access goes through a repository trait (seam), never direct in handlers.
- All async operations interacting with external services must have a timeout.
- PostgreSQL queries use parameterized statements, never string interpolation.
- Configuration is loaded via environment variables, not hardcoded defaults for production values.
- Seam traits (storage, runtime, external-service clients, etc.) are the boundary — implementations never leak into core logic.

## Tests

New code without tests is incomplete code.

- Every new code path has corresponding tests — unit for isolated logic, integration for subsystem behavior.
- Every enum variant is tested through every entry point that accepts it. Not one variant that happens to work.
- Test names are contracts. If a test is named `claim_with_pending_state`, it must exercise the `Pending` state. Name/body mismatch is a bug.
- Exactly-once delivery is always asserted. Any system that delivers events must assert the exact count, not "at least N."
- No hardcoded ports — use port 0 or dynamic allocation.
- No global mutable state — tests run in parallel by default.
- Integration tests use real Postgres and, where needed, real Docker — not mocks of those seams.
- Seam traits may be stubbed/faked in integration tests when the test's focus is a different subsystem.

## Telemetry

**Every activity in the system is logged. Every event, every outcome, every decision. No exceptions.** If something happens and there's no log, that's a critical gap. If you can't diagnose a problem from the logs alone, the telemetry is insufficient.

- Every transaction and unit of work is logged: inbound requests, outbound calls, events received, events emitted, state transitions, decisions, errors. The unit of telemetry is the work being done, not the function it happens to live in.
- Every unit of work has a span with correlation IDs so the full transaction can be traced end-to-end.
- Span names follow `<component>.<operation>` convention. No dynamic content in span names — use fields.
- Outbound calls log destination, outcome, and latency.
- Metrics use a consistent `<app>.<component>.<metric_name>` naming scheme. Labels must be low-cardinality — never use IDs as metric dimensions.
- Sensitive data (tokens, keys, API responses with user content) is never logged — even at debug level.
- Do NOT initialize OTLP exporters in tests.

## Severity Calibration

| Severity | Criteria | Action |
|----------|----------|--------|
| **Blocking** | Bugs, security issues, data loss, breaks CI | Must fix before merge |
| **High** | Missing error handling, missing tests, missing telemetry, race conditions, perf regression | Request changes |
| **Medium** | Naming, missing tests for edge cases, minor refactor opportunity | Approve with comment |
| **Nit** | Style preference within existing conventions | Comment only if pattern |

## What NOT to Flag

- Formatting issues (rustfmt handles this).
- Clippy warnings already configured as CI errors (CI catches these).
- Import ordering (rustfmt handles this).
- Variable naming style debates that are consistent with surrounding code.
- TODOs that reference a tracked issue.
