---
name: integration-test-audit
description: |
  Finds integration-test gaps by diffing unit-test vs integration-test line coverage and
  reading the suspicious test files for mock-the-class-you-own theatre. Surfaces modules
  whose lines look covered but whose behavior is unverified, ranks them as integration-test
  candidates, and names the bug class each new test would catch. Use when user asks to
  "audit integration test coverage", "find integration test gaps", "find integration test
  holes", "where should I add integration tests", "compare unit vs integration coverage",
  or "find test theatre". Pairs with integration-test-engineer, which writes the tests
  this skill recommends.
---

# Integration Test Audit

You are the **Integration Test Auditor**. Your job is to surface modules where integration tests would catch bugs that unit tests systematically miss, by comparing what each test category covers and reading the suspicious test files for verification quality.

## Core insight

> Coverage shows execution, not verification. A line covered by a test that mocks the class under test is "covered" the same way a line covered by a real-behavior test is — but only one catches bugs.

The diff between unit and integration coverage is a *signal*, not an answer. It points at modules worth investigating; the analyst still has to read the test file to tell real coverage from theatre.

## The methodology

### Step 1 — Mark integration tests

This skill assumes `@pytest.mark.integration` as the marker. Apply at module level (one line per file, less noise than per-test):

```python
# tests/test_xyz.py
pytestmark = pytest.mark.integration
```

Register the marker so pytest doesn't warn:

```toml
# pyproject.toml
[tool.pytest.ini_options]
markers = [
    "integration: integration test — exercises a real internal slice with externals faked at the seam",
]
```

**What counts as an integration test** (per the integration-test-engineer skill):
- Uses a real local HTTP server, real SQLite, or in-memory fake-at-the-seam fake → integration
- Mocks classes the project owns → unit (and probably theatre — flag it)

When marking existing files, be conservative: only files that clearly meet the integration definition. The diff is most useful when the marker means something specific.

### Step 2 — Run two coverage passes

```bash
uv run pytest -m integration --cov=<package> --cov-report=json:coverage-integration.json --cov-report=
uv run pytest -m "not integration" --cov=<package> --cov-report=json:coverage-unit.json --cov-report=
```

The two JSON files are regenerated every audit run — add `coverage-*.json` to `.gitignore` if not already covered.

**If the suite has failures, fix them first.** Partial coverage data leads to false positives — a missing line might be missing because the test that would cover it crashed, not because no test exists. Never dismiss a failure as "pre-existing": fix it, then re-run.

### Step 3 — Compute the diff

```python
import json

with open('coverage-integration.json') as f: integ = json.load(f)
with open('coverage-unit.json') as f: unit = json.load(f)

rows = []
for path in sorted(set(integ['files']) | set(unit['files'])):
    i_lines = set(integ['files'].get(path, {}).get('executed_lines', []))
    u_lines = set(unit['files'].get(path, {}).get('executed_lines', []))
    miss = set(integ['files'].get(path, {}).get('missing_lines', []))
    miss |= set(unit['files'].get(path, {}).get('missing_lines', []))
    all_exec = i_lines | u_lines | miss
    rows.append((path,
                 len(i_lines - u_lines),    # int-only
                 len(u_lines - i_lines),    # unit-only
                 len(i_lines & u_lines),    # both
                 len(all_exec - i_lines - u_lines)))  # neither

# Hot spots: rank by unit-only descending, surface those with low int-only
print(f'{"file":<60} {"int-only":>9} {"unit-only":>10} {"both":>6} {"neither":>8}')
for f, io, uo, b, n in sorted(rows, key=lambda r: -r[2])[:20]:
    if io + b > 0 and uo > 20:  # touched by integration; meaningful unit-only count
        print(f'{f:<60} {io:>9} {uo:>10} {b:>6} {n:>8}')
```

Four columns matter, per file:

| Column | Meaning |
|---|---|
| `int-only` | Lines covered ONLY by integration tests — the unique value of integration |
| `unit-only` | Lines covered ONLY by unit tests — *candidates* for integration tests |
| `both` | Overlap |
| `neither` | Truly uncovered — separate problem |

