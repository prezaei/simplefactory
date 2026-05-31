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
           (id, repo_id, kind, worker_name, config_sha, trigger, subject_ref, correlation_id, state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'queued')
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
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some(r) => EnqueueOutcome::Inserted(r.try_get("id")?),
        None => EnqueueOutcome::DuplicateActive,
    })
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
    row.map(map_work_item).transpose()
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
pub async fn set_state(pool: &Pool, id: Uuid, state: State) -> Result<bool> {
    let row = sqlx::query(
        "UPDATE work_items SET state = $2, updated_at = now() WHERE id = $1 RETURNING id",
    )
    .bind(id)
    .bind(state.as_str())
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Reclaim leases whose expiry has passed: return them to `queued`, clear the
/// lease, and count the lapsed attempt. Returns the number reclaimed.
pub async fn reclaim_expired(pool: &Pool) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE work_items
            SET state = 'queued', lease_owner = NULL, lease_expires_at = NULL,
                attempt_count = attempt_count + 1, updated_at = now()
          WHERE state = 'leased' AND lease_expires_at < now()",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
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
    Ok(result.rows_affected())
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
