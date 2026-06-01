//! Dashboard API (P6): a read-only Axum service that projects the record book
//! (Postgres) for humans, with an eventually-consistent SSE live view (the
//! "Fresh view" contract from the technical spec: state is reflected within
//! seconds, not to-the-millisecond).
//!
//! # Read-only by construction
//! Every route is a `GET` (or an SSE stream, which is a `GET`). There is no
//! handler that writes the database, and the router registers no `POST`/`PUT`/
//! `DELETE`/`PATCH` method, so a mutating request to any path is rejected by Axum
//! with `405 Method Not Allowed` (or `404` for an unknown path). The dashboard is
//! a projection; steering happens in GitHub, not here (technical spec: "The
//! dashboard stays read-only by design").
//!
//! # Query isolation
//! All read queries live in THIS crate. We reuse `factory_record` only for its
//! `connect`/`migrate`/`Pool` type and enums; we add no new functions to it. This
//! keeps the dashboard's projections self-contained.
//!
//! # SPA deviation (intentional)
//! The implementation plan names a React/TS SPA for the dashboard front-end. To
//! keep P6 entirely inside the Rust cargo-gated workspace (no node/npm build step),
//! `GET /` serves a SMALL build-free single-file HTML + vanilla-JS page embedded as
//! a Rust `&str`. It connects to the SSE stream and renders the live view, giving
//! the same product outcome (a read-only live view) without a separate toolchain.
#![forbid(unsafe_code)]

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Json;
use axum::Router;
use chrono::{DateTime, Utc};
use factory_record::Pool;
use futures::Stream;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

/// Default cap on rows returned by the work-items projection.
const WORK_ITEMS_LIMIT: i64 = 200;
/// Default cap on rows returned by the events feed; also the hard ceiling for the
/// caller-supplied `?limit=`.
const EVENTS_DEFAULT_LIMIT: i64 = 100;
const EVENTS_MAX_LIMIT: i64 = 1000;
/// How often the SSE stream pushes a fresh snapshot. Short enough that a state
/// change is reflected "within seconds" (the Fresh-view budget); configurable via
/// `DASHBOARD_SSE_INTERVAL_MS`.
const DEFAULT_SSE_INTERVAL: Duration = Duration::from_millis(1000);

/// Shared, read-only service state: just the Postgres pool.
#[derive(Clone)]
pub struct DashboardState {
    pub pool: Pool,
    /// Snapshot push cadence for the SSE stream.
    pub sse_interval: Duration,
}

impl DashboardState {
    /// Construct state with the default SSE cadence.
    pub fn new(pool: Pool) -> Self {
        Self {
            pool,
            sse_interval: DEFAULT_SSE_INTERVAL,
        }
    }
}

/// A single work item, projected for the dashboard.
#[derive(Debug, Serialize)]
pub struct WorkItemView {
    pub id: Uuid,
    pub kind: String,
    pub worker_name: String,
    pub state: String,
    pub subject_ref: String,
    pub attempt_count: i32,
    pub loop_count: i32,
    pub updated_at: DateTime<Utc>,
}

/// One `(state, count)` bucket in the summary.
#[derive(Debug, Serialize)]
pub struct StateCount {
    pub state: String,
    pub count: i64,
}

/// Counts of work items by state plus the open-escalation count for a repo.
#[derive(Debug, Serialize)]
pub struct SummaryView {
    pub owner: String,
    pub name: String,
    pub by_state: Vec<StateCount>,
    pub total: i64,
    pub open_escalations: i64,
}

/// One audit event, projected for the dashboard.
#[derive(Debug, Serialize)]
pub struct EventView {
    pub actor: String,
    pub action: String,
    pub detail: serde_json::Value,
    pub at: DateTime<Utc>,
}

/// The combined snapshot pushed over SSE: the same data the REST endpoints serve,
/// bundled so a single frame fully refreshes the live view.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub summary: SummaryView,
    pub work_items: Vec<WorkItemView>,
    /// True when `work_items` was capped at [`WORK_ITEMS_LIMIT`] and `summary.total`
    /// may exceed the rows shown — so the live view can say "showing first N" instead
    /// of silently mismatching the total against a truncated table.
    pub work_items_truncated: bool,
}

