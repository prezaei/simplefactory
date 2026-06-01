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
| **P4.5 Per-role worker lifecycle** | Manager-triage + Reviewer-verdict workers (not Coder-shaped) | a manager/reviewer work item runs its role via Claude and produces **no** change request; only the coder opens a CR |
| **P5 Resilience** | Crash-resume, sweeper, bounds, config pinning | Kill a worker mid-run → it resumes the same session; all bounds tested |
| **P6 Dashboard** | Live read-only view | SSE stream reflects state within seconds |
| **P7 Local live smoke** | One real end-to-end run locally (real GitHub App + `claude` + Docker + Azurite) | A labelled issue in a throwaway repo drives the full loop locally → real CR + merge-handoff |
| **P8 Azure productionization** | Deploy to dev + prod | Bicep-provisioned dev env runs the full loop on Azure |

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
- **Author the full end-to-end acceptance suite, up front, as stubs.** One named E2E test per behavior the specs promise — at minimum: worked-example happy path (labelled issue → Manager → Coder → CR → Reviewer approve → human-merge handoff; post-merge issue closure is a later capability); changes-requested rework loop + loop-cap escalation; needs-human-input (blocked → human comment → resume same session); crash-resume (worker killed mid-run → resumes); no-double-starts under concurrent coordinators; capacity caps never exceeded (per-worker + global); exactly-once escalation under re-dispatch; opt-in gating (unlabelled issue → nothing); can't-weaken-own-checks → escalated; invalid config → rejected, last-good kept; retention aging. Each is a **compiling, named, `#[ignore]`d stub** (skeleton + `todo!()`/pending assertions) driving the public entry points against the local stack (Claude Code faked at the seam for determinism). This is the **acceptance backbone**: it pins the whole contract now, and every later phase's exit becomes "the E2E stubs it owns are un-ignored and green."

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
- **Enqueue policy — manager-first.** On `issue_labeled`, Intake enqueues a single **Manager** work item; coders/reviewers/testers are enqueued downstream by the Coordinator as the manager-first next step (via the `manager` trigger) or fire on their own CR/comment triggers — never on `issue_labeled` directly. (The Manager worker doesn't dispatch; its success drives the Coordinator.) (This resolves the spec's config-vs-worked-example ambiguity; P0 config validation enforces it.) *(Done.)*
- `services/intake` scheduler loop: full cron parsing (`libs/config`), read each repo's config timers, and fire due `schedule` work **exactly once across replicas** via a per-`(repo,worker)` advisory lock that re-checks `schedule_state.last_fired` (work-item idempotency is the backstop). *(Done.)*
- **blocked→queued transition:** an `issue_comment` on a tracked blocked item flips it to `queued` (`available_at=now()`). *(Done.)*
- Resolve only **active** repos (suspended/removed never produce work); `webhook_deliveries` retention purge helper (the dedupe table is load-bearing, so it must be prunable). *(Done.)*

- **Carried to P3 (outbound GitHub — no consumer until the worker/Tools exist, and needs live App credentials to test):** `libs/github` (octocrab) App auth, short-lived **installation-token** minting, and **Checks/PR read APIs**; **config-auto-reload-on-push** (fetching `.factory/config.yml` content on a push that touches it, then calling `apply_config`). The validate-and-store unit (`apply_config`) and push-path detection (`Push` carries `touched_paths`) are built and tested now; only the *authenticated file fetch* is deferred to P3 where the App client lands.

**Tests:** signature reject/accept; missing-header reject; dedupe (redelivered webhook → one row); **lost-event redrive** (unknown-repo delivery isn't burned; after the repo connects, the redelivery enqueues); label gating (unlabelled → nothing); CR-opened / review-changes-requested enqueue; scheduler **fires once across replicas** (+ stale-slot never re-fires); comment-on-blocked → requeued; active-only repo resolution; deliveries purge. *(All implemented and green against Postgres.)*
**Exit:** a labelled issue (via a webhook fixture) creates exactly one Manager `work_item`. **Depends on:** P1. ✅ **met.**

## P3 — Vertical slice: Dispatcher + Worker + minimal Tools (Coder → CR)

**Goal:** the first real Coder→CR run, locally — proving the worker / dispatcher / Tools / Claude-Code integration. Crude but real. (Full end-to-end *from a labelled issue* arrives in P4, once the Manager exists.)

**Approach (local-first):** every external system the slice touches — GitHub's API, a git clone, the `claude` subprocess, the worker container launch, Blob — is behind a **trait seam** with a deterministic fake, so the whole slice is built and tested locally against real Postgres without live credentials. The matching real implementations are the **deploy-time seams** (they need a live GitHub App key / Docker / the `claude` binary, so they can't run in CI) and are clearly isolated.

