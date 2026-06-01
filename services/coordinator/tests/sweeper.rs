//! DB-gated integration tests for the P5 lease sweeper (`sweeper_tick`).
//!
//! Gated on `DATABASE_URL`; skips cleanly when unset. `sweeper_tick` is *global*
//! (it scans every repo in the shared DB), so these tests assert on the SPECIFIC
//! item's final state — not on `sweeper_tick`'s global return counts — which stays
//! correct even when a concurrent test's items are swept in the same pass.
#![forbid(unsafe_code)]

use std::sync::Arc;

use factory_coordinator::sweeper_tick;
use factory_github::FakeGitHub;
use factory_record::{self as record, Kind, LeaseRequest, NewWorkItem, State};
use uuid::Uuid;

/// A wall-clock cap so large only lease expiry triggers (kept inside Postgres's
/// timestamp range so `now() - cap` can't underflow): ~100 years.
const NEVER_WALL_CLOCK: f64 = 3.15e9;

/// `sweeper_tick` is GLOBAL — it scans/reclaims every repo's expired leases. Two of
/// these tests running in parallel would have one's sweep reclaim the other's item out
/// from under it (e.g. requeue an at-cap item before it's marked at-cap, so it never
/// escalates). Serialize all sweeper tests on this guard so each owns the global expired
/// set for its assertions. (`concurrent_sweeps_escalate_exactly_once` still runs its two
/// sweeps concurrently WITHIN the guarded test — that's the concurrency it exercises.)
static SWEEPER_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn pool_or_skip() -> Option<record::Pool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = record::connect(&url).await.expect("connect");
    record::migrate(&pool).await.expect("migrate");
    Some(pool)
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7())
}

/// A repo with a coder worker and `escalation_assignee` set, so an at-cap reclaim
/// posts the escalation comment AND assigns a human.
async fn seed_repo(pool: &record::Pool) -> Uuid {
    let repo = record::upsert_repo(pool, "acme", &unique("sweeper"), 99)
        .await
        .expect("upsert repo");
    let yaml = "version: 1
settings:
  label: factory
  escalation_assignee: octo-human
limits:
  max_concurrent_workers: 5
workers:
  - name: bug-fixer
    role: coder
    on: [manager]
";
    let cfg = factory_config::parse(yaml).expect("parse config");
    let cfg_json = serde_json::to_value(&cfg).expect("serialize config");
    record::set_repo_config(pool, repo, &cfg_json, "p5-sha")
        .await
        .expect("set config");
    repo
}

fn coder_item(repo: Uuid, subject: &str) -> NewWorkItem {
    NewWorkItem {
        repo_id: repo,
        kind: Kind::Coder,
        worker_name: "bug-fixer".into(),
        config_sha: "p5-sha".into(),
        trigger: "manager".into(),
        subject_ref: subject.into(),
        correlation_id: Uuid::now_v7(),
        priority: 0,
        available_at: None,
    }
}

/// Enqueue a coder item and lease it with `ttl_secs` (0.0 ⇒ already expired).
async fn enqueue_and_lease(pool: &record::Pool, repo: Uuid, subject: &str, ttl_secs: f64) -> Uuid {
    record::enqueue(pool, &coder_item(repo, subject))
        .await
        .expect("enqueue");
    let req = LeaseRequest {
        repo_id: repo,
        worker_name: "bug-fixer".into(),
        lease_owner: unique("job"),
        ttl_secs,
        per_worker_cap: 10,
        global_cap: 10,
    };
    record::lease_next(pool, &req)
        .await
        .expect("lease")
        .expect("an item to lease")
        .id
}

async fn attempt_status(pool: &record::Pool, attempt_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM attempts WHERE id = $1")
        .bind(attempt_id)
        .fetch_one(pool)
        .await
        .expect("attempt status")
}

async fn set_attempt_count(pool: &record::Pool, item: Uuid, n: i32) {
    sqlx::query("UPDATE work_items SET attempt_count = $2 WHERE id = $1")
        .bind(item)
        .bind(n)
        .execute(pool)
        .await
        .expect("set attempt_count");
}