### Step 4 — Identify hot spots

A **hot spot** is a module where:
- `unit-only` is high (>50 lines is a useful threshold; project-dependent)
- `int-only` is low (often 0)
- `both` is non-trivial (the file IS reachable; integration tests just don't drive it)

Inverse: modules with high `int-only` are where integration tests already earn their keep. Don't add more there — add where the diff says they're missing.

### Step 5 — Read the test file for theatre signals

**This is the most important step. The diff produces candidates; this step produces verdicts.** Coverage doesn't reveal whether the unit "coverage" is real or theatre. Skipping or skimming this step is how audits go wrong: they recommend rewrites of code that's already well-tested, or miss modules whose tests only *look* thorough.

#### Procedure

For each hot-spot module from Step 4:

1. **Locate the primary test file(s).** Use `grep -rl "from <module_path>"` or `grep -rl "import <module>"` under `tests/`. There may be more than one — read all of them before judging.
2. **Read the entire test file**, not just one test. Theatre is a property of the file's seam choice, not of any single test.
3. **For each owned class imported from the module under test**, ask: is it constructed for real, or replaced with a `Mock`/`AsyncMock`/`MagicMock`/`patch(...)`? If replaced, that is the strongest theatre signal — record it.
4. **Run the signal table below** against the file. Count signals.
5. **Apply the verdict rule** (below the table).
6. **Record evidence**: for each signal, capture `path:line` and a one-line quote. The final report must cite specific lines, not vibes.

#### Signal table

| # | Signal | What to grep / look for | What it means |
|---|---|---|---|
| 1 | Mocks a class the project owns | `Mock(spec=<OwnedClass>)`, `patch("<your_pkg>.<module>.<OwnedClass>")`, fixtures returning `AsyncMock()` typed as an internal type | Wrong seam — wire format, response parsing, state transitions are all unverified. **Highest weight.** |
| 2 | `Mock`/`AsyncMock`/`MagicMock` without `spec=` | `= MagicMock()`, `= AsyncMock()` with no `spec=` argument | Method typos and signature drift pass silently |
| 3 | Return-value tautology | `mock.x.return_value = V` then `assert result == V` (or `result.x == V`) with no transformation in between | The test asserts the mock returned what it was told to return |
| 4 | Assertions only on the return value | No assertions on DB rows, outbound HTTP, emitted events, persisted state, logs, or metrics — only `assert result.something == ...` | Side effects (the usual contract) are unverified |
| 5 | "Was called" with no outcome check | `mock.foo.assert_called_once()` is the only assertion; no check on what foo *did* | Test name claims behavior; body only checks invocation |
| 6 | Patches an internal method to skip side effects | `patch.object(self_owned, "_do_thing")` to avoid running real logic | Slice is too narrow — the bug lives in the patched-out code |
| 7 | No real entry point | Test calls a class method directly; never goes through the route, message handler, or CLI parser | Wiring, middleware order, validation, serialization unverified |
| 8 | Fake constructed but never reads its state | Builds an in-memory store, never asserts against it | The store is decoration, not a verifier |

#### Verdict rule

Count distinct signals (a single line can only score once):

| Score | Verdict | Action |
|---|---|---|
| Signal 1 present, OR ≥3 other signals | **Confirmed theatre** | Recommend integration test, name bug class |
| 1–2 signals, no signal 1 | **Suspected theatre** | List as "likely hot spot, needs deeper read" — do not promise a rewrite |
| 0 signals, real seams used | **Already integration in spirit** | Recommend marking with `@pytest.mark.integration` and re-running the diff; do NOT recommend new tests |
| File mostly tests pure data / pure functions | **Out of scope** | Note and move on; not every module needs integration tests |

Apply the rule per file. If a module has multiple test files with different verdicts, report each separately — don't average them.

#### What this step is NOT

- Not a count of mocks. A single `Mock(spec=ExternalHttpClient)` at the right seam is correct, not theatre. Signal 1 is about *owned* classes specifically.
- Not a judgment of the unit tests' quality as unit tests. A perfectly good unit test can still leave an integration gap. The audit is about gaps, not about deleting unit tests.
- Not a substitute for running the suite. If you didn't actually execute Step 2's coverage runs, Step 5 has nothing to anchor to.

### Step 6 — Propose integration tests at the right seam

For each confirmed hot spot:
1. Identify the **public entry point** (HTTP route, message handler, CLI command)
2. Identify the **external boundary** to fake (HTTP client, DB driver, message broker, clock)
3. Hand off to the `integration-test-engineer` skill for the actual test writing — that skill owns the seam-choice / hermeticity / assertion-shape decisions.

This skill stops at "here's where to write the tests." Writing them is a different skill.

## Hot-spot patterns to recognize

Modules that almost always show up as hot spots and almost always benefit from integration tests:

- **Production loops** (supervisors, schedulers, pollers, drainers) — their value is in time-driven and state-driven behavior that mocks rarely exercise correctly.
- **HTTP clients to third-party APIs** (`*_client.py`, `*_tracker.py`, `*_api.py`) — usually mocked at the class boundary, hiding wire-format bugs (URL, query params, content-type, body shape, response parsing).
- **Auth / middleware** — often unit-tested with synthetic claims rather than real request flow through the chain.
- **Routes with many handlers** — handler logic exercised in isolation, not through real middleware order or real DI graph.
- **Background workers** — fire-and-forget tasks rarely tested for completion / cleanup / state-on-failure.

## Anti-patterns

### 1. Driving the metric
Don't optimize "% lines covered by integration tests." A higher number is not better — more integration tests is more cost (slower suite, more brittleness) for diminishing returns past the right modules. Coverage is a *signal*, never a goal.

### 2. Flag-flipping every test as integration
Marking everything `@pytest.mark.integration` defeats the diff. The marker should mean what the integration-test-engineer skill says: real internal slice + fake at the external seam. Mock-the-class-you-own tests are not integration tests, no matter how high-level they look.

### 3. Trusting "high coverage" without reading the test
The most common failure mode: see `both=300` for a file and conclude it's well-tested. Coverage shows execution, not verification. Read the test file before declaring a module covered.

### 4. Acting on partial coverage data
If the suite has failing tests, the coverage data is incomplete and probably misleading. Fix the failures first.

### 5. Treating the diff as a verdict
The diff finds *candidates*. It doesn't tell you whether unit coverage is real, whether integration coverage is meaningful, or whether the module *needs* integration testing at all (some modules are pure data structures and don't). Investigation is part of the job.

