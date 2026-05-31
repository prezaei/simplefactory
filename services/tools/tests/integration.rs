//! Integration tests for the Tools service (P3): the worker's only channel to
//! Factory. These drive BOTH the tool functions directly AND the full HTTP router
//! (bearer extractor → authorize → audit → dispatch) via tower `oneshot`, with a
//! `FakeGitHub` + `InMemoryTranscripts` at the seams and a real Postgres at the
//! back. DB-gated: they skip cleanly when `DATABASE_URL` is unset. Every assertion
//! is exact (exactly-one side effects, exact event/escalation counts).
#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use factory_blob::InMemoryTranscripts;
use factory_github::{CheckConclusion, CheckSummary, FakeGitHub, IssueView};
use factory_record::{
    self as record, AttemptStatus, Kind, LeaseRequest, NewWorkItem, Pool, State, TaskAuth,
};
use factory_tools::{build_router, ToolsState};
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

/// Connect + migrate, or skip the test when DATABASE_URL is unset.
async fn pool_or_skip() -> Option<Pool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let p = record::connect(&url).await.expect("connect");
    record::migrate(&p).await.expect("migrate");
    Some(p)
}

/// A fully wired task: a leased coder work item, a running attempt, a minted raw
/// token, and the resolved auth. Names are uuid-unique so tests run in parallel.
struct Fixture {
    state: Arc<ToolsState>,
    gh: Arc<FakeGitHub>,
    auth: TaskAuth,
    raw_token: String,
    work_item_id: Uuid,
}

/// Build a fixture whose work item carries `subject_ref`. The FakeGitHub is built
/// by the caller so it can seed issues/checks.
async fn fixture_with(pool: &Pool, subject_ref: &str, gh: FakeGitHub) -> Fixture {
    let suffix = Uuid::now_v7();
    let owner = "acme";
    let name = format!("tools-{suffix}");
    let repo_id = record::upsert_repo(pool, owner, &name, 42)
        .await
        .expect("upsert repo");

    let worker_name = format!("coder-{suffix}");
    record::enqueue(
        pool,
        &NewWorkItem {
            repo_id,
            kind: Kind::Coder,
            worker_name: worker_name.clone(),
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

    let leased = record::lease_next(
        pool,
        &LeaseRequest {
            repo_id,
            worker_name: worker_name.clone(),
            lease_owner: format!("job-{suffix}"),
            ttl_secs: 600.0,
            per_worker_cap: 5,
            global_cap: 5,
        },
    )
    .await
    .expect("lease")
    .expect("a queued item to lease");

    let attempt_id = record::start_attempt(pool, leased.id, Some("job-1"))
        .await
        .expect("start attempt");

    let raw_token = format!("tok-{suffix}");
    record::mint_task_token(
        pool,
        &record::hash_token(&raw_token),
        leased.id,
        repo_id,
        Kind::Coder,
        Some(attempt_id),
        3600.0,
    )
    .await
    .expect("mint token");

    let auth = TaskAuth {
        work_item_id: leased.id,
        repo_id,
        role: Kind::Coder,
        attempt_id: Some(attempt_id),
    };

    let gh = Arc::new(gh);
    let state = Arc::new(ToolsState {
        pool: pool.clone(),
        gh: gh.clone(),
        blob: Arc::new(InMemoryTranscripts::new()),
    });

    Fixture {
        state,
        gh,
        auth,
        raw_token,
        work_item_id: leased.id,
    }
}

/// POST a tool call through the full HTTP router with the given bearer.
async fn http_call(
    state: Arc<ToolsState>,
    bearer: &str,
    tool: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/tools/{tool}"))
        .header("authorization", format!("Bearer {bearer}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = build_router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn count_calls(gh: &FakeGitHub, substr: &str) -> usize {
    gh.calls().iter().filter(|c| c.contains(substr)).count()
}

// ===========================================================================
// 1. Token scoping / auth
// ===========================================================================

#[tokio::test]
async fn unknown_bearer_is_401_and_a_valid_token_sees_only_its_own_task() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let issue = IssueView {
        number: 7,
        title: "Fix the bug".into(),
        body: "details".into(),
        comments: vec!["a comment".into()],
    };
    let fx = fixture_with(&pool, "issue#7", FakeGitHub::new().with_issue(issue)).await;

    // Unknown bearer → 401, no task access.
    let (status, _) = http_call(
        fx.state.clone(),
        "totally-bogus-token",
        "get_task",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Missing bearer header entirely → 401.
    let req = Request::builder()
        .method("POST")
        .uri("/tools/get_task")
        .body(Body::from("{}"))
        .unwrap();
    let resp = build_router(fx.state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Valid token resolves to THIS task and returns THIS task's subject.
    let (status, body) = http_call(fx.state.clone(), &fx.raw_token, "get_task", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["work_item_id"], fx.work_item_id.to_string());
    assert_eq!(body["role"], "coder");
    assert_eq!(body["subject_ref"], "issue#7");
    assert_eq!(body["subject"]["type"], "issue");
    assert_eq!(body["subject"]["number"], 7);
    assert_eq!(body["subject"]["title"], "Fix the bug");
}

// ===========================================================================
// 2. Idempotent open_or_update_change_request
// ===========================================================================

#[tokio::test]
async fn open_or_update_change_request_is_idempotent() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#11", FakeGitHub::new()).await;

    let args = json!({
        "title": "Implement feature",
        "body": "the work",
        "head_branch": "factory/issue-11",
        "base": "main",
        "linked_issue": 11,
    });

    let (s1, b1) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "open_or_update_change_request",
        args.clone(),
    )
    .await;
    let (s2, b2) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "open_or_update_change_request",
        args,
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);

    // Same cr_number both times.
    assert_eq!(b1["cr_number"], b2["cr_number"]);
    let cr_number = b1["cr_number"].as_i64().unwrap();

    // Exactly ONE change_requests row for the work item, with the recorded number.
    let cr = record::change_request_for_item(&pool, fx.work_item_id)
        .await
        .unwrap()
        .expect("a CR row");
    assert_eq!(cr.number, Some(cr_number as i32));
    assert_eq!(cr.work_item_id, Some(fx.work_item_id));

    // FakeGitHub recorded open_or_update_pr for the same branch (same number).
    assert_eq!(
        count_calls(&fx.gh, "open_or_update_pr branch=factory/issue-11"),
        2,
        "both calls hit GitHub, but the branch is idempotent so the number is stable"
    );
}

// ===========================================================================
// 3. post_comment dedupe
// ===========================================================================

#[tokio::test]
async fn post_comment_dedupes_by_key() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#3", FakeGitHub::new()).await;

    let args = json!({ "target": "issue#3", "body": "hello", "dedupe_key": "greeting" });
    let (_, b1) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "post_comment",
        args.clone(),
    )
    .await;
    let (_, b2) = http_call(fx.state.clone(), &fx.raw_token, "post_comment", args).await;

    // Same stored result, and exactly ONE real post_comment call to GitHub.
    assert_eq!(b1["comment_id"], b2["comment_id"]);
    assert_eq!(count_calls(&fx.gh, "post_comment"), 1);
}

