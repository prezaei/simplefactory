//! Intake integration tests against a real Postgres. Gated on `DATABASE_URL`
//! (skip cleanly when unset). Isolated per-run via unique repo names.

use factory_github::{FakeGitHub, RepoCoord, RepoRef, Subject, WebhookEvent};
use factory_intake::{apply_config_from_push, ingest, ApplyOutcome, IngestOutcome};
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
async fn no_config_delivery_is_redriven_after_config_applied() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // Repo is connected but has no validated config yet.
    let owner = "acme";
    let name = unique("intake-noconfig");
    let id = record::upsert_repo(&pool, owner, &name, 1).await.unwrap();
    let event = WebhookEvent::IssueLabeled {
        repo: RepoRef {
            owner: owner.into(),
            name: name.clone(),
            installation_id: 1,
        },
        issue: 8,
        label: "factory".into(),
    };
    let delivery = format!("d-{}", Uuid::now_v7());

    // No config → NoConfig, and the delivery is NOT recorded (so it can redrive).
    assert_eq!(
        ingest(&pool, &event, &delivery).await.unwrap(),
        IngestOutcome::NoConfig
    );

    // Config arrives; the redelivery now enqueues instead of being deduped.
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
    factory_intake::apply_config(&pool, id, yaml, "sha1")
        .await
        .unwrap();
    assert_eq!(
        ingest(&pool, &event, &delivery).await.unwrap(),
        IngestOutcome::Enqueued(1)
    );
    assert_eq!(record::count_for_repo(&pool, id).await.unwrap(), 1);
}