#[tokio::test]
async fn sweeper_reclaims_an_expired_leased_item() {
    let _serial = SWEEPER_SERIAL.lock().await;
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let repo = seed_repo(&pool).await;
    let item = enqueue_and_lease(&pool, repo, "issue#1", 0.0).await;
    // Mint a token so we can prove the sweep revokes it.
    record::mint_task_token(
        &pool,
        &record::hash_token(&unique("tok")),
        item,
        repo,
        Kind::Coder,
        None,
        600.0,
    )
    .await
    .expect("mint token");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let gh = Arc::new(FakeGitHub::new());
    sweeper_tick(
        &pool,
        Some(gh.as_ref() as &dyn factory_github::GitHubClient),
        NEVER_WALL_CLOCK,
    )
    .await
    .expect("sweep");

    let after = record::work_item_by_id(&pool, item).await.unwrap().unwrap();
    assert_eq!(
        after.state,
        State::Queued,
        "expired lease returned to queue"
    );
    assert_eq!(after.attempt_count, 1, "the lapsed attempt was counted");
    let live_tokens: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_tokens WHERE work_item_id = $1 AND NOT revoked",
    )
    .bind(item)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live_tokens, 0, "the reclaimed item's tokens were revoked");
}

#[tokio::test]
async fn sweeper_reclaims_an_expired_running_item_and_finishes_its_attempt() {
    let _serial = SWEEPER_SERIAL.lock().await;
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = seed_repo(&pool).await;
    let item = enqueue_and_lease(&pool, repo, "issue#2", 0.0).await;
    record::set_state(&pool, item, State::Running)
        .await
        .unwrap();
    let attempt = record::start_attempt(&pool, item, None).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let gh = Arc::new(FakeGitHub::new());
    sweeper_tick(
        &pool,
        Some(gh.as_ref() as &dyn factory_github::GitHubClient),
        NEVER_WALL_CLOCK,
    )
    .await
    .expect("sweep");

    let after = record::work_item_by_id(&pool, item).await.unwrap().unwrap();
    assert_eq!(after.state, State::Queued);
    assert_eq!(attempt_status(&pool, attempt).await, "reclaimed");
}

#[tokio::test]
async fn sweeper_escalates_at_the_retry_cap() {
    let _serial = SWEEPER_SERIAL.lock().await;
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = seed_repo(&pool).await;
    let item = enqueue_and_lease(&pool, repo, "issue#3", 0.0).await;
    // At the default retry cap (3): the next lapse escalates instead of requeuing.
    set_attempt_count(&pool, item, 3).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let gh = Arc::new(FakeGitHub::new());
    sweeper_tick(
        &pool,
        Some(gh.as_ref() as &dyn factory_github::GitHubClient),
        NEVER_WALL_CLOCK,
    )
    .await
    .expect("sweep");

    let after = record::work_item_by_id(&pool, item).await.unwrap().unwrap();
    assert_eq!(
        after.state,
        State::Escalated,
        "at the cap, the item escalates"
    );
    assert_eq!(
        after.attempt_count, 4,
        "the final lapse was counted (3 → 4)"
    );
    assert_eq!(
        record::count_escalations_for_item(&pool, item)
            .await
            .unwrap(),
        1,
        "exactly one retries_exhausted escalation recorded"
    );
    // The escalation reason is retries_exhausted (a terminal escalation).
    let reason: String =
        sqlx::query_scalar("SELECT reason FROM escalations WHERE work_item_id = $1")
            .bind(item)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reason, "retries_exhausted");
    // NOTE: the GitHub side effects (escalation comment + assignment) go through the
    // shared `run_post_commit`, exercised by the P4 coordinator escalation tests. We do
    // NOT assert them here: `sweeper_tick` is GLOBAL, so a concurrent test's sweep can
    // win the claim and post to ITS FakeGitHub, making per-gh comment counts racy. The
    // reliable, per-item contract is the DB state asserted above.
    let _ = gh;
}