**Tasks**
- `libs/github` (**outbound**, carried from P2): `GitHubClient` trait + types + `FakeGitHub` (deterministic double, tested). *(Done.)* Real `OctocrabGitHub` wired against octocrab 0.51 — **all 8 methods** (installation token, read issue/CR, open/update CR idempotent-by-branch, comment, review, get file, **check_status**). `check_status` works around the octocrab 0.51 `combined_status_for_ref` raw-SHA limitation by paging `list_check_runs_for_git_ref(Commitish(sha))` and folding in the legacy combined commit status via a raw REST GET to `/repos/{owner}/{name}/commits/{sha}/status` (see `octo.rs`). Live calls need the App private key, so the network path is verified at deploy, not in CI. 
- **Config-auto-reload-on-push** (carried from P2): `apply_config_from_push` fetches `.factory/config.yml` via `GitHubClient.get_file` on a push that touches it and calls the existing `apply_config`; wired into the intake webhook handler. Tested with `FakeGitHub` (apply / ignore-unrelated / reject-invalid-keep-last-good). *(Done.)*
- `services/tools` (the worker's only channel): Axum server, **per-task bearer-token auth** (sha-256-hashed tokens, scoped to one work item/repo/role, TTL), the full tool surface (`get_task`, `read_issue`, `read_change_request`, `open_or_update_change_request`, `post_comment`, `submit_review`, `get_check_status`, `append_transcript`, `record_outcome`, `heartbeat`, `escalate`), **every call → `events` audit** (single choke point), and idempotency for CR / comment / review / escalate (`dedupe_key`); tokens revoked on task completion. *(Done; 16 REST + 10 MCP integration tests.)* A real **MCP JSON-RPC 2.0 server at `POST /mcp`** (`initialize` / `tools/list` / `tools/call`) sits over the same audited tool dispatch — what a live Claude Code connects to. *(Done; tested.)*
- `services/dispatcher`: `dispatch_once` walks configured repos, leases under per-worker + global caps, mints a per-task token, starts an attempt, and launches a worker via a `WorkerLauncher` seam (`FakeLauncher` tested; `DockerLauncher` = deploy seam, secrets via env). *(Done; 3 integration tests incl. cap binding + token scoping.)*
- `services/worker` harness: `run_task` orchestrates get_task → shallow clone → Claude Code → push branch → `open_or_update_change_request` (idempotent) → `record_outcome`, all over `ToolsClient` / `Workspace` / `ClaudeRunner` seams (fakes tested; `HttpTools`/`GitCli`/`ClaudeCli` = deploy seams). *(Done; pure tests: full sequence, idempotent re-run, failure path.)*
- `libs/blob`: `TranscriptStore` trait + `InMemoryTranscripts` (tested). *(Done.)* P3 also shipped an `FsTranscripts` local-parity store; **P7 replaced it** with the real **Azure Blob** backend (`AzureBlobTranscripts`, run against Azurite locally), now the sole production store — `Fs*` was deleted, `InMemory*` remains only as a unit-test double. Real Azure is the P8 endpoint/credential swap.

**Tests:** worker harness drives the fakes through a clone→edit→CR sequence; idempotent CR (re-run updates same CR); Tools token scoping (an invalid token is rejected; a token acts only on its own task); dispatcher caps bind; config-on-push apply/ignore/reject. *(All implemented and green against Postgres.)*
The real subprocess seams are implemented too: `GitCli` (shallow clone + `--force-with-lease` push, token-in-URL), `ClaudeCli` (runs `claude` headless with the Factory MCP server at `{tools_url}/mcp` + bearer), `DockerLauncher` (`docker run` with secrets via env) — each with pure-helper unit tests (URL/arg/MCP-config construction); only their actual subprocess execution needs git/claude/docker present.

**Tests:** worker harness drives the fakes through clone→edit→CR; idempotent CR; **`HttpTools` ↔ Tools router over the wire** (real reqwest against a live `build_router`); MCP `initialize`/`tools/list`/`tools/call`; token scoping + revoke-on-completion; dispatcher caps bind; config-on-push apply/ignore/reject; **P3 acceptance E2E** (`p3_coder_work_item_drives_a_change_request`). *(All green against Postgres.)*
**Exit (the milestone moment):** a hand-injected coder work item drives a Coder run — clone → Claude Code → CR — locally. The orchestration, the Tools MCP server, and the real GitHub/git/claude/docker impls are all built and tested to the limit of what runs without live credentials; the only remaining step is executing against a real GitHub App + `claude` binary + Docker. Driving it from a *labelled issue* needs the Manager and lands in P4. **Depends on:** P2. ✅ **met (live execution needs real creds/binaries).**

## P4 — Coordinator + Reviewer (full loop)

**Goal:** the **Coordinator** decides/delegates and review closes the loop to a human merge.

**Tasks**
- `services/coordinator` (the Coordinator): react to completions → assign Coder, wake Reviewer on new CR, hand approved+green CR to a human, close the loop, or escalate. Per-issue decisions serialized by an issue row-lock; enqueue next-steps idempotently.
- Reviewer flow: event-driven on `cr_opened`; Tools added here — `read_change_request` (diff + threads), `submit_review` (with `dedupe_key`), `get_check_status`.
- **Bounded loops:** `loop_count` increments on changes-requested rework; escalate at cap.
- **Escalation:** `escalate` tool (with `dedupe_key`) → comment + assign issue + `escalations` row; the single human channel.

**Tests:** full happy path (issue→code→review→ready-for-human-merge); changes-requested loop increments and caps; **exactly-once** escalation under re-dispatch; reviewer verdict idempotent; **a change that weakens/disables its own gating checks is rejected → escalated, never marked ready** (PROD "can't weaken its own checks").
**Exit:** end-to-end issue→merge-handoff works locally for the worked example. **Depends on:** P3.

> **Deferred — Coordinator closes the issue on the human's merge.** P4's exit is the ready-for-human-**merge** handoff: it marks the CR `ready`, comments, and assigns the linked issue to a human, leaving the issue OPEN (`in_progress`). Actually **closing the issue on merge** requires ingesting GitHub's `pull_request closed/merged` webhook so the Coordinator can observe the merge; that ingestion is **not built in P4** (deferred to a later capability). Until then the issue intentionally stays open after handoff rather than being marked `done`.

## P4.5 — Per-role worker lifecycle (Manager triage + Reviewer verdict)

**Goal:** make the worker do the **right thing per agent role**. P3 built the **Coder** lifecycle end-to-end (clone → Claude edits → push branch → open/update CR → record). P4 built the **Coordinator's** reactive side and added the reviewer's *Tools* (`read_change_request`, `submit_review`, `get_check_status`). But the worker harness (`run_task`) is still **Coder-shaped for every role** — so a **Manager** or **Reviewer** work item, when leased, would run Claude correctly and then wrongly **push an empty branch and open a spurious change request** the Coordinator never expects. This step closes that gap.

> **Naming:** "Manager" is two different things. The **Coordinator** service (P4) is deterministic Rust — it reacts to outcomes and never calls a model. The **Manager *role*** is an AI worker that triages an issue via Claude. This step is about the **worker roles**; the Coordinator is unchanged.

**Tasks**
- `services/worker`: make `run_task` **role-aware** (branch on `task.role` from `get_task`) — the branch covers **manager / coder / reviewer**; **`tester` is reserved/unsupported for now**. `tester` is accepted as a valid role (`validate_role` in `services/worker` admits all four), but it has **no P4.5 lifecycle**, so reaching the run path with a `tester` (or any unknown) role is an explicit **unsupported/no-op rejected loudly at the seam** — never given a coder-style default:
  - **Coder** (unchanged): clone → Claude edits → push branch → `open_or_update_change_request` → `record_outcome`.
  - **Reviewer**: read the CR (`read_change_request`) → Claude reviews → `submit_review` verdict (e.g. `approve` / `request_changes`) → `record_outcome`. **No** branch push, **no** CR.
  - **Manager**: read the issue (`read_issue`) → Claude triages. On an **actionable** issue: optionally `post_comment` a triage note, then `record_outcome` `succeeded` — and only a *succeeded* Manager item makes the Coordinator assign a Coder. On a **non-actionable** issue: `escalate` (a terminal escalated outcome; the Coordinator parks it and does **not** assign a Coder). Either way: **no** clone-edit-push, **no** CR. (No label-mutating Tool exists in the Tools surface, so triage acts via comments/escalation, not labels; a manager may not need a clone at all — keep it lean.)
- Role-default prompts: the per-role base prompt and the composition order (role default → repo-wide → per-worker) **already exist** — `role_default_prompt` + `compose_prompt` in `services/tools` (shipped in P3). P4.5 just **reuses/verifies** them for the manager and reviewer roles (and confirms each role's default prompt fits its new no-CR lifecycle); it does NOT add new prompt-composition.
- Keep all worker effects mediated by Tools (the existing trust boundary); no new direct Postgres/Blob access.

**Tests:** per-role worker lifecycle against the fakes — a **manager** run produces **no** CR and records success (→ Coordinator assigns a coder); a **manager** non-actionable run **escalates** (no CR, no success); a **reviewer** run calls `submit_review` with a verdict and opens **no** CR; the **coder** path is unchanged (still opens a CR); **tester** and any unknown role fail loudly at the run seam. The **real role lifecycle is exercised end-to-end over the wire** — `services/worker/tests/http_tools_roundtrip.rs` drives the actual `run_task` for a manager and a reviewer through the real `HttpTools` ↔ Tools router and asserts each performs its terminal action and creates **no** change request. *(Done.)*
**Exit:** the full agent loop runs with correct per-role worker behavior — labelled issue → **manager triages** → Coordinator assigns coder → **coder opens CR** → **reviewer submits a verdict** → Coordinator hands off — with exactly one CR (the coder's) and no spurious branches. **Depends on:** P4. ✅ **met** (worker `run_task` is role-aware; the coder leg's live git/`claude` execution remains a deploy seam, as in P3).

## P5 — Resilience (crash-resume, sweeper, bounds, config pinning)

**Goal:** the reliability guarantees the product spec leads with.

**Tasks**
- Continuous **checkpoint** of Claude Code session + worktree → Blob (short interval + tool boundaries); `attempts.session_id`/`checkpoint_uri`.
- **Resume:** on lease expiry, restore worktree + `claude --resume`; bounded resume count → fresh attempt/escalation.
- `services/coordinator` **lease sweeper:** reclaim expired leases, mark attempt failed/reclaimed, revoke token.
- **Worker heartbeat loop:** the worker's *periodic* heartbeat (a ~60s loop that calls the Tools `heartbeat` endpoint to extend its 15-min lease for the whole run) lands here, alongside crash-resume and the sweeper — it is the liveness signal the sweeper trusts. P3 already ships the `heartbeat` Tools endpoint and emits a single heartbeat; that one beat is correct within the 15-min lease window, but keeping a long run alive needs this loop, which is why it is a P5 resilience task.
- Retry bounds (`attempt_count`), per-attempt wall-clock cap.
- **config_sha pinning** honored: in-flight attempts use pinned config; removed workers finish then stop.

**Tests:** kill a worker mid-run → next dispatch resumes the *same* session; deterministic-crash loop → falls through after N resumes; sweeper reclaims a hung worker; blocked-on-human time excluded from caps; pinned config survives a mid-flight config change.
**Exit:** chaos test (random worker kills) completes work without loss or duplication. **Depends on:** P4. ✅ **met** (the real Blob-checkpoint *bytes* and `claude --resume` *execution* remain deploy seams, as the P3 git/`claude` legs are; Blob retention *lifecycle* is P8).

**What landed (and where the seams are):**
- **Lease sweeper** — `coordinator::sweeper_tick` (a second loop in `services/coordinator/src/main.rs`, 30s) reclaims every expired-lease item: it finishes the lapsed `attempts` row as `reclaimed`, revokes the task's tokens, and requeues it under the retry cap or escalates `retries_exhausted` (terminal) at the cap. Built on `record::{fetch_expired_lease_items, reclaim_claim, reclaim_running_attempt}` (the old blanket `reclaim_expired` was reshaped into these). The claim is optimistic on the observed lease expiry + guarded `state IN ('leased','running')`, serialized per issue with the coordinator's react — exactly-once under racing replicas.
- **Per-attempt wall-clock cap** — `fetch_expired_lease_items` also returns runs whose `attempts.started_at` is older than `FACTORY_WALL_CLOCK_CAP_SECS` (default 30 min, the spec's platform default) even while the lease is live, so a stuck-but-alive heart-beating worker is still bounded. One codepath with lease-expiry reclaim.
- **Retry bounds** — unchanged `decide_failed` model (requeue under `settings.retry_cap`, else escalate `retries_exhausted`); the sweeper reclaim hits the same cap.
- **Worker heartbeat loop** — `worker::heartbeat_loop` (~60s) runs concurrently with the role via `tokio::select!`, extending the lease for the whole run; cancelled when the role completes. P3's single initial beat is kept.
- **Crash-resume + checkpoint** — `worker::checkpoint_loop` snapshots the session + worktree on an interval and persists the pointer via the new `save_checkpoint` Tools call (→ `attempts.session_id`/`checkpoint_uri`). On re-dispatch, `dispatcher::decide_resume` reads `record::last_checkpoint_for_item` and, while the bounded resume count (`DEFAULT_RESUME_CAP = 3`) isn't exhausted, injects `FACTORY_RESUME_SESSION_ID`/`_CHECKPOINT_URI`; the worker restores the worktree and runs `claude --resume <id>` (vs `--session-id <id>` fresh) — past the cap it falls through to a fresh attempt. The Blob snapshot bytes + the `claude` invocation are deploy seams (faked in CI behind `blob::CheckpointStore` + `ClaudeRunner`); the resume-vs-fresh decision, checkpoint cadence, and bounded count are unit/integration tested.
- **config_sha pinning** — the work item's pinned `config_sha` rides into the worker env (`FACTORY_CONFIG_SHA`); a worker removed from config mid-flight is never re-dispatched (the dispatcher only leases workers present in the current config), so in-flight attempts finish and stop.
- **E2E:** the three P5 acceptance stubs are un-ignored and green (`needs_human_input_blocks_then_resumes_same_session_on_reply`, `crash_resume_continues_the_same_session`, `retention_aging_removes_only_terminal_records` — the latter scoped to the `record::purge_terminal` half; Blob lifecycle is P8).

This backs the technical spec's resilience requirements-traceability rows (crash-resume / checkpoint-resume, lease reclamation + token revocation, retry + wall-clock bounds with escalation, config pinning, retention purge) with real, tested mechanisms — except the Blob-bytes / `claude` execution and Blob lifecycle, which are deploy/P8 seams.

## P6 — Dashboard (`services/dashboard-api`, `dashboard/`)

**Goal:** humans can watch.

**Tasks**
- `services/dashboard-api` (Axum): read-only projections of the record book; **SSE/WebSocket** live stream; eventually-consistent "Fresh view" (within seconds).
- `dashboard/` React/TS SPA: workers live, finished runs, ticket/CR state, transcript viewer (links to Blob).

**Tests:** API projection correctness; SSE pushes a state change within the freshness budget; read-only (no mutating endpoints).
**Exit:** open the dashboard, label an issue, watch the run progress live. **Depends on:** P4 (P5 optional but preferred).

## P7 — Local live smoke (one real end-to-end run)

**Goal:** prove the real **deploy seams actually work** — end-to-end, locally, against a throwaway test repo — *before* adding Azure infrastructure. Every external integration the earlier phases built behind a trait and faked in CI (`OctocrabGitHub`, `ClaudeRunner`=`ClaudeCli`, `Workspace`=`GitCli`, `WorkerLauncher`=`DockerLauncher`, Blob via Azurite) gets its first real exercise here. This deliberately **separates seam-correctness from infra-correctness**: if P8's first cloud run fails, the seams are already known-good, so the failure is Azure plumbing — not git auth, `claude --resume`, docker launch, or the octocrab calls.

**Decisions (locked).** The smoke runs at **full P8 fidelity locally** so the only thing P8 adds is cloud infra:
- **Worker runtime: real Docker image + `DockerLauncher`** (not the binary-direct shortcut) — `services/worker/Dockerfile` packages the worker binary + `git` + the `claude` CLI; the dispatcher launches it via `docker run`, exercising the launch seam.
- **Claude auth: `claude setup-token`** — a long-lived token (generated once, interactively, on the host via `claude login` → `claude setup-token`) injected into the worker container as an env var. No host `~/.claude` bind-mount; mirrors how P8 injects a Key Vault secret into Container Apps.
- **Checkpoint/transcript storage: Azure Blob, against Azurite locally.** The throwaway worker container can't keep checkpoints on its own ephemeral filesystem, and using the *real Azure Blob client* against the **Azurite** emulator (already in `docker-compose.yml`) is what actually closes the storage seam: P7 exercises the same `azure_storage` data path P8 runs, so P8 is a pure credential/endpoint swap (shared-key/emulator → managed identity), not a first run of untested code. The Azure-Blob `CheckpointStore`/`TranscriptStore` (`libs/blob::{AzureBlobCheckpoints,AzureBlobTranscripts}`) are the **sole production stores** — `FsCheckpoints`/`FsTranscripts` were deleted; `InMemory*` remain only as hermetic unit-test doubles. The Blob round-trips are CI-tested against an Azurite service (gated on `FACTORY_BLOB_CONNECTION_STRING`, skip-if-unset like the `DATABASE_URL` lane). *(Decision revised from an earlier Fs-on-shared-volume sketch — Azurite gives real P8 fidelity and one production store.)*
- **Real checkpoint/restore (closes the P5 deploy seam):** `write_checkpoint` tars the worktree (+ the `claude` session state) → the CheckpointStore (Azure Blob); `restore_checkpoint` untars it → `workdir` before `claude --resume`. (P5 shipped these as faked seams; P7 makes the tar/untar real — see `libs/blob::{archive_dirs,extract_dirs}`.)
- **Scope: happy path + crash-resume** — not only issue→CR→handoff, but also **kill the worker container mid-run → the sweeper reclaims → the re-dispatch restores from the shared volume and `claude --resume`s the same session**.
- **Local only.** P7 is a local-dev capability (real GitHub App + real `claude` + Docker on the developer's machine), NOT a CI gate — CI can't have a real GitHub App webhook, a paid `claude`, or tolerate stateful per-run repo mutations. CI keeps the existing fixture-driven `e2e` suite + the tar/untar unit tests.

**Tasks** *(CI-testable parts are built + green; the live run is the operator's to execute)*
- **Real tar/untar** in `write_checkpoint`/`restore_checkpoint` via `libs/blob::{archive_dirs,extract_dirs}` (worktree + claude session); the archive round-trip is CI-unit-tested. *(Done.)*
- **Azure Blob stores** `libs/blob::{AzureBlobCheckpoints,AzureBlobTranscripts}` (the sole production stores; `Fs*` deleted); connection-string parsing unit-tested, Blob round-trips CI-tested against an Azurite service. Worker + Tools mains require `FACTORY_BLOB_CONNECTION_STRING`. *(Done.)*
- **`DockerLauncher` runtime wiring**: forwards the Blob connection string + `claude` setup-token to each worker and maps `host.docker.internal` so an off-network worker reaches Tools/Azurite. *(Done.)*
- **CI Azurite lane**: the integration job runs Azurite so the production Blob path is CI-tested, not first-run-in-prod. *(Done.)*
- **Worker `Dockerfile`** (worker binary + `git` + `claude` CLI; `--target worker`) + a `services` image target for the long-running services. *(Done — built locally.)*
- **Compose `live` profile / env** (`docker-compose.live.yml` + `.env.live.example`): services as containers on the base Postgres+Azurite stack; the dispatcher mounts the docker socket and launches the real worker image via `DockerLauncher`; real-credential env (`GITHUB_APP_ID`, `GITHUB_APP_PRIVATE_KEY`, `GITHUB_WEBHOOK_SECRET`, the `claude` setup-token, `FACTORY_*`). *(Done.)*
- **`scripts/smoke.sh`**: label an issue → **manager triage → coder CR → reviewer verdict**, then a crash-resume pass (kill the worker mid-run → sweeper reclaim → re-dispatch resumes). *(Done.)*
- **Runbook** `docs/runbooks/local-live-smoke.md`: GitHub App registration (perms issues/pull_requests/contents/checks read-write; events issues/pull_request/pull_request_review/issue_comment/push; webhook secret) + install on a throwaway repo (opt-in label `factory`, a `.factory/config.yml` with manager+coder+reviewer); `claude login` → `claude setup-token`; `gh webhook forward` → `localhost:8080/webhooks/github`; build images + compose up; run steps. *(Done.)*
- **Live run — operator-pending:** register the App, fill `.env.live`, run `scripts/smoke.sh` on a throwaway repo, and capture the CR + crash-resume result.

**Tests:** the **CI-testable parts are real tests** — the checkpoint tar/untar round-trip (`libs/blob::archive_dirs`/`extract_dirs`) and the Azure Blob `CheckpointStore`/`TranscriptStore` round-trips against an **Azurite** service (gated on `FACTORY_BLOB_CONNECTION_STRING`). The **live run + crash-resume drill** are a scripted **local** smoke (real credentials + the `claude` binary + Docker — not a CI gate); the artifact is the captured CR URL + reviewer verdict.
**Exit:** a labelled issue in the throwaway repo runs the full loop **locally, end-to-end** — real GitHub App, real `claude`, real Docker worker, shared-volume checkpoints — producing a real change request + merge-handoff, AND a worker killed mid-run resumes the same session from its checkpoint on the shared volume. **Depends on:** P4 (the full loop); ideally P4.5/P5/P6 for a production-honest run. It is the first real validation of the deploy seams P3+ built behind traits, so it is the natural predecessor to P8.

## P8 — Azure productionization

**Goal:** the same system, on Azure, in dev then prod.

**Tasks**
- **Bicep** (`infra/`): dev + prod resource groups — Postgres Flexible Server (Burstable, no HA), Blob (LRS, lifecycle = 30d), Container Apps env, ACR, Key Vault, Azure Monitor/Log Analytics. Per-service **user-assigned Managed Identity** + least-privilege roles.
- Worker runtime: switch dev's local Docker to **Azure Container Apps Jobs**; services → Container Apps services (N replicas).
- Secrets to **Key Vault**; OTel exporter → **Azure Monitor**; Blob lifecycle + Postgres retention jobs.
- **Anthropic** commercial API + **zero-data-retention** configured; key in Key Vault.
- CD: GitHub Actions → ACR → Container Apps; the single-runner migration step before rollout.

**Tests:** smoke-test the full loop in dev Azure; deploy/rollback (redeploy prior revision) drill; expand/contract migration applied with old+new replicas coexisting.
**Exit:** a labelled issue in a real connected repo runs end-to-end on Azure dev. **Depends on:** P7 (the seams are proven locally first, so a failure here is infra, not integration); P4 for the full loop; ideally P5–P6 for a production-honest deploy. (A thin P3-only slice can be deployed earlier to validate the Azure plumbing, but that is not the full-loop exit above.)

---

## Cross-cutting workstreams (every phase)

- **Telemetry:** each new component emits spans/metrics/logs with `correlation_id`; "an absent log is a defect."
- **Tests:** the **full E2E acceptance suite is authored up front in P0 as `#[ignore]`d stubs**; each later phase **activates (un-ignores) and makes green** the E2E tests its mechanism satisfies — a phase is not done while an E2E test it owns is still ignored. On top of that: unit per function + integration per slice (real Postgres/Azurite, Claude Code faked at the seam where deterministic). Every enum variant through every entry point.
- **Security:** Managed Identity + per-task tokens enforced from P3; no secrets in code; worker can't weaken its own checks (P4).
- **Docs:** keep the technical spec in sync if implementation reveals a needed change (PR updates the spec too).

## Sequencing rationale & risks

- **Why vertical-slice at P3:** the highest integration risks (GitHub App auth, headless Claude Code, the MCP boundary, job launch) are proven before we invest in coordinator/resilience/dashboard.
- **Why record book before everything (P1):** it's the contract every service shares; getting leasing/caps/idempotency right early avoids reworking all callers.
- **Top risks:** (1) driving Claude Code headless + session resume semantics — de-risk with a spike in P3; (2) Container Apps Jobs cold-start vs the "minutes" pickup target — measure in P8; (3) the no-HA Postgres SPOF — accepted, revisit if dev stability demands HA.
- **Parallelizable:** `dashboard/` (P6) can start after P1's schema stabilizes; `infra/` Bicep (P8) can be drafted alongside P3+.

## Definition of done (per phase)

A phase is done when: its exit criterion holds, its tests pass in CI (`fmt`+`clippy`+`test`), **every E2E acceptance stub the phase owns is un-ignored and green** (and none it doesn't own was weakened), telemetry covers its new paths, and the relevant requirements-traceability rows in the technical spec are backed by a real, tested mechanism. The count of still-`#[ignore]`d E2E tests is the project's burndown to done.
