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
    /// When the row last changed. For a TERMINAL item this is when it became terminal
    /// (`record_outcome`/`set_state` stamp `updated_at = now()` on the transition) and
    /// it is NOT touched by the coordinator's defer path (a defer does not claim the
    /// item, so nothing writes the row). It is therefore a stable "review completed at"
    /// anchor the coordinator's bounded-defer grace window is measured from (FIX 4).
    pub updated_at: DateTime<Utc>,
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
///
/// Executor-generic (see [`mark_reacted`]) so the coordinator can enqueue the next
/// step inside its per-react transaction; existing `&pool` callers are unaffected.
pub async fn enqueue(
    executor: impl sqlx::PgExecutor<'_>,
    item: &NewWorkItem,
) -> Result<EnqueueOutcome> {
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
    .fetch_optional(executor)
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
                   w.lease_owner, w.lease_expires_at, w.updated_at",
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

/// Find work items the lease sweeper should reclaim: those `leased`/`running`
/// whose lease has expired OR whose running attempt has exceeded the absolute
/// wall-clock cap `wall_clock_cap_secs` (a stuck-but-alive worker that keeps
/// heart-beating extends its lease forever, so the cap is the backstop). Both
/// `leased` (never started) and `running` (started then crashed/hung) hold a cap
/// slot, so both are reclaimable. Returns the matching work items (the per-item
/// reclaim — finish attempt, revoke tokens, requeue-or-escalate — is the
/// coordinator's sweeper, which re-validates each one atomically via
/// [`reclaim_claim`] so this read need not be in a transaction).
pub async fn fetch_expired_lease_items(
    pool: &Pool,
    wall_clock_cap_secs: f64,
) -> Result<Vec<WorkItem>> {
    let rows = sqlx::query(
        "SELECT w.id, w.repo_id, w.kind, w.worker_name, w.trigger, w.subject_ref,
                w.correlation_id, w.state, w.attempt_count, w.loop_count, w.config_sha,
                w.lease_owner, w.lease_expires_at, w.updated_at
           FROM work_items w
          -- EXPIRED-LEASE PREDICATE: KEEP IN SYNC with reclaim_claim, which re-validates
          -- this exact condition atomically when claiming. A divergence would let this
          -- read surface an item reclaim_claim then refuses (or vice versa).
          WHERE w.state IN ('leased', 'running')
            AND (
              w.lease_expires_at < now()
              OR EXISTS (
                SELECT 1 FROM attempts a
                 WHERE a.work_item_id = w.id AND a.status = 'running'
                   AND a.started_at < now() - make_interval(secs => $1)
              )
            )
          ORDER BY w.lease_expires_at",
    )
    .bind(wall_clock_cap_secs)
    .fetch_all(pool)
    .await?;
    let items: Vec<WorkItem> = rows.into_iter().map(map_work_item).collect::<Result<_>>()?;
    tracing::info!(
        expired = items.len(),
        wall_clock_cap_secs,
        "fetched expired-lease items for sweep"
    );
    Ok(items)
}

/// Atomically claim an expired-lease item for reclaim, moving it to `to_state`
/// (`Queued` to requeue, `Escalated` to terminalize), bumping `attempt_count`, and
/// clearing the lease. Returns `true` only for the caller that wins.
///
/// Exactly-once + correctness come from RE-CHECKING the same expired predicate
/// [`fetch_expired_lease_items`] used (lease past expiry OR the running attempt past the
/// wall-clock cap), atomically inside the claiming `UPDATE`, NOT from an expiry snapshot:
/// - A heartbeat that *extended* the lease is irrelevant to the wall-clock branch — a
///   stuck-but-alive worker keeps heart-beating, so a guard keyed on the (mutating)
///   `lease_expires_at` value would let it dodge reclaim forever. Re-checking the
///   predicate reclaims it regardless of heartbeats (the case the wall-clock cap exists for).
/// - A concurrent sweeper / dispatcher re-lease is excluded: a re-leased item has a fresh
///   future `lease_expires_at` and a brand-new running attempt, so it satisfies neither
///   branch; and the `state IN ('leased','running')` guard (a terminal item is never
///   resurrected — see [`set_state`]) plus the row lock taken by the `UPDATE` serialize
///   racing reclaims so only the first flips the state and the rest no-op (`false`).
///
/// Executor-generic so the sweeper binds it, the attempt-finish, and the token-revoke
/// into one transaction.
pub async fn reclaim_claim(
    executor: impl sqlx::PgExecutor<'_>,
    work_item_id: Uuid,
    wall_clock_cap_secs: f64,
    to_state: State,
) -> Result<bool> {
    let row = sqlx::query(
        "UPDATE work_items
            SET state = $3,
                attempt_count = attempt_count + 1,
                lease_owner = NULL,
                lease_expires_at = NULL,
                available_at = CASE WHEN $3 = 'queued' THEN now() ELSE available_at END,
                reacted = CASE WHEN $3 = 'queued' THEN false ELSE reacted END,
                updated_at = now()
          WHERE id = $1
            AND state IN ('leased', 'running')
            -- EXPIRED-LEASE PREDICATE: KEEP IN SYNC with fetch_expired_lease_items.
            AND (
              lease_expires_at < now()
              OR EXISTS (
                SELECT 1 FROM attempts a
                 WHERE a.work_item_id = $1 AND a.status = 'running'
                   AND a.started_at < now() - make_interval(secs => $2)
              )
            )
         RETURNING id",
    )
    .bind(work_item_id)
    .bind(wall_clock_cap_secs)
    .bind(to_state.as_str())
    .fetch_optional(executor)
    .await?;
    let claimed = row.is_some();
    tracing::info!(work_item = %work_item_id, to_state = to_state.as_str(), claimed, "reclaim claim");
    Ok(claimed)
}

/// Mark a work item's still-`running` attempt as `reclaimed` (the lease lapsed
/// before it finished). Returns the number of attempt rows updated (0 or 1).
/// Executor-generic so it runs inside the sweeper's reclaim transaction.
pub async fn reclaim_running_attempt(
    executor: impl sqlx::PgExecutor<'_>,
    work_item_id: Uuid,
) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE attempts SET status = 'reclaimed', ended_at = now()
          WHERE work_item_id = $1 AND status = 'running'",
    )
    .bind(work_item_id)
    .execute(executor)
    .await?;
    let n = result.rows_affected();
    tracing::info!(%work_item_id, reclaimed_attempts = n, "reclaimed running attempt");
    Ok(n)
}

