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
// save_checkpoint — persists the session + uri on the token's attempt (P5).
// ===========================================================================

#[tokio::test]
async fn save_checkpoint_persists_to_the_attempt() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#71", FakeGitHub::new()).await;

    let (status, body) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "save_checkpoint",
        json!({ "session_id": "ses-71", "checkpoint_uri": "checkpoints/ses-71.snapshot" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["saved"], json!(true));

    // The pointer is now recoverable for a crash-resume re-dispatch.
    let info = record::last_checkpoint_for_item(&pool, fx.work_item_id)
        .await
        .expect("query")
        .expect("checkpoint persisted");
    assert_eq!(info.session_id, "ses-71");
    assert_eq!(info.checkpoint_uri, "checkpoints/ses-71.snapshot");
}

#[tokio::test]
async fn save_checkpoint_without_attempt_errors() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#72", FakeGitHub::new()).await;
    let auth_no_attempt = TaskAuth {
        attempt_id: None,
        ..fx.auth.clone()
    };
    let err = factory_tools::save_checkpoint(&fx.state, &auth_no_attempt, "s", "u")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no attempt"));
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

    // A terminal reason so the item ends up escalated (FIX 3 maps needs-human-input
    // reasons to blocked; this test asserts the terminal path).
    let args = json!({
        "reason": "conflict",
        "context": "Merge conflict needs a human",
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

    // The work item is escalated (terminal reason).
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, State::Escalated);
}

#[tokio::test]
async fn escalate_same_item_reason_distinct_keys_is_idempotent_not_db_error() {
    // FIX 1 regression: migration 0004 added a UNIQUE (work_item_id, reason) index.
    // Two escalate calls for the SAME (work_item, reason) but with DISTINCT dedupe_keys
    // BYPASS the task-level idempotency claim (different keys → both win their claim),
    // so they both reach the record writer. The writer must be idempotent per
    // (work_item, reason): the second call is a controlled no-op success (created=false),
    // NOT a DB unique-violation. Exactly one escalation row, exactly one comment.
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#137", FakeGitHub::new()).await;

    let (s1, b1) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "escalate",
        json!({ "reason": "conflict", "context": "merge conflict", "dedupe_key": "first" }),
    )
    .await;
    let (s2, b2) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "escalate",
        json!({ "reason": "conflict", "context": "merge conflict again", "dedupe_key": "second" }),
    )
    .await;
    // Neither call errors — the second is a controlled tool result, not a 500.
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(
        s2,
        StatusCode::OK,
        "second (item,reason) escalate must not DB-error"
    );

    // Same escalation id returned both times (the record-layer idempotency).
    assert_eq!(b1["escalation_id"], b2["escalation_id"]);
    // The second call is a no-op: it delivered no comment and changed no state.
    assert_eq!(b2["delivered_comment_id"], serde_json::Value::Null);
    assert_eq!(b2["state_changed"], false);

    // Exactly ONE escalation row and exactly ONE comment for the (item, reason).
    assert_eq!(
        record::count_escalations_for_item(&pool, fx.work_item_id)
            .await
            .unwrap(),
        1,
        "two same-(item,reason) escalates file exactly one escalation row"
    );
    assert_eq!(count_calls(&fx.gh, "post_comment"), 1);
}

#[tokio::test]
async fn escalate_is_idempotent_for_one_reason_without_a_dedupe_key() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#131", FakeGitHub::new()).await;

    // No dedupe_key on either call, same reason: escalate must be idempotent,
    // defaulting to the per-reason key `escalate:<reason>`.
    let args = json!({ "reason": "needs_decision", "context": "ship it?" });
    let (s1, b1) = http_call(fx.state.clone(), &fx.raw_token, "escalate", args.clone()).await;
    let (s2, b2) = http_call(fx.state.clone(), &fx.raw_token, "escalate", args).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b1["escalation_id"], b2["escalation_id"]);

    // Exactly ONE escalation row and exactly ONE comment, even with no dedupe_key.
    assert_eq!(
        record::count_escalations_for_item(&pool, fx.work_item_id)
            .await
            .unwrap(),
        1
    );
    assert_eq!(count_calls(&fx.gh, "post_comment"), 1);
}

