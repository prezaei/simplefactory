//! Integration tests for the Dashboard API (P6): the read-only projection of the
//! record book + the eventually-consistent SSE live view. These drive the full
//! Axum router via tower `oneshot` against a real Postgres, seeding rows with the
//! existing `factory_record` helpers (no new record fns are added). DB-gated: they
//! skip cleanly when `DATABASE_URL` is unset. Repo names are uuid-unique so tests
//! run in parallel and never collide with a parallel branch sharing the container.
#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use factory_dashboard_api::{build_router, DashboardState};
use factory_record::{self as record, EnqueueOutcome, Kind, NewWorkItem, Pool, State};
use serde_json::Value;
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

/// Connect + migrate (idempotent), or skip the test when DATABASE_URL is unset.
async fn pool_or_skip() -> Option<Pool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let p = record::connect(&url).await.expect("connect");
    record::migrate(&p).await.expect("migrate");
    Some(p)
}

/// A unique (owner, name) pair so each test is isolated within the shared DB.
fn unique_repo() -> (String, String) {
    ("acme".to_owned(), format!("dash-{}", Uuid::now_v7()))
}

/// Enqueue one work item and return its id.
async fn enqueue_item(pool: &Pool, repo_id: Uuid, kind: Kind, subject_ref: &str) -> Uuid {
    let outcome = record::enqueue(
        pool,
        &NewWorkItem {
            repo_id,
            kind,
            worker_name: format!("{}-{}", kind.as_str(), Uuid::now_v7()),
            config_sha: "deadbeef".to_owned(),
            trigger: "issue_labeled".to_owned(),
            subject_ref: subject_ref.to_owned(),
            correlation_id: Uuid::now_v7(),
            priority: 0,
            available_at: None,
        },
    )
    .await
    .expect("enqueue");
    match outcome {
        EnqueueOutcome::Inserted(id) => id,
        EnqueueOutcome::DuplicateActive => panic!("unexpected duplicate during seed"),
    }
}

