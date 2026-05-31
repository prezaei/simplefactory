# Factory — Implementation Plan

*How we build Factory, in order. Derived from the [product spec](factory-product-spec.md) (the contract) and the [technical spec](factory-technical-spec.md) (the blueprint). This plan makes no new technology choices; it sequences the build.*

## Approach

**Thin vertical slice first, then harden.** We get a single end-to-end path working early — *labelled issue → Coder writes code+tests → opens a change request* — even if crude, then layer on review, resilience, scale, and the dashboard. This surfaces integration risk (GitHub App auth, driving Claude Code headless, the MCP boundary, Container Apps Jobs) in week one rather than month three.

**Principles** (from `AGENTS.md`):
- **Design → tests → code.** Each task starts from the behavior in the specs, writes the test, then the code.
- **Local-first.** Everything runs on `docker-compose` + Azurite + local Postgres before it runs on Azure. Same images/binaries.
- **One path, no optimizations.** Build the simplest correct mechanism the technical spec describes; don't add second codepaths.
- **Telemetry from day one.** Every service emits OpenTelemetry from its first commit — logging is not a later phase.
- **Every reliability property gets a test that asserts it** (no-double-starts, exactly-once escalation, cap never exceeded, idempotent redo).

**Crate map** (target, per the technical spec's workspace layout):
`services/{intake,dispatcher,coordinator,tools,dashboard-api,worker}`, `libs/{record,github,blob,config,telemetry}`, `dashboard/` (React/TS).

## Milestones at a glance

| Phase | Goal | Exit criteria |
| --- | --- | --- |
| **P0 Foundations** | Workspace, CI, local stack, cross-cutting libs, **full E2E suite stubbed** | `cargo fmt/clippy/test` green in CI; `docker-compose up` brings up Postgres + Azurite; telemetry + config libs usable; whole E2E acceptance suite exists as compiling `#[ignore]`d stubs |
| **P1 Record book + queue** | Durable state, leasing, caps, idempotency | Concurrency tests prove no-double-starts + cap-never-exceeded |
| **P2 GitHub + Intake** | Events in, opt-in gating | A labelled issue creates exactly one `work_item`; webhook dedupe verified |
| **P3 Vertical slice (Coder→CR)** | First Coder→CR run (integration proof) | An injected coder work item yields a real change request, locally (labelled-issue→Coder needs P4) |
| **P4 Coordinator + Reviewer** | Full decide/delegate/review loop | issue → code → review → human-merge handoff works; loop cap + escalation enforced |
| **P5 Resilience** | Crash-resume, sweeper, bounds, config pinning | Kill a worker mid-run → it resumes the same session; all bounds tested |
| **P6 Dashboard** | Live read-only view | SSE stream reflects state within seconds |
| **P7 Azure productionization** | Deploy to dev + prod | Bicep-provisioned dev env runs the full loop on Azure |

---

## P0 — Foundations

**Goal:** a building, testing, observable skeleton with the cross-cutting libraries the rest depends on.

**Tasks**
- Cargo workspace scaffold: `services/*` (bins) + `libs/*` (crates), shared workspace deps (tokio, axum, sqlx, anyhow/thiserror, opentelemetry).
- `libs/telemetry`: OpenTelemetry setup (traces/metrics/logs), `correlation_id` propagation helpers, OTLP exporter (Azure Monitor in prod, console/collector locally).
- `libs/config`: parse + validate `.factory/config.yml` (serde-yaml) → typed model; validation rules (roles, cron, positive caps, `instructions_file` resolvable, and **no non-manager worker subscribed to `issue_labeled`** — manager-first, see P2); prompt-layering resolver (role default → repo-wide → per-worker).
- `docker-compose.yml`: Postgres 16 + Azurite (Blob emulator); `.env` wiring; gitignore `.ship-it/` and local state.
- GitHub Actions CI: `cargo fmt --check && cargo clippy -- -D warnings && cargo test` + build images; integration-test job spins up the compose services.
- `migrations/` directory wired to `sqlx migrate` + the CI migration gate.
- **Author the full end-to-end acceptance suite, up front, as stubs.** One named E2E test per behavior the specs promise — at minimum: worked-example happy path (labelled issue → Manager → Coder → CR → Reviewer approve → human-merge handoff → issue closed); changes-requested rework loop + loop-cap escalation; needs-human-input (blocked → human comment → resume same session); crash-resume (worker killed mid-run → resumes); no-double-starts under concurrent coordinators; capacity caps never exceeded (per-worker + global); exactly-once escalation under re-dispatch; opt-in gating (unlabelled issue → nothing); can't-weaken-own-checks → escalated; invalid config → rejected, last-good kept; retention aging. Each is a **compiling, named, `#[ignore]`d stub** (skeleton + `todo!()`/pending assertions) driving the public entry points against the local stack (Claude Code faked at the seam for determinism). This is the **acceptance backbone**: it pins the whole contract now, and every later phase's exit becomes "the E2E stubs it owns are un-ignored and green."

**Tests:** config parse/validate (valid + every invalid case), prompt-layering composition, telemetry smoke (spans emitted); **plus the full E2E acceptance suite authored as `#[ignore]`d stubs** — compiling and named, not yet passing.
**Exit:** CI green; `docker-compose up` healthy; `cargo test` covers `config` + `telemetry`; the full E2E acceptance suite exists as compiling, ignored stubs (CI runs them as ignored, so the list can't silently rot).
**Depends on:** nothing.

## P1 — Record book, queue & leasing (`libs/record`)

**Goal:** the single source of truth, plus the concurrency-safe queue that everything else leases from.

**Tasks**
- Migrations for all tables: `repos`, `work_items`, `attempts`, `issues`, `change_requests`, `escalations`, `events`, `webhook_deliveries` (UUIDv7 PKs, text+CHECK enums, `config_sha` pin).
- Indexes: `work_items_active_uniq` (partial unique, idempotent enqueue), `work_items_pickup` (partial, by `worker_name`).
- `record` API: enqueue (idempotent), atomic lease (`FOR UPDATE SKIP LOCKED`), capacity check serialized via `pg_advisory_xact_lock` — keyed on `(repo_id, worker_name)` for the per-worker cap and on a coarser `repo_id` key (or a `SERIALIZABLE` txn) for the global ceiling, since a per-pool lock alone does not serialize the global count — state transitions, `events` append (audit), retention purge.
- Lease lifecycle: lease/extend(heartbeat)/expire helpers.

**Tests (the load-bearing ones):**
- **No-double-starts:** N concurrent leasers, M items → each item leased exactly once.
- **Cap never exceeded:** concurrent leasers against a cap of K → running count never > K (per-worker and global).
- **Idempotent enqueue:** duplicate enqueue of same `(repo, subject, kind)` → one active row; terminal row doesn't block re-enqueue.
- Retention purge removes only terminal rows past the window.

**Exit:** the four concurrency/idempotency tests pass under load (run with `--test-threads` high). **Depends on:** P0.

## P2 — GitHub integration + Intake (`libs/github`, `services/intake`)

**Goal:** real events flow in, gated by opt-in.

**Tasks**
- `libs/github` (**inbound**): webhook **HMAC verification** (constant-time) + typed events — issue labelled, CR opened/updated, `review_requested`, `pull_request_review` → changes-requested, **issue_comment**, push. *(Done.)*
- `services/intake` (Axum): verify signature → reject requests missing the GitHub identity headers → dedupe on delivery id → filter by opt-in `label` → enqueue `work_item` (pinning current `config_sha`). Fast 2xx; all work async off the queue. **Lost-event-safe:** a delivery id is recorded only *after* successful processing, and "not ready" outcomes (repo not connected / no config) are left unrecorded so a GitHub redelivery re-drives them. Every ingest writes an `events` audit row. *(Done.)*
- **Enqueue policy — manager-first.** On `issue_labeled`, Intake enqueues a single **Manager** work item; coders/reviewers/testers are woken downstream by the Manager (`manager` trigger) or by their own CR/comment triggers — never by `issue_labeled` directly. (This resolves the spec's config-vs-worked-example ambiguity; P0 config validation enforces it.) *(Done.)*
- `services/intake` scheduler loop: full cron parsing (`libs/config`), read each repo's config timers, and fire due `schedule` work **exactly once across replicas** via a per-`(repo,worker)` advisory lock that re-checks `schedule_state.last_fired` (work-item idempotency is the backstop). *(Done.)*
- **blocked→queued transition:** an `issue_comment` on a tracked blocked item flips it to `queued` (`available_at=now()`). *(Done.)*
- Resolve only **active** repos (suspended/removed never produce work); `webhook_deliveries` retention purge helper (the dedupe table is load-bearing, so it must be prunable). *(Done.)*

- **Carried to P3 (outbound GitHub — no consumer until the worker/Tools exist, and needs live App credentials to test):** `libs/github` (octocrab) App auth, short-lived **installation-token** minting, and **Checks/PR read APIs**; **config-auto-reload-on-push** (fetching `.factory/config.yml` content on a push that touches it, then calling `apply_config`). The validate-and-store unit (`apply_config`) and push-path detection (`Push` carries `touched_paths`) are built and tested now; only the *authenticated file fetch* is deferred to P3 where the App client lands.

**Tests:** signature reject/accept; missing-header reject; dedupe (redelivered webhook → one row); **lost-event redrive** (unknown-repo delivery isn't burned; after the repo connects, the redelivery enqueues); label gating (unlabelled → nothing); CR-opened / review-changes-requested enqueue; scheduler **fires once across replicas** (+ stale-slot never re-fires); comment-on-blocked → requeued; active-only repo resolution; deliveries purge. *(All implemented and green against Postgres.)*
**Exit:** a labelled issue (via a webhook fixture) creates exactly one Manager `work_item`. **Depends on:** P1. ✅ **met.**

## P3 — Vertical slice: Dispatcher + Worker + minimal Tools (Coder → CR)

**Goal:** the first real Coder→CR run, locally — proving the worker / dispatcher / Tools / Claude-Code integration. Crude but real. (Full end-to-end *from a labelled issue* arrives in P4, once the Manager exists.)

**Tasks**
- `libs/github` (**outbound**, carried from P2): octocrab App auth, short-lived **installation-token** minting, **Checks/PR read APIs**. First real consumers are the worker (clone token) and Tools (posting to GitHub); needs the dev App's private key to test against GitHub.
- **Config-auto-reload-on-push** (carried from P2): on a push touching `.factory/config.yml`, fetch the file content via the installation token and call the existing `apply_config`. (Detection + validate/store already exist; this wires the authenticated fetch.)
- `services/tools` (MCP server, HTTP/SSE) — **minimal surface:** `get_task`, `read_issue`, `open_or_update_change_request`, `post_comment`, `append_transcript`, `record_outcome`, `heartbeat`. Per-task token auth; every call → `events`.
- `services/dispatcher`: lease → launch a worker (local Docker container in dev) with the per-task token + Tools URL.
- `services/worker` harness: mint installation token → **fresh shallow clone** → launch **Claude Code headless** with the composed prompt + Factory MCP configured → on finish, `open_or_update_change_request` (idempotent) + `record_outcome`; push branch via `--force-with-lease`.
- `libs/blob` (+ Azurite): transcript append.

**Tests:** worker harness drives a stub MCP through a clone→edit→CR sequence; idempotent CR (re-run updates same CR); Tools token scoping (a token can't touch another task).
**Exit (the milestone moment):** a hand-injected coder work item drives a real Coder run — fresh clone → Claude Code → tests + code → a real change request — locally (this is exactly the seam P3's own test uses). Driving it from a *labelled issue* needs the Manager and lands in P4. **Depends on:** P2.

## P4 — Coordinator + Reviewer (full loop)

**Goal:** the manager decides/delegates and review closes the loop to a human merge.

**Tasks**
- `services/coordinator` (Manager): react to completions → assign Coder, wake Reviewer on new CR, hand approved+green CR to a human, close the loop, or escalate. Per-issue decisions serialized by an issue row-lock; enqueue next-steps idempotently.
- Reviewer flow: event-driven on `cr_opened`; Tools added here — `read_change_request` (diff + threads), `submit_review` (with `dedupe_key`), `get_check_status`.
- **Bounded loops:** `loop_count` increments on changes-requested rework; escalate at cap.
- **Escalation:** `escalate` tool (with `dedupe_key`) → comment + assign issue + `escalations` row; the single human channel.

**Tests:** full happy path (issue→code→review→ready-for-human-merge); changes-requested loop increments and caps; **exactly-once** escalation under re-dispatch; reviewer verdict idempotent; **a change that weakens/disables its own gating checks is rejected → escalated, never marked ready** (PROD "can't weaken its own checks").
**Exit:** end-to-end issue→merge-handoff works locally for the worked example. **Depends on:** P3.

## P5 — Resilience (crash-resume, sweeper, bounds, config pinning)

**Goal:** the reliability guarantees the product spec leads with.

**Tasks**
- Continuous **checkpoint** of Claude Code session + worktree → Blob (short interval + tool boundaries); `attempts.session_id`/`checkpoint_uri`.
- **Resume:** on lease expiry, restore worktree + `claude --resume`; bounded resume count → fresh attempt/escalation.
- `services/coordinator` **lease sweeper:** reclaim expired leases, mark attempt failed/reclaimed, revoke token.
- Retry bounds (`attempt_count`), per-attempt wall-clock cap.
- **config_sha pinning** honored: in-flight attempts use pinned config; removed workers finish then stop.

**Tests:** kill a worker mid-run → next dispatch resumes the *same* session; deterministic-crash loop → falls through after N resumes; sweeper reclaims a hung worker; blocked-on-human time excluded from caps; pinned config survives a mid-flight config change.
**Exit:** chaos test (random worker kills) completes work without loss or duplication. **Depends on:** P4.

## P6 — Dashboard (`services/dashboard-api`, `dashboard/`)

**Goal:** humans can watch.

**Tasks**
- `services/dashboard-api` (Axum): read-only projections of the record book; **SSE/WebSocket** live stream; eventually-consistent "Fresh view" (within seconds).
- `dashboard/` React/TS SPA: workers live, finished runs, ticket/CR state, transcript viewer (links to Blob).

**Tests:** API projection correctness; SSE pushes a state change within the freshness budget; read-only (no mutating endpoints).
**Exit:** open the dashboard, label an issue, watch the run progress live. **Depends on:** P4 (P5 optional but preferred).

## P7 — Azure productionization

**Goal:** the same system, on Azure, in dev then prod.

**Tasks**
- **Bicep** (`infra/`): dev + prod resource groups — Postgres Flexible Server (Burstable, no HA), Blob (LRS, lifecycle = 30d), Container Apps env, ACR, Key Vault, Azure Monitor/Log Analytics. Per-service **user-assigned Managed Identity** + least-privilege roles.
- Worker runtime: switch dev's local Docker to **Azure Container Apps Jobs**; services → Container Apps services (N replicas).
- Secrets to **Key Vault**; OTel exporter → **Azure Monitor**; Blob lifecycle + Postgres retention jobs.
- **Anthropic** commercial API + **zero-data-retention** configured; key in Key Vault.
- CD: GitHub Actions → ACR → Container Apps; the single-runner migration step before rollout.

**Tests:** smoke-test the full loop in dev Azure; deploy/rollback (redeploy prior revision) drill; expand/contract migration applied with old+new replicas coexisting.
**Exit:** a labelled issue in a real connected repo runs end-to-end on Azure dev. **Depends on:** P4 (the full loop the smoke test exercises); ideally P5–P6 for a production-honest deploy. (A thin P3-only slice can be deployed earlier to validate the Azure plumbing, but that is not the full-loop exit above.)

---

## Cross-cutting workstreams (every phase)

- **Telemetry:** each new component emits spans/metrics/logs with `correlation_id`; "an absent log is a defect."
- **Tests:** the **full E2E acceptance suite is authored up front in P0 as `#[ignore]`d stubs**; each later phase **activates (un-ignores) and makes green** the E2E tests its mechanism satisfies — a phase is not done while an E2E test it owns is still ignored. On top of that: unit per function + integration per slice (real Postgres/Azurite, Claude Code faked at the seam where deterministic). Every enum variant through every entry point.
- **Security:** Managed Identity + per-task tokens enforced from P3; no secrets in code; worker can't weaken its own checks (P4).
- **Docs:** keep the technical spec in sync if implementation reveals a needed change (PR updates the spec too).

## Sequencing rationale & risks

- **Why vertical-slice at P3:** the highest integration risks (GitHub App auth, headless Claude Code, the MCP boundary, job launch) are proven before we invest in coordinator/resilience/dashboard.
- **Why record book before everything (P1):** it's the contract every service shares; getting leasing/caps/idempotency right early avoids reworking all callers.
- **Top risks:** (1) driving Claude Code headless + session resume semantics — de-risk with a spike in P3; (2) Container Apps Jobs cold-start vs the "minutes" pickup target — measure in P7; (3) the no-HA Postgres SPOF — accepted, revisit if dev stability demands HA.
- **Parallelizable:** `dashboard/` (P6) can start after P1's schema stabilizes; `infra/` Bicep (P7) can be drafted alongside P3+.

## Definition of done (per phase)

A phase is done when: its exit criterion holds, its tests pass in CI (`fmt`+`clippy`+`test`), **every E2E acceptance stub the phase owns is un-ignored and green** (and none it doesn't own was weakened), telemetry covers its new paths, and the relevant requirements-traceability rows in the technical spec are backed by a real, tested mechanism. The count of still-`#[ignore]`d E2E tests is the project's burndown to done.