#[tokio::test]
async fn escalate_same_reason_twice_without_key_is_idempotent_and_keeps_state() {
    // The default dedupe_key is a per-task constant, so re-escalating the SAME reason
    // (no key) is idempotent — exactly one escalation row, state unchanged.
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#1300", FakeGitHub::new()).await;

    let args = json!({ "reason": "blocked_input", "context": "need a key" });
    let (s1, b1) = http_call(fx.state.clone(), &fx.raw_token, "escalate", args.clone()).await;
    let (s2, b2) = http_call(fx.state.clone(), &fx.raw_token, "escalate", args).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    // Same escalation id, exactly one row, exactly one comment.
    assert_eq!(b1["escalation_id"], b2["escalation_id"]);
    assert_eq!(
        record::count_escalations_for_item(&pool, fx.work_item_id)
            .await
            .unwrap(),
        1,
        "same reason twice files exactly one escalation"
    );
    assert_eq!(count_calls(&fx.gh, "post_comment"), 1);
    // State unchanged: blocked_input parks the item at blocked both times.
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, State::Blocked);
}

#[tokio::test]
async fn escalate_dedupes_same_reason_but_allows_a_distinct_reason() {
    // Invariant: ONE escalation per (task, reason). With no dedupe_key the key defaults
    // to a per-reason value (`escalate:<reason>`), so a repeat of the SAME reason dedupes
    // to the first, but a DIFFERENT reason gets its own escalation — it is NOT swallowed.
    // This mirrors the DB UNIQUE (work_item_id, reason) index.
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#1301", FakeGitHub::new()).await;

    // First: blocked_input parks the item at blocked.
    let (s1, b1) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "escalate",
        json!({ "reason": "blocked_input", "context": "need a decision" }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(b1["state"], "blocked");

    // A REPEAT of the same reason with no key dedupes to the first — same escalation id,
    // no second row, no second comment.
    let (s_dup, b_dup) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "escalate",
        json!({ "reason": "blocked_input", "context": "still waiting" }),
    )
    .await;
    assert_eq!(s_dup, StatusCode::OK);
    assert_eq!(b1["escalation_id"], b_dup["escalation_id"]);

    // A DIFFERENT reason (retries_exhausted) with no key must escalate on its own —
    // distinct escalation id, and it moves the item to the terminal escalated state.
    let (s2, b2) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "escalate",
        json!({ "reason": "retries_exhausted", "context": "gave up" }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_ne!(
        b1["escalation_id"], b2["escalation_id"],
        "a distinct reason must not dedupe to the first escalation"
    );
    assert_eq!(b2["state"], "escalated");

    // Exactly TWO escalation rows (one per distinct reason) and exactly TWO comments.
    assert_eq!(
        record::count_escalations_for_item(&pool, fx.work_item_id)
            .await
            .unwrap(),
        2,
        "one escalation per distinct reason"
    );
    assert_eq!(count_calls(&fx.gh, "post_comment"), 2);

    // The item reflects the most recent (terminal) escalation.
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, State::Escalated);
}

