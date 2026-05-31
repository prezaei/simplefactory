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

/// Register a repo (owner `acme`) with the given config YAML applied.
/// Returns (repo_id, owner, name).
async fn repo_with_yaml(pool: &record::Pool, slug: &str, yaml: &str) -> (Uuid, String, String) {
    let name = unique(slug);
    let id = record::upsert_repo(pool, "acme", &name, 1).await.unwrap();
    assert_eq!(
        factory_intake::apply_config(pool, id, yaml, "sha1")
            .await
            .unwrap(),
        factory_intake::ApplyOutcome::Applied
    );
    (id, "acme".to_owned(), name)
}

/// Register a repo with a config containing a single manager subscribed to
/// issue_labeled. Returns (repo_id, owner, name).
async fn repo_with_manager(pool: &record::Pool, slug: &str) -> (Uuid, String, String) {
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
    repo_with_yaml(pool, slug, yaml).await
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
async fn lost_event_is_redriven_not_burned() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // An event arrives before the repo is connected to Factory.
    let owner = "acme";
    let name = unique("intake-lostevent");
    let event = WebhookEvent::IssueLabeled {
        repo: RepoRef {
            owner: owner.into(),
            name: name.clone(),
            installation_id: 1,
        },
        issue: 5,
        label: "factory".into(),
    };
    let delivery = format!("delivery-{}", Uuid::now_v7());

    // Unknown repo → nothing enqueued, and the delivery id must NOT be recorded
    // (otherwise the redelivery below would be silently dropped as a duplicate).
    assert_eq!(
        ingest(&pool, &event, &delivery).await.unwrap(),
        IngestOutcome::UnknownRepo
    );

    // The repo is connected and configured.
    let id = record::upsert_repo(&pool, owner, &name, 1).await.unwrap();
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
    factory_intake::apply_config(&pool, id, yaml, "sha1")
        .await
        .unwrap();

    // GitHub redelivers the SAME delivery id → it must now enqueue, not dedupe.
    assert_eq!(
        ingest(&pool, &event, &delivery).await.unwrap(),
        IngestOutcome::Enqueued(1)
    );
    assert_eq!(record::count_for_repo(&pool, id).await.unwrap(), 1);
}

#[tokio::test]
async fn cr_opened_enqueues_subscribed_worker_and_audits() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: reviewer\n    role: reviewer\n    on: [cr_opened]\n";
    let (id, owner, name) = repo_with_yaml(&pool, "intake-cropened", yaml).await;
    let event = WebhookEvent::CrOpened {
        repo: RepoRef {
            owner,
            name,
            installation_id: 1,
        },
        number: 42,
    };
    assert_eq!(
        ingest(&pool, &event, &format!("d-{}", Uuid::now_v7()))
            .await
            .unwrap(),
        IngestOutcome::Enqueued(1)
    );
    assert_eq!(record::count_for_repo(&pool, id).await.unwrap(), 1);
    // Intake wrote an audit event for the delivery (spec §340).
    assert!(record::count_events_for_repo(&pool, id).await.unwrap() >= 1);
}

#[tokio::test]
async fn review_changes_requested_enqueues_subscribed_worker() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: fixer\n    role: coder\n    on: [cr_changes_requested]\n";
    let (id, owner, name) = repo_with_yaml(&pool, "intake-changes", yaml).await;
    let event = WebhookEvent::CrChangesRequested {
        repo: RepoRef {
            owner,
            name,
            installation_id: 1,
        },
        number: 7,
    };
    assert_eq!(
        ingest(&pool, &event, &format!("d-{}", Uuid::now_v7()))
            .await
            .unwrap(),
        IngestOutcome::Enqueued(1)
    );
    assert_eq!(record::count_for_repo(&pool, id).await.unwrap(), 1);
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
