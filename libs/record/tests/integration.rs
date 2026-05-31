//! Integration tests for the record book against a real Postgres.
//!
//! Gated on `DATABASE_URL`: when it's unset (e.g. the no-DB CI lane / a laptop
//! without the stack up) each test skips cleanly. The integration CI lane and
//! `docker compose up` provide the DB, where they run for real. Tests isolate by
//! using a unique repo per test, so they're safe to run in parallel.

use chrono::{Duration, Utc};
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
async fn set_state_cannot_resurrect_any_terminal_item() {
    // Every terminal state (succeeded/failed/escalated) is final: set_state out of
    // it is a no-op, so a terminal item can't be re-activated and re-occupy a slot.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("terminal-guard"), 1)
        .await
        .unwrap();
    for (i, terminal) in [State::Succeeded, State::Failed, State::Escalated]
        .into_iter()
        .enumerate()
    {
        let EnqueueOutcome::Inserted(id) =
            record::enqueue(&pool, &coder_item(repo, &format!("issue#{i}")))
                .await
                .unwrap()
        else {
            panic!()
        };
        assert!(record::set_state(&pool, id, terminal).await.unwrap());
        // No transition out of a terminal state.
        assert!(!record::set_state(&pool, id, State::Running).await.unwrap());
        assert!(!record::set_state(&pool, id, State::Queued).await.unwrap());
    }
    // None of the now-terminal items are leasable.
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

#[tokio::test]
async fn blocked_state_frees_the_cap_slot() {
    // "Ask, don't block": moving a leased item to `blocked` releases its cap slot
    // (caps count only leased/running), so other work can proceed.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("blocked-slot"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#2"))
        .await
        .unwrap();

    // Cap of 1: lease the first, the second is blocked by the cap.
    let a = record::lease_next(&pool, &lease_req(repo, 1, 1))
        .await
        .unwrap()
        .unwrap();
    assert!(record::lease_next(&pool, &lease_req(repo, 1, 1))
        .await
        .unwrap()
        .is_none());

    // Block A -> frees the slot -> the second item can now lease.
    assert!(record::set_state(&pool, a.id, State::Blocked)
        .await
        .unwrap());
    let b = record::lease_next(&pool, &lease_req(repo, 1, 1))
        .await
        .unwrap()
        .expect("blocking the first should free the cap slot");
    assert_eq!(b.subject_ref, "issue#2");
}

#[tokio::test]
async fn blocked_item_is_not_reclaimed_and_can_be_requeued() {
    // A blocked item is not lease-backed, so the sweeper must NOT reclaim it even
    // with a stale lease_expires_at; and it can be returned to the queue (the
    // substrate the P2/P5 resume-on-reply path builds on).
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("blocked-reclaim"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    let mut req = lease_req(repo, 10, 10);
    req.ttl_secs = 0.0;
    let leased = record::lease_next(&pool, &req).await.unwrap().unwrap();
    assert!(record::set_state(&pool, leased.id, State::Blocked)
        .await
        .unwrap());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Sweeper does not touch blocked items -> still not leasable.
    record::reclaim_expired(&pool).await.unwrap();
    assert!(record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .is_none());

    // A human reply re-queues it; now it leases again.
    assert!(record::set_state(&pool, leased.id, State::Queued)
        .await
        .unwrap());
    let resumed = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .expect("requeued item should lease");
    assert_eq!(resumed.id, leased.id);
}

#[tokio::test]
async fn valid_lease_is_not_reclaimed() {
    // The negative case for the sweeper: a live (future-expiry) lease must survive
    // reclaim and keep heartbeating. Guards against a `<`-vs-`<=` / dropped-predicate
    // regression that the ttl=0 reclaim tests wouldn't catch.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("valid-lease"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    let leased = record::lease_next(&pool, &lease_req(repo, 10, 10)) // ttl 60s (future)
        .await
        .unwrap()
        .unwrap();

    record::reclaim_expired(&pool).await.unwrap();

    // Still leased (not requeued): nothing is leasable, and the owner can heartbeat.
    assert!(record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .is_none());
    assert!(record::heartbeat(&pool, leased.id, "test-leaser", 60.0)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn concurrent_enqueue_of_same_subject_is_idempotent() {
    // The partial unique index exists to defend a race: many callers enqueueing
    // the same (repo, subject, kind) at once must yield exactly one active item.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("concurrent-enqueue"), 1)
        .await
        .unwrap();

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let pool = pool.clone();
        let item = coder_item(repo, "issue#1");
        set.spawn(async move { record::enqueue(&pool, &item).await.unwrap() });
    }
    let mut inserted = 0usize;
    while let Some(res) = set.join_next().await {
        if matches!(res.unwrap(), EnqueueOutcome::Inserted(_)) {
            inserted += 1;
        }
    }
    assert_eq!(inserted, 1, "exactly one concurrent enqueue wins");
    assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 1);
}

#[tokio::test]
async fn repo_by_name_resolves_only_active_repos() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let name = unique("status-gate");
    let id = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();
    assert!(
        record::repo_by_name(&pool, "acme", &name)
            .await
            .unwrap()
            .is_some(),
        "active repo resolves"
    );
    // A suspended/removed repo must not resolve, so its events never act.
    sqlx::query("UPDATE repos SET status = 'suspended' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        record::repo_by_name(&pool, "acme", &name)
            .await
            .unwrap()
            .is_none(),
        "suspended repo does not resolve"
    );
}

#[tokio::test]
async fn purge_deliveries_removes_only_rows_past_the_cutoff() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // One backdated delivery and one fresh one.
    let old = unique("old-delivery");
    sqlx::query(
        "INSERT INTO webhook_deliveries (delivery_id, received_at)
         VALUES ($1, now() - interval '10 days')",
    )
    .bind(&old)
    .execute(&pool)
    .await
    .unwrap();
    let fresh = unique("fresh-delivery");
    assert!(record::mark_delivery(&pool, &fresh).await.unwrap());

    // Purge older than 1 day: removes the backdated row, keeps the fresh one (and
    // can't touch other tests' recent rows, so this is parallel-safe).
    record::purge_deliveries(&pool, Utc::now() - Duration::days(1))
        .await
        .unwrap();
    assert!(
        !record::delivery_seen(&pool, &old).await.unwrap(),
        "old delivery purged"
    );
    assert!(
        record::delivery_seen(&pool, &fresh).await.unwrap(),
        "fresh delivery retained"
    );
}