/// A read query failed. The HTTP layer maps `NotFound` to 404 (unknown repo) and
/// everything else to 500 with the detail logged server-side.
#[derive(Debug)]
enum ReadError {
    NotFound,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ReadError {
    fn from(e: sqlx::Error) -> Self {
        ReadError::Db(e)
    }
}

impl IntoResponse for ReadError {
    fn into_response(self) -> Response {
        match self {
            ReadError::NotFound => (StatusCode::NOT_FOUND, "repo not found").into_response(),
            ReadError::Db(e) => {
                tracing::error!(error = %e, "dashboard read query failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}

/// Build the read-only dashboard router. Every route is a GET (or SSE); no route
/// mutates the database, so a mutating request to any path returns 405/404.
pub fn build_router(state: DashboardState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route(
            "/api/repos/:owner/:name/work-items",
            get(work_items_handler),
        )
        .route("/api/repos/:owner/:name/summary", get(summary_handler))
        .route("/api/repos/:owner/:name/events", get(events_handler))
        .route("/api/repos/:owner/:name/stream", get(stream_handler))
        .with_state(Arc::new(state))
}

/// Liveness probe.
async fn healthz() -> StatusCode {
    tracing::info!("healthz");
    StatusCode::OK
}

/// Serve the embedded single-file dashboard page.
async fn index() -> Html<&'static str> {
    tracing::info!("serving embedded dashboard page");
    Html(INDEX_HTML)
}

/// `GET /api/repos/{owner}/{name}/work-items` — newest-first, bounded list.
async fn work_items_handler(
    State(state): State<Arc<DashboardState>>,
    Path((owner, name)): Path<(String, String)>,
) -> Result<Json<Vec<WorkItemView>>, ReadError> {
    tracing::info!(%owner, %name, "GET work-items");
    let repo_id = resolve_repo(&state.pool, &owner, &name).await?;
    let items = query_work_items(&state.pool, repo_id, WORK_ITEMS_LIMIT).await?;
    tracing::info!(%owner, %name, count = items.len(), "work-items projection served");
    Ok(Json(items))
}

/// `GET /api/repos/{owner}/{name}/summary` — counts by state + open escalations.
async fn summary_handler(
    State(state): State<Arc<DashboardState>>,
    Path((owner, name)): Path<(String, String)>,
) -> Result<Json<SummaryView>, ReadError> {
    tracing::info!(%owner, %name, "GET summary");
    let repo_id = resolve_repo(&state.pool, &owner, &name).await?;
    let summary = query_summary(&state.pool, repo_id, &owner, &name).await?;
    tracing::info!(%owner, %name, total = summary.total, open_escalations = summary.open_escalations, "summary projection served");
    Ok(Json(summary))
}

/// Query string for the events feed.
#[derive(Debug, serde::Deserialize)]
pub struct EventsParams {
    /// Raw string so a malformed `?limit=` (non-numeric, empty, overflow) is handled
    /// gracefully by `clamp_limit` (→ default) rather than failing Query
    /// deserialization with a 400. The feed stays bounded regardless of input.
    pub limit: Option<String>,
}

/// `GET /api/repos/{owner}/{name}/events?limit=N` — recent audit events,
/// newest-first, bounded.
async fn events_handler(
    State(state): State<Arc<DashboardState>>,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<EventsParams>,
) -> Result<Json<Vec<EventView>>, ReadError> {
    let limit = clamp_limit(params.limit.as_deref());
    tracing::info!(%owner, %name, limit, "GET events");
    let repo_id = resolve_repo(&state.pool, &owner, &name).await?;
    let events = query_events(&state.pool, repo_id, limit).await?;
    tracing::info!(%owner, %name, count = events.len(), "events projection served");
    Ok(Json(events))
}

/// `GET /api/repos/{owner}/{name}/stream` — SSE. Emits the current snapshot on
/// connect and then on every `sse_interval` tick, so a state change is reflected
/// within the Fresh-view budget. Each frame is `data: <json snapshot>\n\n`.
async fn stream_handler(
    State(state): State<Arc<DashboardState>>,
    Path((owner, name)): Path<(String, String)>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ReadError> {
    tracing::info!(%owner, %name, "SSE connect");
    // Resolve the repo up front so an unknown repo fails with 404 at connect time
    // rather than silently producing an empty stream.
    let repo_id = resolve_repo(&state.pool, &owner, &name).await?;
    let interval = state.sse_interval;

    // A periodic-snapshot poller: simple and testable. `unfold` carries the state
    // we need (pool, repo identity, interval) and yields one snapshot frame per
    // tick. The first frame is emitted immediately on connect (no initial sleep),
    // then every `interval` thereafter. A failed snapshot read is logged and that
    // tick emits an SSE *comment* (ignored by EventSource) rather than a data frame,
    // so the client keeps its last good frame instead of blanking — and the stream is
    // never torn down.
    let seed = PollState {
        pool: state.pool.clone(),
        repo_id,
        owner,
        name,
        interval,
        first: true,
    };
    let stream = futures::stream::unfold(seed, |mut s| async move {
        if s.first {
            s.first = false;
        } else {
            tokio::time::sleep(s.interval).await;
        }
        // On a read/serialize failure, emit an SSE *comment* (which EventSource
        // ignores) rather than a `data: {}` frame — an empty data frame would make the
        // client render an empty snapshot and blank the live view. A comment keeps the
        // last good frame on screen until the next successful tick.
        let event = match build_snapshot(&s.pool, s.repo_id, &s.owner, &s.name).await {
            Ok(snap) => match serde_json::to_string(&snap) {
                Ok(json) => {
                    tracing::debug!(owner = %s.owner, name = %s.name, "SSE snapshot pushed");
                    Event::default().data(json)
                }
                Err(e) => {
                    tracing::error!(error = %e, "serializing SSE snapshot");
                    Event::default().comment("snapshot unavailable")
                }
            },
            Err(e) => {
                tracing::error!(owner = %s.owner, name = %s.name, error = ?e, "SSE snapshot read failed; keeping last frame");
                Event::default().comment("snapshot unavailable")
            }
        };
        Some((Ok::<_, Infallible>(event), s))
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Carried state for the SSE `unfold` poller.
struct PollState {
    pool: Pool,
    repo_id: Uuid,
    owner: String,
    name: String,
    interval: Duration,
    first: bool,
}

// ---- Read queries (all local to this crate; read-only) --------------------

/// Resolve `(owner, name)` to a repo id. Unknown repo → `NotFound` (404). We do
/// NOT filter by status here: the dashboard shows suspended/removed repos too, so
/// an operator can see their history.
async fn resolve_repo(pool: &Pool, owner: &str, name: &str) -> Result<Uuid, ReadError> {
    let row = sqlx::query("SELECT id FROM repos WHERE owner = $1 AND name = $2")
        .bind(owner)
        .bind(name)
        .fetch_optional(pool)
        .await?;
    match row {
        Some(r) => Ok(r.try_get("id")?),
        None => Err(ReadError::NotFound),
    }
}

/// Newest-first, bounded work-items projection for a repo.
async fn query_work_items(
    pool: &Pool,
    repo_id: Uuid,
    limit: i64,
) -> Result<Vec<WorkItemView>, ReadError> {
    let rows = sqlx::query(
        "SELECT id, kind, worker_name, state, subject_ref, attempt_count, loop_count, updated_at
           FROM work_items
          WHERE repo_id = $1
          ORDER BY updated_at DESC, id DESC
          LIMIT $2",
    )
    .bind(repo_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(WorkItemView {
            id: r.try_get("id")?,
            kind: r.try_get("kind")?,
            worker_name: r.try_get("worker_name")?,
            state: r.try_get("state")?,
            subject_ref: r.try_get("subject_ref")?,
            attempt_count: r.try_get("attempt_count")?,
            loop_count: r.try_get("loop_count")?,
            updated_at: r.try_get("updated_at")?,
        });
    }
    Ok(out)
}

/// Work-item counts grouped by state, plus the open-escalation count, for a repo.
async fn query_summary(
    pool: &Pool,
    repo_id: Uuid,
    owner: &str,
    name: &str,
) -> Result<SummaryView, ReadError> {
    let rows = sqlx::query(
        "SELECT state, count(*) AS n
           FROM work_items
          WHERE repo_id = $1
          GROUP BY state
          ORDER BY state",
    )
    .bind(repo_id)
    .fetch_all(pool)
    .await?;

    let mut by_state = Vec::with_capacity(rows.len());
    let mut total = 0i64;
    for r in rows {
        let count: i64 = r.try_get("n")?;
        total += count;
        by_state.push(StateCount {
            state: r.try_get("state")?,
            count,
        });
    }

    // Open escalations belonging to this repo's work items.
    let open_escalations: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM escalations e
           JOIN work_items w ON w.id = e.work_item_id
          WHERE w.repo_id = $1 AND e.state = 'open'",
    )
    .bind(repo_id)
    .fetch_one(pool)
    .await?;

    Ok(SummaryView {
        owner: owner.to_owned(),
        name: name.to_owned(),
        by_state,
        total,
        open_escalations,
    })
}

/// Recent audit events for a repo, newest-first, bounded by `limit`.
async fn query_events(pool: &Pool, repo_id: Uuid, limit: i64) -> Result<Vec<EventView>, ReadError> {
    let rows = sqlx::query(
        "SELECT actor, action, detail, at
           FROM events
          WHERE repo_id = $1
          ORDER BY at DESC, id DESC
          LIMIT $2",
    )
    .bind(repo_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(EventView {
            actor: r.try_get("actor")?,
            action: r.try_get("action")?,
            detail: r.try_get("detail")?,
            at: r.try_get("at")?,
        });
    }
    Ok(out)
}

/// Build the combined snapshot pushed over SSE.
async fn build_snapshot(
    pool: &Pool,
    repo_id: Uuid,
    owner: &str,
    name: &str,
) -> Result<Snapshot, ReadError> {
    let summary = query_summary(pool, repo_id, owner, name).await?;
    let work_items = query_work_items(pool, repo_id, WORK_ITEMS_LIMIT).await?;
    // Truncated iff the repo actually has MORE work items than the page shows — use the
    // (unbounded) summary total, so a repo with exactly WORK_ITEMS_LIMIT items isn't
    // falsely flagged as truncated.
    let work_items_truncated = summary.total > work_items.len() as i64;
    Ok(Snapshot {
        summary,
        work_items,
        work_items_truncated,
    })
}

/// Parse and clamp the caller-supplied events `?limit=` into `[1, EVENTS_MAX_LIMIT]`,
/// defaulting when absent, empty, non-numeric, overflowing, or non-positive. Lenient
/// by design: a malformed limit yields the default rather than a 400, and the feed
/// stays bounded regardless of input.
fn clamp_limit(requested: Option<&str>) -> i64 {
    match requested.and_then(|s| s.trim().parse::<i64>().ok()) {
        Some(n) if n > 0 => n.min(EVENTS_MAX_LIMIT),
        _ => EVENTS_DEFAULT_LIMIT,
    }
}

/// Resolve the SSE push cadence from the environment, falling back to the default.
/// Exposed so the binary and tests agree on the parsing rule.
pub fn sse_interval_from_env() -> Duration {
    match std::env::var("DASHBOARD_SSE_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
    {
        Some(ms) => Duration::from_millis(ms),
        None => DEFAULT_SSE_INTERVAL,
    }
}

/// The embedded, build-free dashboard page (see the SPA-deviation note above).
const INDEX_HTML: &str = include_str!("index.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_limit_defaults_and_bounds() {
        assert_eq!(clamp_limit(None), EVENTS_DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some("0")), EVENTS_DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some("-5")), EVENTS_DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some("10")), 10);
        assert_eq!(
            clamp_limit(Some(&(EVENTS_MAX_LIMIT + 1).to_string())),
            EVENTS_MAX_LIMIT
        );
        // Lenient parsing: malformed/empty/overflow all fall back to the default
        // (graceful, never a 400).
        assert_eq!(clamp_limit(Some("abc")), EVENTS_DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some("")), EVENTS_DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some("  25 ")), 25, "whitespace tolerated");
        assert_eq!(
            clamp_limit(Some("99999999999999999999999")),
            EVENTS_DEFAULT_LIMIT,
            "i64 overflow falls back to default"
        );
    }
}
