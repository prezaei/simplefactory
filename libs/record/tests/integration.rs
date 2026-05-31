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
        priority: 0,
        available_at: None,
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

    // reclaim_expired is global (the sweeper sweeps all repos), and parallel
    // reclaim tests can sweep each other's expired items — so don't assert its
    // return count; assert OUR item is back in the queue below.
    record::reclaim_expired(&pool).await.unwrap();

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

#[tokio::test]
async fn reclaim_also_recovers_expired_running_items() {
    // A worker that reaches `running` and then crashes must be reclaimable too —
    // otherwise it holds a cap slot forever.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("reclaim-running"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    let mut req = lease_req(repo, 10, 10);
    req.ttl_secs = 0.0;
    let leased = record::lease_next(&pool, &req).await.unwrap().unwrap();
    // Worker starts running, then "crashes"; lease expires.
    assert!(record::set_state(&pool, leased.id, State::Running)
        .await
        .unwrap());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Global sweep (count not asserted — parallel reclaim tests share it); the
    // re-lease below proves our running item was recovered.
    record::reclaim_expired(&pool).await.unwrap();
    let released = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .expect("running item should be reclaimed and leasable again");
    assert_eq!(released.id, leased.id);
    assert_eq!(released.attempt_count, 1);
}

#[tokio::test]
async fn set_state_cannot_resurrect_a_terminal_item() {
    // Terminal states are final: set_state on a succeeded item is a no-op, so a
    // terminal item can't be re-activated and re-occupy a cap slot.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("terminal-guard"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(id) = record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(record::set_state(&pool, id, State::Succeeded)
        .await
        .unwrap());
    // Attempt to resurrect → rejected (returns false), state stays terminal.
    assert!(!record::set_state(&pool, id, State::Running).await.unwrap());
    assert!(!record::set_state(&pool, id, State::Queued).await.unwrap());
    // Nothing leasable (it stayed terminal, not queued).
    assert!(record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn lease_honors_priority_order() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("priority"), 1)
        .await
        .unwrap();
    let mut low = coder_item(repo, "low");
    low.priority = 0;
    let mut high = coder_item(repo, "high");
    high.priority = 5;
    record::enqueue(&pool, &low).await.unwrap();
    record::enqueue(&pool, &high).await.unwrap();

    // Higher priority leases first regardless of insert order.
    let first = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.subject_ref, "high");
}

#[tokio::test]
async fn lease_respects_available_at_delayed_visibility() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("delayed"), 1)
        .await
        .unwrap();
    let mut future = coder_item(repo, "later");
    future.available_at = Some(Utc::now() + chrono::Duration::hours(1));
    record::enqueue(&pool, &future).await.unwrap();

    // Not yet visible.
    assert!(record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .is_none());

    // An immediately-available item IS leased.
    record::enqueue(&pool, &coder_item(repo, "now"))
        .await
        .unwrap();
    let leased = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .expect("the available item should lease");
    assert_eq!(leased.subject_ref, "now");
}

#[tokio::test]
async fn append_event_writes_the_audit_trail() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("events"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(id) = record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(record::count_events_for_item(&pool, id).await.unwrap(), 0);

    record::append_event(
        &pool,
        Some(repo),
        Some(id),
        Some(Uuid::now_v7()),
        "manager",
        "assigned_coder",
        serde_json::json!({ "worker": "bug-fixer" }),
    )
    .await
    .unwrap();

    assert_eq!(record::count_events_for_item(&pool, id).await.unwrap(), 1);
}