#[tokio::test]
async fn concurrent_escalate_files_exactly_one_escalation() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#132", FakeGitHub::new()).await;

    // Spawn several concurrent escalate calls for the SAME task with NO dedupe_key.
    // The claim-first pattern guarantees exactly one side effect; the losers either
    // observe the recorded result (200) or are told to retry while the winner's side
    // effect is in flight (409, a retryable conflict — never 500). Either way, only
    // ONE escalation/comment is created.
    let mut handles = Vec::new();
    for _ in 0..6 {
        let state = fx.state.clone();
        let token = fx.raw_token.clone();
        handles.push(tokio::spawn(async move {
            http_call(
                state,
                &token,
                "escalate",
                json!({ "reason": "conflict", "context": "merge conflict" }),
            )
            .await
        }));
    }
    for h in handles {
        let (status, _) = h.await.unwrap();
        assert!(
            status == StatusCode::OK || status == StatusCode::CONFLICT,
            "loser is a retryable 409 conflict, never 500; got {status}"
        );
    }

    // EXACTLY ONE escalation row, EXACTLY ONE GitHub comment.
    assert_eq!(
        record::count_escalations_for_item(&pool, fx.work_item_id)
            .await
            .unwrap(),
        1,
        "exactly one escalation despite concurrent calls"
    );
    assert_eq!(
        count_calls(&fx.gh, "post_comment"),
        1,
        "exactly one comment posted despite concurrent calls"
    );

    // The work item is escalated.
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, State::Escalated);
}

#[tokio::test]
async fn concurrent_post_comment_with_key_posts_exactly_once() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#133", FakeGitHub::new()).await;

    // Concurrent post_comment with the SAME dedupe_key: the claim winner posts once,
    // losers either return the stored result (200) or are told to retry (409, a
    // retryable conflict — never 500) — never a 2nd post.
    let mut handles = Vec::new();
    for _ in 0..6 {
        let state = fx.state.clone();
        let token = fx.raw_token.clone();
        handles.push(tokio::spawn(async move {
            http_call(
                state,
                &token,
                "post_comment",
                json!({ "target": "issue#133", "body": "hi", "dedupe_key": "greet" }),
            )
            .await
        }));
    }
    for h in handles {
        let (status, _) = h.await.unwrap();
        assert!(
            status == StatusCode::OK || status == StatusCode::CONFLICT,
            "loser is a retryable 409 conflict, never 500; got {status}"
        );
    }

    assert_eq!(
        count_calls(&fx.gh, "post_comment"),
        1,
        "exactly one comment posted despite concurrent calls"
    );
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

// ===========================================================================
// FIX 2 — get_task returns head_branch, base, and a composed prompt that layers
// the role default, repo-wide instructions, and the matching worker's.
// ===========================================================================

#[tokio::test]
async fn get_task_returns_head_branch_base_and_layered_prompt() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let issue = IssueView {
        number: 7,
        title: "Fix it".into(),
        body: "broken".into(),
        comments: vec![],
    };
    let fx = fixture_with(&pool, "issue#7", FakeGitHub::new().with_issue(issue)).await;

    // Store a config whose worker name matches the fixture's work item so the
    // per-worker instruction layer participates in the composed prompt.
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    let yaml = format!(
        "version: 1\n\
         settings:\n  label: factory\n  instructions: \"Repo-wide house style.\"\n\
         workers:\n  - name: {worker}\n    role: coder\n    on: [manager]\n    instructions: \"Prefer small commits.\"\n",
        worker = item.worker_name
    );
    let cfg = factory_config::parse(&yaml).expect("valid config");
    let cfg_json = serde_json::to_value(&cfg).unwrap();
    record::set_repo_config(&pool, fx.auth.repo_id, &cfg_json, "sha-cfg")
        .await
        .unwrap();

    let (status, body) = http_call(fx.state.clone(), &fx.raw_token, "get_task", json!({})).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(body["head_branch"], "factory/issue-7");
    assert_eq!(body["base"], "main");

    let prompt = body["prompt"].as_str().expect("prompt is a string");
    assert!(!prompt.is_empty(), "prompt must be non-empty");
    // All three layers are present and in order.
    assert!(
        prompt.contains("You are a coder."),
        "role default present: {prompt}"
    );
    assert!(
        prompt.contains("Repo-wide house style."),
        "repo-wide layer present: {prompt}"
    );
    assert!(
        prompt.contains("Prefer small commits."),
        "per-worker layer present: {prompt}"
    );
}