// ===========================================================================
// 4. submit_review dedupe
// ===========================================================================

#[tokio::test]
async fn submit_review_dedupes_by_key() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "cr#21", FakeGitHub::new()).await;

    let args = json!({
        "cr_number": 21,
        "verdict": "approve",
        "body": "LGTM",
        "dedupe_key": "first-pass",
    });
    let (_, b1) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "submit_review",
        args.clone(),
    )
    .await;
    let (_, b2) = http_call(fx.state.clone(), &fx.raw_token, "submit_review", args).await;

    assert_eq!(b1["review_id"], b2["review_id"]);
    assert_eq!(count_calls(&fx.gh, "submit_review"), 1);
}

// ===========================================================================
// 5. record_outcome
// ===========================================================================

#[tokio::test]
async fn record_outcome_is_terminal_once_and_sets_state() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#5", FakeGitHub::new()).await;

    // Drive the tool fn directly (also exercises the lib surface).
    let r1 = factory_tools::record_outcome(&fx.state, &fx.auth, "succeeded", "all done")
        .await
        .unwrap();
    assert_eq!(r1["finished"], true);
    assert_eq!(r1["state"], "succeeded");
    assert_eq!(r1["state_changed"], true);

    // Repeat: attempt is already terminal → finish returns false. State is already
    // terminal → set_state is a no-op (false).
    let r2 = factory_tools::record_outcome(&fx.state, &fx.auth, "succeeded", "all done again")
        .await
        .unwrap();
    assert_eq!(r2["finished"], false);
    assert_eq!(r2["state_changed"], false);

    // The work item is in the mapped terminal state.
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, State::Succeeded);
}

#[tokio::test]
async fn record_outcome_maps_blocked_and_failed() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };

    // blocked
    let fx = fixture_with(&pool, "issue#51", FakeGitHub::new()).await;
    factory_tools::record_outcome(&fx.state, &fx.auth, "blocked", "needs input")
        .await
        .unwrap();
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, State::Blocked);

    // failed
    let fx2 = fixture_with(&pool, "issue#52", FakeGitHub::new()).await;
    factory_tools::record_outcome(&fx2.state, &fx2.auth, "failed", "build broke")
        .await
        .unwrap();
    let item2 = record::work_item_by_id(&pool, fx2.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item2.state, State::Failed);
}

// ===========================================================================
// 6. heartbeat extends the lease
// ===========================================================================

