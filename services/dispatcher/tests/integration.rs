//! DB-gated integration tests for the dispatcher: lease → attempt → token → launch.
//!
//! These exercise the real record layer (Postgres) with the worker launch faked
//! at the [`WorkerLauncher`] seam, so we assert the exact number of leases,
//! attempts, tokens, and launches — caps must bind precisely.
#![forbid(unsafe_code)]

use factory_config as config;
use factory_dispatcher::{dispatch_once, FakeLauncher};
use factory_github::FakeGitHub;
use factory_record as record;
use record::{Kind, NewWorkItem};
use uuid::Uuid;

const TOOLS_URL: &str = "http://tools.local:8080";

/// The installation id every seeded repo is registered under.
const INSTALLATION_ID: i64 = 12345;

/// `dispatch_once` is *global*: it walks every configured repo in the shared test
/// DB and a single call may lease another concurrently-seeded test's items. To
/// keep exact-count assertions meaningful, the tests that drive `dispatch_once`
/// serialize on this guard so only one dispatch pass runs at a time. (This is a
/// test-harness concern only; production runs a single dispatcher loop.)
static DISPATCH_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Launches the FakeLauncher recorded for one specific repo.
fn launches_for_repo(
    launcher: &FakeLauncher,
    repo_id: Uuid,
) -> Vec<factory_dispatcher::LaunchRecord> {
    launcher
        .launched()
        .into_iter()
        .filter(|r| r.repo_id == repo_id)
        .collect()
}

/// Connect + migrate, or skip the test when DATABASE_URL is unset.
async fn pool_or_skip() -> Option<record::Pool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let p = record::connect(&url).await.expect("connect");
    record::migrate(&p).await.expect("migrate");
    Some(p)
}

/// A config with a single coder worker (`on: [manager]`, `max_concurrent: 2`) and
/// a global cap of 5. The worker name is unique per test so concurrent test runs
/// don't share leases.
fn config_yaml(label: &str, worker_name: &str) -> String {
    format!(
        "version: 1
settings:
  label: {label}
limits:
  max_concurrent_workers: 5
workers:
  - name: {worker_name}
    role: coder
    on: [manager]
    max_concurrent: 2
"
    )
}

/// What `seed` returns: the repo and its identity plus the enqueued work items.
struct Seeded {
    repo_id: Uuid,
    owner: String,
    name: String,
    worker_name: String,
    config_sha: String,
    work_ids: Vec<Uuid>,
}

impl Seeded {
    /// The HTTPS clone URL the dispatcher builds for this repo.
    fn repo_url(&self) -> String {
        format!("https://github.com/{}/{}.git", self.owner, self.name)
    }
}

/// Create a repo, store the parsed config, and enqueue `n` coder work items
/// triggered by "manager".
async fn seed(pool: &record::Pool, n: usize) -> Seeded {
    let suffix = Uuid::now_v7();
    let owner = format!("owner-{suffix}");
    let name = format!("repo-{suffix}");
    let worker_name = format!("coder-{suffix}");
    let label = format!("factory-{suffix}");

    let repo_id = record::upsert_repo(pool, &owner, &name, INSTALLATION_ID)
        .await
        .expect("upsert_repo");

    let cfg = config::parse(&config_yaml(&label, &worker_name)).expect("parse config");
    let cfg_json = serde_json::to_value(&cfg).expect("serialize config");
    let config_sha = format!("sha-{suffix}");
    record::set_repo_config(pool, repo_id, &cfg_json, &config_sha)
        .await
        .expect("set_repo_config");

    let mut ids = Vec::new();
    for i in 1..=n {
        let outcome = record::enqueue(
            pool,
            &NewWorkItem {
                repo_id,
                kind: Kind::Coder,
                worker_name: worker_name.clone(),
                config_sha: config_sha.clone(),
                trigger: "manager".to_owned(),
                subject_ref: format!("issue#{i}"),
                correlation_id: Uuid::now_v7(),
                priority: 0,
                available_at: None,
            },
        )
        .await
        .expect("enqueue");
        match outcome {
            record::EnqueueOutcome::Inserted(id) => ids.push(id),
            record::EnqueueOutcome::DuplicateActive => panic!("unexpected duplicate"),
        }
    }
    assert_eq!(ids.len(), n, "enqueued exactly {n} items");
    Seeded {
        repo_id,
        owner,
        name,
        worker_name,
        config_sha,
        work_ids: ids,
    }
}