/// Purge terminal work items (and their cascading children) older than the cutoff.
/// Returns the number of work items removed.
///
/// A terminal work item that still *owns* a non-terminal change_request is spared:
/// `change_requests.work_item_id` is `ON DELETE SET NULL`, so purging such an item
/// would null the live CR's owner link and strand it (e.g. a coder succeeds, but the
/// CR sits in review past the retention window). We only purge once the CR it owns has
/// itself reached a terminal state (`merged`/`closed`) — or it never owned one.
pub async fn purge_terminal(pool: &Pool, older_than: DateTime<Utc>) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM work_items w
          WHERE w.state IN ('succeeded', 'failed', 'escalated') AND w.updated_at < $1
            AND NOT EXISTS (
              SELECT 1 FROM change_requests cr
               WHERE cr.work_item_id = w.id
                 AND cr.factory_state NOT IN ('merged', 'closed')
            )",
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
pub async fn revoke_task_tokens(
    executor: impl sqlx::PgExecutor<'_>,
    work_item_id: Uuid,
) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE task_tokens SET revoked = true WHERE work_item_id = $1 AND NOT revoked",
    )
    .bind(work_item_id)
    .execute(executor)
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

/// A resumable checkpoint recovered for a work item: the Claude session to resume
/// and the Blob URI of its worktree snapshot, plus the resume budget consumed so far
/// (see [`last_checkpoint_for_item`]).
#[derive(Debug, Clone)]
pub struct CheckpointInfo {
    pub session_id: String,
    pub checkpoint_uri: String,
    /// Number of attempts started AFTER the latest checkpointed attempt — i.e. how many
    /// times this session has been re-dispatched (resumed) since it last made progress
    /// (checkpointed), INCLUDING resumes that crashed before checkpointing. The dispatcher
    /// bounds resumes against this (`resume_count >= cap` → start a fresh attempt instead);
    /// a fresh checkpoint resets the budget. See [`last_checkpoint_for_item`] for why this
    /// counts re-dispatches, not attempts-that-checkpointed.
    pub resume_count: u64,
}

