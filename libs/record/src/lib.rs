//! The record book: the durable to-do queue, concurrency-safe leasing, capacity
//! enforcement, idempotent enqueue, lease lifecycle, and retention purge (P1).
//!
//! All shared state lives in Postgres. The two load-bearing guarantees:
//! - **No double-starts**: [`lease_next`] moves a row out of `queued` in one
//!   transaction guarded by `FOR UPDATE SKIP LOCKED`, so concurrent callers
//!   (the dispatcher replicas) never lease the same row twice.
//! - **Caps never exceeded**: the lease transaction takes a per-repo
//!   `pg_advisory_xact_lock`, so the per-worker and global counts are read and
//!   acted on atomically — two replicas cannot both pass the check and overshoot.
#![forbid(unsafe_code)]

use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;
use uuid::Uuid;

/// Connection pool handle used across Factory services.
pub type Pool = sqlx::postgres::PgPool;

/// Result type for record-book operations.
pub type Result<T> = std::result::Result<T, RecordError>;

/// Errors from the record book.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("unrecognized {kind} value from database: {value:?}")]
    UnknownEnum { kind: &'static str, value: String },
}

/// A worker role; the `kind` of a work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Manager,
    Coder,
    Reviewer,
    Tester,
}

impl Kind {
    /// The on-disk text representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Manager => "manager",
            Kind::Coder => "coder",
            Kind::Reviewer => "reviewer",
            Kind::Tester => "tester",
        }
    }
}

impl FromStr for Kind {
    type Err = RecordError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "manager" => Ok(Kind::Manager),
            "coder" => Ok(Kind::Coder),
            "reviewer" => Ok(Kind::Reviewer),
            "tester" => Ok(Kind::Tester),
            other => Err(RecordError::UnknownEnum {
                kind: "kind",
                value: other.to_owned(),
            }),
        }
    }
}

/// The lifecycle state of a work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Queued,
    Leased,
    Running,
    Blocked,
    Succeeded,
    Failed,
    Escalated,
}

impl State {
    /// The on-disk text representation.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Leased => "leased",
            State::Running => "running",
            State::Blocked => "blocked",
            State::Succeeded => "succeeded",
            State::Failed => "failed",
            State::Escalated => "escalated",
        }
    }
}

impl FromStr for State {
    type Err = RecordError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "queued" => Ok(State::Queued),
            "leased" => Ok(State::Leased),
            "running" => Ok(State::Running),
            "blocked" => Ok(State::Blocked),
            "succeeded" => Ok(State::Succeeded),
            "failed" => Ok(State::Failed),
            "escalated" => Ok(State::Escalated),
            other => Err(RecordError::UnknownEnum {
                kind: "state",
                value: other.to_owned(),
            }),
        }
    }
}

/// A new work item to enqueue.
#[derive(Debug, Clone)]
pub struct NewWorkItem {
    pub repo_id: Uuid,
    pub kind: Kind,
    pub worker_name: String,
    pub config_sha: String,
    /// One of the trigger keywords (validated by the DB CHECK constraint).
    pub trigger: String,
    pub subject_ref: String,
    pub correlation_id: Uuid,
    /// Pickup priority; higher leases first (default 0).
    pub priority: i32,
    /// Earliest time the item may be leased; `None` means now (immediate).
    pub available_at: Option<DateTime<Utc>>,
}

/// Outcome of [`enqueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// A new work item was created.
    Inserted(Uuid),
    /// An active item for the same (repo, subject, kind) already exists; no-op.
    DuplicateActive,
}

/// A leased work item, as handed to a dispatcher/worker.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub kind: Kind,
    pub worker_name: String,
    pub trigger: String,
    pub subject_ref: String,
    pub correlation_id: Uuid,
    pub state: State,
    pub attempt_count: i32,
    pub loop_count: i32,
    pub config_sha: String,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
}

/// Parameters for a single lease attempt.
#[derive(Debug, Clone)]
pub struct LeaseRequest {
    pub repo_id: Uuid,
    pub worker_name: String,
    /// Identifies the leasing replica/job (for heartbeat ownership).
    pub lease_owner: String,
    /// Lease duration in seconds.
    pub ttl_secs: f64,
    /// Max concurrent in-flight items for this worker_name.
    pub per_worker_cap: i64,
    /// Max concurrent in-flight items across the whole repo.
    pub global_cap: i64,
}