#[tokio::test]
async fn get_task_prompt_falls_back_to_role_default_without_config() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    // No config stored on this repo → prompt is just the role default.
    let issue = IssueView {
        number: 71,
        title: "Fix it".into(),
        body: "broken".into(),
        comments: vec![],
    };
    let fx = fixture_with(&pool, "issue#71", FakeGitHub::new().with_issue(issue)).await;
    let (status, body) = http_call(fx.state.clone(), &fx.raw_token, "get_task", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["head_branch"], "factory/issue-71");
    assert_eq!(body["base"], "main");
    let prompt = body["prompt"].as_str().expect("prompt is a string");
    assert!(prompt.contains("You are a coder."), "got: {prompt}");
}

// ===========================================================================
// FIX 3 — record_outcome revokes the task's tokens; the token can no longer act.
// ===========================================================================

#[tokio::test]
async fn record_outcome_revokes_task_tokens() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#33", FakeGitHub::new()).await;
    let token_hash = record::hash_token(&fx.raw_token);

    // Before completion the token resolves to this task.
    assert!(
        record::resolve_task_token(&pool, &token_hash)
            .await
            .unwrap()
            .is_some(),
        "token authorizes before the task ends"
    );

    factory_tools::record_outcome(&fx.state, &fx.auth, "succeeded", "done")
        .await
        .unwrap();

    // After completion the token no longer resolves.
    assert!(
        record::resolve_task_token(&pool, &token_hash)
            .await
            .unwrap()
            .is_none(),
        "token is revoked when the task ends"
    );

    // A subsequent tool HTTP call with that bearer is rejected with 401.
    let (status, _) = http_call(fx.state.clone(), &fx.raw_token, "heartbeat", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ===========================================================================
// FIX 4 — the audit row carries the work item's correlation_id and a
// `tool:<name>` actor.
// ===========================================================================

#[tokio::test]
async fn audit_event_has_correlation_id_and_tool_actor() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#19", FakeGitHub::new()).await;
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();

    let (status, _) = http_call(fx.state.clone(), &fx.raw_token, "heartbeat", json!({})).await;
    assert_eq!(status, StatusCode::OK);

    // Inspect the latest audit row for this work item.
    let row: (Option<Uuid>, String, String) = sqlx::query_as(
        "SELECT correlation_id, actor, action FROM events
          WHERE work_item_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(fx.work_item_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        row.0,
        Some(item.correlation_id),
        "audit correlation_id matches the work item's"
    );
    assert_eq!(row.1, "tool:heartbeat", "actor is tool:<name>");
    assert_eq!(row.2, "heartbeat", "action is the tool name");
}

// ===========================================================================
// FIX 4 — client (400) vs internal (500) error classification.
// A missing required arg and an invalid enum value are the CALLER's fault → 400,
// with the message echoed back. (Internal failures map to 500 with a generic body;
// see the unit-level mapping — simulating real DB/GitHub I/O failures here is not
// feasible against a live Postgres + fake GitHub, so the 500 path is asserted by
// construction in the lib's `ToolError::Internal` mapping.)
// ===========================================================================

#[tokio::test]
async fn missing_required_arg_is_400_bad_request() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#201", FakeGitHub::new()).await;

    // read_issue requires issue_number; omitting it is a bad request.
    let (status, body) = http_call(fx.state.clone(), &fx.raw_token, "read_issue", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("issue_number"),
        "400 echoes the offending argument, got {body}"
    );
}

#[tokio::test]
async fn invalid_verdict_is_400_bad_request() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "cr#202", FakeGitHub::new()).await;

    // An unknown verdict value is rejected as a bad request before any side effect.
    let (status, body) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "submit_review",
        json!({ "cr_number": 202, "verdict": "lgtm", "body": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("verdict"),
        "400 explains the bad verdict, got {body}"
    );
    // No review was submitted.
    assert_eq!(count_calls(&fx.gh, "submit_review"), 0);
}

