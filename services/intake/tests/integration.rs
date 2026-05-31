//! Intake integration tests against a real Postgres. Gated on `DATABASE_URL`
//! (skip cleanly when unset). Isolated per-run via unique repo names.

use factory_github::{RepoRef, Subject, WebhookEvent};
use factory_intake::{ingest, IngestOutcome};
use factory_record as record;
use uuid::Uuid;

async fn pool_or_skip() -> Option<record::Pool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = record::connect(&url).await.expect("connect");
    record::migrate(&pool).await.expect("migrate");
    Some(pool)
}

fn unique(p: &str) -> String {
    format!("{p}-{}", Uuid::now_v7())
}

/// Register a repo with a config containing a single manager subscribed to
/// issue_labeled. Returns (repo_id, owner, name).
async fn repo_with_manager(pool: &record::Pool, slug: &str) -> (Uuid, String, String) {
    let name = unique(slug);
    let id = record::upsert_repo(pool, "acme", &name, 1).await.unwrap();
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
    assert_eq!(
        factory_intake::apply_config(pool, id, yaml, "sha1")
            .await
            .unwrap(),
        factory_intake::ApplyOutcome::Applied
    );
    (id, "acme".to_owned(), name)
}

#[tokio::test]
async fn redelivered_webhook_is_deduped() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let (repo, owner, name) = repo_with_manager(&pool, "intake-dedupe").await;
    let event = WebhookEvent::IssueLabeled {
        repo: RepoRef {
            owner,
            name,
            installation_id: 1,
        },
        issue: 7,
        label: "factory".into(),
    };
    let delivery = format!("delivery-{}", Uuid::now_v7());

    // First delivery enqueues; the redelivery (same id) is a no-op.
    assert_eq!(
        ingest(&pool, &event, &delivery).await.unwrap(),
        IngestOutcome::Enqueued(1)
    );
    assert_eq!(
        ingest(&pool, &event, &delivery).await.unwrap(),
        IngestOutcome::Duplicate
    );
    // Exactly one work item despite two deliveries.
    assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 1);
}

#[tokio::test]
async fn issue_comment_resumes_a_blocked_item() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let (repo, owner, name) = repo_with_manager(&pool, "intake-resume").await;

    // A manager work item exists and is blocked waiting on a human.
    let labeled = WebhookEvent::IssueLabeled {
        repo: RepoRef {
            owner: owner.clone(),
            name: name.clone(),
            installation_id: 1,
        },
        issue: 42,
        label: "factory".into(),
    };
    ingest(&pool, &labeled, &format!("d-{}", Uuid::now_v7()))
        .await
        .unwrap();
    // Lease it and block it (stand-in for the worker asking a question).
    let leased = record::lease_next(
        &pool,
        &record::LeaseRequest {
            repo_id: repo,
            worker_name: "manager".into(),
            lease_owner: "t".into(),
            ttl_secs: 60.0,
            per_worker_cap: 10,
            global_cap: 10,
        },
    )
    .await
    .unwrap()
    .unwrap();
    record::set_state(&pool, leased.id, record::State::Blocked)
        .await
        .unwrap();

    // A human comment on issue#42 resumes it.
    let comment = WebhookEvent::IssueComment {
        repo: RepoRef {
            owner,
            name,
            installation_id: 1,
        },
        subject: Subject::Issue(42),
    };
    assert_eq!(
        ingest(&pool, &comment, &format!("d-{}", Uuid::now_v7()))
            .await
            .unwrap(),
        IngestOutcome::Resumed(1)
    );
    // It's leasable again.
    let resumed = record::lease_next(
        &pool,
        &record::LeaseRequest {
            repo_id: repo,
            worker_name: "manager".into(),
            lease_owner: "t".into(),
            ttl_secs: 60.0,
            per_worker_cap: 10,
            global_cap: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(resumed.map(|w| w.id), Some(leased.id));
}

#[tokio::test]
async fn unknown_repo_is_ignored() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // No repo registered with this owner/name.
    let event = WebhookEvent::IssueLabeled {
        repo: RepoRef {
            owner: "nobody".into(),
            name: unique("ghost"),
            installation_id: 1,
        },
        issue: 1,
        label: "factory".into(),
    };
    assert_eq!(
        ingest(&pool, &event, &format!("d-{}", Uuid::now_v7()))
            .await
            .unwrap(),
        IngestOutcome::UnknownRepo
    );
}