/// Open a connection pool.
pub async fn connect(database_url: &str) -> Result<Pool> {
    Ok(PgPoolOptions::new()
        .max_connections(16)
        .connect(database_url)
        .await?)
}

/// Apply all migrations (idempotent).
pub async fn migrate(pool: &Pool) -> Result<()> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}

/// Create (or fetch, by owner/name) a repo row; returns its id.
pub async fn upsert_repo(
    pool: &Pool,
    owner: &str,
    name: &str,
    installation_id: i64,
) -> Result<Uuid> {
    let row = sqlx::query(
        "INSERT INTO repos (id, installation_id, owner, name, status)
         VALUES ($1, $2, $3, $4, 'active')
         ON CONFLICT (owner, name)
           DO UPDATE SET installation_id = EXCLUDED.installation_id, updated_at = now()
         RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(installation_id)
    .bind(owner)
    .bind(name)
    .fetch_one(pool)
    .await?;
    Ok(row.try_get("id")?)
}

/// Enqueue a work item. Idempotent: if an active item for the same
/// (repo, subject, kind) already exists, returns [`EnqueueOutcome::DuplicateActive`].
pub async fn enqueue(pool: &Pool, item: &NewWorkItem) -> Result<EnqueueOutcome> {
    let row = sqlx::query(
        "INSERT INTO work_items
           (id, repo_id, kind, worker_name, config_sha, trigger, subject_ref, correlation_id,
            state, priority, available_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'queued', $9, coalesce($10, now()))
         ON CONFLICT (repo_id, subject_ref, kind)
           WHERE state IN ('queued', 'leased', 'running', 'blocked')
           DO NOTHING
         RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(item.repo_id)
    .bind(item.kind.as_str())
    .bind(&item.worker_name)
    .bind(&item.config_sha)
    .bind(&item.trigger)
    .bind(&item.subject_ref)
    .bind(item.correlation_id)
    .bind(item.priority)
    .bind(item.available_at)
    .fetch_optional(pool)
    .await?;

    let outcome = match row {
        Some(r) => EnqueueOutcome::Inserted(r.try_get("id")?),
        None => EnqueueOutcome::DuplicateActive,
    };
    tracing::info!(
        repo_id = %item.repo_id,
        kind = item.kind.as_str(),
        subject = %item.subject_ref,
        duplicate = matches!(outcome, EnqueueOutcome::DuplicateActive),
        "enqueue"
    );
    Ok(outcome)
}

/// Atomically lease the next ready item for `worker_name`, enforcing the
/// per-worker and global caps. Returns `None` if nothing is ready or a cap is
/// reached. Concurrency-safe across replicas (per-repo advisory lock + SKIP LOCKED).
pub async fn lease_next(pool: &Pool, req: &LeaseRequest) -> Result<Option<WorkItem>> {
    let mut tx = pool.begin().await?;

    // Serialize leasing within the repo so the cap counts below are atomic for
    // both the per-worker and the global ceiling.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('factory:lease'), hashtext($1::text))")
        .bind(req.repo_id)
        .execute(&mut *tx)
        .await?;

    let global: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM work_items WHERE repo_id = $1 AND state IN ('leased', 'running')",
    )
    .bind(req.repo_id)
    .fetch_one(&mut *tx)
    .await?;
    if global >= req.global_cap {
        tx.rollback().await?;
        tracing::info!(repo_id = %req.repo_id, worker = %req.worker_name, global, cap = req.global_cap, "lease blocked: global cap reached");
        return Ok(None);
    }

    let per_worker: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM work_items
         WHERE repo_id = $1 AND worker_name = $2 AND state IN ('leased', 'running')",
    )
    .bind(req.repo_id)
    .bind(&req.worker_name)
    .fetch_one(&mut *tx)
    .await?;
    if per_worker >= req.per_worker_cap {
        tx.rollback().await?;
        tracing::info!(repo_id = %req.repo_id, worker = %req.worker_name, per_worker, cap = req.per_worker_cap, "lease blocked: per-worker cap reached");
        return Ok(None);
    }

    let row = sqlx::query(
        "WITH next AS (
           SELECT id FROM work_items
           WHERE state = 'queued' AND available_at <= now()
             AND repo_id = $1 AND worker_name = $2
           ORDER BY priority DESC, available_at
           FOR UPDATE SKIP LOCKED
           LIMIT 1
         )
         UPDATE work_items w
            SET state = 'leased', lease_owner = $3,
                lease_expires_at = now() + make_interval(secs => $4), updated_at = now()
           FROM next WHERE w.id = next.id
         RETURNING w.id, w.repo_id, w.kind, w.worker_name, w.trigger, w.subject_ref,
                   w.correlation_id, w.state, w.attempt_count, w.loop_count, w.config_sha,
                   w.lease_owner, w.lease_expires_at",
    )
    .bind(req.repo_id)
    .bind(&req.worker_name)
    .bind(&req.lease_owner)
    .bind(req.ttl_secs)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await?;
    let leased = row.map(map_work_item).transpose()?;
    match &leased {
        Some(wi) => {
            tracing::info!(repo_id = %req.repo_id, worker = %req.worker_name, work_item = %wi.id, subject = %wi.subject_ref, "leased")
        }
        None => {
            tracing::info!(repo_id = %req.repo_id, worker = %req.worker_name, "lease: no ready work")
        }
    }
    Ok(leased)
}