// ===========================================================================
// FIX 1 — poison-pending recovery. A side effect that FAILS after winning the
// idempotency claim must release the placeholder so the NEXT retry re-claims and
// succeeds — at-most-once AND eventually-once. Without FIX 1(a)/(b) the first
// failure wedges every retry as Pending (500) forever and the comment/escalation
// can never be made.
// ===========================================================================

#[tokio::test]
async fn idempotency_recovers_after_side_effect_failure() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    // Arm the FakeGitHub so the FIRST post_comment fails once, then succeeds.
    let fx = fixture_with(
        &pool,
        "issue#301",
        FakeGitHub::new().with_fail_next("post_comment"),
    )
    .await;

    let args = json!({ "target": "issue#301", "body": "hello", "dedupe_key": "greet" });

    // First call: the claim is won, the GitHub side effect fails → 500, and the
    // placeholder is released.
    let (s1, _) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "post_comment",
        args.clone(),
    )
    .await;
    assert_eq!(
        s1,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the failed side effect surfaces as 500"
    );
    // No successful post recorded by the fake yet.
    assert_eq!(count_calls(&fx.gh, "post_comment"), 0);

    // Second call with the SAME dedupe_key: re-claims immediately (placeholder was
    // released), posts successfully, and records the real result.
    let (s2, b2) = http_call(fx.state.clone(), &fx.raw_token, "post_comment", args).await;
    assert_eq!(s2, StatusCode::OK, "retry succeeds");
    let comment_id = b2["comment_id"].as_i64().expect("comment_id");

    // EXACTLY ONE successful post overall, and the idempotency row now holds the real
    // (non-null) result.
    assert_eq!(
        count_calls(&fx.gh, "post_comment"),
        1,
        "exactly one successful post after failure + retry"
    );
    let stored: Value = sqlx::query_scalar(
        "SELECT result FROM tool_idempotency
          WHERE work_item_id = $1 AND scope = 'comment' AND dedupe_key = 'greet'",
    )
    .bind(fx.work_item_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored["comment_id"].as_i64(),
        Some(comment_id),
        "the idempotency row holds the real comment result, not the placeholder"
    );
}

#[tokio::test]
async fn escalate_recovers_after_side_effect_failure() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    // escalate's "side effect" runs under a won idempotency claim; if it fails before
    // idempotency_complete, the wrapper releases the placeholder so the next retry
    // re-claims and succeeds. We simulate a winner that crashed after claiming (the
    // poison-pending case) by manually claiming the escalate slot and then releasing
    // it via the same path the tool takes on failure, then assert a real escalate
    // succeeds and produces EXACTLY ONE escalation row (eventually-once liveness).
    let fx = fixture_with(&pool, "issue#302", FakeGitHub::new()).await;
    let key = "esc";

    // A prior attempt won the claim then died before completing (poison placeholder).
    let claim = record::idempotency_claim(&pool, fx.work_item_id, "escalate", key)
        .await
        .unwrap();
    assert_eq!(
        claim,
        record::IdempotencyClaim::Won,
        "first claim wins the placeholder"
    );
    // Its side effect failed: it releases the placeholder (FIX 1a/b) instead of
    // leaving it to poison every retry.
    record::idempotency_release(&pool, fx.work_item_id, "escalate", key)
        .await
        .unwrap();

    // The retry through the real tool re-claims immediately and succeeds.
    let (s, b) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "escalate",
        json!({ "reason": "needs_decision", "context": "which db?", "dedupe_key": key }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "retry after release succeeds");
    assert!(b["escalation_id"].is_string());

    // EXACTLY ONE escalation row overall — the failed attempt created none, the retry
    // created exactly one.
    assert_eq!(
        record::count_escalations_for_item(&pool, fx.work_item_id)
            .await
            .unwrap(),
        1,
        "exactly one escalation row after failed-then-succeeded escalate"
    );

    // And the slot now holds the real (non-null) result.
    let stored: Value = sqlx::query_scalar(
        "SELECT result FROM tool_idempotency
          WHERE work_item_id = $1 AND scope = 'escalate' AND dedupe_key = $2",
    )
    .bind(fx.work_item_id)
    .bind(key)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !stored.is_null() && stored["escalation_id"].is_string(),
        "the escalate slot holds the real result, not the placeholder"
    );
}