#[tokio::test]
async fn sweeper_reclaims_without_github_creds() {
    let _serial = SWEEPER_SERIAL.lock().await;
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = seed_repo(&pool).await;
    // One requeue-path item and one at-cap (escalate-path) item, both expired.
    let requeue = enqueue_and_lease(&pool, repo, "issue#7", 0.0).await;
    let escalate = enqueue_and_lease(&pool, repo, "issue#8", 0.0).await;
    set_attempt_count(&pool, escalate, 3).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // GitHub creds absent: the DB reclaim still runs (free the slot, requeue/escalate),
    // only the escalation comment is suppressed.
    sweeper_tick(&pool, None, NEVER_WALL_CLOCK)
        .await
        .expect("sweep without github");

    let r = record::work_item_by_id(&pool, requeue)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        r.state,
        State::Queued,
        "requeue reclaim works without github"
    );
    let e = record::work_item_by_id(&pool, escalate)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        e.state,
        State::Escalated,
        "at-cap item still escalates (row recorded) without github"
    );
    assert_eq!(
        record::count_escalations_for_item(&pool, escalate)
            .await
            .unwrap(),
        1,
        "escalation row recorded even with no comment delivery"
    );
}

#[tokio::test]
async fn sweeper_does_not_touch_a_valid_lease() {
    let _serial = SWEEPER_SERIAL.lock().await;
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = seed_repo(&pool).await;
    // ttl 600s — a live lease the sweeper must leave alone.
    let item = enqueue_and_lease(&pool, repo, "issue#4", 600.0).await;

    let gh = Arc::new(FakeGitHub::new());
    sweeper_tick(
        &pool,
        Some(gh.as_ref() as &dyn factory_github::GitHubClient),
        NEVER_WALL_CLOCK,
    )
    .await
    .expect("sweep");

    let after = record::work_item_by_id(&pool, item).await.unwrap().unwrap();
    assert_eq!(after.state, State::Leased, "a valid lease is untouched");
    assert_eq!(after.attempt_count, 0, "no lapse counted");
}

#[tokio::test]
async fn sweeper_wall_clock_cap_reclaims_a_long_running_attempt() {
    let _serial = SWEEPER_SERIAL.lock().await;
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = seed_repo(&pool).await;
    // A VALID lease (600s) — only the wall-clock cap can reclaim it.
    let item = enqueue_and_lease(&pool, repo, "issue#5", 600.0).await;
    record::set_state(&pool, item, State::Running)
        .await
        .unwrap();
    let attempt = record::start_attempt(&pool, item, None).await.unwrap();
    sqlx::query("UPDATE attempts SET started_at = now() - interval '1 hour' WHERE id = $1")
        .bind(attempt)
        .execute(&pool)
        .await
        .unwrap();

    let gh = Arc::new(FakeGitHub::new());
    // A 60s wall-clock cap catches the hour-old run despite the live lease.
    sweeper_tick(
        &pool,
        Some(gh.as_ref() as &dyn factory_github::GitHubClient),
        60.0,
    )
    .await
    .expect("sweep");

    let after = record::work_item_by_id(&pool, item).await.unwrap().unwrap();
    assert_eq!(after.state, State::Queued, "stuck-but-alive run reclaimed");
    assert_eq!(attempt_status(&pool, attempt).await, "reclaimed");
}

#[tokio::test]
async fn concurrent_sweeps_escalate_exactly_once() {
    let _serial = SWEEPER_SERIAL.lock().await;
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let repo = seed_repo(&pool).await;
    let item = enqueue_and_lease(&pool, repo, "issue#6", 0.0).await;
    set_attempt_count(&pool, item, 3).await; // at cap → escalates
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Two sweeps race on the same at-cap item; the optimistic claim + idempotent
    // escalation must yield exactly one escalation row, not two.
    let gh = Arc::new(FakeGitHub::new());
    let (a, b) = tokio::join!(
        sweeper_tick(
            &pool,
            Some(gh.as_ref() as &dyn factory_github::GitHubClient),
            NEVER_WALL_CLOCK
        ),
        sweeper_tick(
            &pool,
            Some(gh.as_ref() as &dyn factory_github::GitHubClient),
            NEVER_WALL_CLOCK
        ),
    );
    a.expect("sweep a");
    b.expect("sweep b");

    let after = record::work_item_by_id(&pool, item).await.unwrap().unwrap();
    assert_eq!(after.state, State::Escalated);
    assert_eq!(
        record::count_escalations_for_item(&pool, item)
            .await
            .unwrap(),
        1,
        "exactly one escalation despite the concurrent sweep"
    );
    assert_eq!(
        after.attempt_count, 4,
        "the lapse was counted exactly once (3 → 4)"
    );
}