/// Persist the Claude session id + worktree checkpoint URI onto an attempt (the
/// worker reports these via the Tools `save_checkpoint` call as the run progresses).
/// Last write wins (a later checkpoint supersedes an earlier one). Returns whether a
/// row was updated.
pub async fn save_attempt_checkpoint(
    pool: &Pool,
    attempt_id: Uuid,
    session_id: &str,
    checkpoint_uri: &str,
) -> Result<bool> {
    let row = sqlx::query(
        "UPDATE attempts SET session_id = $2, checkpoint_uri = $3 WHERE id = $1 RETURNING id",
    )
    .bind(attempt_id)
    .bind(session_id)
    .bind(checkpoint_uri)
    .fetch_optional(pool)
    .await?;
    let updated = row.is_some();
    tracing::info!(attempt = %attempt_id, session_id, updated, "saved attempt checkpoint");
    Ok(updated)
}

/// The most recent resumable checkpoint for a work item (its latest attempt that
/// recorded both a `session_id` and a `checkpoint_uri`), with the resume budget. `None`
/// when no attempt has checkpointed — the dispatcher then launches a fresh attempt.
///
/// `resume_count` is the number of attempts started AFTER that latest checkpointed
/// attempt — i.e. how many times this session has been re-dispatched (resumed) since it
/// last made progress (checkpointed). It must count *re-dispatches*, NOT
/// attempts-that-checkpointed: a resumed attempt that crashes before its first checkpoint
/// records no `session_id`, so counting only checkpointed attempts would let a
/// crash-before-checkpoint loop resume the same snapshot forever, never reaching the cap.
/// Counting attempts-after means a fresh checkpoint resets the budget (progress is being
/// made) while a stuck loop exhausts it — matching "repeated resumes failing at the same
/// point fall through after a few" (technical spec, Crash-resume).
pub async fn last_checkpoint_for_item(
    pool: &Pool,
    work_item_id: Uuid,
) -> Result<Option<CheckpointInfo>> {
    let row = sqlx::query(
        "SELECT a.session_id, a.checkpoint_uri,
                (SELECT count(*) FROM attempts a2
                  WHERE a2.work_item_id = $1 AND a2.started_at > a.started_at) AS resume_count
           FROM attempts a
          WHERE a.work_item_id = $1
            AND a.session_id IS NOT NULL
            AND a.checkpoint_uri IS NOT NULL
          ORDER BY a.started_at DESC, a.id DESC
          LIMIT 1",
    )
    .bind(work_item_id)
    .fetch_optional(pool)
    .await?;
    let info = match row {
        Some(r) => {
            let resume_count: i64 = r.try_get("resume_count")?;
            let info = CheckpointInfo {
                session_id: r.try_get("session_id")?,
                checkpoint_uri: r.try_get("checkpoint_uri")?,
                resume_count: resume_count.max(0) as u64,
            };
            tracing::info!(%work_item_id, session_id = %info.session_id, resume_count = info.resume_count, "found resumable checkpoint");
            Some(info)
        }
        None => {
            tracing::info!(%work_item_id, "no resumable checkpoint");
            None
        }
    };
    Ok(info)
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
    /// The bounded review-rework counter (advanced by [`increment_cr_loop_count`]).
    /// Loaded with the row so a pre-tx decision can predict whether the NEXT
    /// changes-requested cycle would hit the loop cap without a second read.
    pub loop_count: i32,
}