/// Extend the lease on `id` if still held by `lease_owner`. Returns the new
/// expiry, or `None` if the item is no longer leased by that owner.
pub async fn heartbeat(
    pool: &Pool,
    id: Uuid,
    lease_owner: &str,
    ttl_secs: f64,
) -> Result<Option<DateTime<Utc>>> {
    let row = sqlx::query(
        "UPDATE work_items
            SET lease_expires_at = now() + make_interval(secs => $3), updated_at = now()
          WHERE id = $1 AND lease_owner = $2 AND state IN ('leased', 'running')
         RETURNING lease_expires_at",
    )
    .bind(id)
    .bind(lease_owner)
    .bind(ttl_secs)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some(r) => Some(r.try_get("lease_expires_at")?),
        None => None,
    })
}

/// Transition a work item to `state`. Returns whether a row was updated.
/// Terminal states (`succeeded`, `failed`, `escalated`) are final: a work item
/// already in one is never transitioned again (returns `false`), so a terminal
/// item can't be resurrected back into the active/cap-counted set.
pub async fn set_state(pool: &Pool, id: Uuid, state: State) -> Result<bool> {
    let row = sqlx::query(
        "UPDATE work_items SET state = $2, updated_at = now()
          WHERE id = $1 AND state NOT IN ('succeeded', 'failed', 'escalated')
         RETURNING id",
    )
    .bind(id)
    .bind(state.as_str())
    .fetch_optional(pool)
    .await?;
    let updated = row.is_some();
    tracing::info!(work_item = %id, to_state = state.as_str(), updated, "set_state");
    Ok(updated)
}

/// Reclaim work items whose lease has expired: return them to `queued`, clear
/// the lease, and count the lapsed attempt. Covers both `leased` (never started)
/// and `running` (started then the worker crashed/hung) — both hold a cap slot,
/// so both must be reclaimable. Returns the number reclaimed.
pub async fn reclaim_expired(pool: &Pool) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE work_items
            SET state = 'queued', lease_owner = NULL, lease_expires_at = NULL,
                attempt_count = attempt_count + 1, updated_at = now()
          WHERE state IN ('leased', 'running') AND lease_expires_at < now()",
    )
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    tracing::info!(reclaimed = n, "reclaim sweep");
    Ok(n)
}

/// Purge terminal work items (and their cascading children) older than the cutoff.
/// Returns the number of work items removed.
pub async fn purge_terminal(pool: &Pool, older_than: DateTime<Utc>) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM work_items
          WHERE state IN ('succeeded', 'failed', 'escalated') AND updated_at < $1",
    )
    .bind(older_than)
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    tracing::info!(purged = n, "purged terminal work items");
    Ok(n)
}

/// Append a row to the append-only `events` audit trail. Producers (intake,
/// dispatcher, manager, Tools — P2+) call this to record every decision, tool
/// call, and state transition. Returns the new event id.
pub async fn append_event(
    pool: &Pool,
    repo_id: Option<Uuid>,
    work_item_id: Option<Uuid>,
    correlation_id: Option<Uuid>,
    actor: &str,
    action: &str,
    detail: serde_json::Value,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO events (id, repo_id, work_item_id, correlation_id, actor, action, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(repo_id)
    .bind(work_item_id)
    .bind(correlation_id)
    .bind(actor)
    .bind(action)
    .bind(detail)
    .execute(pool)
    .await?;
    tracing::info!(actor, action, work_item = ?work_item_id, "event recorded");
    Ok(id)
}

/// Count audit events recorded for a work item (diagnostic/test helper).
pub async fn count_events_for_item(pool: &Pool, work_item_id: Uuid) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM events WHERE work_item_id = $1")
            .bind(work_item_id)
            .fetch_one(pool)
            .await?,
    )
}

