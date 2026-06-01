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

/// A wall-clock cap so large only *lease expiry* ever triggers, but still safely
/// inside Postgres's timestamp range (`now() - cap` must not underflow): ~100 years.
const NEVER_WALL_CLOCK: f64 = 3.15e9;

/// `fetch_expired_lease_items` and `sweep_expired` are GLOBAL (they scan/mutate every
/// repo's expired leases in the shared DB). Tests that *observe* the global expired set
/// — asserting their own expired items are still fetchable, or that a specific
/// `reclaim_claim` wins — must not have a parallel test's sweep reclaim those items out
/// from under them. They (and every sweep) serialize on this guard; the per-item reclaim
/// tests that only assert their own item's final state don't need it (their assertions
/// hold whichever sweep wins).
static EXPIRED_LEASE_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Record-level lease sweep used by the reclaim tests: requeue every expired-lease
/// item (the coordinator's `sweeper_tick` layers the retry-cap escalate decision on
/// top of these same primitives). [`NEVER_WALL_CLOCK`] means only *lease expiry*
/// triggers here, matching the old global `reclaim_expired` one-shot these tests
/// were written against. Holds [`EXPIRED_LEASE_SERIAL`] so it can't reclaim a
/// concurrent observing test's items mid-assertion.
async fn sweep_expired(pool: &record::Pool) {
    let _guard = EXPIRED_LEASE_SERIAL.lock().await;
    let items = record::fetch_expired_lease_items(pool, NEVER_WALL_CLOCK)
        .await
        .unwrap();
    for item in items {
        record::reclaim_running_attempt(pool, item.id)
            .await
            .unwrap();
        record::revoke_task_tokens(pool, item.id).await.unwrap();
        record::reclaim_claim(pool, item.id, item.lease_expires_at, State::Queued)
            .await
            .unwrap();
    }
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

    // The sweep is global (it sweeps all repos), and parallel reclaim tests can
    // sweep each other's expired items — so don't assert a swept count; assert OUR
    // item is back in the queue below.
    sweep_expired(&pool).await;

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

    // purge_terminal is global; a future/now cutoff would nuke OTHER parallel
    // tests' briefly-terminal rows (e.g. an item transiting through `failed`),
    // causing cross-test flakes. So backdate only THIS test's succeeded row and
    // purge with a PAST cutoff that catches just it.
    record::purge_terminal(&pool, Utc::now() - chrono::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 2);

    // Age the succeeded item ~10 days, then purge older-than-1-day: it goes, the
    // active one stays, and no other test's recent terminal rows are at risk.
    sqlx::query("UPDATE work_items SET updated_at = now() - interval '10 days' WHERE id = $1")
        .bind(done)
        .execute(&pool)
        .await
        .unwrap();
    record::purge_terminal(&pool, Utc::now() - chrono::Duration::days(1))
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
async fn purge_spares_a_terminal_item_that_still_owns_a_live_cr() {
    // A coder succeeds (terminal) but its change request is still in review past the
    // retention window. Purging the work item would null the live CR's owner link
    // (FK is ON DELETE SET NULL) and strand it — so purge must SPARE it until the CR
    // itself is terminal (merged/closed).
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("purge-livecr"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(coder) = record::enqueue(&pool, &coder_item(repo, "issue#7"))
        .await
        .unwrap()
    else {
        panic!()
    };
    let cr = record::upsert_change_request_for_item(&pool, repo, coder, "factory/issue-7", Some(7))
        .await
        .unwrap();
    record::set_state(&pool, coder, State::Succeeded)
        .await
        .unwrap();
    // Age the succeeded coder well past any cutoff.
    sqlx::query("UPDATE work_items SET updated_at = now() - interval '10 days' WHERE id = $1")
        .bind(coder)
        .execute(&pool)
        .await
        .unwrap();

    // CR is still 'draft' (non-terminal) → the owning work item is spared.
    record::purge_terminal(&pool, Utc::now() - chrono::Duration::days(1))
        .await
        .unwrap();
    assert!(
        record::work_item_by_id(&pool, coder)
            .await
            .unwrap()
            .is_some(),
        "a terminal item owning a non-terminal CR must NOT be purged"
    );
    assert!(
        record::change_request_for_item(&pool, coder)
            .await
            .unwrap()
            .is_some(),
        "the live CR's owner link must be intact"
    );

    // Once the CR reaches a terminal state, the work item becomes purgeable.
    record::set_change_request_state(&pool, cr.id, "merged")
        .await
        .unwrap();
    record::purge_terminal(&pool, Utc::now() - chrono::Duration::days(1))
        .await
        .unwrap();
    assert!(
        record::work_item_by_id(&pool, coder)
            .await
            .unwrap()
            .is_none(),
        "once the CR is terminal, the terminal work item is purged"
    );
}

#[tokio::test]
async fn finish_attempt_persists_each_terminal_status_and_is_idempotent() {
    // "Every enum variant through every entry point": drive each terminal AttemptStatus
    // through finish_attempt, assert it persists, and that re-finishing is a no-op
    // (a terminal attempt is never re-finished).
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("attempt-status"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(item) = record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap()
    else {
        panic!()
    };

    for status in [
        record::AttemptStatus::Succeeded,
        record::AttemptStatus::Failed,
        record::AttemptStatus::Blocked,
        record::AttemptStatus::Reclaimed,
    ] {
        let attempt = record::start_attempt(&pool, item, None).await.unwrap();
        // First finish wins.
        assert!(
            record::finish_attempt(&pool, attempt, status, Some("done"))
                .await
                .unwrap(),
            "first finish to {} succeeds",
            status.as_str()
        );
        // The status persisted.
        let persisted: String = sqlx::query_scalar("SELECT status FROM attempts WHERE id = $1")
            .bind(attempt)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(persisted, status.as_str());
        // Re-finishing a terminal attempt is a no-op (idempotent).
        assert!(
            !record::finish_attempt(&pool, attempt, record::AttemptStatus::Failed, None)
                .await
                .unwrap(),
            "a terminal attempt is not re-finished"
        );
    }
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
    sweep_expired(&pool).await;
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
    sweep_expired(&pool).await;
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

    sweep_expired(&pool).await;

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
    // effects exactly once. The conflict path is a no-op self-assign: it leaves the
    // existing row (and its context) UNCHANGED even when the second call passes a
    // different context, so the DB always matches the already-delivered escalation.
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

    // The conflict did NOT overwrite the original context with "second context": the
    // stored row still carries the context delivered to the human on the first call.
    let stored_context: String =
        sqlx::query_scalar("SELECT context FROM escalations WHERE id = $1")
            .bind(id1)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        stored_context, "first context",
        "the conflict path leaves the original context unchanged (no DO UPDATE SET context)"
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

#[tokio::test]
async fn mark_reacted_with_stale_updated_at_does_not_double_react_after_requeue() {
    // The double-react race: two replicas list the SAME failed item at terminal time T0.
    // Replica A claims it and requeues it (state=queued, reacted=false, updated_at bumped
    // to T1). Replica B, holding a stale snapshot showing updated_at=T0, must NOT be able
    // to re-claim it even though `reacted` is false again — its claim is anchored to the
    // exact terminal occurrence via updated_at, so the stale claim fails.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("double-react"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(id) = record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap()
    else {
        panic!("fresh insert");
    };
    // Drive it to a terminal failed occurrence; updated_at becomes the terminal anchor T0.
    record::set_state(&pool, id, State::Failed).await.unwrap();
    let listed = record::list_unreacted_terminal(&pool).await.unwrap();
    let listed = listed
        .into_iter()
        .find(|w| w.id == id)
        .expect("failed item listed as unreacted terminal");
    let t0 = listed.updated_at;

    // Replica A wins the claim at T0, then requeues (bumping updated_at to T1 != T0).
    assert!(
        record::mark_reacted(&pool, id, t0).await.unwrap(),
        "replica A wins the single legitimate claim at the terminal occurrence"
    );
    record::requeue_for_retry(&pool, id).await.unwrap();
    let after = record::work_item_by_id(&pool, id).await.unwrap().unwrap();
    assert_ne!(
        after.updated_at, t0,
        "requeue advances updated_at off the terminal anchor"
    );
    assert_eq!(after.state, State::Queued, "requeued back to queued");

    // Replica B's stale claim at the ORIGINAL terminal updated_at must fail, even though
    // `reacted` is false again after the requeue — no stale re-claim, no second decide.
    assert!(
        !record::mark_reacted(&pool, id, t0).await.unwrap(),
        "stale claim at the old terminal updated_at does not re-claim the requeued row"
    );
    let reacted: bool = sqlx::query_scalar("SELECT reacted FROM work_items WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !reacted,
        "the requeued (active) row is left unreacted by the rejected stale claim"
    );
}

#[tokio::test]
async fn mark_reacted_single_claim_with_matching_updated_at_succeeds_once() {
    // The normal single-claim path: a claim with the matching terminal updated_at wins
    // exactly once; a second claim with the same updated_at loses (reacted is now true).
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("single-claim"), 1)
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(id) = record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap()
    else {
        panic!("fresh insert");
    };
    record::set_state(&pool, id, State::Failed).await.unwrap();
    let item = record::work_item_by_id(&pool, id).await.unwrap().unwrap();
    let t0 = item.updated_at;

    assert!(
        record::mark_reacted(&pool, id, t0).await.unwrap(),
        "first claim with the matching terminal updated_at wins"
    );
    assert!(
        !record::mark_reacted(&pool, id, t0).await.unwrap(),
        "second claim with the same updated_at loses (already reacted)"
    );
}

#[tokio::test]
async fn upsert_cr_preserves_terminal_state_and_owner_against_stale_coder() {
    // A CR that already advanced to a terminal-ish state ('ready') must NOT be dragged
    // back to 'open' nor have its work_item_id stolen by a late/retried coder tool call
    // on the same deterministic branch. This mirrors the two record-layer writes that
    // `open_or_update_change_request` performs in sequence: upsert_change_request_for_item
    // then set_change_request_number.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("cr-terminal-guard"), 1)
        .await
        .unwrap();
    let branch = "factory/issue-9";

    // Work item A opens the CR on the branch and it advances through 'open' to 'ready'.
    let EnqueueOutcome::Inserted(item_a) = record::enqueue(&pool, &coder_item(repo, "issue#9"))
        .await
        .unwrap()
    else {
        panic!("fresh insert A");
    };
    let cr = record::upsert_change_request_for_item(&pool, repo, item_a, branch, Some(9))
        .await
        .unwrap();
    record::set_change_request_number(&pool, cr.id, 42)
        .await
        .unwrap();
    record::set_change_request_state(&pool, cr.id, "ready")
        .await
        .unwrap();
    // Coder A finishes (terminal) so it no longer occupies the active
    // (repo, issue#9, coder) dedup slot — letting the rework coder B enqueue.
    record::set_state(&pool, item_a, State::Succeeded)
        .await
        .unwrap();

    // Work item B is a stale/retried coder pushing the SAME branch.
    let EnqueueOutcome::Inserted(item_b) = record::enqueue(&pool, &coder_item(repo, "issue#9"))
        .await
        .unwrap()
    else {
        panic!("fresh insert B");
    };
    // The stale coder even supplies a DIFFERENT linked_issue (999) — it must not stick.
    let returned = record::upsert_change_request_for_item(&pool, repo, item_b, branch, Some(999))
        .await
        .unwrap();
    // Contract still holds: the same CR row is returned...
    assert_eq!(returned.id, cr.id, "same CR row returned");
    // ...but the terminal-ish state, original owner, and linked_issue are preserved.
    assert_eq!(
        returned.factory_state, "ready",
        "terminal-ish state not dragged back by the stale upsert"
    );
    assert_eq!(
        returned.work_item_id,
        Some(item_a),
        "original owner not stolen by the stale upsert"
    );
    assert_eq!(
        returned.linked_issue,
        Some(9),
        "linked_issue not overwritten on a terminal-ish CR by the stale upsert"
    );

    // The follow-up set_change_request_number must NOT downgrade 'ready' to 'open'.
    record::set_change_request_number(&pool, cr.id, 42)
        .await
        .unwrap();
    let after = record::change_request_by_number(&pool, repo, 42)
        .await
        .unwrap()
        .expect("CR by number");
    assert_eq!(
        after.factory_state, "ready",
        "set_change_request_number does not downgrade a terminal-ish CR"
    );
    assert_eq!(
        after.work_item_id,
        Some(item_a),
        "owner still preserved after set_change_request_number"
    );
}

#[tokio::test]
async fn upsert_cr_rework_path_repoints_and_reopens_non_terminal_cr() {
    // The normal rework path is unchanged: a non-terminal CR (here 'changes_requested')
    // is re-pointed to the new rework coder and set_change_request_number reopens it.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("cr-rework-path"), 1)
        .await
        .unwrap();
    let branch = "factory/issue-12";

    let EnqueueOutcome::Inserted(item_a) = record::enqueue(&pool, &coder_item(repo, "issue#12"))
        .await
        .unwrap()
    else {
        panic!("fresh insert A");
    };
    let cr = record::upsert_change_request_for_item(&pool, repo, item_a, branch, Some(12))
        .await
        .unwrap();
    record::set_change_request_number(&pool, cr.id, 77)
        .await
        .unwrap();
    // Reviewer requested changes: a non-terminal state.
    record::set_change_request_state(&pool, cr.id, "changes_requested")
        .await
        .unwrap();
    // Coder A finishes (terminal) so the rework coder B can claim the active slot.
    record::set_state(&pool, item_a, State::Succeeded)
        .await
        .unwrap();

    // The rework coder pushes the same branch: the row is re-pointed to it.
    let EnqueueOutcome::Inserted(item_b) = record::enqueue(&pool, &coder_item(repo, "issue#12"))
        .await
        .unwrap()
    else {
        panic!("fresh insert B");
    };
    let returned = record::upsert_change_request_for_item(&pool, repo, item_b, branch, Some(12))
        .await
        .unwrap();
    assert_eq!(returned.id, cr.id, "same CR row reused");
    assert_eq!(
        returned.work_item_id,
        Some(item_b),
        "non-terminal CR re-pointed to the rework coder"
    );

    record::set_change_request_number(&pool, cr.id, 77)
        .await
        .unwrap();
    let after = record::change_request_by_number(&pool, repo, 77)
        .await
        .unwrap()
        .expect("CR by number");
    assert_eq!(
        after.factory_state, "open",
        "non-terminal CR reopened on set_change_request_number"
    );
    assert_eq!(
        after.work_item_id,
        Some(item_b),
        "rework coder remains the owner"
    );
}

// ===========================================================================
// P5: lease sweeper + crash-resume checkpoint primitives.
// ===========================================================================

#[tokio::test]
async fn fetch_expired_includes_leased_and_running_excludes_valid() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // Hold the global expired-lease guard so no parallel sweep reclaims our items
    // before we assert they are fetchable.
    let _serial = EXPIRED_LEASE_SERIAL.lock().await;
    let repo = record::upsert_repo(&pool, "acme", &unique("fetch-expired"), 1)
        .await
        .unwrap();
    // An expired LEASED item, a still-RUNNING item with an expired lease, and a
    // VALID (future-expiry) leased item.
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#2"))
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#3"))
        .await
        .unwrap();
    let mut expired_req = lease_req(repo, 10, 10);
    expired_req.ttl_secs = 0.0;
    let leased = record::lease_next(&pool, &expired_req)
        .await
        .unwrap()
        .unwrap();
    let running = record::lease_next(&pool, &expired_req)
        .await
        .unwrap()
        .unwrap();
    record::set_state(&pool, running.id, State::Running)
        .await
        .unwrap();
    let valid = record::lease_next(&pool, &lease_req(repo, 10, 10)) // ttl 60s, future
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let ids: std::collections::HashSet<Uuid> =
        record::fetch_expired_lease_items(&pool, NEVER_WALL_CLOCK)
            .await
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
    assert!(ids.contains(&leased.id), "expired leased item is swept");
    assert!(ids.contains(&running.id), "expired running item is swept");
    assert!(
        !ids.contains(&valid.id),
        "a live (future-expiry) lease is NOT swept"
    );
}

#[tokio::test]
async fn fetch_expired_wall_clock_catches_old_running_attempt() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let _serial = EXPIRED_LEASE_SERIAL.lock().await;
    let repo = record::upsert_repo(&pool, "acme", &unique("wall-clock"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    // A VALID lease (ttl 60s, future) — lease-expiry alone would never sweep it.
    let leased = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .unwrap();
    record::set_state(&pool, leased.id, State::Running)
        .await
        .unwrap();
    let attempt = record::start_attempt(&pool, leased.id, None).await.unwrap();
    // Backdate the running attempt so it has been going for an hour.
    sqlx::query("UPDATE attempts SET started_at = now() - interval '1 hour' WHERE id = $1")
        .bind(attempt)
        .execute(&pool)
        .await
        .unwrap();

    // Under a huge wall-clock cap the still-valid lease is NOT swept...
    let big: std::collections::HashSet<Uuid> =
        record::fetch_expired_lease_items(&pool, NEVER_WALL_CLOCK)
            .await
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
    assert!(
        !big.contains(&leased.id),
        "valid lease under a huge wall-clock cap is not swept"
    );
    // ...but a 60s cap catches the hour-old running attempt despite the live lease.
    let capped: std::collections::HashSet<Uuid> = record::fetch_expired_lease_items(&pool, 60.0)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert!(
        capped.contains(&leased.id),
        "wall-clock cap reclaims a stuck-but-alive run"
    );
}

#[tokio::test]
async fn reclaim_claim_is_optimistic_and_exactly_once() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let _serial = EXPIRED_LEASE_SERIAL.lock().await;
    let repo = record::upsert_repo(&pool, "acme", &unique("reclaim-claim"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    let mut req = lease_req(repo, 10, 10);
    req.ttl_secs = 0.0;
    let leased = record::lease_next(&pool, &req).await.unwrap().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // A claim with the WRONG expected expiry no-ops (optimistic-concurrency guard).
    assert!(
        !record::reclaim_claim(&pool, leased.id, Some(Utc::now()), State::Queued)
            .await
            .unwrap(),
        "a stale expected_expiry must not claim"
    );
    // The correct observed expiry wins exactly once.
    assert!(
        record::reclaim_claim(&pool, leased.id, leased.lease_expires_at, State::Queued)
            .await
            .unwrap(),
        "the observed expiry claims the item"
    );
    // A second claim with the same (now stale) expiry loses: the item is queued, no
    // longer leased/running, so it can't be reclaimed twice.
    assert!(
        !record::reclaim_claim(&pool, leased.id, leased.lease_expires_at, State::Queued)
            .await
            .unwrap(),
        "an already-reclaimed item is not reclaimed again"
    );
    let after = record::work_item_by_id(&pool, leased.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state, State::Queued);
    assert_eq!(after.attempt_count, 1, "exactly one lapse counted");
}

#[tokio::test]
async fn reclaim_running_attempt_marks_running_reclaimed_once() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("reclaim-attempt"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    let leased = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .unwrap();
    let attempt = record::start_attempt(&pool, leased.id, None).await.unwrap();

    assert_eq!(
        record::reclaim_running_attempt(&pool, leased.id)
            .await
            .unwrap(),
        1,
        "the running attempt is reclaimed"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM attempts WHERE id = $1")
        .bind(attempt)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "reclaimed");
    assert_eq!(
        record::reclaim_running_attempt(&pool, leased.id)
            .await
            .unwrap(),
        0,
        "an already-reclaimed attempt is not touched again"
    );
}

#[tokio::test]
async fn save_and_load_checkpoint_roundtrip_and_overwrite() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("checkpoint"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    let leased = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .unwrap();
    let attempt = record::start_attempt(&pool, leased.id, None).await.unwrap();

    assert!(
        record::save_attempt_checkpoint(&pool, attempt, "ses-1", "cp://1")
            .await
            .unwrap()
    );
    let info = record::last_checkpoint_for_item(&pool, leased.id)
        .await
        .unwrap()
        .expect("checkpoint present");
    assert_eq!(info.session_id, "ses-1");
    assert_eq!(info.checkpoint_uri, "cp://1");
    assert_eq!(info.resume_count, 1);

    // Last write wins.
    assert!(
        record::save_attempt_checkpoint(&pool, attempt, "ses-2", "cp://2")
            .await
            .unwrap()
    );
    let info = record::last_checkpoint_for_item(&pool, leased.id)
        .await
        .unwrap()
        .expect("checkpoint present");
    assert_eq!(info.session_id, "ses-2");
    assert_eq!(info.checkpoint_uri, "cp://2");
    assert_eq!(info.resume_count, 1, "still one checkpointed attempt");
}

#[tokio::test]
async fn last_checkpoint_none_when_no_session() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("no-checkpoint"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    let leased = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .unwrap();
    // An attempt with no checkpoint saved.
    record::start_attempt(&pool, leased.id, None).await.unwrap();
    assert!(record::last_checkpoint_for_item(&pool, leased.id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn last_checkpoint_returns_latest_with_resume_count() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = record::upsert_repo(&pool, "acme", &unique("checkpoint-latest"), 1)
        .await
        .unwrap();
    record::enqueue(&pool, &coder_item(repo, "issue#1"))
        .await
        .unwrap();
    let leased = record::lease_next(&pool, &lease_req(repo, 10, 10))
        .await
        .unwrap()
        .unwrap();
    // Three attempts; only the 2nd and 3rd carry a session id.
    let a1 = record::start_attempt(&pool, leased.id, None).await.unwrap();
    let a2 = record::start_attempt(&pool, leased.id, None).await.unwrap();
    let a3 = record::start_attempt(&pool, leased.id, None).await.unwrap();
    // Deterministic ordering by started_at (now_v7 ids can tie within a ms).
    for (id, ago) in [(a1, 3), (a2, 2), (a3, 1)] {
        sqlx::query(&format!(
            "UPDATE attempts SET started_at = now() - interval '{ago} minutes' WHERE id = $1"
        ))
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    }
    record::save_attempt_checkpoint(&pool, a2, "ses-2", "cp://2")
        .await
        .unwrap();
    record::save_attempt_checkpoint(&pool, a3, "ses-3", "cp://3")
        .await
        .unwrap();

    let info = record::last_checkpoint_for_item(&pool, leased.id)
        .await
        .unwrap()
        .expect("checkpoint present");
    assert_eq!(
        info.session_id, "ses-3",
        "the most recent session is returned"
    );
    assert_eq!(info.resume_count, 2, "two attempts carried a session id");
}