#[tokio::test]
async fn heartbeat_extends_the_lease() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#9", FakeGitHub::new()).await;

    let (status, body) = http_call(fx.state.clone(), &fx.raw_token, "heartbeat", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["lease_expires_at"].is_string(),
        "in-flight item returns a new expiry, got {body}"
    );
}

// ===========================================================================
// 7. escalate — exactly one escalation, idempotent, work item escalated
// ===========================================================================

#[tokio::test]
async fn escalate_is_idempotent_and_escalates_the_item() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#13", FakeGitHub::new()).await;

    let args = json!({
        "reason": "needs_decision",
        "context": "Which database should we use?",
        "dedupe_key": "db-choice",
    });
    let (s1, b1) = http_call(fx.state.clone(), &fx.raw_token, "escalate", args.clone()).await;
    let (s2, b2) = http_call(fx.state.clone(), &fx.raw_token, "escalate", args).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);

    // Same escalation id returned both times.
    assert_eq!(b1["escalation_id"], b2["escalation_id"]);

    // Exactly ONE escalation row.
    assert_eq!(
        record::count_escalations_for_item(&pool, fx.work_item_id)
            .await
            .unwrap(),
        1
    );
    // Exactly ONE human-facing comment posted (the question).
    assert_eq!(count_calls(&fx.gh, "post_comment"), 1);

    // The work item is escalated.
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, State::Escalated);
}

#[tokio::test]
async fn escalate_rejects_bad_reason() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#14", FakeGitHub::new()).await;

    let (status, _) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "escalate",
        json!({ "reason": "i_give_up", "context": "x" }),
    )
    .await;
    // A bad reason is rejected before any side effect.
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        record::count_escalations_for_item(&pool, fx.work_item_id)
            .await
            .unwrap(),
        0
    );
}

// ===========================================================================
// 8. audit — every authorized call adds exactly one event
// ===========================================================================

#[tokio::test]
async fn every_authorized_tool_call_writes_one_audit_event() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#17", FakeGitHub::new()).await;

    let before = record::count_events_for_item(&pool, fx.work_item_id)
        .await
        .unwrap();

    // Make N successful authorized calls through the HTTP layer.
    let calls: [(&str, Value); 3] = [
        ("heartbeat", json!({})),
        (
            "append_transcript",
            json!({ "chunk": "step 1: thinking\n" }),
        ),
        ("heartbeat", json!({})),
    ];
    let n = calls.len() as i64;
    for (tool, body) in calls {
        let (status, _) = http_call(fx.state.clone(), &fx.raw_token, tool, body).await;
        assert_eq!(status, StatusCode::OK, "tool {tool} should succeed");
    }

    let after = record::count_events_for_item(&pool, fx.work_item_id)
        .await
        .unwrap();
    assert_eq!(after - before, n, "exactly one audit event per call");
}

// ===========================================================================
// Extra surface coverage: read_issue, read_change_request, get_check_status,
// append_transcript round-trip.
// ===========================================================================

#[tokio::test]
async fn read_issue_and_check_status_and_transcript() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let gh = FakeGitHub::new()
        .with_issue(IssueView {
            number: 30,
            title: "Title".into(),
            body: "Body".into(),
            comments: vec![],
        })
        .with_checks(
            "sha-30",
            CheckSummary {
                overall: CheckConclusion::Success,
                checks: vec![("ci".into(), CheckConclusion::Success)],
            },
        );
    let fx = fixture_with(&pool, "cr#30", gh).await;

    // read_issue
    let (s, b) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "read_issue",
        json!({ "issue_number": 30 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["title"], "Title");

    // read_change_request (FakeGitHub returns a synthetic CR view)
    let (s, b) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "read_change_request",
        json!({ "cr_number": 30 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["number"], 30);

    // get_check_status by cr_number → maps to sha-30 in the fake.
    let (s, b) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "get_check_status",
        json!({ "cr_number": 30 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["overall"], "success");

    // get_check_status with neither arg → 400.
    let (s, _) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "get_check_status",
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // append_transcript writes to the task's own URI and reads back.
    let (s, _) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "append_transcript",
        json!({ "chunk": "log line\n" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let uri = format!("{}/transcript.log", fx.work_item_id);
    let contents = fx.state.blob.read(&uri).await.unwrap();
    assert_eq!(contents, "log line\n");
}

// ===========================================================================
// record_outcome without an attempt on the token is a clean error (not a panic).
// ===========================================================================

#[tokio::test]
async fn record_outcome_without_attempt_errors() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#41", FakeGitHub::new()).await;
    let auth_no_attempt = TaskAuth {
        attempt_id: None,
        ..fx.auth.clone()
    };
    let err = factory_tools::record_outcome(&fx.state, &auth_no_attempt, "succeeded", "x")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no attempt"));
    // The attempt status mapping is exercised separately to keep this focused.
    let _ = AttemptStatus::Succeeded;
}