/// Count audit events recorded for a repo (diagnostic/test helper).
pub async fn count_events_for_repo(pool: &Pool, repo_id: Uuid) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM events WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_one(pool)
            .await?,
    )
}

/// Count all work items belonging to a repo (diagnostic/test helper).
pub async fn count_for_repo(pool: &Pool, repo_id: Uuid) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM work_items WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_one(pool)
            .await?,
    )
}

/// A repo and its stored config snapshot (for intake).
#[derive(Debug, Clone)]
pub struct RepoConfig {
    pub id: Uuid,
    /// The last validated `.factory/config.yml` as jsonb, if any.
    pub config: Option<serde_json::Value>,
    pub config_sha: Option<String>,
}

/// Fetch an **active** repo and its stored config by owner/name. Suspended or
/// removed repos resolve to `None`, so their webhook events never become work
/// items (the connection is the authorization to act on a repo).
pub async fn repo_by_name(pool: &Pool, owner: &str, name: &str) -> Result<Option<RepoConfig>> {
    let row = sqlx::query(
        "SELECT id, config, config_sha FROM repos
          WHERE owner = $1 AND name = $2 AND status = 'active'",
    )
    .bind(owner)
    .bind(name)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some(r) => Some(RepoConfig {
            id: r.try_get("id")?,
            config: r.try_get("config")?,
            config_sha: r.try_get("config_sha")?,
        }),
        None => None,
    })
}

/// Store a validated config snapshot on a repo (config-apply on push).
pub async fn set_repo_config(
    pool: &Pool,
    repo_id: Uuid,
    config: &serde_json::Value,
    config_sha: &str,
) -> Result<()> {
    sqlx::query("UPDATE repos SET config = $2, config_sha = $3, updated_at = now() WHERE id = $1")
        .bind(repo_id)
        .bind(config)
        .bind(config_sha)
        .execute(pool)
        .await?;
    Ok(())
}

/// Whether a webhook delivery id has already been processed (redelivery dedupe
/// read; the write happens via [`mark_delivery`] only after work succeeds).
pub async fn delivery_seen(pool: &Pool, delivery_id: &str) -> Result<bool> {
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM webhook_deliveries WHERE delivery_id = $1")
            .bind(delivery_id)
            .fetch_one(pool)
            .await?;
    Ok(n > 0)
}

/// Mark a webhook delivery id as processed; returns `false` if it was already
/// seen (redelivery dedupe).
pub async fn mark_delivery(pool: &Pool, delivery_id: &str) -> Result<bool> {
    let row = sqlx::query(
        "INSERT INTO webhook_deliveries (delivery_id) VALUES ($1)
         ON CONFLICT (delivery_id) DO NOTHING RETURNING delivery_id",
    )
    .bind(delivery_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Purge webhook delivery dedupe rows older than the cutoff. The dedupe table
/// only needs to outlive GitHub's redelivery window; this keeps it bounded.
/// Returns the number of rows removed.
pub async fn purge_deliveries(pool: &Pool, older_than: DateTime<Utc>) -> Result<u64> {
    let result = sqlx::query("DELETE FROM webhook_deliveries WHERE received_at < $1")
        .bind(older_than)
        .execute(pool)
        .await?;
    let n = result.rows_affected();
    tracing::info!(purged = n, "purged webhook deliveries");
    Ok(n)
}

/// All active repos that have a stored config, as `(repo_id, config_sha, config)`.
/// The scheduler iterates these to find due timers. `config` is the raw jsonb;
/// the caller deserializes it (keeps the record layer free of the config schema).
pub async fn list_configured_repos(pool: &Pool) -> Result<Vec<(Uuid, String, serde_json::Value)>> {
    let rows = sqlx::query(
        "SELECT id, config_sha, config FROM repos
          WHERE status = 'active' AND config IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let id: Uuid = r.try_get("id")?;
        let sha: String = r
            .try_get::<Option<String>, _>("config_sha")?
            .unwrap_or_default();
        let cfg: serde_json::Value = r.try_get("config")?;
        out.push((id, sha, cfg));
    }
    Ok(out)
}

/// The last scheduled tick fired for `(repo, worker)`, if any. The scheduler uses
/// this to bound the window it scans for newly-due ticks.
pub async fn last_scheduled_fire(
    pool: &Pool,
    repo_id: Uuid,
    worker_name: &str,
) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar(
        "SELECT last_fired FROM schedule_state WHERE repo_id = $1 AND worker_name = $2",
    )
    .bind(repo_id)
    .bind(worker_name)
    .fetch_optional(pool)
    .await?)
}

