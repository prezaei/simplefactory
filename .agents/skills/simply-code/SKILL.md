---
name: simply-code
description: |
  Grooms code toward the simplest, most readable factoring — introduces
  abstractions where a natural interface exists, breaks up long methods, fixes
  boundary violations (layering, encapsulation, close coupling), tightens
  contracts, improves testability, and removes over-engineering. Both peephole
  and class/method level. Use when user asks to "simply-code", "groom this
  code", "refactor for clarity", "improve readability", "break up this
  method", "fix layering", "fix encapsulation", "fix coupling", "improve
  testability", "tighten this contract", "rethink this design", "clean up this
  code", "continual grooming", or wants the diff reviewed for factoring.
---

# Simply-Code

Continual grooming toward the **simplest, most readable
factoring**. Not minimum lines. Not maximum abstraction. The clearest version
of the code that does what it actually needs to do — today, given what we know
now.

## When to Use
- Continual grooming pass over a package, file, or diff
- After writing new code, to refactor toward a natural shape
- When requirements just changed and the old factoring no longer fits
- User asks to groom, refactor for clarity, improve testability, fix layering,
  or tighten contracts
- Before opening a PR, as a readability/factoring check

## When NOT to Use
- Formatting / import order → `uv run ruff format` / `uv run ruff check --fix`
- Type-only issues → type checker
- Test-quality review → `/test-engineer`
- New feature or cross-cutting design → `/architect-and-design`
- Security review → `/security-review`

## Scope

Ask the user, or pick one of these, in order of preference:
1. A named file, class, or package.
2. The current branch diff: `git diff main...HEAD`.
3. Files modified in the current session.

Stay inside the named scope. Don't fan out into unrelated code. If a layering
fix or contract tightening requires touching a caller outside scope, surface
it to the user and get confirmation before expanding.

## Core Principle

**Readability is the goal. Line count is a proxy, not the goal.**

Both directions are on the table:
- **Add** abstraction when it exposes a natural interface, isolates a concern,
  enables a better test, or removes duplication that has now earned its name.
- **Remove** abstraction when it's speculative, single-use with no imminent
  second caller, or obscures more than it clarifies.

When in doubt, pick the factoring that is easier to explain to a teammate in
one sentence.

## Six Lenses

Apply in this order. A single location can yield findings in multiple lenses —
pick the one that produces the highest-leverage change and move on.

### 1. Peephole (local) improvements

Micro-refactors within a single function:
- Early returns over nested conditionals
- Guard clauses over deep `if` pyramids
- Named constants for magic values and strings
- Rename a variable or parameter that misleads
- Collapse redundant intermediate variables
- Replace a nested ternary with `if/else`
- Inline a single-use local helper that hurts flow
- Extract a named local when a sub-expression carries meaning

### 2. Method / class level

Structural refactors inside a module:
- **Extract method** from any function longer than ~40 lines or doing two jobs.
  Give it a name that describes *intent*, not implementation.
- **Inline method** that is single-use, short, and only obscures the flow.
- **Extract class** when a cohesive group of methods + state wants to leave.
- **Collapse class** that only holds one method and no meaningful state —
  make it a function.
- **Separate pure logic from I/O** — the I/O-bound method calls the pure one.
  Makes the pure core trivially testable.
- **Parameter-object** when a function takes 5+ related primitives that always
  travel together.
- **Shared scaffolding, not shared identity.** When N functions share a
  surrounding pattern — setup/teardown bracket, try/except, retry loop,
  instrumentation wrapper — but have distinct signatures or contracts,
  extract the pattern as a helper and **keep the N wrappers**. Collapsing the
  wrappers themselves is wrong whenever their differences are real at the
  call site, even when the differences are hidden behind `Any` or `**kwargs`
  in the declaration. The helper deduplicates the pattern; the wrappers
  preserve the per-case surface.

### 3. Boundaries: layering, encapsulation, coupling

Three flavors of boundary problem. Check each — a single location often shows
two at once.

