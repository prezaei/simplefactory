//! Integration tests for the MCP (JSON-RPC 2.0) transport on `POST /mcp`. These
//! drive the SAME `build_router` via tower `oneshot` and assert the JSON-RPC
//! envelopes, the tool listing, that `tools/call` routes to the real tool fns AND
//! writes an audit event, and the auth + error behaviors. DB-gated: they skip
//! cleanly when `DATABASE_URL` is unset.
#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use factory_blob::InMemoryTranscripts;
use factory_github::{FakeGitHub, IssueView};
use factory_record::{self as record, Kind, LeaseRequest, NewWorkItem, Pool, TaskAuth};
use factory_tools::{build_router, ToolsState};
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

async fn pool_or_skip() -> Option<Pool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let p = record::connect(&url).await.expect("connect");
    record::migrate(&p).await.expect("migrate");
    Some(p)
}

struct Fixture {
    state: Arc<ToolsState>,
    raw_token: String,
    work_item_id: Uuid,
}

/// A fully wired leased coder task with a minted token — same shape as the REST
/// integration tests, kept self-contained so this file stands alone.
async fn fixture_with(pool: &Pool, subject_ref: &str, gh: FakeGitHub) -> Fixture {
    let suffix = Uuid::now_v7();
    let name = format!("mcp-{suffix}");
    let repo_id = record::upsert_repo(pool, "acme", &name, 42)
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

    let _auth = TaskAuth {
        work_item_id: leased.id,
        repo_id,
        role: Kind::Coder,
        attempt_id: Some(attempt_id),
    };

    let state = Arc::new(ToolsState {
        pool: pool.clone(),
        gh: Arc::new(gh),
        blob: Arc::new(InMemoryTranscripts::new()),
    });

    Fixture {
        state,
        raw_token,
        work_item_id: leased.id,
    }
}

/// POST a JSON-RPC request to `/mcp` with the given bearer; return (status, body).
async fn mcp_call(
    state: Arc<ToolsState>,
    bearer: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json");
    if let Some(b) = bearer {
        builder = builder.header("authorization", format!("Bearer {b}"));
    }
    let req = builder
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

// ===========================================================================
// 1. initialize
// ===========================================================================

#[tokio::test]
async fn mcp_initialize_returns_protocol_and_server_info() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#1", FakeGitHub::new()).await;

    let (status, body) = mcp_call(
        fx.state.clone(),
        Some(&fx.raw_token),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(body["result"]["serverInfo"]["name"], "factory-tools");
    assert!(body["result"]["serverInfo"]["version"].is_string());
    assert!(body["result"]["capabilities"]["tools"].is_object());
}

// ===========================================================================
// 2. tools/list — exactly 11 tools, each with a name + inputSchema
// ===========================================================================

#[tokio::test]
async fn mcp_tools_list_lists_exactly_eleven_tools() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#2", FakeGitHub::new()).await;

    let (status, body) = mcp_call(
        fx.state.clone(),
        Some(&fx.raw_token),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let tools = body["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 11, "exactly 11 tools listed");
    for t in tools {
        assert!(t["name"].is_string(), "each tool has a name");
        assert_eq!(t["inputSchema"]["type"], "object", "each tool has a schema");
    }
}

// ===========================================================================
// 3. tools/call get_task — routes through the real tool fn AND audits
// ===========================================================================

#[tokio::test]
async fn mcp_tools_call_get_task_routes_and_audits() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let issue = IssueView {
        number: 7,
        title: "Fix the bug".into(),
        body: "details".into(),
        comments: vec![],
    };
    let fx = fixture_with(&pool, "issue#7", FakeGitHub::new().with_issue(issue)).await;

    let before = record::count_events_for_item(&pool, fx.work_item_id)
        .await
        .unwrap();

    let (status, body) = mcp_call(
        fx.state.clone(),
        Some(&fx.raw_token),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "get_task", "arguments": {} }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], 3);
    assert_eq!(body["result"]["isError"], false);
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    // The stringified tool result carries this task's facts.
    let inner: Value = serde_json::from_str(text).expect("content is the tool's JSON result");
    assert_eq!(inner["work_item_id"], fx.work_item_id.to_string());
    assert_eq!(inner["subject_ref"], "issue#7");
    assert_eq!(inner["subject"]["title"], "Fix the bug");

    // The MCP call wrote exactly one audit event, same as a REST call.
    let after = record::count_events_for_item(&pool, fx.work_item_id)
        .await
        .unwrap();
    assert_eq!(
        after - before,
        1,
        "tools/call writes exactly one audit event"
    );

    // And the audit row is a tool:<name> actor with the tool name as the action.
    let row: (String, String) = sqlx::query_as(
        "SELECT actor, action FROM events WHERE work_item_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(fx.work_item_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0, "tool:get_task");
    assert_eq!(row.1, "get_task");
}