/// Fire a due scheduled tick exactly once across replicas. Under a per-(repo,
/// worker) advisory lock it re-checks the stored `last_fired`: if `slot` was
/// already fired (another replica won the race, or a re-tick recomputed the same
/// slot), it's a no-op ([`EnqueueOutcome::DuplicateActive`]). Otherwise it
/// enqueues `item` (idempotently) and advances `last_fired` to `slot`, atomically.
pub async fn record_scheduled_fire(
    pool: &Pool,
    item: &NewWorkItem,
    slot: DateTime<Utc>,
) -> Result<EnqueueOutcome> {
    let mut tx = pool.begin().await?;

    // Serialize firing for this (repo, worker) so the last_fired check + advance
    // is atomic across replicas.
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtext('factory:schedule'), hashtext($1::text || ':' || $2))",
    )
    .bind(item.repo_id)
    .bind(&item.worker_name)
    .execute(&mut *tx)
    .await?;

    let last: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT last_fired FROM schedule_state WHERE repo_id = $1 AND worker_name = $2",
    )
    .bind(item.repo_id)
    .bind(&item.worker_name)
    .fetch_optional(&mut *tx)
    .await?;
    if matches!(last, Some(l) if l >= slot) {
        tx.rollback().await?;
        tracing::info!(repo_id = %item.repo_id, worker = %item.worker_name, %slot, "schedule already fired; skipping");
        return Ok(EnqueueOutcome::DuplicateActive);
    }

    let row = sqlx::query(
        "INSERT INTO work_items
           (id, repo_id, kind, worker_name, config_sha, trigger, subject_ref, correlation_id,
            state, priority, available_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'queued', $9, coalesce($10, now()))
         ON CONFLICT (repo_id, subject_ref, kind)
           WHERE state IN ('queued', 'leased', 'running', 'blocked')
           DO NOTHING
         RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(item.repo_id)
    .bind(item.kind.as_str())
    .bind(&item.worker_name)
    .bind(&item.config_sha)
    .bind(&item.trigger)
    .bind(&item.subject_ref)
    .bind(item.correlation_id)
    .bind(item.priority)
    .bind(item.available_at)
    .fetch_optional(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO schedule_state (repo_id, worker_name, last_fired) VALUES ($1, $2, $3)
         ON CONFLICT (repo_id, worker_name) DO UPDATE SET last_fired = EXCLUDED.last_fired",
    )
    .bind(item.repo_id)
    .bind(&item.worker_name)
    .bind(slot)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let outcome = match row {
        Some(r) => EnqueueOutcome::Inserted(r.try_get("id")?),
        None => EnqueueOutcome::DuplicateActive,
    };
    tracing::info!(
        repo_id = %item.repo_id,
        worker = %item.worker_name,
        %slot,
        fired = matches!(outcome, EnqueueOutcome::Inserted(_)),
        "schedule fire"
    );
    Ok(outcome)
}

/// Re-queue any `blocked` work items for a subject (the blocked → resume-on-reply
/// transition). Returns the number re-queued.
pub async fn requeue_blocked(pool: &Pool, repo_id: Uuid, subject_ref: &str) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE work_items SET state = 'queued', available_at = now(), updated_at = now()
          WHERE repo_id = $1 AND subject_ref = $2 AND state = 'blocked'",
    )
    .bind(repo_id)
    .bind(subject_ref)
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    tracing::info!(repo_id = %repo_id, subject = subject_ref, requeued = n, "requeue blocked on reply");
    Ok(n)
}

// ===========================================================================
// P3: the Tools/MCP channel — task tokens, attempts, idempotent change requests,
// and tool-write idempotency. Tools (not the worker) own these writes.
// ===========================================================================