**a. Layering.** Directional layer rules (adapt to your repo):
- `libs/` must not import from `services/`.
- `services/<A>/` must not import from `services/<B>/` — go through the
  service's public API (HTTP, MCP, etc.) or a published client lib in `libs/`.
- Route handlers in `main.py` should be thin: parse request → call domain
  module → shape response. Business logic belongs in domain modules
  (`dispatch.py`, `scheduler.py`, `runner.py`, etc.), not in route bodies.
- Settings / config loading belongs in `config.py`. Modules consume settings
  as parameters or via a single import — they don't re-read environment
  variables ad hoc.
- Telemetry/OTel setup belongs in `telemetry.py` and is wired once at startup.
  Domain modules don't configure logging or tracing — they just use loggers.
- Auth verification belongs in `auth.py` (and the FastAPI dependency that
  wraps it). Route handlers and domain modules don't decode JWTs themselves.
- Storage access belongs behind `storage.py` / `session_db.py` / similar —
  domain modules call those, not blob clients or SQLAlchemy sessions
  directly.

**b. Encapsulation.** Code reaching past another module's interface into its
internals:
- Reading or writing `_private` / `__dunder` attributes of a class the caller
  doesn't own.
- Calling an underscored helper function from another module instead of the
  package's public surface.
- Importing from an internal submodule (`from mypkg._internal import X`)
  instead of from the package root.
- Duck-typing on the presence or shape of a "private" attribute.
- Bypassing a wrapper to manipulate its wrapped field directly.
- Monkey-patching a module's state from outside in production code (tests may
  be fine; production is not).

Fix by lifting the needed capability onto the owning module's public API, or
by moving the work into the module that owns the state. If the private symbol
is really the right API, rename it — the leading underscore is lying.

**c. Close coupling.** Modules that know too much about each other:
- Circular imports, or bidirectional dependencies between modules that should
  have a clear direction.
- A public signature that exposes an internal type, forcing every caller to
  import the internal (type leak).
- **Feature envy** — a method on A that mostly reads and mutates B's fields.
  Move the method to B, or have A ask B to do the work via a verb on B's API.
- **Data clumps** — 4+ parameters that always travel together across many call
  sites. Promote to a parameter object or to a type that owns both the data
  and the operations on it.
- **Shotgun-surgery signal** — a conceptually small change requires edits in
  N unrelated-looking files. N itself is the finding, even if you can't fix
  it in this pass.
- Shared mutable state beyond what the API declares (module-level globals
  consumed by logic, class-level mutable defaults, implicit singletons).
- Module A catches an exception type defined inside B's internals — B's error
  model has leaked. Either promote the exception to B's public surface or have
  A catch a contract-level type.
- Caller constructs a dependency using knowledge of that dependency's
  internals (hard-coded defaults that belong to the dependency, magic config
  values, private constructor arguments).

### 4. Badly-defined contracts

Tighten interfaces:
- Return type is `Any`, `object`, or missing — infer the real type and annotate.
- `Optional[X]` where no caller passes `None` — drop `Optional`.
- Function "sometimes returns `None`, sometimes raises" — pick one, document why.
- A method mutates *and* returns — split into `mutate()` and `get()`, or return
  `None` from the mutator.
- `**kwargs` passthrough to an inner call — replace with explicit params (unless
  the set is genuinely open).
- Boolean flags that switch the function's entire behavior — split into two
  functions with verb-phrase names.
- Pydantic / FastAPI request models with stringly-typed fields whose semantics
  aren't captured — use `Enum`, `Literal`, or split the field.
- Exception types that are too broad (`Exception`, `RuntimeError`) when a
  specific type exists or can be introduced.

### 5. Design rethink when requirements changed

Code often carries the shape of an older requirement. Look for:
- A feature flag, config knob, or code path that is always the same value now —
  collapse the dead branch.
- An abstraction introduced for a second implementation that never arrived —
  inline the one real implementation.
- A conditional import or capability check for an optional dependency that has
  become required — unconditional.
- `if settings.new_path: do_new() else: do_old()` where `new_path` is always
  on in every environment — delete `do_old` and the flag.