// ===========================================================================
// 4. bad / absent bearer → 401
// ===========================================================================

#[tokio::test]
async fn mcp_bad_or_absent_bearer_is_401() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#8", FakeGitHub::new()).await;

    // Absent bearer.
    let (status, _) = mcp_call(
        fx.state.clone(),
        None,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Bogus bearer.
    let (status, _) = mcp_call(
        fx.state.clone(),
        Some("totally-bogus-token"),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ===========================================================================
// 5. unknown method → JSON-RPC error -32601
// ===========================================================================

#[tokio::test]
async fn mcp_unknown_method_is_method_not_found() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#9", FakeGitHub::new()).await;

    let (status, body) = mcp_call(
        fx.state.clone(),
        Some(&fx.raw_token),
        json!({ "jsonrpc": "2.0", "id": 42, "method": "does/not/exist" }),
    )
    .await;

    // JSON-RPC errors ride HTTP 200 with the error in the envelope.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], 42);
    assert_eq!(body["error"]["code"], -32601);
    assert!(body["result"].is_null());
}

// ===========================================================================
// 6. tools/call with an unknown tool name → JSON-RPC error (-32602)
// ===========================================================================

#[tokio::test]
async fn mcp_tools_call_unknown_tool_is_invalid_params() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#10", FakeGitHub::new()).await;

    let before = record::count_events_for_item(&pool, fx.work_item_id)
        .await
        .unwrap();

    let (status, body) = mcp_call(
        fx.state.clone(),
        Some(&fx.raw_token),
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": { "name": "no_such_tool", "arguments": {} }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32602);

    // An unknown tool never reaches the audited choke point → no audit event.
    let after = record::count_events_for_item(&pool, fx.work_item_id)
        .await
        .unwrap();
    assert_eq!(after - before, 0, "unknown tool writes no audit event");
}

// ===========================================================================
// 7. notifications/initialized — a notification with no response body
// ===========================================================================

#[tokio::test]
async fn mcp_notifications_initialized_has_no_body() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#11", FakeGitHub::new()).await;

    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("authorization", format!("Bearer {}", fx.raw_token))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .unwrap(),
        ))
        .unwrap();
    let resp = build_router(fx.state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(bytes.is_empty(), "notification has no response body");
}

// ===========================================================================
// 8. tools/call get_check_status with no args → tool error result (isError)
// ===========================================================================

#[tokio::test]
async fn mcp_tools_call_tool_error_is_is_error_result() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "cr#12", FakeGitHub::new()).await;

    // get_check_status with neither cr_number nor sha is a tool-level error.
    let (status, body) = mcp_call(
        fx.state.clone(),
        Some(&fx.raw_token),
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": { "name": "get_check_status", "arguments": {} }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // MCP convention: tool errors are results with isError, not JSON-RPC errors.
    assert!(
        body["error"].is_null(),
        "tool error is not a JSON-RPC error"
    );
    assert_eq!(body["result"]["isError"], true);
    assert!(body["result"]["content"][0]["text"].is_string());
}

// ===========================================================================
// 9. malformed body → parse error -32700
// ===========================================================================

#[tokio::test]
async fn mcp_malformed_body_is_parse_error() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#13", FakeGitHub::new()).await;

    // A non-object JSON body (an array) is a parse error once authorized.
    let (status, body) = mcp_call(fx.state.clone(), Some(&fx.raw_token), json!([1, 2, 3])).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32700);
}

// ===========================================================================
// 10. GET /mcp → 405 (no SSE stream)
// ===========================================================================

#[tokio::test]
async fn mcp_get_is_method_not_allowed() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let fx = fixture_with(&pool, "issue#14", FakeGitHub::new()).await;

    let req = Request::builder()
        .method("GET")
        .uri("/mcp")
        .body(Body::empty())
        .unwrap();
    let resp = build_router(fx.state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}