/// SHA-256 hex of a raw per-task token. Only the hash is ever stored or compared,
/// so a leaked DB never exposes a usable token.
pub fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

/// A resolved task token: exactly what the bearer is authorized to act on. The
/// Tools service authorizes every call against this — there is no cross-task access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAuth {
    pub work_item_id: Uuid,
    pub repo_id: Uuid,
    pub role: Kind,
    pub attempt_id: Option<Uuid>,
}

/// The terminal (or running) status of a worker attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
    Blocked,
    Reclaimed,
}

impl AttemptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptStatus::Running => "running",
            AttemptStatus::Succeeded => "succeeded",
            AttemptStatus::Failed => "failed",
            AttemptStatus::Blocked => "blocked",
            AttemptStatus::Reclaimed => "reclaimed",
        }
    }
}

/// Mint a per-task token: store only its hash, scoped to one work item / repo /
/// role / attempt, with a TTL. The raw token is generated and held by the caller
/// (dispatcher) and injected into the job; it is never stored.
#[allow(clippy::too_many_arguments)]
pub async fn mint_task_token(
    pool: &Pool,
    token_hash: &str,
    work_item_id: Uuid,
    repo_id: Uuid,
    role: Kind,
    attempt_id: Option<Uuid>,
    ttl_secs: f64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO task_tokens
           (token_hash, work_item_id, repo_id, role, attempt_id, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))",
    )
    .bind(token_hash)
    .bind(work_item_id)
    .bind(repo_id)
    .bind(role.as_str())
    .bind(attempt_id)
    .bind(ttl_secs)
    .execute(pool)
    .await?;
    tracing::info!(%work_item_id, role = role.as_str(), "minted task token");
    Ok(())
}

/// Resolve a presented token *hash* to its task, only if the token is still valid
/// (not revoked, not expired). `None` means unauthorized.
pub async fn resolve_task_token(pool: &Pool, token_hash: &str) -> Result<Option<TaskAuth>> {
    let row = sqlx::query(
        "SELECT work_item_id, repo_id, role, attempt_id FROM task_tokens
          WHERE token_hash = $1 AND NOT revoked AND expires_at > now()",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some(r) => {
            let role: String = r.try_get("role")?;
            Some(TaskAuth {
                work_item_id: r.try_get("work_item_id")?,
                repo_id: r.try_get("repo_id")?,
                role: role.parse()?,
                attempt_id: r.try_get("attempt_id")?,
            })
        }
        None => None,
    })
}

/// Revoke every token for a work item (task ended or lease reclaimed). Returns the
/// number revoked.
pub async fn revoke_task_tokens(pool: &Pool, work_item_id: Uuid) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE task_tokens SET revoked = true WHERE work_item_id = $1 AND NOT revoked",
    )
    .bind(work_item_id)
    .execute(pool)
    .await?;
    let n = result.rows_affected();
    tracing::info!(%work_item_id, revoked = n, "revoked task tokens");
    Ok(n)
}

/// Open a new attempt row for a work item (one row per worker execution). Returns
/// its id.
pub async fn start_attempt(
    pool: &Pool,
    work_item_id: Uuid,
    job_execution_id: Option<&str>,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO attempts (id, work_item_id, job_execution_id, status)
         VALUES ($1, $2, $3, 'running')",
    )
    .bind(id)
    .bind(work_item_id)
    .bind(job_execution_id)
    .execute(pool)
    .await?;
    tracing::info!(%work_item_id, attempt = %id, "started attempt");
    Ok(id)
}

/// Set the terminal status + summary on a still-running attempt. Returns whether a
/// row was updated (a terminal attempt is never re-finished — idempotent).
pub async fn finish_attempt(
    pool: &Pool,
    attempt_id: Uuid,
    status: AttemptStatus,
    summary: Option<&str>,
) -> Result<bool> {
    let row = sqlx::query(
        "UPDATE attempts SET status = $2, summary = $3, ended_at = now()
          WHERE id = $1 AND status = 'running'
         RETURNING id",
    )
    .bind(attempt_id)
    .bind(status.as_str())
    .bind(summary)
    .fetch_optional(pool)
    .await?;
    let updated = row.is_some();
    tracing::info!(attempt = %attempt_id, status = status.as_str(), updated, "finish attempt");
    Ok(updated)
}

