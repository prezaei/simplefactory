# Factory — Technical Spec

*How Factory is built. This document makes the technology choices the [product spec](factory-product-spec.md) deliberately deferred. Every requirement in the product spec maps to a concrete mechanism here (see [Requirements traceability](#requirements-traceability)). Where the product spec is the contract, this is the implementation blueprint.*

Hosting target: **everything runs on Azure, using Azure-native services.** Factory itself is written in **Rust**; each worker drives **Claude Code** (the CLI coding assistant) rather than a bespoke agent loop.

## Technology decisions

| Concern | Decision | Why |
| --- | --- | --- |
| Implementation language | **Rust** (Cargo workspace: `services/`, `libs/`) | Single static binary, runs identically on laptop and cluster; matches repo conventions (clippy/fmt gates, no `unwrap` in prod). |
| Async runtime / errors | **tokio**, **anyhow** (apps) + **thiserror** (libs) | Repo standard. |
| Coding assistant (per worker) | **Claude Code CLI**, headless, driven as a subprocess | "Reuse, don't reinvent." Native session persistence + `--resume` maps onto the crash-resume requirement; tools via MCP. |
| Model access | **Anthropic API directly** over the internet; key in Key Vault | Anthropic is not first-party on Azure; the workroom already has open egress. Uses the **commercial API** (no training on inputs/outputs) with **zero-data-retention** enabled where available. Model provider sits behind Claude Code config, so it stays swappable. |
| Code host | **GitHub App** (per-repo install) · **octocrab** client | Webhooks for events; short-lived **installation tokens** for least-privilege repo access; Checks/PR APIs for gating and handoff. |
| State / queue / arbitration | **Azure Database for PostgreSQL — Flexible Server** · **sqlx** | One transactional store for the record book, the durable to-do queue (`FOR UPDATE SKIP LOCKED`), and pickup arbitration. Single dependency; same engine locally. |
| Worker runtime (sealed workroom) | **Azure Container Apps Jobs** (one-shot, per task) | Throwaway by construction; scale-to-zero; managed scaling for many concurrent workers without running a cluster. |
| Session/worktree checkpoints + transcripts | **Azure Blob Storage** (lifecycle rules = bounded retention) | Cheap, scalable, native expiry. Postgres holds only pointers + state. |
| Secrets / identity | **Azure Key Vault** + **Managed Identity**; workers get short-lived, task-scoped tokens | No secrets in config; mediated, least-privilege worker access. |
| Telemetry | **OpenTelemetry** → **Azure Monitor / Application Insights** (traces, metrics) + **Log Analytics / KQL** (logs) | Vendor-neutral instrumentation, Azure-native sink. Satisfies the "log every event/decision/error" mandate. |
| Build / deploy | **Azure Container Registry** + **GitHub Actions** | Code lives on GitHub; Actions runs the Rust gates and deploys to Azure. |
| Long-lived services | **Azure Container Apps** services, **N replicas** | Always-on with replica redundancy; concurrency-safe via Postgres arbitration. |
| Dashboard | **React/TypeScript SPA** + **Rust (Axum) API**, SSE/WebSocket for live view | Rich UI; eventually-consistent stream off the record book. |
| Factory Tools (worker → Factory) | **MCP server** (remote, HTTP/SSE) | Claude Code's native tool path; clean contract for read-ticket / open-CR / comment / review / write-transcript / escalate. |
| Worker trust boundary | Worker reaches Factory **only** via Tools/MCP with a per-task token; **no** direct Postgres/Blob | Tightest least-privilege; every worker effect is mediated and logged. |
| Webhook intake | Direct **HTTPS endpoint → Postgres** (signature-verified) | Keeps Postgres the single source of truth; no extra broker. |
| Timers | **In-app scheduler** enqueues due work into Postgres | Schedule lives with the repo config; one system everywhere. |
| Repo config file | **YAML** (`.factory/config.yml`) | Familiar (GitHub-Actions-like), comments, readable nesting. |
| Local parity | **docker-compose** + **Azurite** (Blob emulator) + local Postgres; workers as local Docker | Same images/binaries as prod, Azure deps emulated. |

## Architecture

```mermaid
flowchart LR
    subgraph GH["GitHub (per-repo App)"]
      WH["Webhooks"]
    end

    subgraph ACA["Azure Container Apps — services (N replicas)"]
      IN["Intake API<br/>(Axum, verifies sig)"]
      SCH["Scheduler<br/>(timers from config)"]
      DISP["Dispatcher<br/>(capacity caps · SKIP LOCKED)"]
      MGR["Coordinator<br/>(decides next step)"]
      TOOLS["Factory Tools<br/>(MCP server)"]
      API["Dashboard API<br/>(Axum + SSE)"]
      SWEEP["Lease sweeper<br/>(stuck-worker reclaim)"]
    end

    PG[("Azure DB for PostgreSQL<br/>record book · queue · leases")]
    BLOB[("Azure Blob<br/>transcripts · checkpoints")]
    KV[["Azure Key Vault<br/>+ Managed Identity"]]

    subgraph JOB["Azure Container Apps Job (throwaway workroom)"]
      W["Worker harness (Rust)"]
      CC["Claude Code (headless)"]
    end

    SPA["Dashboard SPA<br/>(React/TS)"]

    WH -->|HTTPS| IN
    IN --> PG
    SCH --> PG
    PG --> DISP
    DISP -->|start job + per-task token| JOB
    W --> CC
    CC <-->|model| ANTH(["Anthropic API"])
    W -->|tools / record book / transcript| TOOLS
    CC -->|MCP| TOOLS
    TOOLS --> PG
    TOOLS --> BLOB
    W -->|checkpoint session+worktree| BLOB
    PG -->|completions| MGR
    MGR -->|new work| PG
    SWEEP --> PG
    PG --> API
    API -->|SSE / WS| SPA
    ACA -. Managed Identity .- KV
    JOB -. scoped token .- KV
```

**Service boundaries.** All long-lived components run as Azure Container Apps services (multiple replicas). They are stateless; **all** shared state is in Postgres + Blob, so any replica can handle any request and a dying replica loses nothing. Workers run as Container Apps **Jobs** — one execution per task, destroyed on completion.

## Components

### Intake API
Axum service. Receives GitHub webhooks, verifies the HMAC signature, filters by the **opt-in label** (`factory`), and writes a work item into the Postgres to-do list inside one transaction. Returns 2xx fast; all downstream work is async off the queue. Idempotent on GitHub delivery ID (dedupes redelivered webhooks).

### Scheduler
Reads each onboarded repo's `.factory/config.yml` timer definitions and enqueues due work into Postgres. Uses a Postgres advisory lock so exactly one replica fires a given schedule tick (no duplicate nightly runs across replicas).

### Record book, queue & leases (Postgres)
Single source of truth. The to-do list is a table; the dispatcher leases rows with `SELECT … FOR UPDATE SKIP LOCKED`, which is the **no-double-starts arbitration** — it works correctly with many dispatcher replicas and after restarts, with no "exactly one coordinator" requirement. (See [Data model](#data-model).)

### Dispatcher
Polls the queue under capacity caps (global + per repo-and-worker-type, enforced as counts in SQL). For each leased task it starts a Container Apps Job execution, injecting a **per-task scoped token** and the task id. The lease has an expiry; the worker heartbeats via the Tools API to extend it.

### Coordinator
The **deterministic** orchestrator — **never** calls a model. Reacts to completions in the record book and decides the next step (assign a Coder, wake a Reviewer on a new CR, hand an approved+green CR to a human, close the loop, or escalate). It only decides and delegates — it enqueues new work items; it never writes code. Runs as replicas; per-issue decisions are serialized with a row lock on the issue.

> **"Manager" disambiguation.** The Coordinator is *not* the **Manager role**. The Manager is an **AI worker role** (see [Worker lifecycle by role](#worker-lifecycle-by-role)) that triages an issue via Claude; the Coordinator is the AI-free Rust service that reacts to that triage (and to coder/reviewer outcomes) and routes the next step. Earlier drafts called the Coordinator "Coordinator / Manager"; that alias is dropped to keep the two distinct. (The **product spec** draws the same distinction: a deterministic **Coordinator** that decides/delegates/closes, plus a **Manager** AI worker that triages a new issue.)

### Worker harness + Claude Code (Container Apps Job)
The throwaway workroom. The Rust harness:
1. Mints/receives a short-lived **GitHub installation token**, does a **fresh shallow clone** of the repo.
2. Launches **Claude Code headless** with the task prompt and the Factory **MCP** endpoint configured as its tool server.
3. **Continuously checkpoints** Claude Code's session state (session id / transcript) + the working-tree state to **Blob** (see [Crash-resume](#key-workflows)).
4. On success, the harness performs the **role's terminal action** via Factory Tools (see below), records the outcome, and exits; the job is destroyed.

The worker has **no** direct Postgres or Blob credentials — everything goes through Factory Tools (the one exception being model calls straight to Anthropic). Outbound internet is open (builds fetch packages); the trusted-tenant model is assumed per the product spec's non-goals.

#### Worker lifecycle by role
Every role runs Claude Code over the same MCP/Tools channel, but the harness's terminal action differs by `role` (only the **Coder** produces a change request):

| Role | Reads | Claude does | Terminal action (via Tools) | Produces a CR? |
| --- | --- | --- | --- | --- |
| **Manager** | the issue (`read_issue`) | triages: actionable vs not | actionable → `post_comment` + `record_outcome` **success** (only a *succeeded* Manager makes the Coordinator assign a Coder); non-actionable → `escalate` (terminal; no Coder) | **No** |
| **Coder** | the issue / CR | writes code on a fresh clone | push branch → `open_or_update_change_request` → `record_outcome` | **Yes** (the only role that does) |
| **Reviewer** | the CR (`read_change_request`) | reviews the diff + checks | `submit_review` verdict (approve / request changes) → `record_outcome` | **No** |

A Manager or Reviewer therefore never pushes a branch or opens a CR. (The Coder→CR lifecycle ships in P3; the Manager-triage and Reviewer-verdict lifecycles are **P4.5** in the implementation plan.) This table defines the **manager / coder / reviewer** lifecycles only; the **`tester`** role is reserved (no shipped lifecycle yet — consistent with the config note below), so it has no terminal-action contract here.

### Factory Tools (MCP server)
Axum-hosted MCP server exposing the worker's only channel to Factory: read the ticket/diff, open/update the change request, leave a comment, submit a review, read check status, append to the transcript, record outcome, heartbeat the lease, raise an escalation. A worker sees only its own task's context (no cross-task lookup). Authenticates the per-task token, enforces that a token may only act on its own task/repo, and writes every call to the record book + audit log. Tools (not the worker) own writes to Postgres/Blob. Full surface in [Factory Tools (MCP) surface](#factory-tools-mcp-surface).

### Lease sweeper
Periodically finds leases past expiry (crashed or hung workers — "stuck workers are caught"), marks the attempt failed, revokes the task token, and returns the work item for a fresh attempt (subject to the retry/loop caps).

### Dashboard
React/TS SPA over the Axum Dashboard API. Live view via SSE/WebSocket fed from the record book — an **eventually-consistent** projection (the "Fresh view" contract: within seconds, not to-the-millisecond). Read-only.

## Data model

Postgres, managed with `sqlx` migrations. All ids are **UUIDv7** (time-ordered, minted app-side). Enums are **text + CHECK** (cheap to evolve, map cleanly to Rust enums via sqlx).

```sql
CREATE TABLE repos (
    id               uuid PRIMARY KEY,
    installation_id  bigint NOT NULL,             -- GitHub App installation
    owner            text NOT NULL,
    name             text NOT NULL,
    config           jsonb,                       -- last VALID .factory/config.yml (parsed)
    config_sha       text,                        -- commit/blob sha of the applied config
    status           text NOT NULL DEFAULT 'active'
                       CHECK (status IN ('active','suspended','removed')),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner, name)
);

CREATE TABLE work_items (                          -- the durable to-do list / queue
    id               uuid PRIMARY KEY,
    repo_id          uuid NOT NULL REFERENCES repos(id),
    kind             text NOT NULL CHECK (kind IN ('manager','coder','reviewer','tester')),
    worker_name      text NOT NULL,               -- which configured worker
    config_sha       text NOT NULL,               -- repo config version this item was enqueued under (pinned)
    trigger          text NOT NULL CHECK (trigger IN
                       ('issue_labeled','cr_opened','cr_updated','cr_changes_requested',
                        'review_requested','issue_comment','schedule','manager')),
    subject_ref      text NOT NULL,               -- "issue#123" | "cr#456"
    correlation_id   uuid NOT NULL,               -- threads telemetry across the lifecycle
    state            text NOT NULL DEFAULT 'queued'
                       CHECK (state IN ('queued','leased','running','blocked',
                                        'succeeded','failed','escalated')),
    priority         int  NOT NULL DEFAULT 0,
    available_at     timestamptz NOT NULL DEFAULT now(),  -- delayed visibility / backoff
    lease_owner      text,                         -- dispatcher replica / job execution
    lease_expires_at timestamptz,
    attempt_count    int  NOT NULL DEFAULT 0,      -- failed-attempt retries
    loop_count       int  NOT NULL DEFAULT 0,      -- review-rework cycles
    payload          jsonb NOT NULL DEFAULT '{}',  -- trigger details
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

-- Idempotent enqueue: at most one ACTIVE item per (repo, subject, kind).
-- Terminal rows don't block a later re-run.
CREATE UNIQUE INDEX work_items_active_uniq ON work_items (repo_id, subject_ref, kind)
    WHERE state IN ('queued','leased','running','blocked');

-- SKIP LOCKED pickup: ready, unleased items, oldest-first.
CREATE INDEX work_items_pickup ON work_items (repo_id, worker_name, priority DESC, available_at)
    WHERE state = 'queued';

CREATE TABLE attempts (                            -- one row per worker execution
    id               uuid PRIMARY KEY,
    work_item_id     uuid NOT NULL REFERENCES work_items(id),
    job_execution_id text,                         -- Container Apps Job execution
    session_id       text,                         -- Claude Code session id for --resume
    checkpoint_uri   text,                         -- Blob: session + worktree snapshot
    transcript_uri   text,                         -- Blob: full transcript
    status           text NOT NULL DEFAULT 'running'
                       CHECK (status IN ('running','succeeded','failed','blocked','reclaimed')),
    summary          text,
    started_at       timestamptz NOT NULL DEFAULT now(),
    ended_at         timestamptz
);
CREATE INDEX attempts_by_item ON attempts (work_item_id);

CREATE TABLE issues (
    id            uuid PRIMARY KEY,
    repo_id       uuid NOT NULL REFERENCES repos(id),
    number        int  NOT NULL,
    factory_state text NOT NULL DEFAULT 'open'
                    CHECK (factory_state IN ('open','in_progress','blocked','done')),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, number)
);

CREATE TABLE change_requests (                     -- enables idempotent CR reuse
    id            uuid PRIMARY KEY,
    repo_id       uuid NOT NULL REFERENCES repos(id),
    number        int,                             -- null until opened
    head_branch   text NOT NULL,
    linked_issue  int,
    work_item_id  uuid REFERENCES work_items(id),  -- owning task → re-run updates the SAME CR
    factory_state text NOT NULL DEFAULT 'draft'
                    CHECK (factory_state IN ('draft','open','changes_requested',
                                             'approved','ready','merged','closed')),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, number)
);

CREATE TABLE escalations (
    id           uuid PRIMARY KEY,
    work_item_id uuid NOT NULL REFERENCES work_items(id),
    reason       text NOT NULL CHECK (reason IN
                   ('retries_exhausted','loop_cap','needs_decision','blocked_input','conflict')),
    context      text NOT NULL,
    delivered_to text,                             -- "issue#123 @alice"
    state        text NOT NULL DEFAULT 'open' CHECK (state IN ('open','resolved')),
    created_at   timestamptz NOT NULL DEFAULT now(),
    resolved_at  timestamptz
);

CREATE TABLE events (                              -- append-only audit trail
    id             uuid PRIMARY KEY,
    repo_id        uuid REFERENCES repos(id),
    work_item_id   uuid REFERENCES work_items(id),
    correlation_id uuid,
    actor          text NOT NULL,    -- 'intake'|'dispatcher'|'manager'|'worker:<name>'|'tool:<name>'|'human:<login>'
    action         text NOT NULL,    -- machine-readable verb
    detail         jsonb NOT NULL DEFAULT '{}',
    at             timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX events_by_item ON events (work_item_id, at);
CREATE INDEX events_by_corr ON events (correlation_id, at);

CREATE TABLE webhook_deliveries (                  -- dedupe redelivered webhooks
    delivery_id  text PRIMARY KEY,                 -- X-GitHub-Delivery
    received_at  timestamptz NOT NULL DEFAULT now()
);
```

**Atomic pickup (no double-starts).** The dispatcher leases in one statement; `SKIP LOCKED` lets many replicas pick concurrently without colliding:

```sql
WITH next AS (
  SELECT id FROM work_items
  WHERE state = 'queued' AND available_at <= now() AND repo_id = $1 AND worker_name = $2
  ORDER BY priority DESC, available_at
  FOR UPDATE SKIP LOCKED
  LIMIT 1
)
UPDATE work_items w
   SET state='leased', lease_owner=$3, lease_expires_at = now() + $4::interval, updated_at = now()
  FROM next WHERE w.id = next.id
RETURNING w.*;
```

**Capacity caps** are enforced inside the lease transaction, serialized per pool with a `pg_advisory_xact_lock` on `(repo_id, worker_name)` — the advisory-lock pattern the scheduler already uses — so two dispatcher replicas can't both pass the check and overshoot. A lease is granted only when **both** the per-worker count `count(state IN ('leased','running')) WHERE repo_id=$1 AND worker_name=$2 < that worker's max_concurrent` **and** the global count `count(...) WHERE repo_id=$1 < limits.max_concurrent_workers` hold. The global ceiling needs a coarser advisory key (or a `SERIALIZABLE` transaction), since a per-pool lock alone does not serialize the global count.

**Idempotency.** `change_requests.work_item_id` ties a CR to its task, so a re-run's `open_or_update_change_request` finds and updates the *same* CR; `post_comment`, `submit_review`, and `escalate` dedupe via a `dedupe_key` (one comment / review / escalation per task) so a lease-expiry re-dispatch never duplicates them; the branch push is idempotent too (`git push --force-with-lease` to the task's own branch). The partial unique index prevents duplicate live work items.

**Retention.** A scheduled purge job deletes terminal `work_items` (+ their `attempts`, `escalations`, `events`) older than the bounded period; Blob lifecycle rules expire the linked transcripts/checkpoints. No archive tables — purge in place.

## The `.factory/config.yml` schema

The per-repo team file. Version-controlled in the user's repo, re-read automatically on every push that touches it. With no file there are no workers and Factory does nothing on the repo.

```yaml
# .factory/config.yml — defines your Factory team.
version: 1

settings:
  label: factory                 # opt-in label; Factory acts only on work carrying it (default: factory)
  default_model: claude-opus-4-8 # model used unless a worker overrides it (optional)
  instructions: |                # repo-wide guidance appended to EVERY worker's prompt (optional)
    This is a Rust workspace — follow AGENTS.md. Prefer small, focused change requests.
  retry_cap: 3                   # a Failed work item with this many attempts (or more) escalates retries_exhausted instead of retrying (default: 3)
  loop_cap: 5                    # review-rework cycles before the loop escalates loop_cap instead of dispatching another coder (default: 5)
  gating_paths:                  # path prefixes whose changes weaken a CR's own gating checks; such a CR is escalated, never marked ready (default: [".github/workflows/"])
    - ".github/workflows/"
  escalation_assignee: octo-human # optional GitHub login assigned the linked issue on a ready-for-merge handoff / escalation; when unset, the comment is posted but NO assignment is made (default: unset/none)

limits:
  max_concurrent_workers: 5      # global ceiling across all workers in this repo (platform default: 5)

workers:
  - name: bug-fixer
    role: coder                  # manager | coder | reviewer | tester
    on:                          # declarative trigger list
      - manager                  # the Manager assigns this coder (manager-first; see below)
      - cr_changes_requested     # a reviewer asked for changes (the rework loop)
    max_concurrent: 3
    model: claude-opus-4-8       # optional per-worker model override
    instructions: |              # optional; appended to the role's default prompt
      Always add regression tests. Touch only the module named in the issue.

  - name: pr-reviewer
    role: reviewer
    on: [cr_opened, cr_updated]
    max_concurrent: 2
    instructions_file: .factory/prompts/reviewer.md   # alternative: a version-controlled prompt file

  - name: nightly-maintenance
    role: coder
    on:
      - schedule: "0 2 * * *"    # cron (UTC)
    max_concurrent: 1
    instructions_file: .factory/prompts/nightly.md

  - name: manager
    role: manager
    on: [issue_labeled, cr_opened, manager]
    max_concurrent: 1
```

**Worker model.** `workers:` is a named list; each entry binds a `name` to a `role` (`manager` | `coder` | `reviewer` | `tester`), its triggers, and its caps. The named-list form (vs fixed role blocks) allows multiple workers of the same role (e.g. a `bug-fixer` and a `refactorer`, both coders) and future roles without a schema change. (The `tester` role and the `review_requested` trigger are reserved for future workflows — no shipped workflow drives them yet.) **Manager-first:** only the Manager subscribes to `issue_labeled`; coders/reviewers/testers are woken by the Manager (`manager` trigger) or by their own CR/comment triggers, never by `issue_labeled` directly — so the single labelled-issue event yields exactly one Manager work item.

**Trigger vocabulary** (`on:`):

| Trigger | Fires when |
| --- | --- |
| `issue_labeled` | an issue gains the opt-in label — **wakes the Manager only**; coders/reviewers are dispatched by the Manager via `manager` (manager-first) |
| `cr_opened` | a change request is opened |
| `cr_updated` | new commits pushed to a tracked CR |
| `cr_changes_requested` | a reviewer requested changes (drives the rework loop) |
| `review_requested` | review explicitly requested on a CR |
| `issue_comment` | a human comments on / edits a tracked issue or CR — resumes a `blocked` item and carries human steering |
| `schedule: "<cron>"` | a timer fires (UTC); enqueued by the in-app scheduler |
| `manager` | the Manager dispatches this worker as a next step |

**Prompt overrides** (the key per-repo customization). Each worker's effective prompt is composed, in order:
1. the **role default** prompt (shipped with Factory),
2. `settings.instructions` (repo-wide, optional),
3. the worker's `instructions` (inline) **or** `instructions_file` (a path to a version-controlled markdown file in the repo).

Later layers append to earlier ones, so a repo can shape behavior without forking Factory. `instructions_file` lets teams maintain rich, reviewed prompts as normal repo files.

**Per-worker tunables:** `on`, `max_concurrent`, `model` (optional), `instructions`/`instructions_file` (optional).

**Validation & safety.** On each change Factory parses and validates the file before applying it: `version` recognized, every `role` valid, every `cron` parseable, caps positive integers, any `instructions_file` resolvable. **Invalid config is rejected atomically** — Factory keeps the last good config, leaves a comment describing the error, and changes nothing else (fail-safe). The opt-in `label` still gates entry regardless of worker config: no `workers:` arrangement can make Factory act on an issue that wasn't labelled. (The gate is on the **issue entry point** — `issue_labeled`. Downstream events on a change request a Factory worker opened — `cr_opened`, `cr_updated`, `cr_changes_requested`, `review_requested`, and `issue_comment` resume — descend from an already-opted-in issue and are not independently re-gated; a CR webhook doesn't carry the originating issue's label anyway.)

**Mid-flight changes.** Each work item pins the `config_sha` it was enqueued under. An in-flight attempt runs to completion under its **pinned** config (prompt, model, caps); only newly enqueued work uses the updated config. A worker whose definition is removed mid-flight finishes its current attempt and is not re-dispatched.

## Factory Tools (MCP) surface

The worker's only channel to Factory, exposed by the Tools service as a **remote MCP server over HTTP/SSE**. At job start the dispatcher injects the Tools endpoint URL and a **per-task token**; Claude Code is configured with that MCP server. The token identifies the task, repo, and role; the server resolves all three and authorizes every call against the task's own repo. **All tools are available to every role** — the role is recorded on each call (and the role's prompt steers correct use), but tools are not per-role-gated. Every call is written to the record book + `events` audit log.

| Tool | Purpose | Key args | Returns |
| --- | --- | --- | --- |
| `get_task` | Fetch this work item: role, instructions (composed prompt context), and subject context | — | role, subject ref, issue body or CR diff, linked refs |
| `read_issue` | Read an issue in this repo | `issue_number` | title, body, comments |
| `read_change_request` | Read a CR: metadata, diff, review threads | `cr_number` | diff, files, reviews, check summary |
| `open_or_update_change_request` | Create the CR for this task, or update it if it already exists (**idempotent by work item**) | `title`, `body`, `head_branch`, `base`, `linked_issue` | `cr_number`, url |
| `post_comment` | Comment on an issue/CR; with `dedupe_key`, updates the same comment on re-run | `target`, `body`, `dedupe_key?` | comment id |
| `submit_review` | Leave a review verdict on a CR (reviewer role); idempotent by `dedupe_key` | `cr_number`, `verdict` (approve\|request_changes\|comment), `body`, `dedupe_key?` | review id |
| `get_check_status` | Read required-check states for a ref (to know if CI is green) | `cr_number` or `sha` | per-check conclusion + overall |
| `append_transcript` | Append a chunk to this run's transcript in Blob | `chunk` | ok |
| `record_outcome` | Set the attempt's terminal state | `status` (succeeded\|failed\|blocked), `summary` | ok |
| `heartbeat` | Extend the task lease (liveness) | — | new lease expiry |
| `escalate` | Raise an escalation: posts a question as an issue/CR comment + assigns a human, marks the item blocked/escalated; idempotent by `dedupe_key` (one escalation per task) | `reason`, `context`, `dedupe_key?` | escalation id |

**Branch push vs CR creation.** The worker pushes its branch directly with the short-lived **git installation token**, via `git push --force-with-lease` to the task's own branch — idempotent, so a lease-expiry re-dispatch re-pushes without duplicating or diverging commits. The CR itself and all comments/reviews go through Factory Tools so they are recorded and **idempotent** — a re-run calls `open_or_update_change_request`, `post_comment`, `submit_review`, and `escalate` (each with `dedupe_key`) and updates the *same* objects rather than duplicating them.

**No cross-task lookup.** There is deliberately no "search prior runs / how was this done before?" tool — consistent with the product spec's removal of the "It learns" subsystem. A worker sees only its own task's context.

## Key workflows

**Issue → merged change (the worked example).** Webhook → Intake writes a Manager-role work item → Dispatcher leases it → the Manager worker triages; on its success the **Coordinator** assigns a Coder (enqueues a coder work item, comments via Tools) → Dispatcher starts a Container Apps Job → Worker clones, runs Claude Code (tests-first, then code), opens a CR via Tools, records success → the **Coordinator** (reacting to completion) + the new-CR webhook wake a Reviewer job → Reviewer verdict via Tools → on *changes requested*, the **Coordinator** re-enqueues a Coder against the same CR (counts toward the per-issue loop cap); on *approved* + required checks green, the **Coordinator** marks ready and hands to a human to merge → human merges → the **Coordinator** closes the issue.

**No-double-starts.** Every pickup is a single `UPDATE … WHERE id IN (SELECT … FOR UPDATE SKIP LOCKED)` transaction; the row's lease columns are set atomically. Two dispatcher replicas can never lease the same row.

**Crash-resume.** The worker harness streams Claude Code's session state + a worktree snapshot to Blob on a short interval and at each tool boundary. If the job dies (or the coordinator restarts), the work item's lease expires; the next dispatch restores the worktree from Blob and runs `claude --resume <session>` to continue the *same* session. Repeated resumes failing at the same point fall through, after a few, to a fresh attempt (idempotent on the same CR) or escalation — bounding deterministic crash loops. If checkpoint state is unrecoverable, it's a fresh attempt.

**Escalation.** When work exhausts retries / hits the loop cap / needs a human decision, the Coordinator (or a worker, for blocked-on-input) writes the question as an **issue/CR comment and assigns the issue** — the single human channel — and records an `escalations` row. Blocked-on-human time does not count against retry/loop caps.

**Blocked → resume on reply (also the human-steer path).** A `blocked: waiting on a human` item has freed its slot and sits in `state='blocked'`. When the human comments on or edits the issue/CR, the `issue_comment` webhook reaches Intake, which flips the matching item to `state='queued'` and sets `available_at = now()`; the next dispatch re-leases it and `claude --resume` continues the *same* session. The same path lets a human **steer** an item proactively (PROD "comment or edit … to steer it"). The dashboard stays read-only by design — steering happens in GitHub, not the dashboard.

## Requirements traceability

| Product-spec requirement | Mechanism |
| --- | --- |
| Nothing is lost; full restart resumes | Postgres durable record book + queue; stateless services. |
| Crash resumes the same session | Blob checkpoints + `claude --resume`; lease expiry → re-dispatch. |
| Safe to redo (idempotent) | `issues`/`change_requests` tracking; Tools update the same CR/comment. |
| Bounded effort / bounded loops | `attempt_count` + `loop_count` caps in `work_items`; escalate at cap. |
| Needs human input → ask, don't block | `blocked` state + `escalate` tool writes the question to issue/CR; freed lease; an `issue_comment` event flips `blocked→queued` and `claude --resume` continues the session. |
| Stuck workers caught | Lease expiry + sweeper; heartbeats extend leases. |
| Capped & scalable | Capacity counts in SQL (global + per repo/type); add Container Apps replicas/jobs. |
| No double-starts | `FOR UPDATE SKIP LOCKED` leasing; works across replicas. |
| Good work (tests-first, checks, review) | Worker prompt enforces tests-first; Checks API gate + Reviewer approval before human merge. |
| Records don't pile up | Blob lifecycle expiry + Postgres retention jobs. |
| Opt-in only | Intake filters on the `factory` label. |
| Least privilege | GitHub installation tokens (short-lived); Key Vault + Managed Identity; worker → Tools-only, per-task token; can't weaken its own checks. |
| Confidential by default | Factory's own records: Postgres/Blob access-controlled, retention-bounded. Code/prompts egressed to Anthropic use the **commercial API under no-train terms + zero-data-retention** — not used for training. |
| Transparent play-by-play | Transcript in Blob + `events` audit trail; live SSE dashboard. |
| Always on | Multi-replica Container Apps + Postgres arbitration; survives loss of any single **service** replica. **Postgres HA is deferred** (single instance, no zone-redundant HA) — the record book is a known SPOF; see [Infrastructure & sizing](#infrastructure--sizing-azure). |
| Reuse a capable coding assistant | Claude Code headless per worker. |
| Prompt pickup (start within minutes) | Synchronous webhook→enqueue + dispatcher polls the ready queue on a short interval (oldest-first); handoff-to-lease is seconds, not hours. |
| Config version-controlled, auto re-read, no redeploy | `.factory/config.yml` in the repo; Intake/Scheduler re-parse + validate on every push that touches it (atomic apply, last-good retained). |
| Human can step in / steer | Human comment or edit on the issue/CR arrives as an `issue_comment` webhook → Intake → resumes/steers the item (same path as a blocked-on-human reply). |

## Security & least privilege

- **Factory services** authenticate to Postgres/Blob/Key Vault via **Managed Identity** — no secrets in images or config.
- **Workers** receive only a **per-task token** (scoped to one task/repo) for the Tools/MCP endpoint and a **short-lived GitHub installation token** for the clone/PR. No DB/Blob creds. Tokens revoked when the task ends or its lease is reclaimed.
- **Anthropic key** lives in Key Vault; the worker reads the model endpoint config, the key is injected at job start and never written to the transcript (best-effort secret scrubbing, per the product spec's stated limits).
- **Model-provider data handling.** Code and prompts sent to Anthropic use the **commercial API**, whose terms do not train on inputs/outputs; **zero-data-retention** is enabled where available so the provider persists nothing. This is what backs the product spec's "never used to train models" for the egressed *code* (not just Factory's own records).
- A worker **may not modify the automated checks that gate its own work**; such edits are escalated to a human.
- Trust model is the product spec's: trusted tenant code, no prompt-injection hardening in v1.

## Telemetry

Every service and the worker harness emit **OpenTelemetry** traces/metrics/logs, exported to **Azure Monitor / Application Insights** (traces, metrics) and **Log Analytics** (structured logs, queried via KQL). A `correlation_id` per work item threads webhook → queue → dispatch → worker → tools → completion. Per the repo mandate: every decision, state transition, tool call, and error path is logged; an absent log is a defect.

## Deployment & local parity

- **Azure:** Container Apps environment hosting the services (N replicas) and worker Jobs; Azure DB for PostgreSQL Flexible Server; Blob Storage with lifecycle policies; Key Vault; ACR for images; Azure Monitor. Provisioned as **Bicep** (see [Infrastructure & sizing](#infrastructure--sizing-azure)).
- **CI/CD:** GitHub Actions runs `cargo fmt --check && cargo clippy -D warnings && cargo test`, builds images, pushes to ACR, deploys to Container Apps.
- **Local:** `docker-compose` brings up Postgres + **Azurite** (Blob emulator) + the Factory services; workers run as local Docker containers driving Claude Code. Same binaries/images as production — one codebase, not two.

## Infrastructure & sizing (Azure)

Provisioned as **Bicep** (Azure-native, no state backend), across two environments — **dev** and **prod** — as separate resource groups (ideally separate subscriptions), parameterized by environment. **Single region** (regional-outage survival is an explicit non-goal).

| Resource | dev | prod | Notes |
| --- | --- | --- | --- |
| Azure DB for PostgreSQL Flexible Server | Burstable (B1ms) | Burstable (B-series), **no HA for now** | Cost-first; automated backups on. **Tradeoff:** without zone-redundant HA the record book is a single point of failure — accepted for now. Enabling HA later is a config flip (`highAvailability: ZoneRedundant`), not a redesign. |
| Azure Blob Storage | Standard **LRS**, hot | Standard **LRS**, hot | Lifecycle rule expires transcripts/checkpoints at 30 days. |
| Azure Container Apps env | Consumption | Consumption | Services as apps (N replicas + scale rules); workers as Jobs (scale-to-zero). |
| Azure Container Registry | Basic | Standard | Images for the services + worker. |
| Azure Key Vault | Standard | Standard | Secrets; accessed via Managed Identity. |
| Azure Monitor / Log Analytics / App Insights | shared workspace | dedicated workspace | OTel export sink. |

**Identity & access:** each service runs with a **user-assigned Managed Identity** holding least-privilege roles (Postgres login, Blob Data Contributor on its own container, Key Vault Secrets User). **Networking:** services exposed over HTTPS; in prod, Postgres/Blob reached via private endpoints; in dev, public access + firewall rules for simplicity.

## Database migrations & rollback

- **Tool:** **sqlx migrations** — versioned SQL files in `migrations/`, committed to the repo, applied with `sqlx migrate run`. No second tool beyond our DB layer.
- **Application:** a **dedicated pipeline step** runs migrations exactly once (single runner) **before** the new service revision rolls out, so the N replicas never race and schema change is decoupled from app start.
- **Model — forward-only + expand/contract** (no down-migrations):
  1. **Expand** — additive only (new nullable column, new table, index built `CONCURRENTLY`); deployed first, so old *and* new app versions both work against it.
  2. **Backfill** — migrate/populate data in a later step if needed.
  3. **Contract** — drop the old column/constraint in a still-later release, once no running version references it.
- **Zero-downtime:** because every migration is backward-compatible, a rolling Container Apps deploy where old and new replicas briefly coexist is always safe.
- **Rollback:** redeploy the **previous app revision** — it still works against the expanded schema. The DB is *not* rolled back; a bad data migration is corrected with a new forward migration. Postgres point-in-time restore (automated backups) covers catastrophic corruption only.
- **CI gate:** migrations are applied against the integration Postgres in CI (per AGENTS.md) before merge; one that doesn't apply cleanly fails the build.

## Defaults (retention, capacity, effort, timeouts)

Starting values. Per-repo `.factory/config.yml` may override the **capacity caps**, the **retry/loop caps** (`settings.retry_cap` / `settings.loop_cap`), and the **gating paths** (`settings.gating_paths`); the rest are platform defaults. The values in the table below remain the platform defaults when a repo omits these fields.

| Setting | Default | Notes |
| --- | --- | --- |
| Retention — operational records (`work_items`, `attempts`, `events`, `escalations`) | **30 days** | Purged in place by the retention job. |
| Retention — Blob transcripts + checkpoints | **30 days** | Blob lifecycle expiry. |
| Global concurrency (`limits.max_concurrent_workers`) | **5** | Per repo; overridable. |
| Per-worker concurrency (`max_concurrent`) | **2** | Default when a worker omits it; overridable. |
| Failed-attempt retries (`settings.retry_cap`) | **3** | Then escalate `retries_exhausted`; per-repo overridable. |
| Per-issue review-rework loop cap (`settings.loop_cap`) | **5** | Coder↔Reviewer cycles; then escalate `loop_cap`; per-repo overridable. |
| Gating paths (`settings.gating_paths`) | **`[".github/workflows/"]`** | A CR touching one weakens its own gating checks → escalated, never marked ready; per-repo overridable. |
| Escalation/handoff assignee (`settings.escalation_assignee`) | **unset (none)** | GitHub login assigned the linked issue on a ready-for-merge handoff / escalation; when unset, the comment is posted but no assignment is made; per-repo overridable. |
| Deterministic-crash resumes | **3** | Before falling through to a fresh attempt / escalation. |
| Dispatcher poll interval | **5 s** | Ready-queue scan cadence (backs prompt-pickup-in-minutes). |
| Worker lease | **15 min** | Extended by heartbeat; sweeper reclaims past expiry. |
| Heartbeat interval | **60 s** | Via the Tools `heartbeat` call. |
| Per-attempt wall-clock cap | **30 min** | Attempt killed + reclaimed past this. |

## Workspace layout (Rust)

```
services/
  intake/         # webhook + scheduler (Axum)
  dispatcher/     # capacity caps + SKIP LOCKED leasing + job launch
  coordinator/    # Coordinator: decide-and-delegate, lease sweeper
  tools/          # Factory Tools MCP server (Axum)
  dashboard-api/  # Axum + SSE; serves the SPA
  worker/         # workroom harness: clone, drive Claude Code, checkpoint
libs/
  record/         # Postgres models + sqlx queries (queue, leases, idempotency)
  github/         # octocrab-based App auth, webhooks, Checks/PR
  blob/           # Azure Blob client (transcripts, checkpoints) + Azurite
  config/         # .factory/config.yml (YAML) parsing + validation
  telemetry/      # OpenTelemetry setup, correlation ids
dashboard/        # React/TS SPA
```