## Output format

Report findings as a prioritized list, each item with the bug class it would catch:

```
## Integration test gaps in <project>

### Confirmed hot spots (theatre detected, integration tests recommended)
1. **<module>** — <unit-only lines> unit-only, <int-only> integration-only.
   - Primary test file: <path>
   - Theatre signals: <e.g. mocks AdoIssueTracker without spec=, return-value tautology>
   - Recommended slice: drive <entry point> → real <component> → fake <seam>
   - Bug class caught: <e.g. wire-format regression, status mapping, state transitions>

2. **<module>** — ...

### Likely hot spots (high unit-only, theatre not yet confirmed)
- <module>: <unit-only> lines unit-only — read <test file> to confirm

### Already well-covered (high int-only)
- <module> — integration tests already earn their keep; no gap.

### Out of scope
- <module> (e.g. pure data classes, generated code) — not an integration-test target.
```

## Checklist before reporting

- [ ] Marker is registered in pyproject.toml (no `PytestUnknownMarkWarning`)
- [ ] Both coverage passes ran without test failures
- [ ] Diff script ran successfully and produced ranked output
- [ ] At least one hot-spot test file has been read for theatre signals
- [ ] Each recommended hot spot names a specific bug class
- [ ] Report distinguishes confirmed-theatre from suspected-theatre from out-of-scope
- [ ] Hand-off to integration-test-engineer is explicit for any new tests proposed
