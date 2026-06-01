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

// ---------------------------------------------------------------------------
// P4.5: the per-role worker lifecycle driven through the REAL HttpTools ↔ Tools
// router over the wire, proving a Manager and a Reviewer run produce NO change
// request (only the Coder does) and perform their role's terminal action.
// ---------------------------------------------------------------------------

use factory_worker::{ClaudeRunner, HarnessConfig, ReviewVerdict, RunOutcome, Workspace};
use std::path::Path;

/// A git double that panics if used — a Manager/Reviewer must never clone or push.
struct NoGit;

#[async_trait::async_trait]
impl Workspace for NoGit {
    async fn shallow_clone(&self, _u: &str, _t: &str, _d: &Path) -> anyhow::Result<()> {
        panic!("a non-coder role must not clone");
    }
    async fn push_branch(&self, _d: &Path, _b: &str) -> anyhow::Result<()> {
        panic!("a non-coder role must not push");
    }
}

/// A Claude double that returns a fixed role outcome.
struct FixedClaude(RunOutcome);

#[async_trait::async_trait]
impl ClaudeRunner for FixedClaude {
    async fn run(
        &self,
        _workdir: &Path,
        _role: &str,
        _prompt: &str,
        _tools_url: &str,
        _token: &str,
    ) -> anyhow::Result<RunOutcome> {
        Ok(self.0.clone())
    }
}

/// Lease a work item of `kind` with `subject`, mint its token, and serve the REAL
/// Tools router on an ephemeral port. Returns (base_url, raw_token, item, server).
async fn serve_with_item(
    pool: &record::Pool,
    gh: Arc<FakeGitHub>,
    repo_id: Uuid,
    kind: record::Kind,
    subject: &str,
) -> (
    String,
    String,
    record::WorkItem,
    tokio::task::JoinHandle<()>,
) {
    let worker_name = "w".to_string();
    record::enqueue(
        pool,
        &record::NewWorkItem {
            repo_id,
            kind,
            worker_name: worker_name.clone(),
            config_sha: "test-sha".into(),
            trigger: "manager".into(),
            subject_ref: subject.into(),
            correlation_id: Uuid::now_v7(),
            priority: 0,
            available_at: None,
        },
    )
    .await
    .expect("enqueue");
    let item = record::lease_next(
        pool,
        &record::LeaseRequest {
            repo_id,
            worker_name,
            lease_owner: "test".into(),
            ttl_secs: 300.0,
            per_worker_cap: 10,
            global_cap: 10,
        },
    )
    .await
    .expect("lease")
    .expect("a leasable item");
    let attempt_id = record::start_attempt(pool, item.id, None)
        .await
        .expect("attempt");
    let raw_token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    record::mint_task_token(
        pool,
        &record::hash_token(&raw_token),
        item.id,
        repo_id,
        kind,
        Some(attempt_id),
        300.0,
    )
    .await
    .expect("mint token");

    let state = Arc::new(ToolsState {
        pool: pool.clone(),
        gh,
        blob: Arc::new(InMemoryTranscripts::new()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, build_router(state)).await;
    });
    (format!("http://{addr}"), raw_token, item, server)
}

fn harness(base_url: &str, token: &str) -> HarnessConfig {
    HarnessConfig {
        repo_url: "https://github.com/acme/repo.git".into(),
        install_token: "ghs_x".into(),
        tools_url: base_url.into(),
        task_token: token.into(),
        workdir: std::env::temp_dir().join(unique("p45-wd")),
    }
}

#[tokio::test]
async fn manager_lifecycle_over_the_wire_posts_comment_and_opens_no_cr() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let name = unique("worker-p45-manager");
    let repo_id = record::upsert_repo(&pool, "acme", &name, 7)
        .await
        .expect("repo");
    let gh = Arc::new(FakeGitHub::new().with_issue(IssueView {
        number: 5,
        title: "Add rate limiting".into(),
        body: "details".into(),
        comments: vec![],
    }));
    let (base, token, item, server) =
        serve_with_item(&pool, gh.clone(), repo_id, record::Kind::Manager, "issue#5").await;

    let claude = FixedClaude(RunOutcome::Triage {
        actionable: true,
        summary: "In scope; assigning a coder".into(),
    });
    factory_worker::run_task(
        &HttpTools::new(base.clone(), token.clone()),
        &NoGit,
        &claude,
        &harness(&base, &token),
    )
    .await
    .expect("manager run succeeds");

    // NO change request was created by the manager...
    assert!(
        record::change_request_for_item(&pool, item.id)
            .await
            .unwrap()
            .is_none(),
        "manager opens no CR"
    );
    // ...a triage comment was posted on the issue...
    assert!(
        gh.calls()
            .iter()
            .any(|c| c.contains("post_comment") && c.contains("Issue(5)")),
        "manager posted a triage comment on the issue: {:?}",
        gh.calls()
    );
    // ...and the work item succeeded (so the Coordinator will assign a coder).
    let final_item = record::work_item_by_id(&pool, item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(final_item.state, record::State::Succeeded);

    server.abort();
}