#[tokio::test]
async fn record_scheduled_fire_is_exactly_once_across_replicas() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("sched-fire"), 1)
        .await
        .unwrap();
    let slot = Utc::now();
    let item = NewWorkItem {
        repo_id: repo,
        kind: Kind::Coder,
        worker_name: "nightly".into(),
        config_sha: "sha".into(),
        trigger: "schedule".into(),
        subject_ref: format!("schedule@nightly@{}", slot.to_rfc3339()),
        correlation_id: Uuid::now_v7(),
        priority: 0,
        available_at: None,
    };

    // Many replicas race to fire the same slot; exactly one inserts.
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let pool = pool.clone();
        let item = item.clone();
        set.spawn(async move {
            record::record_scheduled_fire(&pool, &item, slot)
                .await
                .unwrap()
        });
    }
    let mut fired = 0usize;
    while let Some(res) = set.join_next().await {
        if matches!(res.unwrap(), EnqueueOutcome::Inserted(_)) {
            fired += 1;
        }
    }
    assert_eq!(fired, 1, "the slot fires exactly once across replicas");
    assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 1);

    // An earlier slot never re-fires after a later one has been recorded.
    let earlier = slot - Duration::minutes(5);
    let mut earlier_item = item.clone();
    earlier_item.subject_ref = format!("schedule@nightly@{}", earlier.to_rfc3339());
    assert_eq!(
        record::record_scheduled_fire(&pool, &earlier_item, earlier)
            .await
            .unwrap(),
        EnqueueOutcome::DuplicateActive,
        "a stale slot is not fired"
    );
    assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 1);
}

#[tokio::test]
async fn upsert_repo_is_idempotent_by_owner_name() {
    // A second upsert of the same (owner, name) returns the SAME repo id (no
    // duplicate repo), updating the installation id rather than inserting.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let name = unique("upsert-idem");
    let first = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();
    let second = record::upsert_repo(&pool, "acme", &name, 2).await.unwrap();
    assert_eq!(first, second, "same (owner,name) must map to one repo id");
}

#[tokio::test]
async fn record_scheduled_fire_fires_each_distinct_slot() {
    // Successive distinct cron slots must EACH fire — the exactly-once guard is
    // per-slot, it must not coalesce different periods into one.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("sched-distinct"), 1)
        .await
        .unwrap();
    let slot1 = Utc::now();
    let slot2 = slot1 + Duration::minutes(1);
    let item_for_slot = |slot: chrono::DateTime<Utc>| NewWorkItem {
        repo_id: repo,
        kind: Kind::Coder,
        worker_name: "nightly".into(),
        config_sha: "sha".into(),
        trigger: "schedule".into(),
        subject_ref: format!("schedule@nightly@{}", slot.to_rfc3339()),
        correlation_id: Uuid::now_v7(),
        priority: 0,
        available_at: None,
    };

    assert!(matches!(
        record::record_scheduled_fire(&pool, &item_for_slot(slot1), slot1)
            .await
            .unwrap(),
        EnqueueOutcome::Inserted(_)
    ));
    assert!(matches!(
        record::record_scheduled_fire(&pool, &item_for_slot(slot2), slot2)
            .await
            .unwrap(),
        EnqueueOutcome::Inserted(_)
    ));
    assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 2);
}