/// Test 1: a single pass launches exactly the per-worker cap (2), opens 2
/// attempts, mints 2 resolvable tokens scoped to the right task/repo/role, and
/// the launcher records exactly 2 launches.
#[tokio::test]
async fn dispatch_launches_up_to_per_worker_cap() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };
    // Serialize the dispatch pass so no other test's pass leases our items.
    let _guard = DISPATCH_GUARD.lock().await;
    let seeded = seed(&pool, 3).await;
    let repo_id = seeded.repo_id;
    let work_ids = &seeded.work_ids;
    let lease_owner = format!("disp-{}", Uuid::now_v7());

    let launcher = FakeLauncher::new();
    let gh = FakeGitHub::new();
    dispatch_once(&pool, &launcher, &gh, TOOLS_URL, &lease_owner)
        .await
        .expect("dispatch_once");

    // Per-worker cap of 2 binds even though 3 are queued and the global cap is 5.
    let launched = launches_for_repo(&launcher, repo_id);
    assert_eq!(
        launched.len(),
        2,
        "exactly 2 launched for this repo (per-worker cap binds)"
    );
    for rec in &launched {
        assert_eq!(rec.repo_id, repo_id);
        assert_eq!(rec.role, Kind::Coder);
        assert_eq!(rec.worker_name, seeded.worker_name);
        assert_eq!(rec.tools_url, TOOLS_URL);
        // The launch carries the clone URL and the minted installation token.
        assert_eq!(rec.repo_url, seeded.repo_url());
        assert_eq!(
            rec.install_token,
            format!("ghs_fake_install_{INSTALLATION_ID}"),
            "install token is the one the fake GitHub minted for this installation"
        );
        assert!(
            work_ids.contains(&rec.work_item_id),
            "launched a seeded work item"
        );
    }

    // Exactly 2 attempts opened across the seeded items.
    let mut attempts = 0i64;
    for id in work_ids.iter() {
        attempts +=
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attempts WHERE work_item_id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("count attempts");
    }
    assert_eq!(attempts, 2, "exactly 2 attempts started");

    // Exactly 2 task tokens minted across the seeded items.
    let mut tokens = 0i64;
    for id in work_ids.iter() {
        tokens += sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM task_tokens WHERE work_item_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("count tokens");
    }
    assert_eq!(tokens, 2, "exactly 2 task tokens minted");
}

/// Test 2: a second immediate pass launches 0 — the 2 in-flight (leased) items
/// keep the per-worker cap bound, so no further work can be leased.
#[tokio::test]
async fn second_pass_launches_nothing_while_cap_bound() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };
    // Serialize the dispatch pass so no other test's pass leases our items.
    let _guard = DISPATCH_GUARD.lock().await;
    let repo_id = seed(&pool, 3).await.repo_id;
    let lease_owner = format!("disp-{}", Uuid::now_v7());

    let launcher = FakeLauncher::new();
    let gh = FakeGitHub::new();
    dispatch_once(&pool, &launcher, &gh, TOOLS_URL, &lease_owner)
        .await
        .expect("first dispatch");
    assert_eq!(
        launches_for_repo(&launcher, repo_id).len(),
        2,
        "first pass launches 2 for this repo"
    );

    // The 2 leased items stay in-flight (leased/running), so the per-worker cap
    // is still bound on the second pass — no further work can be leased.
    dispatch_once(&pool, &launcher, &gh, TOOLS_URL, &lease_owner)
        .await
        .expect("second dispatch");
    assert_eq!(
        launches_for_repo(&launcher, repo_id).len(),
        2,
        "second pass launches 0 more for this repo (cap still bound)"
    );
}

/// Test 4 (P5): the launch carries the work item's pinned config sha (so an in-flight
/// attempt is traceable to the config it was dispatched under), and a first dispatch of
/// a never-checkpointed item is a FRESH launch (no resume).
#[tokio::test]
async fn launch_carries_config_sha_and_is_fresh_without_a_checkpoint() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DISPATCH_GUARD.lock().await;
    let seeded = seed(&pool, 1).await;
    let repo_id = seeded.repo_id;
    let lease_owner = format!("disp-{}", Uuid::now_v7());

    let launcher = FakeLauncher::new();
    let gh = FakeGitHub::new();
    dispatch_once(&pool, &launcher, &gh, TOOLS_URL, &lease_owner)
        .await
        .expect("dispatch_once");

    let launched = launches_for_repo(&launcher, repo_id);
    assert_eq!(launched.len(), 1, "the single queued item launched");
    assert_eq!(
        launched[0].config_sha, seeded.config_sha,
        "the launch carries the pinned config sha"
    );
    assert_eq!(
        launched[0].resume, None,
        "a never-checkpointed item launches fresh, not resumed"
    );
}

