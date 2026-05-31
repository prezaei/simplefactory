//! FIX 1 integration test: the worker's REAL `HttpTools` against the REAL Tools
//! `build_router`, over the wire on an ephemeral port.
//!
//! This proves the two seams actually connect: `HttpTools` posts to
//! `POST {base}/tools/{tool}` and the router serves exactly that surface. Before
//! the fix `HttpTools` posted to bare paths (`task`, `change_request`, …) which
//! the router 404s, so this exercise would fail end-to-end.
//!
//! Gated on `DATABASE_URL` (skips cleanly when unset), like the rest of the
//! DB-backed suite.
#![forbid(unsafe_code)]

use std::sync::Arc;

use factory_blob::InMemoryTranscripts;
use factory_github::{FakeGitHub, IssueView};
use factory_record as record;
use factory_tools::{build_router, ToolsState};
use factory_worker::{CrRequest, HttpTools, ToolsClient};
use uuid::Uuid;

async fn pool_or_skip() -> Option<record::Pool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = record::connect(&url)
        .await
        .expect("connect to DATABASE_URL");
    record::migrate(&pool).await.expect("run migrations");
    Some(pool)
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7())
}

#[tokio::test]
async fn http_tools_drives_tools_router_over_the_wire() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };

    // Seed a repo with an issue the worker's task points at.
    let owner = "acme";
    let name = unique("worker-http-roundtrip");
    let repo_id = record::upsert_repo(&pool, owner, &name, 7)
        .await
        .expect("upsert repo");

    let issue_number = 7;
    let gh = Arc::new(FakeGitHub::new().with_issue(IssueView {
        number: issue_number,
        title: "Fix the widget".into(),
        body: "It is broken".into(),
        comments: vec![],
    }));
    let blob = Arc::new(InMemoryTranscripts::new());

    // Enqueue a coder work item, then lease it (so it is in-flight) and open an
    // attempt — exactly the state a launched worker would find.
    let correlation_id = Uuid::now_v7();
    record::enqueue(
        &pool,
        &record::NewWorkItem {
            repo_id,
            kind: record::Kind::Coder,
            worker_name: "bug-fixer".into(),
            config_sha: "test-sha".into(),
            trigger: "manager".into(),
            subject_ref: format!("issue#{issue_number}"),
            correlation_id,
            priority: 0,
            available_at: None,
        },
    )
    .await
    .expect("enqueue");

    let item = record::lease_next(
        &pool,
        &record::LeaseRequest {
            repo_id,
            worker_name: "bug-fixer".into(),
            lease_owner: "test".into(),
            ttl_secs: 300.0,
            per_worker_cap: 10,
            global_cap: 10,
        },
    )
    .await
    .expect("lease_next")
    .expect("a leasable item");

    let attempt_id = record::start_attempt(&pool, item.id, None)
        .await
        .expect("start attempt");

    // Mint the per-task token the worker would carry.
    let raw_token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    record::mint_task_token(
        &pool,
        &record::hash_token(&raw_token),
        item.id,
        repo_id,
        record::Kind::Coder,
        Some(attempt_id),
        300.0,
    )
    .await
    .expect("mint token");

    // Serve the REAL Tools router on an ephemeral port.
    let state = Arc::new(ToolsState {
        pool: pool.clone(),
        gh,
        blob,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let router = build_router(state);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let base_url = format!("http://{addr}");
    let tools = HttpTools::new(base_url, raw_token);

    // Drive the worker's real client through the full happy sequence. Each of
    // these would 404 before the route-contract fix.
    let task = tools.get_task().await.expect("get_task over the wire");
    assert_eq!(task.role, "coder", "role from get_task");
    assert_eq!(task.subject_ref, format!("issue#{issue_number}"));
    assert_eq!(task.head_branch, format!("factory/issue-{issue_number}"));
    assert_eq!(task.base, "main");
    assert!(
        !task.prompt.is_empty(),
        "get_task returns a composed prompt"
    );

    tools.heartbeat().await.expect("heartbeat over the wire");

    let cr = tools
        .open_or_update_change_request(&CrRequest {
            title: "[coder] issue#7".into(),
            body: "Implemented".into(),
            head_branch: task.head_branch.clone(),
            base: task.base.clone(),
            linked_issue: Some(issue_number),
        })
        .await
        .expect("open_or_update_change_request over the wire");
    assert!(cr.cr_number > 0, "a CR number was assigned");

    tools
        .append_transcript("did the work")
        .await
        .expect("append_transcript over the wire");

    tools
        .record_outcome("succeeded", "all done")
        .await
        .expect("record_outcome over the wire");

    // The work item reached its terminal state.
    let final_item = record::work_item_by_id(&pool, item.id)
        .await
        .expect("load item")
        .expect("item exists");
    assert_eq!(final_item.state, record::State::Succeeded);

    server.abort();
}