// ===========================================================================
// FIX 4 — post_comment with a dedupe_key UPDATES the same comment on re-run with a
// DIFFERENT body (create once, update on the re-run; never a second create).
// ===========================================================================

#[tokio::test]
async fn post_comment_updates_same_comment_on_rerun() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#303", FakeGitHub::new()).await;

    let (s1, b1) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "post_comment",
        json!({ "target": "issue#303", "body": "first body", "dedupe_key": "k" }),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);

    // Re-run with the SAME dedupe_key but a DIFFERENT body.
    let (s2, b2) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "post_comment",
        json!({ "target": "issue#303", "body": "second body", "dedupe_key": "k" }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);

    // Same (stable) comment id both times.
    assert_eq!(b1["comment_id"], b2["comment_id"]);

    // EXACTLY ONE create and EXACTLY ONE update — the re-run updated, not created.
    assert_eq!(
        count_calls(&fx.gh, "post_comment"),
        1,
        "exactly one create_comment"
    );
    assert_eq!(
        count_calls(&fx.gh, "update_comment"),
        1,
        "the re-run updated the same comment"
    );
}

// ===========================================================================
// FIX 3 — escalate maps reason to resumable (blocked) vs terminal (escalated).
// ===========================================================================

#[tokio::test]
async fn escalate_blocked_input_is_resumable_and_terminal_reasons_escalate() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };

    // blocked_input → Blocked, and a human reply (requeue_blocked) re-queues it.
    let fx = fixture_with(&pool, "issue#304", FakeGitHub::new()).await;
    let (s, b) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "escalate",
        json!({ "reason": "blocked_input", "context": "need an API key", "dedupe_key": "b" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["state"], "blocked");

    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, State::Blocked, "blocked_input parks at blocked");

    // The attempt is FINISHED as `blocked`, not left running — so after the re-dispatch
    // its stale started_at can't trip the wall-clock cap on the resumed run.
    let attempt_status: String = sqlx::query_scalar("SELECT status FROM attempts WHERE id = $1")
        .bind(fx.auth.attempt_id.unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        attempt_status, "blocked",
        "the blocked attempt is finished, not left running"
    );

    // A human reply re-queues exactly this blocked item.
    let requeued = record::requeue_blocked(&pool, item.repo_id, "issue#304")
        .await
        .unwrap();
    assert_eq!(requeued, 1, "the blocked item is re-queued on a reply");
    let item = record::work_item_by_id(&pool, fx.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(item.state, State::Queued, "re-queued back to queued");

    // retries_exhausted → Escalated (terminal), not re-queued by requeue_blocked.
    let fx2 = fixture_with(&pool, "issue#305", FakeGitHub::new()).await;
    let (s2, b2) = http_call(
        fx2.state.clone(),
        &fx2.raw_token,
        "escalate",
        json!({ "reason": "retries_exhausted", "context": "gave up", "dedupe_key": "r" }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["state"], "escalated");
    let item2 = record::work_item_by_id(&pool, fx2.work_item_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        item2.state,
        State::Escalated,
        "retries_exhausted is terminal"
    );
    // requeue_blocked never touches a terminal (escalated) item.
    let requeued2 = record::requeue_blocked(&pool, item2.repo_id, "issue#305")
        .await
        .unwrap();
    assert_eq!(requeued2, 0, "escalated items are not re-queued");
}

// ===========================================================================
// FIX 5 — variants through entry points: submit_review request_changes & comment;
// post_comment to a cr#N target; concurrent submit_review dedupe (exactly one).
// ===========================================================================

#[tokio::test]
async fn submit_review_request_changes_and_comment_through_dispatch() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };

    // request_changes
    let fx = fixture_with(&pool, "cr#401", FakeGitHub::new()).await;
    let (s, _) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "submit_review",
        json!({ "cr_number": 401, "verdict": "request_changes", "body": "please fix" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        count_calls(&fx.gh, "submit_review cr#401 request_changes"),
        1
    );

    // comment
    let fx2 = fixture_with(&pool, "cr#402", FakeGitHub::new()).await;
    let (s2, _) = http_call(
        fx2.state.clone(),
        &fx2.raw_token,
        "submit_review",
        json!({ "cr_number": 402, "verdict": "comment", "body": "a note" }),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(count_calls(&fx2.gh, "submit_review cr#402 comment"), 1);
}

#[tokio::test]
async fn post_comment_to_change_request_target_through_dispatch() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "cr#403", FakeGitHub::new()).await;

    // A cr#N target drives CommentTarget::ChangeRequest through dispatch.
    let (s, _) = http_call(
        fx.state.clone(),
        &fx.raw_token,
        "post_comment",
        json!({ "target": "cr#403", "body": "on the change request" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        count_calls(&fx.gh, "post_comment ChangeRequest(403)"),
        1,
        "the comment targeted the change request"
    );
}

#[tokio::test]
async fn concurrent_submit_review_with_key_submits_exactly_once() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "cr#404", FakeGitHub::new()).await;

    // Concurrent submit_review with the SAME dedupe_key: exactly one review under
    // concurrency (winner submits; losers return the stored result (200) or are told
    // to retry (409, a retryable conflict — never 500)).
    let mut handles = Vec::new();
    for _ in 0..6 {
        let state = fx.state.clone();
        let token = fx.raw_token.clone();
        handles.push(tokio::spawn(async move {
            http_call(
                state,
                &token,
                "submit_review",
                json!({ "cr_number": 404, "verdict": "approve", "body": "LGTM", "dedupe_key": "once" }),
            )
            .await
        }));
    }
    for h in handles {
        let (status, _) = h.await.unwrap();
        assert!(
            status == StatusCode::OK || status == StatusCode::CONFLICT,
            "loser is a retryable 409 conflict, never 500; got {status}"
        );
    }

    assert_eq!(
        count_calls(&fx.gh, "submit_review"),
        1,
        "exactly one review submitted despite concurrent calls"
    );
}