/// Test 5 (P5): when a crashed attempt left a checkpoint and the item is re-queued, the
/// next dispatch RESUMES it — the launch carries the prior session id + checkpoint uri.
#[tokio::test]
async fn redispatch_after_checkpoint_resumes_the_same_session() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };
    let _guard = DISPATCH_GUARD.lock().await;
    let seeded = seed(&pool, 1).await;
    let repo_id = seeded.repo_id;
    let item_id = seeded.work_ids[0];
    let lease_owner = format!("disp-{}", Uuid::now_v7());

    // First dispatch opens attempt #1 and launches fresh.
    let launcher = FakeLauncher::new();
    let gh = FakeGitHub::new();
    dispatch_once(&pool, &launcher, &gh, TOOLS_URL, &lease_owner)
        .await
        .expect("first dispatch");
    let first = launches_for_repo(&launcher, repo_id);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].resume, None, "first launch is fresh");

    // The attempt checkpoints its session, then the worker "crashes": the lease
    // sweeper reclaims the item back to queued (attempt #1 carries the checkpoint).
    let attempt = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM attempts WHERE work_item_id = $1 ORDER BY started_at DESC LIMIT 1",
    )
    .bind(item_id)
    .fetch_one(&pool)
    .await
    .expect("attempt row");
    record::save_attempt_checkpoint(
        &pool,
        attempt,
        "ses-redispatch",
        "checkpoints/ses-redispatch.snapshot",
    )
    .await
    .expect("save checkpoint");
    let reclaimed = record::work_item_by_id(&pool, item_id)
        .await
        .unwrap()
        .unwrap();
    record::reclaim_running_attempt(&pool, item_id)
        .await
        .unwrap();
    assert!(
        record::reclaim_claim(
            &pool,
            item_id,
            reclaimed.lease_expires_at,
            record::State::Queued
        )
        .await
        .unwrap(),
        "the crashed item is requeued"
    );

    // Second dispatch leases the requeued item and RESUMES the crashed session.
    dispatch_once(&pool, &launcher, &gh, TOOLS_URL, &lease_owner)
        .await
        .expect("second dispatch");
    let all = launches_for_repo(&launcher, repo_id);
    let second = all.last().expect("a second launch");
    assert_eq!(
        second.resume,
        Some(factory_dispatcher::ResumeLaunch {
            session_id: "ses-redispatch".into(),
            checkpoint_uri: "checkpoints/ses-redispatch.snapshot".into(),
        }),
        "the re-dispatch resumes the prior session from its checkpoint"
    );
}

/// Test 3: each minted token resolves ONLY to its own task — the token for one
/// work item never authorizes another. We dispatch, then for every launched item
/// reconstruct its scope and confirm resolution is per-task.
#[tokio::test]
async fn tokens_are_scoped_to_their_own_task() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("DATABASE_URL unset; skipping");
        return;
    };
    // Serialize the dispatch pass so no other test's pass leases our items.
    let _guard = DISPATCH_GUARD.lock().await;
    let seeded = seed(&pool, 3).await;
    let repo_id = seeded.repo_id;
    let work_ids = &seeded.work_ids;
    let lease_owner = format!("disp-{}", Uuid::now_v7());

    let launcher = FakeLauncher::new();
    let gh = FakeGitHub::new();
    dispatch_once(&pool, &launcher, &gh, TOOLS_URL, &lease_owner)
        .await
        .expect("dispatch_once");
    assert_eq!(launches_for_repo(&launcher, repo_id).len(), 2);

    // Pull the two minted token hashes (joined to the seeded work items) and
    // confirm each resolves to exactly its own work item / repo / role.
    let rows: Vec<(String, Uuid)> = sqlx::query_as(
        "SELECT t.token_hash, t.work_item_id
           FROM task_tokens t
           JOIN work_items w ON w.id = t.work_item_id
          WHERE w.repo_id = $1",
    )
    .bind(repo_id)
    .fetch_all(&pool)
    .await
    .expect("fetch tokens");
    assert_eq!(rows.len(), 2, "exactly 2 tokens for this repo");

    for (hash, expected_item) in &rows {
        assert!(
            work_ids.contains(expected_item),
            "token belongs to a seeded item"
        );
        let auth = record::resolve_task_token(&pool, hash)
            .await
            .expect("resolve")
            .expect("token resolves");
        assert_eq!(
            auth.work_item_id, *expected_item,
            "token scoped to its work item"
        );
        assert_eq!(auth.repo_id, repo_id, "token scoped to its repo");
        assert_eq!(auth.role, Kind::Coder, "token scoped to its role");
    }

    // A bogus hash resolves to nothing (no cross-task / blanket access).
    let bogus = record::hash_token(&format!("nonexistent-{}", Uuid::now_v7()));
    assert!(
        record::resolve_task_token(&pool, &bogus)
            .await
            .expect("resolve bogus")
            .is_none(),
        "unknown token grants no access"
    );
}