/// Idempotent-by-head-branch CR row. The CR identity is `(repo_id, head_branch)`
/// (one CR per branch, deterministic per issue: `factory/issue-N`), NOT the work
/// item — so the rework loop reuses the same CR row across coder work items. The
/// first call for a branch creates the row (number stays NULL until the worker opens
/// it on GitHub); a re-dispatch OR a later REWORK coder pushing the same branch
/// re-points `work_item_id` to the CURRENT owning coder (preserving the CR number),
/// so the CR is never duplicated and never collides on `UNIQUE(repo_id, number)`.
/// `linked_issue` is preserved if the new caller does not supply one.
///
/// INVARIANT (see also [`change_request_for_item`]): `work_item_id` on a CR row is "the
/// most recent coder to own the branch", re-pointed on every rework push — it is NOT a
/// stable link to the coder that first opened the CR. This is safe because a succeeded
/// coder is always reacted (in-tx, waking its reviewer) BEFORE any rework coder for the
/// same issue is enqueued, so the coordinator's by-`work_item_id` CR lookup for a
/// succeeded coder is never stranded by a later rework coder re-pointing the row.
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
         ON CONFLICT (repo_id, head_branch)
           DO UPDATE SET
             -- A late/retried coder tool call on the same deterministic branch must NOT
             -- drag a CR that already advanced to a terminal-ish state ('ready', 'merged',
             -- 'closed') back by stealing its `work_item_id` or bumping `updated_at`. For
             -- those states the SET is a no-op (row still RETURNED so the contract holds);
             -- non-terminal rows are re-pointed/refreshed as before (the rework loop).
             work_item_id = CASE WHEN change_requests.factory_state IN ('ready', 'merged', 'closed')
                                 THEN change_requests.work_item_id ELSE EXCLUDED.work_item_id END,
             linked_issue = CASE WHEN change_requests.factory_state IN ('ready', 'merged', 'closed')
                                 THEN change_requests.linked_issue
                                 ELSE COALESCE(EXCLUDED.linked_issue, change_requests.linked_issue) END,
             updated_at   = CASE WHEN change_requests.factory_state IN ('ready', 'merged', 'closed')
                                 THEN change_requests.updated_at ELSE now() END
         RETURNING id, repo_id, number, head_branch, linked_issue, work_item_id, factory_state, loop_count",
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
///
/// Setting the number never DOWNGRADES a terminal-ish CR ('ready', 'merged', 'closed')
/// back to 'open': a late/retried coder tool call on the same deterministic branch could
/// otherwise drag a CR that already advanced (e.g. to 'ready' after review approval) back
/// to 'open'. Terminal-ish states are preserved; only non-terminal CRs move to 'open'.
pub async fn set_change_request_number(pool: &Pool, cr_id: Uuid, number: i32) -> Result<()> {
    sqlx::query(
        "UPDATE change_requests
            SET number = $2,
                factory_state = CASE WHEN factory_state IN ('ready', 'merged', 'closed')
                                     THEN factory_state ELSE 'open' END,
                updated_at = now()
          WHERE id = $1",
    )
    .bind(cr_id)
    .bind(number)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch the CR row for a work item, if one exists.
///
/// INVARIANT: a CR row's `work_item_id` is "the most recent coder to own the branch"
/// (the upsert re-points it on every rework push — see [`upsert_change_request_for_item`]),
/// NOT a stable 1:1 link to one coder. The coordinator only ever looks a CR up by
/// `work_item_id` for a SUCCEEDED coder, and a succeeded coder is always reacted (in the
/// same tx that wakes the reviewer) BEFORE any later rework coder for the same issue is
/// enqueued — so the succeeded coder's CR lookup can never be stranded by a subsequent
/// rework coder having re-pointed the row.
pub async fn change_request_for_item(
    pool: &Pool,
    work_item_id: Uuid,
) -> Result<Option<ChangeRequest>> {
    let row = sqlx::query(
        "SELECT id, repo_id, number, head_branch, linked_issue, work_item_id, factory_state, loop_count
           FROM change_requests WHERE work_item_id = $1",
    )
    .bind(work_item_id)
    .fetch_optional(pool)
    .await?;
    row.map(map_change_request).transpose()
}

/// The outcome of trying to claim a tool-idempotency slot for
/// `(work_item, scope, dedupe_key)`.
///
/// This is the claim-first ("intent") pattern: a caller atomically inserts a
/// placeholder row BEFORE performing its (non-idempotent) external side effect.
/// Only the caller that wins the insert ([`IdempotencyClaim::Won`]) is allowed to
/// run the side effect; it then records the result with [`idempotency_complete`].
/// Concurrent or retried callers observe either [`IdempotencyClaim::Pending`] (the
/// winner's side effect is still in flight — they must back off and retry) or
/// [`IdempotencyClaim::Done`] (the result is already recorded — return it without
/// any side effect). This guarantees the side effect runs at most once even under
/// concurrent retries, where the old get→side-effect→store sequence could not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyClaim {
    /// This caller inserted the placeholder and now owns the side effect.
    Won,
    /// Another caller holds the claim and its side effect has not completed yet.
    Pending,
    /// The side effect already completed; this is the recorded result.
    Done(serde_json::Value),
}

/// How long a claim placeholder may sit unfinished before it is considered
/// abandoned and may be re-claimed. Matches the 15-minute lease/attempt window: a
/// claim older than this can only belong to a process that crashed (or whose side
/// effect hung past its own lease), so re-claiming it restores liveness without
/// risking a second side effect from a still-live winner.
const CLAIM_STALE_SECS: f64 = 900.0;