/// Drive a request through the router and return (status, body bytes).
async fn call(state: DashboardState, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = build_router(state).oneshot(req).await.expect("router call");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body")
        .to_vec();
    (status, bytes)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

// ---- (a) work-items projection -------------------------------------------

#[tokio::test]
async fn work_items_returns_repo_items_and_excludes_others() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let (owner, name) = unique_repo();
    let repo_id = record::upsert_repo(&pool, &owner, &name, 1).await.unwrap();

    // Seed two items in our repo with distinct kinds/subjects.
    let coder = enqueue_item(&pool, repo_id, Kind::Coder, "issue#1").await;
    record::set_state(&pool, coder, State::Running)
        .await
        .unwrap();
    let _mgr = enqueue_item(&pool, repo_id, Kind::Manager, "issue#2").await;

    // Seed a DIFFERENT repo that must be excluded.
    let (oo, on) = unique_repo();
    let other_id = record::upsert_repo(&pool, &oo, &on, 1).await.unwrap();
    enqueue_item(&pool, other_id, Kind::Coder, "issue#9").await;

    let (status, body) = call(
        DashboardState::new(pool.clone()),
        get(&format!("/api/repos/{owner}/{name}/work-items")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items: Vec<Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(items.len(), 2, "exactly our two items, other repo excluded");

    // Fields present and correct on the running coder item.
    let coder_view = items
        .iter()
        .find(|i| i["id"] == Value::String(coder.to_string()))
        .expect("coder item present");
    assert_eq!(coder_view["kind"], "coder");
    assert_eq!(coder_view["state"], "running");
    assert_eq!(coder_view["subject_ref"], "issue#1");
    assert!(coder_view["worker_name"].is_string());
    assert_eq!(coder_view["attempt_count"], 0);
    assert_eq!(coder_view["loop_count"], 0);
    assert!(coder_view["updated_at"].is_string());

    // No subject from the other repo leaked in.
    assert!(items.iter().all(|i| i["subject_ref"] != "issue#9"));
}

#[tokio::test]
async fn work_items_unknown_repo_is_404() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let (status, _) = call(
        DashboardState::new(pool),
        get("/api/repos/acme/does-not-exist-xyz/work-items"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---- (b) summary counts ---------------------------------------------------

#[tokio::test]
async fn summary_counts_by_state_and_open_escalations() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let (owner, name) = unique_repo();
    let repo_id = record::upsert_repo(&pool, &owner, &name, 1).await.unwrap();

    // Two queued, one running, one escalated (with an open escalation row).
    enqueue_item(&pool, repo_id, Kind::Coder, "s1").await;
    enqueue_item(&pool, repo_id, Kind::Coder, "s2").await;
    let running = enqueue_item(&pool, repo_id, Kind::Manager, "s3").await;
    record::set_state(&pool, running, State::Running)
        .await
        .unwrap();
    let esc = enqueue_item(&pool, repo_id, Kind::Reviewer, "s4").await;
    record::set_state(&pool, esc, State::Escalated)
        .await
        .unwrap();
    record::create_escalation_idempotent(&pool, esc, "needs_decision", "stuck", None)
        .await
        .unwrap();

    let (status, body) = call(
        DashboardState::new(pool.clone()),
        get(&format!("/api/repos/{owner}/{name}/summary")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let summary: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(summary["owner"], owner);
    assert_eq!(summary["name"], name);
    assert_eq!(summary["total"], 4);
    assert_eq!(summary["open_escalations"], 1);

    // by_state must report queued=2, running=1, escalated=1.
    let by_state = summary["by_state"].as_array().unwrap();
    let count_for = |st: &str| -> i64 {
        by_state
            .iter()
            .find(|b| b["state"] == st)
            .map(|b| b["count"].as_i64().unwrap())
            .unwrap_or(0)
    };
    assert_eq!(count_for("queued"), 2);
    assert_eq!(count_for("running"), 1);
    assert_eq!(count_for("escalated"), 1);
    assert_eq!(count_for("succeeded"), 0);
}

// ---- (c) events feed ------------------------------------------------------

#[tokio::test]
async fn events_feed_newest_first_and_bounded_by_limit() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let (owner, name) = unique_repo();
    let repo_id = record::upsert_repo(&pool, &owner, &name, 1).await.unwrap();

    // Append three events in order; `at` defaults to now() so insertion order is
    // chronological. id is UUIDv7 (monotonic) so the secondary sort is stable.
    for i in 0..3 {
        record::append_event(
            &pool,
            Some(repo_id),
            None,
            None,
            "coordinator",
            &format!("action-{i}"),
            serde_json::json!({ "i": i }),
        )
        .await
        .unwrap();
    }

    // Full feed: newest-first.
    let (status, body) = call(
        DashboardState::new(pool.clone()),
        get(&format!("/api/repos/{owner}/{name}/events")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events: Vec<Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["action"], "action-2", "newest first");
    assert_eq!(events[2]["action"], "action-0", "oldest last");
    assert_eq!(events[0]["actor"], "coordinator");
    assert_eq!(events[0]["detail"], serde_json::json!({ "i": 2 }));
    assert!(events[0]["at"].is_string());

    // Bounded by limit: only the newest one.
    let (status, body) = call(
        DashboardState::new(pool.clone()),
        get(&format!("/api/repos/{owner}/{name}/events?limit=1")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events: Vec<Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(events.len(), 1, "bounded by limit=1");
    assert_eq!(events[0]["action"], "action-2");
}

// ---- (d) SSE freshness ----------------------------------------------------

/// Connect to the stream, then seed a state change and assert a SUBSEQUENT SSE
/// frame reflects it within the freshness budget. The poll interval is set small
/// (50ms) so the test is fast but still exercises the real periodic push.
#[tokio::test]
async fn sse_stream_reflects_state_change_within_budget() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let (owner, name) = unique_repo();
    let repo_id = record::upsert_repo(&pool, &owner, &name, 1).await.unwrap();
    let item = enqueue_item(&pool, repo_id, Kind::Coder, "live#1").await;

    let state = DashboardState {
        pool: pool.clone(),
        sse_interval: Duration::from_millis(50),
    };
    let resp = build_router(state)
        .oneshot(get(&format!("/api/repos/{owner}/{name}/stream")))
        .await
        .expect("connect stream");
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/event-stream"),
        "SSE content-type, got {ct}"
    );

    let mut body = resp.into_body();

    // Drain the first (connect-time) frame: it should show the item as queued.
    let first = next_data_frame(&mut body, Duration::from_secs(3))
        .await
        .expect("first SSE frame");
    let snap: Value = serde_json::from_str(&first).expect("frame is json");
    let state_of = |snap: &Value| -> Option<String> {
        snap["work_items"]
            .as_array()?
            .iter()
            .find(|w| w["id"] == Value::String(item.to_string()))
            .map(|w| w["state"].as_str().unwrap_or("").to_owned())
    };
    assert_eq!(state_of(&snap).as_deref(), Some("queued"));

    // Seed a state change AFTER connecting.
    record::set_state(&pool, item, State::Succeeded)
        .await
        .unwrap();

    // A subsequent frame must reflect 'succeeded' well within the budget.
    let budget = Duration::from_secs(3);
    let deadline = Instant::now() + budget;
    let mut saw = None;
    while Instant::now() < deadline {
        let Some(frame) = next_data_frame(&mut body, budget).await else {
            break;
        };
        if let Ok(snap) = serde_json::from_str::<Value>(&frame) {
            if let Some(st) = state_of(&snap) {
                if st == "succeeded" {
                    saw = Some(st);
                    break;
                }
            }
        }
    }
    assert_eq!(
        saw.as_deref(),
        Some("succeeded"),
        "SSE reflected the state change within {budget:?}"
    );
}

/// Read the next `data: ...` payload from an SSE body within `timeout`.
/// Returns the decoded JSON string of the frame, or None on timeout/end.
async fn next_data_frame(body: &mut Body, timeout: Duration) -> Option<String> {
    use http_body_util::BodyExt;
    let deadline = Instant::now() + timeout;
    let mut buf = String::new();
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let frame = tokio::time::timeout(remaining, body.frame()).await.ok()??;
        let frame = frame.ok()?;
        if let Some(data) = frame.data_ref() {
            buf.push_str(&String::from_utf8_lossy(data));
            // SSE frames are terminated by a blank line.
            while let Some(idx) = buf.find("\n\n") {
                let block = buf[..idx].to_owned();
                buf = buf[idx + 2..].to_owned();
                for line in block.lines() {
                    if let Some(payload) = line.strip_prefix("data:") {
                        let payload = payload.strip_prefix(' ').unwrap_or(payload);
                        if !payload.is_empty() {
                            return Some(payload.to_owned());
                        }
                    }
                }
            }
        }
    }
}

// ---- (e) read-only --------------------------------------------------------

/// A mutating request to any API path must be rejected: the router registers only
/// GET routes, so a POST/PUT/DELETE to a known path returns 405 Method Not Allowed
/// (and an unknown path returns 404). Either way, no route can mutate.
#[tokio::test]
async fn mutating_requests_are_rejected() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let (owner, name) = unique_repo();
    record::upsert_repo(&pool, &owner, &name, 1).await.unwrap();

    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        for path in [
            format!("/api/repos/{owner}/{name}/work-items"),
            format!("/api/repos/{owner}/{name}/summary"),
            format!("/api/repos/{owner}/{name}/events"),
            format!("/api/repos/{owner}/{name}/stream"),
            "/".to_owned(),
            "/healthz".to_owned(),
        ] {
            let req = Request::builder()
                .method(method)
                .uri(&path)
                .body(Body::empty())
                .unwrap();
            let (status, _) = call(DashboardState::new(pool.clone()), req).await;
            assert!(
                status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::NOT_FOUND,
                "{method} {path} should be 405/404, got {status}"
            );
        }
    }
}

// ---- healthz --------------------------------------------------------------

#[tokio::test]
async fn healthz_is_ok() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let (status, _) = call(DashboardState::new(pool), get("/healthz")).await;
    assert_eq!(status, StatusCode::OK);
}

// ---- index page -----------------------------------------------------------

#[tokio::test]
async fn index_serves_html() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let resp = build_router(DashboardState::new(pool))
        .oneshot(get("/"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/html"), "html content-type, got {ct}");
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("EventSource"), "page wires up SSE");
    assert!(html.contains("Factory"), "page has the title");
}