- A pluggable backend interface with exactly one backend — inline.
- A data model field that no one reads — delete (check session/DB migrations
  if it's a persisted field).

### 6. Testability

Make the code easier to verify:
- Move I/O to edges, keep core logic pure and importable.
- Introduce a seam (a protocol, an injected dependency, a parameter) where
  tests currently require heavy monkeypatching.
- Parameterize the clock, the random source, or the UUID factory when tests
  would benefit from determinism.
- Replace module-level globals used by logic with instance state or parameters.
- If a function is currently untested and hard to test, ask: what refactor
  makes the next assertion trivial to write?

**Do not** introduce a seam purely for hypothetical future tests. The seam
earns its place when it eliminates a specific testing pain point.

## Decision Rule: Add or Remove?

When deciding whether to extract an abstraction:

| Signal | Direction |
|--------|-----------|
| Two real call sites today, plus one more about to land | **Extract** |
| One call site, speculative second caller | **Inline / leave** |
| Long method mixing three concerns | **Extract** (by concern, named by intent) |
| Short helper with a name no better than the body | **Inline** |
| Testing requires extensive patching of the callee | **Extract** a seam |
| Extract would span two layers that shouldn't know about each other | **Do not extract**; fix the layering first |
| "Generic" helper with one type of input in practice | **Inline** or **rename to the specific thing** |
| Duplication across layers with different lifetimes | **Leave duplicated** — coincidental similarity, not shared concept |
| N functions share a scaffolding pattern (setup/teardown, try/except) but have distinct signatures | **Extract the scaffolding** as a helper; **keep the N wrappers** |
| N functions look identical only because their differences are erased into `Any` or `**kwargs` | **Do not collapse** — similarity is type erasure, not semantic equivalence |

**Three similar lines is fine.** Extract when the third caller arrives *and* a
clear name presents itself. "Something similar happens in three places" is
not enough — the extracted function needs a real name.

## Project-Specific Rules

These override generic instincts (adapt to your project's frameworks):

- **Trust framework guarantees.** FastAPI handles request parsing, validation,
  and response serialization for typed routes. The OpenTelemetry instrumentation
  in `telemetry.py` injects trace/span IDs automatically — delete any code that
  sets them manually. Pydantic validates models — don't re-validate fields the
  model already constrains.
- **No feature flags** for code you can change in place.
- **No back-compat shims** for internal APIs (anything not exposed via HTTP or
  a published client lib). Change callers directly.
- **`_unused` parameters are a signal, not a pattern.** Remove the parameter
  and update callers.
- **Route handlers are thin.** A `main.py` route that does more than parse →
  delegate → respond is doing too much. Move logic into the appropriate
  domain module.
- **Settings flow downward.** Domain modules receive `Settings` (or a focused
  subset) as a parameter. They don't import-and-instantiate it themselves.

## Guardrails

- **Behavior preserved.** No observable change unless the user explicitly asks
  for a behavior change. If you discover a bug, surface it — do not silently fix.
- **One concern per edit.** Don't mix a layering fix with a contract fix with a
  peephole pass in the same hunk — separate commits or clearly-labeled diffs
  make review tractable.
- **Public API changes** (HTTP route shapes, request/response models exposed
  by the server, exported symbols in published client libraries) require explicit
  user confirmation.
- **Scope discipline.** If a finding is outside the named scope, record it and
  surface it at the end; don't silently expand.
- **Tests are load-bearing.** If a refactor breaks a test, fix the refactor —
  don't delete or weaken the test.

## Verification

Run after changes:
1. Run tests for each affected package (e.g. `pytest <path>`).
2. `uv run ruff check .` if imports or top-level structure changed.
3. `uv run ruff format --check .` if you reformatted anything.

If any check fails, fix before returning. Do not hand back a broken tree.

## Examples

### Peephole: early return

Before:
```python
def resolve_agent_image(name: str, settings: Settings) -> str:
    if name in settings.agent_images:
        image = settings.agent_images[name]
        return image
    else:
        default = f"{settings.default_registry}/{name}:latest"
        return default
```
After:
```python
def resolve_agent_image(name: str, settings: Settings) -> str:
    if override := settings.agent_images.get(name):
        return override
    return f"{settings.default_registry}/{name}:latest"
```

### Method extraction: long handler, mixed concerns

Before (route handler does auth extraction, validation, dispatch, and
response shaping inline — 60 lines):
```python
@app.post("/dispatch")
async def dispatch(request: Request, body: DispatchBody):
    # ... 15 lines extracting + parsing auth claims ...
    # ... 20 lines validating dispatch payload ...
    # ... 10 lines calling dispatcher ...
    # ... 15 lines shaping response ...
```
After:
```python
@app.post("/dispatch")
async def dispatch(claims: Claims, body: DispatchBody) -> DispatchResponse:
    job = _validated_job(body)
    started = await dispatcher.start(claims.user_id, job)
    return DispatchResponse(job_id=started.id, started_at=started.ts)
```
`Claims` arrives via a FastAPI dependency that wraps `auth.py`. Each `_`
helper is named by intent and independently testable.

### Boundary: route handler reaching past the domain module

Before:
```python
# src/myapp/main.py
from myapp.dispatch import _ActiveJobs   # underscored internal
@app.get("/jobs")
async def list_jobs():
    return list(_ActiveJobs.entries.values())   # touches private state
```
After:
```python
# src/myapp/dispatch.py
def list_active_jobs() -> list[Job]: ...

# src/myapp/main.py
from myapp.dispatch import list_active_jobs
@app.get("/jobs")
async def list_jobs() -> list[Job]:
    return list_active_jobs()
```

### Boundary: feature envy

Before (a method on `Scheduler` mostly manipulates `SessionDB` internals):
```python
class Scheduler:
    async def archive_old(self, db: SessionDB, before: datetime) -> int:
        victims = [s for s in db._sessions.values() if s.created < before]
        for s in victims:
            del db._sessions[s.id]
            db._archive.append(s)
        return len(victims)
```
After (operation moves to the module that owns the data):
```python
class SessionDB:
    async def archive_before(self, cutoff: datetime) -> int:
        victims = [s for s in self._sessions.values() if s.created < cutoff]
        for s in victims:
            del self._sessions[s.id]
            self._archive.append(s)
        return len(victims)

class Scheduler:
    async def archive_old(self, db: SessionDB, before: datetime) -> int:
        return await db.archive_before(before)
```

### Boundary: type leak across modules

Before (public signature exposes an internal type, forcing callers to import
`_jwks`):
```python
# myapp/_jwks.py
class _CachedKey: ...

# myapp/auth.py
def resolve_key(tid: str) -> _CachedKey: ...   # _CachedKey leaks
```
After (hide the internal; return the contract-level type):
```python
# myapp/_jwks.py
class _CachedKey: ...

# myapp/auth.py
def resolve_key(tid: str) -> jwt.PyJWK: ...    # callers see the public type
```

### Contract tightening: ambiguous return

Before:
```python
async def lookup_session(session_id: str):  # returns Session | None | raises
    row = await db.get(session_id)
    if row is None:
        return None
    if row.deleted:
        raise SessionDeleted(session_id)
    return Session.from_row(row)
```
After (pick one error mode, document it):
```python
async def lookup_session(session_id: str) -> Session:
    """Raises SessionNotFound if no row. Raises SessionDeleted if tombstoned."""
    row = await db.get(session_id)
    if row is None:
        raise SessionNotFound(session_id)
    if row.deleted:
        raise SessionDeleted(session_id)
    return Session.from_row(row)
```

### Design rethink: obsolete flag

Before:
```python
if settings.use_new_auth:           # always true in every env for 3 releases
    claims = new_jwt.decode(token)
else:
    claims = legacy_jwt.decode(token)
```
After:
```python
claims = new_jwt.decode(token)
```
Delete `legacy_jwt` module and the `use_new_auth` setting.

### Testability: seam for I/O-bound logic

Before:
```python
class SessionDB:
    async def remember(self, payload: str) -> str:
        session_id = str(uuid.uuid4())
        await self._db.insert(session_id, payload, datetime.utcnow())
        return session_id
```
After:
```python
class SessionDB:
    def __init__(self, db, *, clock=datetime.utcnow, id_factory=uuid.uuid4):
        self._db = db
        self._clock = clock
        self._id_factory = id_factory

    async def remember(self, payload: str) -> str:
        session_id = str(self._id_factory())
        await self._db.insert(session_id, payload, self._clock())
        return session_id
```
Now tests pass deterministic `clock` and `id_factory` — no global patching.

### Inlining: speculative helper with one caller

Before:
```python
def _format_display_name(user: User) -> str:
    return f"{user.first} {user.last}"

greeting = f"Hello, {_format_display_name(user)}"  # only caller
```
After:
```python
greeting = f"Hello, {user.first} {user.last}"
```

### Method extraction: shared scaffolding, not collapsed wrappers

Several trigger handlers differ on input shape (webhook payload vs cron tick
vs manual dispatch) but share a setup/teardown bracket pattern (acquire
session, run with telemetry span, release).

**Wrong fix** (collapse wrappers along bracket shape, hiding input
differences behind `Any`):
```python
async def handle(payload: Any, ctx) -> Any:   # lies for cron (no payload)
    return await _run_with_bracket(inner, setup, payload, ctx)
```
The parameter is named `payload` but for cron triggers there is no payload.
Similarity was an artifact of `Any`; collapsing lost contract clarity.

**Right fix** (extract the bracket pattern, keep distinct wrappers with
correct per-trigger signatures):
```python
async def _run_with_bracket(inner, setup, *args, **kwargs): ...

async def handle_webhook(payload: WebhookPayload, ctx) -> WebhookResponse:
    return await _run_with_bracket(handle_webhook_inner, setup, payload, ctx)

async def handle_cron(tick: CronTick, ctx) -> None:
    return await _run_with_bracket(handle_cron_inner, setup, tick, ctx)

async def handle_manual(req: ManualDispatch, ctx) -> DispatchResponse:
    return await _run_with_bracket(handle_manual_inner, setup, req, ctx)
```
Helper deduplicates the bracket; wrappers preserve the per-case surface.

## Report Format

After the pass, emit:

```
simply-code report
scope: <what was reviewed — file/package/diff>
files touched: N

extractions:
  src/myapp/main.py:42  extract `_validated_job` from /dispatch (test seam + readability)

inlines:
  src/myapp/config.py:88  inline `_parse_kv_uri` (1 caller, name no clearer than body)

boundaries:
  src/myapp/main.py:7  move `_ActiveJobs` access behind `dispatch.list_active_jobs()` — encapsulation
  src/myapp/auth.py:88  hide internal `_CachedKey` type from `resolve_key` return — type leak
  src/myapp/scheduler.py:61  move `archive_old` logic onto `SessionDB` — feature envy

contracts:
  src/myapp/dispatch.py:31  narrow return from `Any` to `Job | None`; split `Job`-vs-`JobDeleted` error modes

peephole:
  src/myapp/runner.py:120  collapse nested if with walrus
  src/myapp/main.py:64  early-return guard clause

rethinks:
  src/myapp/auth.py:12  delete `use_new_auth` flag branch (always on in prod 3 releases)

considered-and-rejected:
  src/myapp/triggers.py  collapse three near-identical handle_* methods — each takes a distinct trigger payload type; collapsing would erase types via *args. Leave.
  src/myapp/auth.py:88  extract `_assert_ready` helper for repeated `assert self._jwks is not None` — pure cosmetic, widens a private signature for no runtime win. Leave.

out-of-scope findings (surface, do not fix):
  src/myapp/telemetry.py: `configure_telemetry` silently swallows OTLP endpoint typos — fix in a focused PR

net LOC: +18 / -63 = -45

verification:
  uv run pytest tests/   PASS
  uv run ruff check .                   PASS
  uv run ruff format --check .          PASS
```

**`considered-and-rejected` is not optional.** List every candidate refactor
you seriously evaluated but did not apply, with the decision-rule reason in
one clause. Restraint has to be visible — a pass with no findings and no
rejections reads as a shallow scan. Omit only for genuinely trivial packages.
