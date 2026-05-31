//! Integration tests for the record book against a real Postgres.
//!
//! Gated on `DATABASE_URL`: when it's unset (e.g. the no-DB CI lane / a laptop
//! without the stack up) each test skips cleanly. The integration CI lane and
//! `docker compose up` provide the DB, where they run for real. Tests isolate by
//! using a unique repo per test, so they're safe to run in parallel.

use chrono::Utc;
use factory_record as record;
use record::{EnqueueOutcome, Kind, LeaseRequest, NewWorkItem, State};
use uuid::Uuid;

async fn pool_or_skip() -> Option<record::Pool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = record::connect(&url)
        .await
        .expect("connect to DATABASE_URL");
    record::migrate(&pool).await.expect("run migrations");
    Some(pool)
}

fn coder_item(repo: Uuid, subject: &str) -> NewWorkItem {
    NewWorkItem {
        repo_id: repo,
        kind: Kind::Coder,
        worker_name: "bug-fixer".into(),
        config_sha: "test-sha".into(),
        trigger: "manager".into(),
        subject_ref: subject.into(),
        correlation_id: Uuid::now_v7(),
    }
}

fn lease_req(repo: Uuid, per_worker: i64, global: i64) -> LeaseRequest {
    LeaseRequest {
        repo_id: repo,
        worker_name: "bug-fixer".into(),
        lease_owner: "test-leaser".into(),
        ttl_secs: 60.0,
        per_worker_cap: per_worker,
        global_cap: global,
    }
}

/// A repo name unique to this invocation, so tests are isolated across repeated
/// runs against the same database (each gets a fresh repo_id, hence fresh state).
fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7())
}

#[tokio::test]
async fn enqueue_is_idempotent_per_active_subject() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("enqueue-idem"), 1)
        .await
        .unwrap();
    let item = coder_item(repo, "issue#1");

    let first = record::enqueue(&pool, &item).await.unwrap();
    let second = record::enqueue(&pool, &item).await.unwrap();

    assert!(matches!(first, EnqueueOutcome::Inserted(_)));
    assert_eq!(second, EnqueueOutcome::DuplicateActive);

    // A different subject is a distinct active item.
    let other = record::enqueue(&pool, &coder_item(repo, "issue#2"))
        .await
        .unwrap();
    assert!(matches!(other, EnqueueOutcome::Inserted(_)));
}

#[tokio::test]
async fn terminal_item_does_not_block_reenqueue() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("reenqueue"), 1)
        .await
        .unwrap();
    let item = coder_item(repo, "issue#1");

    let EnqueueOutcome::Inserted(id) = record::enqueue(&pool, &item).await.unwrap() else {
        panic!("expected insert");
    };
    // Finish it; the partial unique index should now allow a fresh enqueue.
    assert!(record::set_state(&pool, id, State::Succeeded)
        .await
        .unwrap());
    let again = record::enqueue(&pool, &item).await.unwrap();
    assert!(matches!(again, EnqueueOutcome::Inserted(_)));
}

#[tokio::test]
async fn lease_then_empty_then_complete() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("lease-basic"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();

    let leased = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .expect("one item should lease");
    assert_eq!(leased.state, State::Leased);
    assert_eq!(leased.subject_ref, "issue#1");

    // Nothing else queued.
    assert!(record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .is_none());

    assert!(record::set_state(&pool, leased.id, State::Succeeded)
        .await
        .unwrap());
}

#[tokio::test]
async fn heartbeat_extends_only_for_owner() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("heartbeat"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    let leased = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .unwrap();

    assert!(record::heartbeat(&pool, leased.id, "test-leaser", 120.0)
        .await
        .unwrap()
        .is_some());
    assert!(record::heartbeat(&pool, leased.id, "someone-else", 120.0)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn reclaim_returns_expired_lease_to_queue() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("reclaim"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    // Short lease, then let it expire.
    let mut req = lease_req(repo, 10, 10);
    req.ttl_secs = 0.0;
    let leased = record::lease_next(&pool, &req).await.unwrap().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // reclaim_expired is global (the production sweeper sweeps all repos), so it
    // reclaims at least our expired lease and may also sweep stragglers from other
    // tests — assert our item specifically rather than the global count.
    assert!(record::reclaim_expired(&pool).await.unwrap() >= 1);

    // It's leasable again, and the lapsed attempt was counted.
    let released = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(released.id, leased.id);
    assert_eq!(released.attempt_count, 1);
}

#[tokio::test]
async fn purge_removes_only_old_terminal_items() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("purge"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(done) = record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap()
    else {
        panic!()
    };
    record::enqueue(&pool, &coder_item(repo, "issue#2"))
        .await
        .unwrap(); // stays queued (active)
    record::set_state(&pool, done, State::Succeeded)
        .await
        .unwrap();
    assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 2);

    // purge_terminal is global; assert its effect scoped to THIS repo so other
    // tests' terminal rows don't perturb the count.
    // Nothing in this repo is older than an hour, so the repo is untouched.
    record::purge_terminal(&pool, Utc::now() - chrono::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 2);

    // Purge terminal up to now: the succeeded item goes, the active one stays.
    record::purge_terminal(&pool, Utc::now() + chrono::Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 1);
    // The surviving item is the still-active one, and it remains leasable.
    let survivor = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .expect("active item should remain");
    assert_eq!(survivor.subject_ref, "issue#2");
}