/// A change request tracked in the record book.
#[derive(Debug, Clone)]
pub struct ChangeRequest {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub number: Option<i32>,
    pub head_branch: String,
    pub linked_issue: Option<i32>,
    pub work_item_id: Option<Uuid>,
    pub factory_state: String,
}

/// Idempotent-by-work-item CR row. The first call for a work item creates the row
/// (number stays NULL until the worker opens it on GitHub); a re-dispatch returns
/// the SAME row, so the CR is never duplicated.
pub async fn upsert_change_request_for_item(
    pool: &Pool,
    repo_id: Uuid,
    work_item_id: Uuid,
    head_branch: &str,
    linked_issue: Option<i32>,
) -> Result<ChangeRequest> {
    let row = sqlx::query(
        "INSERT INTO change_requests (id, repo_id, work_item_id, head_branch, linked_issue)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (work_item_id) WHERE work_item_id IS NOT NULL
           DO UPDATE SET head_branch = EXCLUDED.head_branch, updated_at = now()
         RETURNING id, repo_id, number, head_branch, linked_issue, work_item_id, factory_state",
    )
    .bind(Uuid::now_v7())
    .bind(repo_id)
    .bind(work_item_id)
    .bind(head_branch)
    .bind(linked_issue)
    .fetch_one(pool)
    .await?;
    map_change_request(row)
}

/// Record the CR number once the worker has opened it on GitHub.
pub async fn set_change_request_number(pool: &Pool, cr_id: Uuid, number: i32) -> Result<()> {
    sqlx::query("UPDATE change_requests SET number = $2, factory_state = 'open', updated_at = now() WHERE id = $1")
        .bind(cr_id)
        .bind(number)
        .execute(pool)
        .await?;
    Ok(())
}

/// Fetch the CR row for a work item, if one exists.
pub async fn change_request_for_item(
    pool: &Pool,
    work_item_id: Uuid,
) -> Result<Option<ChangeRequest>> {
    let row = sqlx::query(
        "SELECT id, repo_id, number, head_branch, linked_issue, work_item_id, factory_state
           FROM change_requests WHERE work_item_id = $1",
    )
    .bind(work_item_id)
    .fetch_optional(pool)
    .await?;
    row.map(map_change_request).transpose()
}

/// Get-or-set a tool idempotency record. Returns the result stored by the FIRST
/// caller for `(work_item, scope, dedupe_key)` — so a re-run sees the original
/// external ref and updates the same object instead of creating a duplicate.
pub async fn idempotency_get_or_set(
    pool: &Pool,
    work_item_id: Uuid,
    scope: &str,
    dedupe_key: &str,
    result: &serde_json::Value,
) -> Result<serde_json::Value> {
    // First writer wins; losers read the stored value back.
    sqlx::query(
        "INSERT INTO tool_idempotency (work_item_id, scope, dedupe_key, result)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (work_item_id, scope, dedupe_key) DO NOTHING",
    )
    .bind(work_item_id)
    .bind(scope)
    .bind(dedupe_key)
    .bind(result)
    .execute(pool)
    .await?;
    let stored: serde_json::Value = sqlx::query_scalar(
        "SELECT result FROM tool_idempotency
          WHERE work_item_id = $1 AND scope = $2 AND dedupe_key = $3",
    )
    .bind(work_item_id)
    .bind(scope)
    .bind(dedupe_key)
    .fetch_one(pool)
    .await?;
    Ok(stored)
}

/// A repo's GitHub identity (for the Tools service to scope API calls).
#[derive(Debug, Clone)]
pub struct RepoIdent {
    pub id: Uuid,
    pub owner: String,
    pub name: String,
    pub installation_id: i64,
}

/// Fetch a repo's identity by id.
pub async fn repo_by_id(pool: &Pool, id: Uuid) -> Result<Option<RepoIdent>> {
    let row = sqlx::query("SELECT id, owner, name, installation_id FROM repos WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(match row {
        Some(r) => Some(RepoIdent {
            id: r.try_get("id")?,
            owner: r.try_get("owner")?,
            name: r.try_get("name")?,
            installation_id: r.try_get("installation_id")?,
        }),
        None => None,
    })
}