#[tokio::test]
async fn create_escalation_idempotent_reports_created_then_conflict() {
    // The `created` bool distinguishes the first insert from a later conflict-update:
    // first call → created=true; a second call for the same (work_item, reason) →
    // created=false (same row id), so the coordinator runs the human-facing side
    // effects exactly once.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("escalate-idem"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(work_item_id) =
        record::enqueue(&pool, &coder_item(repo, "issue#1"))
            .await
            .unwrap()
    else {
        panic!("fresh insert");
    };

    let mut conn = pool.acquire().await.unwrap();
    let (id1, created1) = record::create_escalation_idempotent(
        &mut *conn,
        work_item_id,
        "retries_exhausted",
        "first context",
        Some("github"),
    )
    .await
    .unwrap();
    assert!(created1, "the first escalation is newly created");

    let (id2, created2) = record::create_escalation_idempotent(
        &mut *conn,
        work_item_id,
        "retries_exhausted",
        "second context",
        Some("github"),
    )
    .await
    .unwrap();
    assert!(
        !created2,
        "a second call for the same (item, reason) is a conflict, not created"
    );
    assert_eq!(
        id1, id2,
        "the conflict path returns the same escalation row id"
    );

    // Still exactly one row on the book.
    assert_eq!(
        record::count_escalations_for_item(&pool, work_item_id)
            .await
            .unwrap(),
        1,
        "exactly one escalation row for the item",
    );
}

#[tokio::test]
async fn set_change_request_state_if_is_a_conditional_guard() {
    // The conditional transition succeeds only from the expected `from` state. This
    // is the approve→ready exactly-once guard: a second attempt from `approved` no
    // longer matches (state is now `ready`) and returns false.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("cr-cond"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(work_item_id) =
        record::enqueue(&pool, &coder_item(repo, "issue#1"))
            .await
            .unwrap()
    else {
        panic!("fresh insert");
    };
    let cr = record::upsert_change_request_for_item(
        &pool,
        repo,
        work_item_id,
        "factory/issue-1",
        Some(1),
    )
    .await
    .unwrap();
    record::set_change_request_state(&pool, cr.id, "approved")
        .await
        .unwrap();

    // approved → ready transitions exactly once.
    assert!(
        record::set_change_request_state_if(&pool, cr.id, "approved", "ready")
            .await
            .unwrap(),
        "first approved→ready transition succeeds",
    );
    // A second attempt from `approved` no longer matches (state is `ready`).
    assert!(
        !record::set_change_request_state_if(&pool, cr.id, "approved", "ready")
            .await
            .unwrap(),
        "a stale approved→ready attempt is a no-op",
    );
    // The CR has no GitHub number (it was created via upsert_change_request_for_item,
    // which leaves number NULL), so read its final state back by id.
    let final_state: String =
        sqlx::query_scalar("SELECT factory_state FROM change_requests WHERE id = $1")
            .bind(cr.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(final_state, "ready");
}

#[tokio::test]
async fn requeue_for_retry_resets_state_attempts_and_reacted() {
    // A terminal, reacted, failed item is requeued for a fresh attempt: back to
    // queued, attempt_count bumped, lease cleared, and reacted reset so a later
    // failure is reacted to again.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("requeue-retry"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(id) = record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap()
    else {
        panic!("fresh insert");
    };
    // Drive it to failed with attempt_count=1 and reacted=true (a reacted terminal).
    sqlx::query(
        "UPDATE work_items SET state = 'failed', attempt_count = 1, reacted = true WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();

    record::requeue_for_retry(&pool, id).await.unwrap();

    let item = record::work_item_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(item.state, State::Queued, "requeued back to queued");
    assert_eq!(item.attempt_count, 2, "attempt_count bumped");
    assert!(item.lease_owner.is_none(), "lease cleared");
    let reacted: bool = sqlx::query_scalar("SELECT reacted FROM work_items WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !reacted,
        "reacted reset so a later terminal occurrence is reacted again"
    );
}