/// Claim a tool-idempotency slot, the first step of the claim-first pattern.
///
/// Atomically inserts a placeholder row (`result = 'null'::jsonb`) for
/// `(work_item, scope, dedupe_key)`:
/// - if the insert wins, the caller owns the side effect → [`IdempotencyClaim::Won`];
/// - otherwise the existing row is read back: a non-null result is the recorded
///   outcome → [`IdempotencyClaim::Done`]; a JSON-null result means a placeholder is
///   still present. A *fresh* placeholder means the winner's side effect is still in
///   flight → [`IdempotencyClaim::Pending`]; a *stale* one (older than
///   [`CLAIM_STALE_SECS`]) belongs to a winner that died before completing or
///   releasing it, so it is atomically re-claimed → [`IdempotencyClaim::Won`]. This
///   stale-reclaim is what restores liveness: without it a crashed winner would
///   wedge every retry as `Pending` forever.
pub async fn idempotency_claim(
    pool: &Pool,
    work_item_id: Uuid,
    scope: &str,
    dedupe_key: &str,
) -> Result<IdempotencyClaim> {
    // Try to win the claim by inserting a placeholder. A returned row means we won.
    let won = sqlx::query_scalar::<_, i32>(
        "INSERT INTO tool_idempotency (work_item_id, scope, dedupe_key, result)
         VALUES ($1, $2, $3, 'null'::jsonb)
         ON CONFLICT (work_item_id, scope, dedupe_key) DO NOTHING
         RETURNING 1",
    )
    .bind(work_item_id)
    .bind(scope)
    .bind(dedupe_key)
    .fetch_optional(pool)
    .await?;

    if won.is_some() {
        tracing::info!(%work_item_id, scope, dedupe_key, "idempotency: claim won");
        return Ok(IdempotencyClaim::Won);
    }

    // Someone else holds the claim; read the stored result back.
    let stored: serde_json::Value = sqlx::query_scalar(
        "SELECT result FROM tool_idempotency
          WHERE work_item_id = $1 AND scope = $2 AND dedupe_key = $3",
    )
    .bind(work_item_id)
    .bind(scope)
    .bind(dedupe_key)
    .fetch_one(pool)
    .await?;

    if !stored.is_null() {
        tracing::info!(%work_item_id, scope, dedupe_key, "idempotency: claim already done");
        return Ok(IdempotencyClaim::Done(stored));
    }

    // A placeholder is present. If it is stale (the winner crashed before completing
    // or releasing it), atomically re-claim it by stamping `created_at = now()`. The
    // `created_at < now() - interval` guard in the UPDATE makes the reclaim a no-op
    // for any caller that loses the race, so exactly one re-claimer wins.
    let reclaimed = sqlx::query_scalar::<_, i32>(
        "UPDATE tool_idempotency SET created_at = now()
          WHERE work_item_id = $1 AND scope = $2 AND dedupe_key = $3
            AND result = 'null'::jsonb
            AND created_at < now() - make_interval(secs => $4)
         RETURNING 1",
    )
    .bind(work_item_id)
    .bind(scope)
    .bind(dedupe_key)
    .bind(CLAIM_STALE_SECS)
    .fetch_optional(pool)
    .await?;

    if reclaimed.is_some() {
        tracing::info!(%work_item_id, scope, dedupe_key, "idempotency: re-claimed abandoned placeholder");
        Ok(IdempotencyClaim::Won)
    } else {
        // Either the placeholder is still fresh (winner's side effect in flight), or
        // another caller just re-claimed the stale one. Both mean: back off and retry.
        tracing::info!(%work_item_id, scope, dedupe_key, "idempotency: claim pending (side effect in flight)");
        Ok(IdempotencyClaim::Pending)
    }
}

/// Release a claim previously won via [`idempotency_claim`] when the side effect
/// FAILED before [`idempotency_complete`] could record its result. Deletes the row
/// ONLY while it is still the placeholder (`result = 'null'::jsonb`), so a completed
/// result is never destroyed. After release the next retry re-claims immediately
/// rather than waiting out the stale window.
pub async fn idempotency_release(
    pool: &Pool,
    work_item_id: Uuid,
    scope: &str,
    dedupe_key: &str,
) -> Result<()> {
    let result = sqlx::query(
        "DELETE FROM tool_idempotency
          WHERE work_item_id = $1 AND scope = $2 AND dedupe_key = $3
            AND result = 'null'::jsonb",
    )
    .bind(work_item_id)
    .bind(scope)
    .bind(dedupe_key)
    .execute(pool)
    .await?;
    tracing::info!(%work_item_id, scope, dedupe_key, released = result.rows_affected(), "idempotency: released placeholder");
    Ok(())
}