// Variant coverage: submit_review's record_verdict_cr_state side effect — approve →
// approved, request_changes → changes_requested, comment → no state change — and
// record_outcome's terminal statuses driven through the REST HTTP route.

#[tokio::test]
async fn submit_review_verdict_reflects_onto_cr_factory_state() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };

    // approve → approved
    let fa = fixture_with(&pool, "cr#601", FakeGitHub::new()).await;
    let cra = record::upsert_change_request_for_item(
        &pool,
        fa.auth.repo_id,
        fa.work_item_id,
        "factory/cr-601",
        Some(1),
    )
    .await
    .unwrap();
    record::set_change_request_number(&pool, cra.id, 601)
        .await
        .unwrap();
    let (s, _) = http_call(
        fa.state.clone(),
        &fa.raw_token,
        "submit_review",
        json!({ "cr_number": 601, "verdict": "approve", "body": "lgtm", "dedupe_key": "r" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        record::change_request_by_number(&pool, fa.auth.repo_id, 601)
            .await
            .unwrap()
            .unwrap()
            .factory_state,
        "approved"
    );

    // request_changes → changes_requested
    let fr = fixture_with(&pool, "cr#602", FakeGitHub::new()).await;
    let crr = record::upsert_change_request_for_item(
        &pool,
        fr.auth.repo_id,
        fr.work_item_id,
        "factory/cr-602",
        Some(1),
    )
    .await
    .unwrap();
    record::set_change_request_number(&pool, crr.id, 602)
        .await
        .unwrap();
    let (s, _) = http_call(
        fr.state.clone(),
        &fr.raw_token,
        "submit_review",
        json!({ "cr_number": 602, "verdict": "request_changes", "body": "nope", "dedupe_key": "r" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        record::change_request_by_number(&pool, fr.auth.repo_id, 602)
            .await
            .unwrap()
            .unwrap()
            .factory_state,
        "changes_requested"
    );

    // comment → no state change (stays at its initial draft)
    let fc = fixture_with(&pool, "cr#603", FakeGitHub::new()).await;
    let crc = record::upsert_change_request_for_item(
        &pool,
        fc.auth.repo_id,
        fc.work_item_id,
        "factory/cr-603",
        Some(1),
    )
    .await
    .unwrap();
    record::set_change_request_number(&pool, crc.id, 603)
        .await
        .unwrap();
    let initial = record::change_request_by_number(&pool, fc.auth.repo_id, 603)
        .await
        .unwrap()
        .unwrap()
        .factory_state;
    let (s, _) = http_call(
        fc.state.clone(),
        &fc.raw_token,
        "submit_review",
        json!({ "cr_number": 603, "verdict": "comment", "body": "fyi", "dedupe_key": "r" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        record::change_request_by_number(&pool, fc.auth.repo_id, 603)
            .await
            .unwrap()
            .unwrap()
            .factory_state,
        initial,
        "a comment verdict does not change the CR factory state"
    );
}

#[tokio::test]
async fn record_outcome_terminal_statuses_through_rest_route() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    // succeeded and failed are work-item-terminal; blocked is resumable. Drive each
    // through the REST HTTP route (not just the lib fn) and assert the 200 + state.
    for (status, want_state) in [
        ("succeeded", "succeeded"),
        ("failed", "failed"),
        ("blocked", "blocked"),
    ] {
        let fx = fixture_with(&pool, &format!("issue#{status}"), FakeGitHub::new()).await;
        let (s, body) = http_call(
            fx.state.clone(),
            &fx.raw_token,
            "record_outcome",
            json!({ "status": status, "summary": "via rest" }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{status} returns 200 over REST");
        assert_eq!(body["state"], want_state, "{status} sets work item state");
        let item = record::work_item_by_id(&pool, fx.work_item_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.state.as_str(), want_state);
    }
}

#[tokio::test]
async fn post_comment_redispatch_of_updated_body_does_not_reput() {
    // Regression for the redo path: after a genuine body change (A → B), an IDENTICAL
    // re-dispatch of the now-current body B must NOT re-PUT (which would clobber a human
    // edit made after the B write). The fix re-persists the last-written body so the
    // equality check compares against B, not the original A.
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#1701", FakeGitHub::new()).await;
    let mk = |body: &str| json!({ "target": "issue#1701", "body": body, "dedupe_key": "k" });

    // A: first post. Each call must return 200 — otherwise a redo-path error could make
    // this test pass for the wrong reason (GitHub call counts alone wouldn't catch it).
    let (sa, _) = http_call(fx.state.clone(), &fx.raw_token, "post_comment", mk("A")).await;
    assert_eq!(sa, StatusCode::OK, "first post returns 200");
    // B: changed body → one update.
    let (sb, _) = http_call(fx.state.clone(), &fx.raw_token, "post_comment", mk("B")).await;
    assert_eq!(sb, StatusCode::OK, "body-change re-run returns 200");
    assert_eq!(count_calls(&fx.gh, "update_comment"), 1, "A→B updates once");
    // B again: identical re-dispatch of the current body → NO further update.
    let (sb2, _) = http_call(fx.state.clone(), &fx.raw_token, "post_comment", mk("B")).await;
    assert_eq!(
        sb2,
        StatusCode::OK,
        "identical re-dispatch still returns 200"
    );
    assert_eq!(
        count_calls(&fx.gh, "update_comment"),
        1,
        "an identical re-dispatch of the last-written body must not re-PUT"
    );
    // Exactly one comment was ever created.
    assert_eq!(count_calls(&fx.gh, "post_comment"), 1);
}