#[tokio::test]
async fn cr_updated_enqueues_subscribed_worker() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: reviewer\n    role: reviewer\n    on: [cr_updated]\n";
    let (id, owner, name) = repo_with_yaml(&pool, "intake-crupdated", yaml).await;
    let event = WebhookEvent::CrUpdated {
        repo: RepoRef {
            owner,
            name,
            installation_id: 1,
        },
        number: 3,
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
async fn review_requested_enqueues_subscribed_worker() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: reviewer\n    role: reviewer\n    on: [review_requested]\n";
    let (id, owner, name) = repo_with_yaml(&pool, "intake-reviewreq", yaml).await;
    let event = WebhookEvent::ReviewRequested {
        repo: RepoRef {
            owner,
            name,
            installation_id: 1,
        },
        number: 4,
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
async fn push_and_unmatched_events_are_ignored() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // A configured repo whose workers subscribe to nothing relevant.
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
    let (id, owner, name) = repo_with_yaml(&pool, "intake-ignored", yaml).await;

    // Push is parsed but config-apply-on-push is a P3 concern → Ignored here.
    let push = WebhookEvent::Push {
        repo: RepoRef {
            owner,
            name,
            installation_id: 1,
        },
        head_sha: "abc".into(),
        touched_paths: vec![".factory/config.yml".into()],
    };
    assert_eq!(
        ingest(&pool, &push, &format!("d-{}", Uuid::now_v7()))
            .await
            .unwrap(),
        IngestOutcome::Ignored
    );

    // An event Factory doesn't model at all → Ignored, no repo lookup.
    assert_eq!(
        ingest(
            &pool,
            &WebhookEvent::Other,
            &format!("d-{}", Uuid::now_v7())
        )
        .await
        .unwrap(),
        IngestOutcome::Ignored
    );
    assert_eq!(record::count_for_repo(&pool, id).await.unwrap(), 0);
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

#[tokio::test]
async fn scheduler_tick_fires_due_schedule_for_configured_repo() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // A configured repo with an every-minute schedule; the all-repos pass must
    // fire its due tick. (We assert on THIS repo's count, not the global return,
    // since other tests' repos may also be configured in the shared DB.)
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: nightly\n    role: coder\n    on:\n      - schedule: \"* * * * *\"\n";
    let (id, _owner, _name) = repo_with_yaml(&pool, "intake-tick", yaml).await;

    factory_intake::scheduler_tick(&pool, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(record::count_for_repo(&pool, id).await.unwrap(), 1);
}

#[tokio::test]
async fn scheduler_tick_skips_repo_with_unparseable_stored_config() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // A repo whose stored config jsonb is valid JSON but not a Config. The tick
    // must skip it without erroring (a logged decision), not blow up the pass.
    let name = unique("intake-badcfg");
    let id = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();
    sqlx::query(
        "UPDATE repos SET config = '\"not a config\"'::jsonb, config_sha = 'x' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();

    // Does not error despite the malformed config row.
    factory_intake::scheduler_tick(&pool, chrono::Utc::now())
        .await
        .unwrap();
    // The malformed repo produced no work.
    assert_eq!(record::count_for_repo(&pool, id).await.unwrap(), 0);
}

#[tokio::test]
async fn config_on_push_applies_when_config_file_changed() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    // A connected repo with no config yet — the push is what installs it.
    let owner = "acme";
    let name = unique("intake-pushapply");
    let id = record::upsert_repo(&pool, owner, &name, 1).await.unwrap();

    let head_sha = format!("head-{}", Uuid::now_v7());
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
    let gh = FakeGitHub::new().with_file(&head_sha, ".factory/config.yml", yaml);
    let repo_ref = RepoRef {
        owner: owner.into(),
        name: name.clone(),
        installation_id: 1,
    };

    assert_eq!(
        apply_config_from_push(
            &pool,
            &gh,
            &repo_ref,
            &head_sha,
            &[".factory/config.yml".into()],
        )
        .await
        .unwrap(),
        Some(ApplyOutcome::Applied)
    );

    // The config was actually applied: stored config_sha is now the pushed head.
    let rc = record::repo_by_name(&pool, owner, &name)
        .await
        .unwrap()
        .expect("repo present");
    assert_eq!(rc.config_sha.as_deref(), Some(head_sha.as_str()));

    // And the applied config drives intake: an IssueLabeled enqueues exactly one.
    let event = WebhookEvent::IssueLabeled {
        repo: repo_ref,
        issue: 1,
        label: "factory".into(),
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
async fn config_on_push_fetches_with_the_stored_installation_id() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    // The repo is connected with a real installation id...
    let owner = "acme";
    let name = unique("intake-pushinst");
    record::upsert_repo(&pool, owner, &name, 4242)
        .await
        .unwrap();
    let head_sha = format!("head-{}", Uuid::now_v7());
    let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
    let gh = FakeGitHub::new().with_file(&head_sha, ".factory/config.yml", yaml);
    // ...but the inbound (repository) webhook's RepoRef carries installation_id 0
    // (a repo webhook payload has no `installation` object).
    let repo_ref = RepoRef {
        owner: owner.into(),
        name: name.clone(),
        installation_id: 0,
    };

    apply_config_from_push(
        &pool,
        &gh,
        &repo_ref,
        &head_sha,
        &[".factory/config.yml".into()],
    )
    .await
    .unwrap();

    // get_file must have been called with the STORED installation id (4242), not the
    // webhook's 0 — otherwise minting an installation token would fail in production.
    assert!(
        gh.calls()
            .iter()
            .any(|c| c == "get_file .factory/config.yml inst=4242"),
        "expected get_file with stored inst=4242, got: {:?}",
        gh.calls()
    );
}

#[tokio::test]
async fn config_on_push_ignores_unrelated_push() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // A repo with a known-good config already applied (via apply_config, sha1).
    let (_id, owner, name) = repo_with_manager(&pool, "intake-pushunrelated").await;

    let head_sha = format!("head-{}", Uuid::now_v7());
    // FakeGitHub has the config seeded, but the push didn't touch it, so the file
    // must never be fetched and nothing must change.
    let gh = FakeGitHub::new().with_file(
        &head_sha,
        ".factory/config.yml",
        "version: 1\nsettings:\n  label: other\nworkers: []\n",
    );
    let repo_ref = RepoRef {
        owner: owner.clone(),
        name: name.clone(),
        installation_id: 1,
    };

    assert_eq!(
        apply_config_from_push(&pool, &gh, &repo_ref, &head_sha, &["src/main.rs".into()])
            .await
            .unwrap(),
        None
    );

    // Config unchanged: still the original sha1 from repo_with_manager.
    let rc = record::repo_by_name(&pool, &owner, &name)
        .await
        .unwrap()
        .expect("repo present");
    assert_eq!(rc.config_sha.as_deref(), Some("sha1"));
}

#[tokio::test]
async fn config_on_push_rejects_invalid_yaml_keeps_last_good() {
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    // Seed a first valid config (manager on issue_labeled) via apply_config.
    let (id, owner, name) = repo_with_manager(&pool, "intake-pushreject").await;

    // A coder may not subscribe to issue_labeled (manager-first policy) — invalid.
    let head_sha2 = format!("head-{}", Uuid::now_v7());
    let bad_yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: coder\n    role: coder\n    on: [issue_labeled]\n";
    let gh = FakeGitHub::new().with_file(&head_sha2, ".factory/config.yml", bad_yaml);
    let repo_ref = RepoRef {
        owner: owner.clone(),
        name: name.clone(),
        installation_id: 1,
    };

    assert_eq!(
        apply_config_from_push(
            &pool,
            &gh,
            &repo_ref,
            &head_sha2,
            &[".factory/config.yml".into()],
        )
        .await
        .unwrap(),
        Some(ApplyOutcome::RejectedKeptLastGood)
    );

    // The last-good config is still in effect: sha1, and intake still enqueues the
    // manager on issue_labeled.
    let rc = record::repo_by_name(&pool, &owner, &name)
        .await
        .unwrap()
        .expect("repo present");
    assert_eq!(rc.config_sha.as_deref(), Some("sha1"));

    let event = WebhookEvent::IssueLabeled {
        repo: repo_ref,
        issue: 9,
        label: "factory".into(),
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
async fn apply_config_writes_an_audit_event_for_both_outcomes() {
    // Every config decision must leave a queryable audit trail, not just a tracing line
    // (the "every decision logged" mandate) — especially the security-relevant rejection.
    let Some(pool) = pool_or_skip().await else {
        return;
    };
    let id = record::upsert_repo(&pool, "acme", &format!("cfg-audit-{}", Uuid::now_v7()), 1)
        .await
        .unwrap();

    let good = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
    assert_eq!(
        factory_intake::apply_config(&pool, id, good, "sha-good")
            .await
            .unwrap(),
        ApplyOutcome::Applied
    );
    let bad = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: coder\n    role: coder\n    on: [issue_labeled]\n";
    assert_eq!(
        factory_intake::apply_config(&pool, id, bad, "sha-bad")
            .await
            .unwrap(),
        ApplyOutcome::RejectedKeptLastGood
    );

    let applied: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE repo_id = $1 AND action = $2")
            .bind(id)
            .bind("config_applied")
            .fetch_one(&pool)
            .await
            .unwrap();
    let rejected: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE repo_id = $1 AND action = $2")
            .bind(id)
            .bind("config_rejected_kept_last_good")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(applied, 1, "an Applied decision writes one audit event");
    assert_eq!(rejected, 1, "a rejection writes one audit event");
}

// ---- P8: installation-webhook repo onboarding/offboarding ----

#[tokio::test]
async fn installation_webhook_onboards_offboards_and_reactivates_repo() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    let name = unique("onboard");
    let coord = |n: &str| RepoCoord {
        owner: "acme".to_owned(),
        name: n.to_owned(),
    };

    // Onboard: an `installation` created delivery upserts the repos row active with the
    // delivery's installation id — no manual SQL seeding (the P7 gap this closes).
    let created = WebhookEvent::Installation {
        installation_id: 4242,
        added: vec![coord(&name)],
        removed: vec![],
        remove_all: false,
    };
    assert_eq!(
        ingest(&pool, &created, &unique("dlv")).await.unwrap(),
        IngestOutcome::Installation {
            onboarded: 1,
            offboarded: 0
        }
    );
    let ident = record::repo_by_name(&pool, "acme", &name)
        .await
        .unwrap()
        .expect("repo onboarded");
    let id = ident.id;
    assert_eq!(
        record::repo_by_id(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .installation_id,
        4242,
        "onboarding records the delivery's installation id"
    );

    // Offboard: an uninstall marks it removed (one row affected).
    let deleted = WebhookEvent::Installation {
        installation_id: 4242,
        added: vec![],
        removed: vec![coord(&name)],
        remove_all: false,
    };
    assert_eq!(
        ingest(&pool, &deleted, &unique("dlv")).await.unwrap(),
        IngestOutcome::Installation {
            onboarded: 0,
            offboarded: 1
        }
    );
    // Offboarding again is idempotent — already removed, so zero rows affected.
    assert_eq!(
        ingest(&pool, &deleted, &unique("dlv")).await.unwrap(),
        IngestOutcome::Installation {
            onboarded: 0,
            offboarded: 0
        }
    );

    // Re-onboard (re-install) reactivates the SAME repo with a refreshed installation id.
    let reinstalled = WebhookEvent::Installation {
        installation_id: 5555,
        added: vec![coord(&name)],
        removed: vec![],
        remove_all: false,
    };
    assert_eq!(
        ingest(&pool, &reinstalled, &unique("dlv")).await.unwrap(),
        IngestOutcome::Installation {
            onboarded: 1,
            offboarded: 0
        }
    );
    let reident = record::repo_by_name(&pool, "acme", &name)
        .await
        .unwrap()
        .expect("repo still present");
    assert_eq!(
        reident.id, id,
        "re-onboard updates the same row, not a duplicate"
    );
    assert_eq!(
        record::repo_by_id(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .installation_id,
        5555,
        "re-onboard refreshes the installation id"
    );
}

#[tokio::test]
async fn installation_deleted_offboards_all_repos_for_the_installation() {
    let Some(pool) = pool_or_skip().await else {
        eprintln!("SKIP: DATABASE_URL not set");
        return;
    };
    // A UNIQUE installation id so the by-installation offboard can't touch other tests' repos.
    let inst: i64 = (Uuid::now_v7().as_u128() & 0x3fff_ffff) as i64 + 1;
    let a = unique("uninstall-a");
    let b = unique("uninstall-b");
    record::upsert_repo(&pool, "acme", &a, inst).await.unwrap();
    record::upsert_repo(&pool, "acme", &b, inst).await.unwrap();

    // Uninstall: GitHub's `installation` `deleted` carries no repo list → remove_all=true,
    // which must offboard EVERY repo for the installation (not the empty `removed` list).
    let deleted = WebhookEvent::Installation {
        installation_id: inst,
        added: vec![],
        removed: vec![],
        remove_all: true,
    };
    assert_eq!(
        ingest(&pool, &deleted, &unique("dlv")).await.unwrap(),
        IngestOutcome::Installation {
            onboarded: 0,
            offboarded: 2
        }
    );
    // Both repos are now removed (repo_by_name only returns active repos).
    assert!(record::repo_by_name(&pool, "acme", &a)
        .await
        .unwrap()
        .is_none());
    assert!(record::repo_by_name(&pool, "acme", &b)
        .await
        .unwrap()
        .is_none());

    // Re-delivery is idempotent — nothing active left, so zero offboarded.
    assert_eq!(
        ingest(&pool, &deleted, &unique("dlv")).await.unwrap(),
        IngestOutcome::Installation {
            onboarded: 0,
            offboarded: 0
        }
    );
}