/// Record the side-effect result for a slot previously won via [`idempotency_claim`],
/// the second step of the claim-first pattern. Replaces the placeholder JSON-null
/// with the real result so later callers observe [`IdempotencyClaim::Done`].
pub async fn idempotency_complete(
    pool: &Pool,
    work_item_id: Uuid,
    scope: &str,
    dedupe_key: &str,
    result: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "UPDATE tool_idempotency SET result = $4
          WHERE (work_item_id, scope, dedupe_key) = ($1, $2, $3)",
    )
    .bind(work_item_id)
    .bind(scope)
    .bind(dedupe_key)
    .bind(result)
    .execute(pool)
    .await?;
    tracing::info!(%work_item_id, scope, dedupe_key, "idempotency: completed");
    Ok(())
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

/// Fetch a repo's identity AND its stored config snapshot in a single SELECT. The
/// Tools `get_task` call needs both (config to compose the prompt, identity to make
/// GitHub calls) and uses this to avoid loading the same row twice.
pub async fn repo_by_id_with_config(
    pool: &Pool,
    id: Uuid,
) -> Result<Option<(RepoIdent, Option<serde_json::Value>)>> {
    let row =
        sqlx::query("SELECT id, owner, name, installation_id, config FROM repos WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(match row {
        Some(r) => Some((
            RepoIdent {
                id: r.try_get("id")?,
                owner: r.try_get("owner")?,
                name: r.try_get("name")?,
                installation_id: r.try_get("installation_id")?,
            },
            r.try_get::<Option<serde_json::Value>, _>("config")?,
        )),
        None => None,
    })
}

/// Fetch a single work item by id (e.g. for the Tools `get_task` call).
pub async fn work_item_by_id(pool: &Pool, id: Uuid) -> Result<Option<WorkItem>> {
    let row = sqlx::query(
        "SELECT id, repo_id, kind, worker_name, trigger, subject_ref, correlation_id, state,
                attempt_count, loop_count, config_sha, lease_owner, lease_expires_at, updated_at
           FROM work_items WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(map_work_item).transpose()
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

/// Count escalations recorded for a work item (test helper).
pub async fn count_escalations_for_item(pool: &Pool, work_item_id: Uuid) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM escalations WHERE work_item_id = $1")
            .bind(work_item_id)
            .fetch_one(pool)
            .await?,
    )
}

// ===========================================================================
// P4: the Coordinator (Manager) loop — react-once, issue/CR state, rework loop,
// exactly-once escalation.
// ===========================================================================

/// Terminal work items the coordinator has not yet reacted to (oldest first).
pub async fn list_unreacted_terminal(pool: &Pool) -> Result<Vec<WorkItem>> {
    let rows = sqlx::query(
        "SELECT id, repo_id, kind, worker_name, trigger, subject_ref, correlation_id, state,
                attempt_count, loop_count, config_sha, lease_owner, lease_expires_at, updated_at
           FROM work_items
          WHERE state IN ('succeeded', 'failed', 'escalated') AND NOT reacted
          ORDER BY updated_at",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(map_work_item).collect()
}

/// Claim a terminal work item for coordination: set `reacted = true` if it wasn't
/// already. Returns `true` for the single caller that wins (exactly-once react).
///
/// Executor-generic so the coordinator can run this as the claim INSIDE its
/// per-react transaction (binding the claim and the decision writes atomically),
/// while existing callers passing `&pool` still work (`&Pool: PgExecutor`).
pub async fn mark_reacted(
    executor: impl sqlx::PgExecutor<'_>,
    work_item_id: Uuid,
    expected_updated_at: DateTime<Utc>,
) -> Result<bool> {
    // Do NOT touch `updated_at`: it records when the item became TERMINAL, and both
    // `purge_terminal` retention and the coordinator's `WorkItem.updated_at` defer-grace
    // anchor rely on that meaning. Bumping it on the react claim would make an old
    // terminal row look newly terminal (retained longer; stale grace window).
    //
    // The claim is tied to the EXACT terminal occurrence via `updated_at`: a requeue
    // (`requeue_for_retry`) revives the row and bumps `updated_at`, so a stale snapshot
    // listed at the old terminal time can no longer re-claim it even though `reacted`
    // was reset to false. This closes the double-react race where two replicas list the
    // same Failed row at T0, one requeues it (reacted=false again), and the other would
    // otherwise re-run the failed decision on its stale snapshot.
    let row = sqlx::query(
        "UPDATE work_items SET reacted = true
          WHERE id = $1 AND NOT reacted AND updated_at = $2
         RETURNING id",
    )
    .bind(work_item_id)
    .bind(expected_updated_at)
    .fetch_optional(executor)
    .await?;
    Ok(row.is_some())
}

/// Take the per-issue serialization lock for the lifetime of `tx`. Two coordinator
/// replicas reacting to terminal items for the SAME issue serialize here, so the
/// per-issue decision (enqueue next / set issue+CR state / escalate) is applied by
/// one at a time — satisfying the spec's "per-issue decisions serialized with a row
/// lock on the issue". The lock is a `pg_advisory_xact_lock` keyed on the
/// `(repo_id, issue_number)` pair, released automatically when `tx` commits or rolls
/// back.
pub async fn issue_advisory_lock(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo_id: Uuid,
    issue_number: i64,
) -> Result<()> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtext('factory:issue'), hashtext($1::text || ':' || $2::text))",
    )
    .bind(repo_id)
    .bind(issue_number)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Set an issue's factory state (`open` | `in_progress` | `blocked` | `done`).