/// Fetch a single work item by id (e.g. for the Tools `get_task` call).
pub async fn work_item_by_id(pool: &Pool, id: Uuid) -> Result<Option<WorkItem>> {
    let row = sqlx::query(
        "SELECT id, repo_id, kind, worker_name, trigger, subject_ref, correlation_id, state,
                attempt_count, loop_count, config_sha, lease_owner, lease_expires_at
           FROM work_items WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(map_work_item).transpose()
}

/// Look up a previously stored tool idempotency result, if any.
pub async fn idempotency_get(
    pool: &Pool,
    work_item_id: Uuid,
    scope: &str,
    dedupe_key: &str,
) -> Result<Option<serde_json::Value>> {
    Ok(sqlx::query_scalar(
        "SELECT result FROM tool_idempotency
          WHERE work_item_id = $1 AND scope = $2 AND dedupe_key = $3",
    )
    .bind(work_item_id)
    .bind(scope)
    .bind(dedupe_key)
    .fetch_optional(pool)
    .await?)
}

/// Extend the lease on a work item by `ttl_secs`, authorized by the task token
/// (not the lease owner). Returns the new expiry, or `None` if the item is no
/// longer in-flight. This is the Tools `heartbeat` call.
pub async fn heartbeat_task(
    pool: &Pool,
    work_item_id: Uuid,
    ttl_secs: f64,
) -> Result<Option<DateTime<Utc>>> {
    let row = sqlx::query(
        "UPDATE work_items
            SET lease_expires_at = now() + make_interval(secs => $2), updated_at = now()
          WHERE id = $1 AND state IN ('leased', 'running')
         RETURNING lease_expires_at",
    )
    .bind(work_item_id)
    .bind(ttl_secs)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some(r) => Some(r.try_get("lease_expires_at")?),
        None => None,
    })
}

/// Record an escalation against a work item (the single human channel). Returns
/// the new escalation id.
pub async fn create_escalation(
    pool: &Pool,
    work_item_id: Uuid,
    reason: &str,
    context: &str,
    delivered_to: Option<&str>,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO escalations (id, work_item_id, reason, context, delivered_to)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(work_item_id)
    .bind(reason)
    .bind(context)
    .bind(delivered_to)
    .execute(pool)
    .await?;
    tracing::info!(%work_item_id, reason, "escalation recorded");
    Ok(id)
}

/// Count escalations recorded for a work item (test helper).
pub async fn count_escalations_for_item(pool: &Pool, work_item_id: Uuid) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM escalations WHERE work_item_id = $1")
            .bind(work_item_id)
            .fetch_one(pool)
            .await?,
    )
}

fn map_change_request(row: sqlx::postgres::PgRow) -> Result<ChangeRequest> {
    Ok(ChangeRequest {
        id: row.try_get("id")?,
        repo_id: row.try_get("repo_id")?,
        number: row.try_get("number")?,
        head_branch: row.try_get("head_branch")?,
        linked_issue: row.try_get("linked_issue")?,
        work_item_id: row.try_get("work_item_id")?,
        factory_state: row.try_get("factory_state")?,
    })
}

fn map_work_item(row: sqlx::postgres::PgRow) -> Result<WorkItem> {
    let kind: String = row.try_get("kind")?;
    let state: String = row.try_get("state")?;
    Ok(WorkItem {
        id: row.try_get("id")?,
        repo_id: row.try_get("repo_id")?,
        kind: kind.parse()?,
        worker_name: row.try_get("worker_name")?,
        trigger: row.try_get("trigger")?,
        subject_ref: row.try_get("subject_ref")?,
        correlation_id: row.try_get("correlation_id")?,
        state: state.parse()?,
        attempt_count: row.try_get("attempt_count")?,
        loop_count: row.try_get("loop_count")?,
        config_sha: row.try_get("config_sha")?,
        lease_owner: row.try_get("lease_owner")?,
        lease_expires_at: row.try_get("lease_expires_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrips_through_text() {
        for k in [Kind::Manager, Kind::Coder, Kind::Reviewer, Kind::Tester] {
            assert_eq!(k.as_str().parse::<Kind>().unwrap(), k);
        }
        assert!("nope".parse::<Kind>().is_err());
    }

    #[test]
    fn state_roundtrips_through_text() {
        for s in [
            State::Queued,
            State::Leased,
            State::Running,
            State::Blocked,
            State::Succeeded,
            State::Failed,
            State::Escalated,
        ] {
            assert_eq!(s.as_str().parse::<State>().unwrap(), s);
        }
        assert!("nope".parse::<State>().is_err());
    }
}