#[tokio::test]
async fn reviewer_lifecycle_over_the_wire_submits_verdict_and_opens_no_cr() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let name = unique("worker-p45-reviewer");
    let repo_id = record::upsert_repo(&pool, "acme", &name, 7)
        .await
        .expect("repo");
    let gh = Arc::new(FakeGitHub::new());
    let (base, token, item, server) =
        serve_with_item(&pool, gh.clone(), repo_id, record::Kind::Reviewer, "cr#12").await;

    let claude = FixedClaude(RunOutcome::Review {
        verdict: ReviewVerdict::RequestChanges,
        summary: "Please add tests".into(),
    });
    factory_worker::run_task(
        &HttpTools::new(base.clone(), token.clone()),
        &NoGit,
        &claude,
        &harness(&base, &token),
    )
    .await
    .expect("reviewer run succeeds");

    // NO change request created by the reviewer...
    assert!(
        record::change_request_for_item(&pool, item.id)
            .await
            .unwrap()
            .is_none(),
        "reviewer opens no CR"
    );
    // ...a verdict was submitted on the CR...
    assert!(
        gh.calls()
            .iter()
            .any(|c| c.contains("submit_review") && c.contains("request_changes")),
        "reviewer submitted a verdict: {:?}",
        gh.calls()
    );
    // ...and the work item succeeded.
    let final_item = record::work_item_by_id(&pool, item.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(final_item.state, record::State::Succeeded);

    server.abort();
}

// ---------------------------------------------------------------------------
// HttpTools retries a transient 409 Conflict (Tools idempotency claim held by a
// concurrent attempt) instead of surfacing it as a failure.
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn http_tools_retries_409_conflict_then_succeeds() {
    use axum::http::StatusCode as Sc;
    use axum::routing::post;
    use axum::Json;

    // A mock Tools endpoint that returns 409 on the first call, then 200.
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let hits_route = hits.clone();
    let app = axum::Router::new().route(
        "/tools/heartbeat",
        post(move || {
            let n = hits_route.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    (
                        Sc::CONFLICT,
                        Json(serde_json::json!({"error":"claim in flight"})),
                    )
                } else {
                    (Sc::OK, Json(serde_json::json!({"ok":true})))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let tools = HttpTools::new(format!("http://{addr}"), "tok".into());
    // heartbeat() goes through call_tool; it must succeed despite the first 409.
    tools
        .heartbeat()
        .await
        .expect("heartbeat retries past the 409 and succeeds");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "first call 409'd, the retry succeeded (exactly two attempts)"
    );

    server.abort();
}

#[tokio::test]
async fn http_tools_persistent_409_surfaces_claim_contended() {
    use axum::http::StatusCode as Sc;
    use axum::routing::post;
    use axum::Json;
    use factory_worker::ClaimContended;

    // A mock Tools endpoint that ALWAYS returns 409 (a concurrent attempt holds the
    // claim for the whole window). call_tool must exhaust its retry budget and surface
    // ClaimContended — NOT a generic error — so run_task can bow out without
    // terminalizing the work item.
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let hits_route = hits.clone();
    let app = axum::Router::new().route(
        "/tools/heartbeat",
        post(move || {
            hits_route.fetch_add(1, Ordering::SeqCst);
            async move {
                (
                    Sc::CONFLICT,
                    Json(serde_json::json!({"error": "claim in flight"})),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let tools = HttpTools::new(format!("http://{addr}"), "tok".into());
    let err = tools
        .heartbeat()
        .await
        .expect_err("persistent 409 must surface an error");
    assert!(
        err.downcast_ref::<ClaimContended>().is_some(),
        "persistent 409 surfaces ClaimContended (non-terminal), got: {err}"
    );
    // Tried the full bounded budget (more than one attempt), not just once.
    assert!(
        hits.load(Ordering::SeqCst) >= 6,
        "exhausted the retry budget: {} attempts",
        hits.load(Ordering::SeqCst)
    );

    server.abort();
}