/// Executor-generic (see [`mark_reacted`]) so it can run inside the coordinator's
/// per-react transaction.
pub async fn set_issue_state(
    executor: impl sqlx::PgExecutor<'_>,
    repo_id: Uuid,
    number: i32,
    state: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO issues (id, repo_id, number, factory_state) VALUES ($1, $2, $3, $4)
         ON CONFLICT (repo_id, number) DO UPDATE SET factory_state = EXCLUDED.factory_state, updated_at = now()",
    )
    .bind(Uuid::now_v7())
    .bind(repo_id)
    .bind(number)
    .bind(state)
    .execute(executor)
    .await?;
    tracing::info!(%repo_id, issue = number, state, "set issue state");
    Ok(())
}

/// Set a change request's factory state. Executor-generic (see [`mark_reacted`]).
pub async fn set_change_request_state(
    executor: impl sqlx::PgExecutor<'_>,
    cr_id: Uuid,
    factory_state: &str,
) -> Result<()> {
    sqlx::query("UPDATE change_requests SET factory_state = $2, updated_at = now() WHERE id = $1")
        .bind(cr_id)
        .bind(factory_state)
        .execute(executor)
        .await?;
    tracing::info!(cr = %cr_id, factory_state, "set CR state");
    Ok(())
}

/// Conditionally transition a CR's factory state ONLY when it is currently `from`,
/// returning whether the transition happened. This is the exactly-once idempotency
/// guard for the approve→ready handoff: the `WHERE factory_state = from` makes the
/// UPDATE succeed for exactly one caller even if two reacts (or a stale reviewer
/// race) try to advance the same CR. Executor-generic (see [`mark_reacted`]) so it
/// runs inside the coordinator's per-react transaction.
pub async fn set_change_request_state_if(
    executor: impl sqlx::PgExecutor<'_>,
    cr_id: Uuid,
    from: &str,
    to: &str,
) -> Result<bool> {
    let row = sqlx::query(
        "UPDATE change_requests SET factory_state = $3, updated_at = now()
          WHERE id = $1 AND factory_state = $2
         RETURNING id",
    )
    .bind(cr_id)
    .bind(from)
    .bind(to)
    .fetch_optional(executor)
    .await?;
    let did_transition = row.is_some();
    tracing::info!(cr = %cr_id, from, to, did_transition, "set CR state (conditional)");
    Ok(did_transition)
}

/// Conditionally set a CR's factory state ONLY when it is not in a terminal-ish
/// state (`ready`/`merged`/`closed`), returning whether the write happened. This
/// closes the TOCTOU in the review-verdict path: a late or duplicate verdict must
/// never drag a CR the coordinator has already advanced (e.g. `approved → ready`)
/// back to `approved`/`changes_requested`. A single atomic UPDATE guarded by
/// `factory_state NOT IN (...)` replaces a read-then-write, so a racing coordinator
/// transition in the window between the read and the write can no longer be
/// clobbered. Executor-generic (see [`mark_reacted`]).
pub async fn set_change_request_state_if_not_terminal(
    executor: impl sqlx::PgExecutor<'_>,
    cr_id: Uuid,
    to: &str,
) -> Result<bool> {
    let row = sqlx::query(
        "UPDATE change_requests SET factory_state = $2, updated_at = now()
          WHERE id = $1 AND factory_state NOT IN ('ready', 'merged', 'closed')
         RETURNING id",
    )
    .bind(cr_id)
    .bind(to)
    .fetch_optional(executor)
    .await?;
    let wrote = row.is_some();
    tracing::info!(cr = %cr_id, to, wrote, "set CR state (if not terminal)");
    Ok(wrote)
}

/// Re-queue a work item for a fresh retry attempt: transition it back to `queued`,
/// make it immediately available, bump `attempt_count`, clear any lease, and reset
/// `reacted` so a SUBSEQUENT terminal occurrence of this same row is reacted to
/// again (the coordinator already reacted to THIS terminal occurrence; the retry is
/// a new active row-state). Executor-generic (see [`mark_reacted`]) so the
/// coordinator runs it INSIDE its per-react transaction.
pub async fn requeue_for_retry(
    executor: impl sqlx::PgExecutor<'_>,
    work_item_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "UPDATE work_items
            SET state = 'queued', available_at = now(),
                attempt_count = attempt_count + 1,
                lease_owner = NULL, lease_expires_at = NULL,
                reacted = false, updated_at = now()
          WHERE id = $1",
    )
    .bind(work_item_id)
    .execute(executor)
    .await?;
    tracing::info!(work_item = %work_item_id, "requeued for retry");
    Ok(())
}

/// Fetch a change request by its GitHub number.
pub async fn change_request_by_number(
    pool: &Pool,
    repo_id: Uuid,
    number: i32,
) -> Result<Option<ChangeRequest>> {
    let row = sqlx::query(
        "SELECT id, repo_id, number, head_branch, linked_issue, work_item_id, factory_state, loop_count
           FROM change_requests WHERE repo_id = $1 AND number = $2",
    )
    .bind(repo_id)
    .bind(number)
    .fetch_optional(pool)
    .await?;
    row.map(map_change_request).transpose()
}

/// Increment a CR's review-rework loop count; returns the new value.
/// Executor-generic (see [`mark_reacted`]).
pub async fn increment_cr_loop_count(
    executor: impl sqlx::PgExecutor<'_>,
    cr_id: Uuid,
) -> Result<i32> {
    let n: i32 = sqlx::query_scalar(
        "UPDATE change_requests SET loop_count = loop_count + 1, updated_at = now()
          WHERE id = $1 RETURNING loop_count",
    )
    .bind(cr_id)
    .fetch_one(executor)
    .await?;
    Ok(n)
}

/// A CR's current loop count (test/diagnostic helper).
pub async fn cr_loop_count(pool: &Pool, cr_id: Uuid) -> Result<i32> {
    Ok(
        sqlx::query_scalar("SELECT loop_count FROM change_requests WHERE id = $1")
            .bind(cr_id)
            .fetch_one(pool)
            .await?,
    )
}

/// Record an escalation **exactly once** per `(work_item, reason)`. Returns the
/// escalation id and whether this call created it (`true`) or it already existed
/// (`false`) — so a re-dispatch / racing replica never double-escalates.
///
/// A single `INSERT … ON CONFLICT DO UPDATE … RETURNING` does both the claim and the
/// existing-row read: `xmax = 0` is true only for a freshly-inserted row (no prior
/// tuple version), so `(xmax = 0) AS created` distinguishes an insert from a
/// conflict-update without a second statement. Because it is one statement it is
/// `impl PgExecutor` like its siblings; the coordinator runs it INSIDE its per-react
/// transaction via `&mut **tx`.
///
/// On conflict the existing row is left **unchanged** — the `DO UPDATE` is a no-op
/// self-assign (`work_item_id = excluded.work_item_id`) chosen only so the row is
/// returned (a `DO NOTHING` would return no row). The original `context` is preserved,
/// matching the already-delivered human-facing escalation; the second call is a pure
/// no-op success (`created = false`).
pub async fn create_escalation_idempotent(
    executor: impl sqlx::PgExecutor<'_>,
    work_item_id: Uuid,
    reason: &str,
    context: &str,
    delivered_to: Option<&str>,
) -> Result<(Uuid, bool)> {
    let row = sqlx::query(
        "INSERT INTO escalations (id, work_item_id, reason, context, delivered_to)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (work_item_id, reason) DO UPDATE SET work_item_id = excluded.work_item_id
         RETURNING id, (xmax = 0) AS created",
    )
    .bind(Uuid::now_v7())
    .bind(work_item_id)
    .bind(reason)
    .bind(context)
    .bind(delivered_to)
    .fetch_one(executor)
    .await?;
    let id: Uuid = row.try_get("id")?;
    let created: bool = row.try_get("created")?;
    tracing::info!(%work_item_id, reason, created, "escalation recorded (idempotent)");
    Ok((id, created))
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
        loop_count: row.try_get("loop_count")?,
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
        updated_at: row.try_get("updated_at")?,
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
