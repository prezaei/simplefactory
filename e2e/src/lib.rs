//! Factory end-to-end acceptance suite.
//!
//! Every behavior the specs promise has exactly one named test here, authored
//! up front (P0) as a compiling `#[ignore]`d stub. A later phase **un-ignores**
//! and implements the tests it satisfies; a phase is not done while a test it
//! owns is still ignored. The count of still-`#[ignore]`d tests is the project's
//! burndown to done. DB-backed tests gate on `DATABASE_URL` and skip cleanly
//! when it's unset.
#![forbid(unsafe_code)]

#[cfg(test)]
mod acceptance {
    use factory_record as record;
    use uuid::Uuid;

    async fn pool_or_skip() -> Option<record::Pool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = record::connect(&url)
            .await
            .expect("connect to DATABASE_URL");
        record::migrate(&pool).await.expect("run migrations");
        Some(pool)
    }

    fn item_for(repo: Uuid, worker: &str, subject: &str) -> record::NewWorkItem {
        record::NewWorkItem {
            repo_id: repo,
            kind: record::Kind::Coder,
            worker_name: worker.into(),
            config_sha: "test-sha".into(),
            trigger: "manager".into(),
            subject_ref: subject.into(),
            correlation_id: Uuid::now_v7(),
            priority: 0,
            available_at: None,
        }
    }

    fn coder_item(repo: Uuid, subject: &str) -> record::NewWorkItem {
        item_for(repo, "bug-fixer", subject)
    }

    /// A repo name unique to this invocation, so the acceptance tests are isolated
    /// across repeated runs against the same database (fresh repo_id → fresh state).
    fn unique(prefix: &str) -> String {
        format!("{prefix}-{}", Uuid::now_v7())
    }

    // P1 — record book / queue. These drive the record-layer arbitration directly
    // (the exact mechanism the dispatcher replicas rely on): concurrent leasers
    // stand in for concurrent coordinators.

    #[tokio::test]
    async fn no_double_starts_under_concurrent_coordinators() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let repo = record::upsert_repo(&pool, "acme", &unique("e2e-no-double-starts"), 1)
            .await
            .unwrap();
        const ITEMS: usize = 60;
        for i in 0..ITEMS {
            record::enqueue(&pool, &coder_item(repo, &format!("issue#{i}")))
                .await
                .unwrap();
        }

        // 8 concurrent coordinators race to lease; caps high so only the queue limits.
        let mut set = tokio::task::JoinSet::new();
        for n in 0..8 {
            let pool = pool.clone();
            set.spawn(async move {
                let mut mine = Vec::new();
                loop {
                    let req = record::LeaseRequest {
                        repo_id: repo,
                        worker_name: "bug-fixer".into(),
                        lease_owner: format!("coordinator-{n}"),
                        ttl_secs: 60.0,
                        per_worker_cap: 1000,
                        global_cap: 1000,
                    };
                    match record::lease_next(&pool, &req).await.unwrap() {
                        Some(wi) => mine.push(wi.id),
                        None => break,
                    }
                }
                mine
            });
        }

        let mut all = Vec::new();
        while let Some(res) = set.join_next().await {
            all.extend(res.unwrap());
        }
        let unique: std::collections::HashSet<_> = all.iter().copied().collect();
        assert_eq!(all.len(), ITEMS, "every queued item is leased exactly once");
        assert_eq!(unique.len(), ITEMS, "no item is leased twice");
    }

    #[tokio::test]
    async fn capacity_caps_never_exceeded() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let repo = record::upsert_repo(&pool, "acme", &unique("e2e-caps"), 1)
            .await
            .unwrap();
        const ITEMS: usize = 40;
        const CAP: i64 = 3;
        for i in 0..ITEMS {
            record::enqueue(&pool, &coder_item(repo, &format!("issue#{i}")))
                .await
                .unwrap();
        }

        // Concurrent leasers; none complete, so in-flight count must never exceed CAP.
        let mut set = tokio::task::JoinSet::new();
        for n in 0..8 {
            let pool = pool.clone();
            set.spawn(async move {
                let mut leased = 0usize;
                loop {
                    let req = record::LeaseRequest {
                        repo_id: repo,
                        worker_name: "bug-fixer".into(),
                        lease_owner: format!("c-{n}"),
                        ttl_secs: 60.0,
                        per_worker_cap: CAP,
                        global_cap: 100,
                    };
                    match record::lease_next(&pool, &req).await.unwrap() {
                        Some(_) => leased += 1,
                        None => break,
                    }
                }
                leased
            });
        }

        let mut total = 0usize;
        while let Some(res) = set.join_next().await {
            total += res.unwrap();
        }
        assert_eq!(
            total, CAP as usize,
            "exactly CAP items leased; the rest blocked by the cap"
        );

        // Cap still holds: a fresh attempt yields nothing while CAP are in-flight.
        let req = record::LeaseRequest {
            repo_id: repo,
            worker_name: "bug-fixer".into(),
            lease_owner: "verify".into(),
            ttl_secs: 60.0,
            per_worker_cap: CAP,
            global_cap: 100,
        };
        assert!(
            record::lease_next(&pool, &req).await.unwrap().is_none(),
            "cap remains enforced"
        );
    }

    #[tokio::test]
    async fn global_cap_never_exceeded_across_workers() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let repo = record::upsert_repo(&pool, "acme", &unique("e2e-global-cap"), 1)
            .await
            .unwrap();
        const GLOBAL: i64 = 4;
        // Two worker pools with plenty of items each; per-worker cap is high so
        // ONLY the global ceiling can bind.
        for w in ["coder-a", "coder-b"] {
            for i in 0..20 {
                record::enqueue(&pool, &item_for(repo, w, &format!("{w}#{i}")))
                    .await
                    .unwrap();
            }
        }
        let mut set = tokio::task::JoinSet::new();
        for w in ["coder-a", "coder-b"] {
            for n in 0..4 {
                let pool = pool.clone();
                set.spawn(async move {
                    let mut leased = 0usize;
                    loop {
                        let req = record::LeaseRequest {
                            repo_id: repo,
                            worker_name: w.into(),
                            lease_owner: format!("{w}-{n}"),
                            ttl_secs: 60.0,
                            per_worker_cap: 1000,
                            global_cap: GLOBAL,
                        };
                        match record::lease_next(&pool, &req).await.unwrap() {
                            Some(_) => leased += 1,
                            None => break,
                        }
                    }
                    leased
                });
            }
        }
        let mut total = 0usize;
        while let Some(res) = set.join_next().await {
            total += res.unwrap();
        }
        assert_eq!(
            total, GLOBAL as usize,
            "the global ceiling binds across all worker pools"
        );
    }

    // P2 — intake / opt-in / config application
    #[tokio::test]
    async fn opt_in_gating_unlabelled_issue_is_ignored() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        // Register a repo whose config has a manager subscribed to issue_labeled.
        let name = unique("e2e-optin");
        let repo = record::upsert_repo(&pool, "acme", &name, 99).await.unwrap();
        let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
        assert_eq!(
            factory_intake::apply_config(&pool, repo, yaml, "sha1")
                .await
                .unwrap(),
            factory_intake::ApplyOutcome::Applied
        );

        let repo_ref = factory_github::RepoRef {
            owner: "acme".into(),
            name: name.clone(),
            installation_id: 99,
        };

        // A labelled issue → exactly one Manager work item (the P2 exit criterion).
        let labeled = factory_github::WebhookEvent::IssueLabeled {
            repo: repo_ref.clone(),
            issue: 123,
            label: "factory".into(),
        };
        assert_eq!(
            factory_intake::ingest(&pool, &labeled, &format!("d-{}", Uuid::now_v7()))
                .await
                .unwrap(),
            factory_intake::IngestOutcome::Enqueued(1)
        );
        assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 1);

        // An issue WITHOUT the opt-in label → ignored, no new work item.
        let unlabelled = factory_github::WebhookEvent::IssueLabeled {
            repo: repo_ref,
            issue: 124,
            label: "wontfix".into(),
        };
        assert_eq!(
            factory_intake::ingest(&pool, &unlabelled, &format!("d-{}", Uuid::now_v7()))
                .await
                .unwrap(),
            factory_intake::IngestOutcome::Ignored
        );
        assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn invalid_config_is_rejected_and_last_good_kept() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let name = unique("e2e-invalidcfg");
        let repo = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();

        // A good config takes effect.
        let good = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
        assert_eq!(
            factory_intake::apply_config(&pool, repo, good, "good-sha")
                .await
                .unwrap(),
            factory_intake::ApplyOutcome::Applied
        );

        // An invalid config (coder on issue_labeled violates manager-first) is rejected.
        let bad = "version: 1\nworkers:\n  - name: c\n    role: coder\n    on: [issue_labeled]\n";
        assert_eq!(
            factory_intake::apply_config(&pool, repo, bad, "bad-sha")
                .await
                .unwrap(),
            factory_intake::ApplyOutcome::RejectedKeptLastGood
        );

        // The last good config is still in effect: a labelled issue still enqueues
        // the manager.
        let labeled = factory_github::WebhookEvent::IssueLabeled {
            repo: factory_github::RepoRef {
                owner: "acme".into(),
                name,
                installation_id: 1,
            },
            issue: 1,
            label: "factory".into(),
        };
        assert_eq!(
            factory_intake::ingest(&pool, &labeled, &format!("d-{}", Uuid::now_v7()))
                .await
                .unwrap(),
            factory_intake::IngestOutcome::Enqueued(1)
        );
    }

    #[tokio::test]
    async fn scheduler_fires_a_due_tick_once_across_replicas() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let name = unique("e2e-scheduler");
        let repo = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();
        // A coder on an every-minute schedule: the (now-90s, now] window always
        // contains a due tick.
        let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: nightly\n    role: coder\n    on:\n      - schedule: \"* * * * *\"\n";
        let config = factory_config::parse(yaml).expect("valid config");
        let now = chrono::Utc::now();

        // Eight replicas run the SAME tick concurrently; the due slot must fire
        // exactly once (per-(repo,worker) advisory lock + last_fired re-check).
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let pool = pool.clone();
            let config = config.clone();
            set.spawn(async move {
                factory_intake::scheduler_tick_for_repo(&pool, repo, &config, "sha", now)
                    .await
                    .unwrap()
            });
        }
        let mut fired = 0usize;
        while let Some(res) = set.join_next().await {
            fired += res.unwrap();
        }
        assert_eq!(fired, 1, "the due tick fires exactly once across replicas");
        assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 1);

        // Re-running the same tick does not double-fire (idempotent on the slot).
        let again = factory_intake::scheduler_tick_for_repo(&pool, repo, &config, "sha", now)
            .await
            .unwrap();
        assert_eq!(again, 0, "an already-fired slot is not re-fired");
        assert_eq!(record::count_for_repo(&pool, repo).await.unwrap(), 1);
    }

    // P3 — dispatcher → worker → Tools slice (Coder drives a change request).
    //
    // Exercises the P3 vertical over its seams with fakes: the dispatcher leases a
    // coder work item and launches it (minting the per-task token), then — standing
    // in for the launched worker — we drive the Tools service with that exact token
    // to open the change request and record the outcome. Asserts the CR is recorded,
    // the item succeeds, the token is revoked when the task ends, and the audit
    // trail is exactly what the slice should produce.

    /// A dispatcher launcher that captures the raw per-task token the dispatcher
    /// minted, so the test can act as the worker would with that token.
    #[derive(Default)]
    struct CapturingLauncher {
        captured: std::sync::Mutex<Vec<factory_dispatcher::LaunchSpec>>,
    }

    #[async_trait::async_trait]
    impl factory_dispatcher::WorkerLauncher for CapturingLauncher {
        async fn launch(&self, spec: &factory_dispatcher::LaunchSpec) -> anyhow::Result<String> {
            self.captured.lock().expect("lock").push(spec.clone());
            Ok(format!("e2e-job-{}", Uuid::now_v7()))
        }
    }

    #[tokio::test]
    async fn p3_coder_work_item_drives_a_change_request() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };

        // A repo with a coder worker triggered by the manager.
        let name = unique("e2e-p3");
        let repo = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();
        let worker_name = "bug-fixer";
        let yaml = format!(
            "version: 1\n\
             settings:\n  label: factory\n\
             limits:\n  max_concurrent_workers: 4\n\
             workers:\n  - name: {worker_name}\n    role: coder\n    on: [manager]\n"
        );
        let cfg = factory_config::parse(&yaml).expect("valid config");
        let cfg_json = serde_json::to_value(&cfg).unwrap();
        record::set_repo_config(&pool, repo, &cfg_json, "p3-sha")
            .await
            .unwrap();

        // Enqueue one coder work item (kind Coder, the worker_name, trigger manager).
        let correlation_id = Uuid::now_v7();
        let enqueued = record::enqueue(
            &pool,
            &record::NewWorkItem {
                repo_id: repo,
                kind: record::Kind::Coder,
                worker_name: worker_name.into(),
                config_sha: "p3-sha".into(),
                trigger: "manager".into(),
                subject_ref: "issue#1".into(),
                correlation_id,
                priority: 0,
                available_at: None,
            },
        )
        .await
        .unwrap();
        let work_item_id = match enqueued {
            record::EnqueueOutcome::Inserted(id) => id,
            other => panic!("expected a fresh insert, got {other:?}"),
        };

        // Dispatch over the seam. The dispatcher scans every configured repo in the
        // shared DB, so we scope our assertion to THIS work item: it is launched
        // exactly once, and we capture its minted raw token.
        let launcher = CapturingLauncher::default();
        let dispatch_gh = factory_github::FakeGitHub::new();
        factory_dispatcher::dispatch_once(
            &pool,
            &launcher,
            &dispatch_gh,
            "http://tools.test",
            "e2e",
        )
        .await
        .unwrap();
        let specs = launcher.captured.lock().expect("lock").clone();
        let mine: Vec<_> = specs
            .iter()
            .filter(|s| s.work_item_id == work_item_id)
            .collect();
        assert_eq!(mine.len(), 1, "this work item is launched exactly once");
        let spec = mine[0];
        assert_eq!(spec.role, record::Kind::Coder);
        assert_eq!(spec.worker_name, worker_name);
        // The launch carries the clone URL and a minted installation token the worker
        // needs to clone/push — neither is empty.
        assert!(!spec.repo_url.is_empty(), "launch spec carries a clone URL");
        assert!(
            spec.repo_url.starts_with("https://github.com/"),
            "clone URL is an https github url: {}",
            spec.repo_url
        );
        assert!(
            !spec.install_token.is_empty(),
            "launch spec carries a minted install token"
        );
        let raw_token = spec.raw_token.clone();

        // Build the Tools service the worker would call, with the captured token's
        // task scope. Seed the issue the subject points at.
        let gh = std::sync::Arc::new(factory_github::FakeGitHub::new().with_issue(
            factory_github::IssueView {
                number: 1,
                title: "Fix the bug".into(),
                body: "broken".into(),
                comments: vec![],
            },
        ));
        let state = std::sync::Arc::new(tools_state(&pool, gh));

        // Resolve the captured token to the task auth (as the Tools HTTP layer does).
        let auth = factory_tools::authorize(&pool, &raw_token)
            .await
            .unwrap()
            .expect("captured token authorizes the task");

        // The worker opens its change request, then records success.
        let cr = factory_tools::open_or_update_change_request(
            &state,
            &auth,
            "[coder] issue#1",
            "Implemented the fix with tests",
            "factory/issue-1",
            "main",
            Some(1),
        )
        .await
        .unwrap();
        let cr_number = cr["cr_number"].as_i64().expect("a cr_number");

        factory_tools::record_outcome(&state, &auth, "succeeded", "all done")
            .await
            .unwrap();

        // A change_requests row exists for the work item, carrying the cr_number.
        let cr_row = record::change_request_for_item(&pool, work_item_id)
            .await
            .unwrap()
            .expect("a CR row for the work item");
        assert_eq!(cr_row.number, Some(cr_number as i32));
        assert_eq!(cr_row.work_item_id, Some(work_item_id));

        // The work item succeeded.
        let item = record::work_item_by_id(&pool, work_item_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.state, record::State::Succeeded);

        // The task token is revoked once the task ended (record_outcome).
        assert!(
            record::resolve_task_token(&pool, &record::hash_token(&raw_token))
                .await
                .unwrap()
                .is_none(),
            "token revoked when the task ends"
        );

        // Exactly the expected audit trail: the dispatcher's launch event plus one
        // event per Tools call routed through the audit choke point. Here we drove
        // the tool fns directly (not via HTTP), so only the dispatcher launch event
        // is on the book for this item.
        assert_eq!(
            record::count_events_for_item(&pool, work_item_id)
                .await
                .unwrap(),
            1,
            "exactly the dispatcher launch event is audited for this item"
        );
    }

    // P4 — coordinator + reviewer (full loop)
    //
    // These drive the REAL coordinator (`factory_coordinator::coordinator_tick`)
    // against the real record book, with Claude/the workers faked at the seam: a
    // worker "completing" is simulated by driving the Tools fns for its GitHub side
    // effect (open CR / submit review) and then marking its work item terminal. A
    // single shared `FakeGitHub` records every comment + assignment the coordinator
    // makes, so the exactly-once handoff/escalation assertions are exact.

    use std::sync::Arc;

    use factory_coordinator::coordinator_tick;
    use factory_github::{CheckConclusion, CheckSummary, FakeGitHub};
    use factory_tools::ToolsState;
    use sqlx::Row;

    /// `coordinator_tick` scans the WHOLE record book (it is a global service), so in
    /// a shared test DB the P4 tests — which assert exact per-tick react counts and
    /// exact FakeGitHub call counts against their OWN `FakeGitHub` — must not overlap
    /// with each other or react to each other's leftover terminal items. This mutex
    /// serializes the four P4 coordinator tests; each one [`drain_terminal`]s any
    /// pre-existing unreacted terminal items (through a throwaway client) before it
    /// begins, so its own `FakeGitHub` only ever records effects for its own repo.
    static P4_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Quarantine every currently-unreacted terminal item so a subsequent P4 tick sees
    /// only items THIS test creates. `coordinator_tick` scans the WHOLE record book, so
    /// without this a terminal item left by another test — a `failed` item past the
    /// retry cap (→ an escalation comment on this test's `FakeGitHub`), the crash-safe
    /// test's deliberately-unreactable `issue#<huge>` manager item (which errors on
    /// every tick forever), or any concurrently-created terminal item — would be reacted
    /// during this test's tick and corrupt its exact-count assertions.
    ///
    /// We mark them `reacted` directly (a bulk DB write) rather than re-running them
    /// through `coordinator_tick`: running them would (a) re-trigger their human-facing
    /// side effects and (b) never settle the crash-safe poison item (its decision errors
    /// before the claim commits, so a tick never marks it reacted). A direct claim
    /// retires every one of them in a single statement, with no side effects.
    async fn drain_terminal(pool: &record::Pool) {
        sqlx::query(
            "UPDATE work_items SET reacted = true
              WHERE NOT reacted AND state IN ('succeeded', 'failed', 'escalated')",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    /// Three-role config (manager + coder + reviewer) stored on a fresh repo.
    /// Returns the repo id and the worker names `(manager, coder, reviewer)`.
    async fn setup_three_role_repo(
        pool: &record::Pool,
        repo: Uuid,
    ) -> (&'static str, &'static str, &'static str) {
        let (manager, coder, reviewer) = ("manager", "bug-fixer", "reviewer");
        let yaml = format!(
            "version: 1\n\
             settings:\n  label: factory\n\
             limits:\n  max_concurrent_workers: 5\n\
             workers:\n\
             \x20 - name: {manager}\n    role: manager\n    on: [issue_labeled]\n\
             \x20 - name: {coder}\n    role: coder\n    on: [manager, cr_changes_requested]\n\
             \x20 - name: {reviewer}\n    role: reviewer\n    on: [cr_opened]\n"
        );
        let cfg = factory_config::parse(&yaml).expect("valid three-role config");
        let cfg_json = serde_json::to_value(&cfg).unwrap();
        record::set_repo_config(pool, repo, &cfg_json, "p4-sha")
            .await
            .unwrap();
        (manager, coder, reviewer)
    }

    /// Enqueue a work item of `kind` for `subject`, returning its id.
    async fn enqueue_item(
        pool: &record::Pool,
        repo: Uuid,
        kind: record::Kind,
        worker: &str,
        subject: &str,
    ) -> Uuid {
        let outcome = record::enqueue(
            pool,
            &record::NewWorkItem {
                repo_id: repo,
                kind,
                worker_name: worker.into(),
                config_sha: "p4-sha".into(),
                trigger: "manager".into(),
                subject_ref: subject.into(),
                correlation_id: Uuid::now_v7(),
                priority: 0,
                available_at: None,
            },
        )
        .await
        .unwrap();
        match outcome {
            record::EnqueueOutcome::Inserted(id) => id,
            other => panic!("expected fresh insert for {subject}, got {other:?}"),
        }
    }

    /// Mark a work item `succeeded` (simulates the worker finishing its task).
    async fn mark_succeeded(pool: &record::Pool, id: Uuid) {
        assert!(
            record::set_state(pool, id, record::State::Succeeded)
                .await
                .unwrap(),
            "work item should transition to succeeded"
        );
    }

    /// A `TaskAuth` for driving Tools fns as the worker for `work_item`. No attempt
    /// is needed: the P4 tests use the GitHub-facing tool fns (open CR / submit
    /// review), not `record_outcome`.
    fn task_auth(work_item: Uuid, repo: Uuid, role: record::Kind) -> record::TaskAuth {
        record::TaskAuth {
            work_item_id: work_item,
            repo_id: repo,
            role,
            attempt_id: None,
        }
    }

    /// Build a `ToolsState` over the shared `FakeGitHub` and an in-memory blob store.
    fn tools_state(pool: &record::Pool, gh: Arc<FakeGitHub>) -> ToolsState {
        ToolsState {
            pool: pool.clone(),
            gh,
            blob: Arc::new(factory_blob::InMemoryTranscripts::new()),
        }
    }

    /// The coordinator's mutating GitHub calls of a given prefix (e.g.
    /// `"assign_issue"`), used for exact-count assertions.
    fn calls_with_prefix(gh: &FakeGitHub, prefix: &str) -> Vec<String> {
        gh.calls()
            .into_iter()
            .filter(|c| c.starts_with(prefix))
            .collect()
    }

    /// The shared preamble for every P4 coordinator test: take the cross-test serial
    /// lock, drain any pre-existing terminal items so this test's `FakeGitHub` only
    /// records its own effects, create a fresh uniquely-named repo with the three-role
    /// config, and build the `(FakeGitHub, ToolsState)` the test drives the workers
    /// through. Returns the held lock guard (kept alive for the test's duration), the
    /// repo id, the `(manager, coder, reviewer)` worker names, and the gh/state pair.
    #[allow(clippy::type_complexity)]
    async fn p4_fixture(
        pool: &record::Pool,
        name_prefix: &str,
    ) -> (
        tokio::sync::MutexGuard<'static, ()>,
        Uuid,
        (&'static str, &'static str, &'static str),
        Arc<FakeGitHub>,
        ToolsState,
    ) {
        let serial = P4_SERIAL.lock().await;
        drain_terminal(pool).await;
        let repo = record::upsert_repo(pool, "acme", &unique(name_prefix), 1)
            .await
            .unwrap();
        let names = setup_three_role_repo(pool, repo).await;
        let gh = Arc::new(FakeGitHub::new());
        let state = tools_state(pool, gh.clone());
        (serial, repo, names, gh, state)
    }

    /// Bootstrap a CR on `issue#1`: enqueue a coder, open the change request through
    /// the Tools seam, and read its record-book row back. Returns the CR number and
    /// the persisted [`record::ChangeRequest`] row. The opening coder item id is not
    /// returned (callers that need to finish it look it up by subject).
    async fn open_cr(
        pool: &record::Pool,
        repo: Uuid,
        coder: &str,
        state: &ToolsState,
        body: &str,
    ) -> (i64, record::ChangeRequest) {
        let coder_item = enqueue_item(pool, repo, record::Kind::Coder, coder, "issue#1").await;
        let coder_auth = task_auth(coder_item, repo, record::Kind::Coder);
        let cr = factory_tools::open_or_update_change_request(
            state,
            &coder_auth,
            "[coder] issue#1",
            body,
            "factory/issue-1",
            "main",
            Some(1),
        )
        .await
        .unwrap();
        let cr_number = cr["cr_number"].as_i64().unwrap();
        let cr_row = record::change_request_by_number(pool, repo, cr_number as i32)
            .await
            .unwrap()
            .unwrap();
        (cr_number, cr_row)
    }

    #[tokio::test]
    async fn worked_example_happy_path_issue_to_merge_handoff() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (manager, _coder, _reviewer), gh, state) =
            p4_fixture(&pool, "e2e-p4-happy").await;

        // The manager triaged issue#1 and succeeded → coordinator assigns a coder.
        let manager_item =
            enqueue_item(&pool, repo, record::Kind::Manager, manager, "issue#1").await;
        mark_succeeded(&pool, manager_item).await;
        coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        // A coder item now exists for issue#1, and the issue is in_progress.
        assert_eq!(
            issue_state(&pool, repo, 1).await.as_deref(),
            Some("in_progress"),
            "issue moves to in_progress when delegated to a coder"
        );
        let coder_id = find_item(&pool, repo, record::Kind::Coder, "issue#1")
            .await
            .expect("a coder item was enqueued for issue#1");

        // Simulate the coder: open the CR, then mark the coder item succeeded.
        let coder_auth = task_auth(coder_id, repo, record::Kind::Coder);
        let cr = factory_tools::open_or_update_change_request(
            &state,
            &coder_auth,
            "[coder] issue#1",
            "Implemented the fix with tests",
            "factory/issue-1",
            "main",
            Some(1),
        )
        .await
        .unwrap();
        let cr_number = cr["cr_number"].as_i64().expect("a cr_number");
        mark_succeeded(&pool, coder_id).await;
        coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        // A reviewer item now exists for cr#<n>.
        let cr_subject = format!("cr#{cr_number}");
        let reviewer_id = find_item(&pool, repo, record::Kind::Reviewer, &cr_subject)
            .await
            .expect("a reviewer item was enqueued for the CR");

        // Simulate the reviewer: approve (sets CR factory_state=approved). Seed the
        // CR head's checks green; default file list is non-gating.
        let reviewer_auth = task_auth(reviewer_id, repo, record::Kind::Reviewer);
        factory_tools::submit_review(
            &state,
            &reviewer_auth,
            cr_number,
            "approve",
            "LGTM",
            Some("rev-1"),
        )
        .await
        .unwrap();
        let gh = Arc::new(FakeGitHub::new().with_checks(
            &format!("sha-{cr_number}"),
            CheckSummary {
                overall: CheckConclusion::Success,
                checks: vec![("ci".into(), CheckConclusion::Success)],
            },
        ));
        mark_succeeded(&pool, reviewer_id).await;
        coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        // The CR is marked ready, exactly one handoff comment + one assignment ran,
        // and the issue stays OPEN (in_progress) awaiting the human's merge — P4 hands
        // off for merge; it does not close the issue (that needs merge-webhook ingestion).
        let cr_row = record::change_request_by_number(&pool, repo, cr_number as i32)
            .await
            .unwrap()
            .expect("CR row");
        assert_eq!(cr_row.factory_state, "ready", "CR is ready-for-human-merge");
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            1,
            "exactly one handoff comment"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            1,
            "exactly one assignment"
        );
        assert_eq!(
            issue_state(&pool, repo, 1).await.as_deref(),
            Some("in_progress"),
            "the issue stays open (in_progress) awaiting the human's merge; NOT closed at handoff"
        );
    }

    #[tokio::test]
    async fn changes_requested_rework_loop_increments_and_caps() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (_manager, coder, reviewer), gh, state) =
            p4_fixture(&pool, "e2e-p4-rework").await;

        // Bootstrap a CR on issue#1: one coder opens it.
        let (cr_number, cr_row) = open_cr(&pool, repo, coder, &state, "first attempt").await;
        let cr_subject = format!("cr#{cr_number}");
        // The bootstrap coder finished opening the CR: mark it terminal AND reacted
        // so it neither occupies the active (repo, issue#1, coder) slot (which would
        // make the coordinator's rework dispatch dedupe against it) nor shows up as
        // an extra item for the coordinator to react to in the cycles below.
        let first_coder = find_item(&pool, repo, record::Kind::Coder, "issue#1")
            .await
            .expect("the bootstrap coder item");
        mark_succeeded(&pool, first_coder).await;
        assert!(record::mark_reacted(&pool, first_coder).await.unwrap());

        // The reviewer requests changes repeatedly. Each cycle: a reviewer item for
        // the CR requests changes and succeeds; the coordinator increments the loop
        // count and (below the cap) re-dispatches a coder for the issue. The first
        // cycle's reviewer is enqueued here; each subsequent cycle's reviewer is the
        // one the coordinator wakes when the cycle's rework coder succeeds.
        // LOOP_CAP = 5: cycles 1..=4 re-dispatch; the 5th escalates.
        let mut reviewer_id =
            enqueue_item(&pool, repo, record::Kind::Reviewer, reviewer, &cr_subject).await;
        for cycle in 1..=5 {
            let reviewer_auth = task_auth(reviewer_id, repo, record::Kind::Reviewer);
            factory_tools::submit_review(
                &state,
                &reviewer_auth,
                cr_number,
                "request_changes",
                "needs work",
                Some(&format!("rev-{cycle}")),
            )
            .await
            .unwrap();
            mark_succeeded(&pool, reviewer_id).await;
            coordinator_tick(&pool, gh.as_ref()).await.unwrap();

            assert_eq!(
                record::cr_loop_count(&pool, cr_row.id).await.unwrap(),
                cycle,
                "loop count increments each changes-requested cycle"
            );

            if cycle < 5 {
                // A fresh rework coder item exists for the issue (the coordinator's
                // rework dispatch). The rework pushes to the EXISTING CR/branch, so it
                // opens no new CR row; mark it succeeded and react (a coder-succeeded
                // with no CR row is a no-op). Then wake the next reviewer for the CR
                // directly (idempotent by active (repo, subject, kind)).
                let rework = find_item(&pool, repo, record::Kind::Coder, "issue#1")
                    .await
                    .expect("a rework coder item exists below the cap");
                assert_ne!(rework, first_coder, "rework is a new coder item");
                mark_succeeded(&pool, rework).await;
                coordinator_tick(&pool, gh.as_ref()).await.unwrap();
                reviewer_id =
                    enqueue_item(&pool, repo, record::Kind::Reviewer, reviewer, &cr_subject).await;
            }
        }

        // At the cap: no NEW coder was enqueued for the rework, and exactly one
        // loop_cap escalation exists with exactly one comment + one assignment.
        assert!(
            find_item(&pool, repo, record::Kind::Coder, "issue#1")
                .await
                .is_none(),
            "no further coder is enqueued once the loop cap is hit"
        );
        // The escalation is recorded against the reviewer item that hit the cap.
        let escalations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM escalations WHERE reason = 'loop_cap' \
             AND work_item_id IN (SELECT id FROM work_items WHERE repo_id = $1)",
        )
        .bind(repo)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(escalations, 1, "exactly one loop_cap escalation");
    }

    #[tokio::test]
    async fn exactly_once_escalation_under_redispatch() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (_manager, coder, reviewer), gh, state) =
            p4_fixture(&pool, "e2e-p4-once").await;

        // A CR already at the loop cap (loop_count = LOOP_CAP - 1, so the next
        // changes-requested reaches it). Open the CR via the coder seam.
        let (cr_number, cr_row) = open_cr(&pool, repo, coder, &state, "attempt").await;
        let cr_subject = format!("cr#{cr_number}");
        for _ in 0..4 {
            record::increment_cr_loop_count(&pool, cr_row.id)
                .await
                .unwrap();
        }

        // The reviewer requests changes → the next increment hits the cap → escalate.
        let reviewer_id =
            enqueue_item(&pool, repo, record::Kind::Reviewer, reviewer, &cr_subject).await;
        let reviewer_auth = task_auth(reviewer_id, repo, record::Kind::Reviewer);
        factory_tools::submit_review(
            &state,
            &reviewer_auth,
            cr_number,
            "request_changes",
            "still wrong",
            Some("rev-final"),
        )
        .await
        .unwrap();
        mark_succeeded(&pool, reviewer_id).await;
        coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        // Simulate a re-dispatch: clear the react flag and run the tick AGAIN. The
        // exactly-once escalation index + run-side-effects-only-when-created rule
        // mean no second escalation, comment, or assignment.
        sqlx::query("UPDATE work_items SET reacted = false WHERE id = $1")
            .bind(reviewer_id)
            .execute(&pool)
            .await
            .unwrap();
        let _ = coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        assert_eq!(
            record::count_escalations_for_item(&pool, reviewer_id)
                .await
                .unwrap(),
            1,
            "exactly one escalation survives the re-dispatch"
        );
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            1,
            "exactly one escalation comment"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            1,
            "exactly one escalation assignment"
        );
    }

    #[tokio::test]
    async fn cannot_weaken_own_gating_checks_is_escalated() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        // The fixture's gh/state act as the setup pair: open the CR on a default
        // FakeGitHub so we learn its number, then below build the assertion FakeGitHub
        // seeded with a gating file list + green checks for that CR (green checks prove
        // the guard, not the checks, blocks it).
        let (_serial, repo, (_manager, coder, reviewer), _setup_gh, setup_state) =
            p4_fixture(&pool, "e2e-p4-gating").await;
        let (cr_number, _cr_row) = open_cr(&pool, repo, coder, &setup_state, "disables CI").await;
        let cr_subject = format!("cr#{cr_number}");

        let gh = Arc::new(
            FakeGitHub::new()
                .with_cr_files(cr_number, vec![".github/workflows/ci.yml".into()])
                .with_checks(
                    &format!("sha-{cr_number}"),
                    CheckSummary {
                        overall: CheckConclusion::Success,
                        checks: vec![("ci".into(), CheckConclusion::Success)],
                    },
                ),
        );
        let state = tools_state(&pool, gh.clone());

        // The reviewer approves the gating-weakening CR.
        let reviewer_id =
            enqueue_item(&pool, repo, record::Kind::Reviewer, reviewer, &cr_subject).await;
        let reviewer_auth = task_auth(reviewer_id, repo, record::Kind::Reviewer);
        factory_tools::submit_review(
            &state,
            &reviewer_auth,
            cr_number,
            "approve",
            "ship it",
            Some("rev-1"),
        )
        .await
        .unwrap();
        mark_succeeded(&pool, reviewer_id).await;
        coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        // The CR is NEVER marked ready; exactly one conflict escalation exists with
        // one comment + one assignment.
        let cr_row = record::change_request_by_number(&pool, repo, cr_number as i32)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            cr_row.factory_state, "ready",
            "CR must never be marked ready"
        );
        assert_eq!(cr_row.factory_state, "approved", "CR stays approved");
        let conflicts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM escalations WHERE reason = 'conflict' AND work_item_id = $1",
        )
        .bind(reviewer_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(conflicts, 1, "exactly one conflict escalation");
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            1,
            "exactly one escalation comment"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            1,
            "exactly one escalation assignment"
        );
    }

    /// Like the happy/gating tests, but the gating prefix is a CUSTOM, non-default
    /// path (`ci/`) declared via `settings.gating_paths`. A CR touching `ci/…` must be
    /// escalated as a conflict and NEVER marked ready — proving gating is config-driven,
    /// not hardcoded to `.github/workflows/`. (F3)
    #[tokio::test]
    async fn custom_gating_path_weaken_is_escalated() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let serial = P4_SERIAL.lock().await;
        drain_terminal(&pool).await;
        let repo = record::upsert_repo(&pool, "acme", &unique("e2e-p4-customgate"), 1)
            .await
            .unwrap();

        // A three-role config whose gating_paths is a CUSTOM directory ("ci/"), NOT the
        // default. A CR touching `ci/…` must trip the can't-weaken guard.
        let (manager, coder, reviewer) = ("manager", "bug-fixer", "reviewer");
        let yaml = format!(
            "version: 1\n\
             settings:\n  label: factory\n  gating_paths:\n    - \"ci/\"\n\
             limits:\n  max_concurrent_workers: 5\n\
             workers:\n\
             \x20 - name: {manager}\n    role: manager\n    on: [issue_labeled]\n\
             \x20 - name: {coder}\n    role: coder\n    on: [manager, cr_changes_requested]\n\
             \x20 - name: {reviewer}\n    role: reviewer\n    on: [cr_opened]\n"
        );
        let cfg = factory_config::parse(&yaml).expect("valid custom-gating config");
        assert_eq!(
            cfg.settings.gating_paths,
            vec!["ci/".to_owned()],
            "the custom gating prefix is in effect for this repo"
        );
        let cfg_json = serde_json::to_value(&cfg).unwrap();
        record::set_repo_config(&pool, repo, &cfg_json, "p4-sha")
            .await
            .unwrap();

        // Open the CR on a throwaway client to learn its number.
        let setup_gh = Arc::new(FakeGitHub::new());
        let setup_state = tools_state(&pool, setup_gh.clone());
        let (cr_number, _cr_row) =
            open_cr(&pool, repo, coder, &setup_state, "tweaks the ci dir").await;
        let cr_subject = format!("cr#{cr_number}");

        // The assertion client: the CR touches a CUSTOM gating path (`ci/deploy.sh`),
        // with green checks (proving the GUARD, not the checks, blocks it).
        let gh = Arc::new(
            FakeGitHub::new()
                .with_cr_files(cr_number, vec!["ci/deploy.sh".into()])
                .with_checks(
                    &format!("sha-{cr_number}"),
                    CheckSummary {
                        overall: CheckConclusion::Success,
                        checks: vec![("ci".into(), CheckConclusion::Success)],
                    },
                ),
        );
        let state = tools_state(&pool, gh.clone());

        let reviewer_id =
            enqueue_item(&pool, repo, record::Kind::Reviewer, reviewer, &cr_subject).await;
        let reviewer_auth = task_auth(reviewer_id, repo, record::Kind::Reviewer);
        factory_tools::submit_review(
            &state,
            &reviewer_auth,
            cr_number,
            "approve",
            "ship it",
            Some("rev-1"),
        )
        .await
        .unwrap();
        mark_succeeded(&pool, reviewer_id).await;
        coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        // Never ready; exactly one conflict escalation with one comment + one assignment.
        let cr_row = record::change_request_by_number(&pool, repo, cr_number as i32)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            cr_row.factory_state, "ready",
            "a CR weakening a CUSTOM gating path must never be ready"
        );
        assert_eq!(cr_row.factory_state, "approved", "CR stays approved");
        let conflicts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM escalations WHERE reason = 'conflict' AND work_item_id = $1",
        )
        .bind(reviewer_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(conflicts, 1, "exactly one conflict escalation");
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            1,
            "exactly one escalation comment"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            1,
            "exactly one escalation assignment"
        );
        drop(serial);
    }

    /// F5 + F7: a Failed reviewer item (subject `cr#N`) with attempts exhausted escalates
    /// `retries_exhausted` — exactly one escalation, one comment, AND one assignment. The
    /// assignment proves the escalation resolves the human from the CR's linked issue even
    /// though the failed item's own subject (`cr#N`) carries no issue (F7).
    #[tokio::test]
    async fn retries_exhausted_failed_reviewer_escalates_and_assigns() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (_manager, coder, reviewer), gh, state) =
            p4_fixture(&pool, "e2e-p4-retries").await;

        // A CR on issue#1 (open_cr links the CR to issue 1).
        let (cr_number, _cr_row) = open_cr(&pool, repo, coder, &state, "attempt").await;
        let cr_subject = format!("cr#{cr_number}");

        // A reviewer item for that CR that FAILED with attempts at the cap.
        let reviewer_id =
            enqueue_item(&pool, repo, record::Kind::Reviewer, reviewer, &cr_subject).await;
        force_failed(&pool, reviewer_id, 3).await; // RETRY_CAP = 3 → escalate

        coordinator_tick(&pool, gh.as_ref()).await.unwrap();
        assert!(
            is_reacted(&pool, reviewer_id).await,
            "the failed reviewer item is reacted"
        );

        // Exactly one retries_exhausted escalation, with a comment AND an assignment
        // (the linked issue #1 resolved from the CR, not from the cr# subject).
        let retries: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM escalations WHERE reason = 'retries_exhausted' AND work_item_id = $1",
        )
        .bind(reviewer_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(retries, 1, "exactly one retries_exhausted escalation");
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            1,
            "exactly one escalation comment"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            1,
            "the failed reviewer escalation assigns the CR's linked issue to a human"
        );
    }

    /// C1: an explicitly-Failed item UNDER the retry cap is RE-QUEUED for a fresh
    /// attempt (state `queued`, `attempt_count + 1`, `reacted = false`), NOT escalated
    /// and NOT silently dropped. Then, at the cap, the same kind of failure escalates
    /// exactly once. (Default `retry_cap` = 3: attempt_count 1 → requeue to 2; a fresh
    /// failed row at attempt_count 3 → escalate.)
    #[tokio::test]
    async fn failed_item_under_cap_is_requeued_then_escalated_at_cap() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (_manager, coder, _reviewer), gh, _state) =
            p4_fixture(&pool, "e2e-p4-retry").await;

        // (A) A failed coder on issue#1 at attempt_count 1 (< cap 3): re-queued, not
        // escalated. `reacted` must reset to false so a SUBSEQUENT failure of this same
        // row is reacted to again.
        let failed = enqueue_item(&pool, repo, record::Kind::Coder, coder, "issue#1").await;
        force_failed(&pool, failed, 1).await;

        assert_eq!(
            coordinator_tick(&pool, gh.as_ref()).await.unwrap(),
            1,
            "the failed item is reacted exactly once"
        );

        let (state, attempts, reacted) = item_state_attempts_reacted(&pool, failed).await;
        assert_eq!(state, "queued", "a failed item under the cap is re-queued");
        assert_eq!(attempts, 2, "attempt_count is bumped on re-queue (1 → 2)");
        assert!(
            !reacted,
            "reacted is reset so the next failure of this row is reacted to again"
        );
        assert_eq!(
            record::count_escalations_for_item(&pool, failed)
                .await
                .unwrap(),
            0,
            "an under-cap retry never escalates"
        );
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            0,
            "no escalation comment for an under-cap retry"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            0,
            "no assignment for an under-cap retry"
        );

        // (B) A fresh failed coder on issue#2 AT the cap (attempt_count 3): escalates
        // exactly once with one comment + one assignment, and is NOT re-queued.
        let exhausted = enqueue_item(&pool, repo, record::Kind::Coder, coder, "issue#2").await;
        force_failed(&pool, exhausted, 3).await;

        coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        let (state2, _attempts2, _reacted2) = item_state_attempts_reacted(&pool, exhausted).await;
        assert_eq!(
            state2, "failed",
            "an at-cap item is escalated, not re-queued"
        );
        let retries: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM escalations WHERE reason = 'retries_exhausted' AND work_item_id = $1",
        )
        .bind(exhausted)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            retries, 1,
            "exactly one retries_exhausted escalation at the cap"
        );
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            1,
            "exactly one escalation comment (only the at-cap item)"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            1,
            "exactly one escalation assignment (only the at-cap item)"
        );
    }

    /// Lease the next queued item for `worker`, asserting one becomes available and is
    /// the expected row. Mirrors the dispatcher's lease leg of the real retry loop.
    async fn lease_one(pool: &record::Pool, repo: Uuid, worker: &str, expect: Uuid) {
        let req = record::LeaseRequest {
            repo_id: repo,
            worker_name: worker.into(),
            lease_owner: "e2e-dispatcher".into(),
            ttl_secs: 60.0,
            per_worker_cap: 1000,
            global_cap: 1000,
        };
        let leased = record::lease_next(pool, &req)
            .await
            .unwrap()
            .expect("a re-queued item must be re-leasable");
        assert_eq!(
            leased.id, expect,
            "the re-leased item is the same requeued row"
        );
    }

    /// Fail a leased item the way `record_outcome(\"failed\")` does: set state Failed
    /// WITHOUT bumping `attempt_count` (only lease-expiry reclaim bumps attempts). This
    /// is the REAL failure the coordinator's retry path must observe — not `force_failed`.
    async fn fail_via_outcome(pool: &record::Pool, id: Uuid) {
        assert!(
            record::set_state(pool, id, record::State::Failed)
                .await
                .unwrap(),
            "a leased item transitions to failed"
        );
    }

    /// F1: drive the REAL failure → requeue → re-lease → re-fail loop end to end (NOT
    /// `force_failed`). A coder is leased, fails via the `record_outcome` path (state
    /// Failed, no attempt bump), and the coordinator requeues it (state queued,
    /// attempt_count++, reacted reset). The dispatcher then RE-LEASES the same row — the
    /// re-lease leg that was previously untested — and the cycle repeats until the retry
    /// cap, where exactly one `retries_exhausted` escalation is recorded. Verifies the
    /// item was re-leased the expected number of times between failures.
    #[tokio::test]
    async fn failed_item_requeue_release_loop_until_cap() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (_manager, coder, _reviewer), gh, _state) =
            p4_fixture(&pool, "e2e-p4-relooploop").await;

        // RETRY_CAP is the default 3: a fresh item (attempt_count 0) is requeued at
        // attempts 0, 1, 2 (3 retries, each followed by a real re-lease) and escalates
        // once attempt_count reaches 3.
        const CAP: i32 = 3;
        let item = enqueue_item(&pool, repo, record::Kind::Coder, coder, "issue#1").await;

        let mut releases = 0;
        for attempt in 0..CAP {
            // Lease the item (the FIRST iteration leases the freshly-enqueued row; every
            // later iteration leases the REQUEUED row — the previously-untested leg).
            lease_one(&pool, repo, coder, item).await;
            releases += 1;
            // The worker fails (real record_outcome path: Failed, no attempt bump).
            fail_via_outcome(&pool, item).await;

            // The coordinator reacts: under the cap → requeue (not escalate).
            assert_eq!(
                coordinator_tick(&pool, gh.as_ref()).await.unwrap(),
                1,
                "the failed item is reacted exactly once on attempt {attempt}"
            );
            let (state, attempts, reacted) = item_state_attempts_reacted(&pool, item).await;
            assert_eq!(
                state, "queued",
                "under-cap failure is requeued (attempt {attempt})"
            );
            assert_eq!(
                attempts,
                attempt + 1,
                "attempt_count is bumped on each requeue"
            );
            assert!(
                !reacted,
                "reacted is reset so the NEXT real failure is reacted to again"
            );
            assert_eq!(
                record::count_escalations_for_item(&pool, item)
                    .await
                    .unwrap(),
                0,
                "no escalation while under the cap (attempt {attempt})"
            );
            assert_eq!(
                calls_with_prefix(&gh, "post_comment").len(),
                0,
                "no escalation comment while under the cap"
            );
        }
        assert_eq!(
            releases, CAP,
            "the item was re-leased once per under-cap retry"
        );

        // Now AT the cap (attempt_count == 3): lease, fail, tick → escalate exactly once.
        lease_one(&pool, repo, coder, item).await;
        fail_via_outcome(&pool, item).await;
        coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        let (state, attempts, _reacted) = item_state_attempts_reacted(&pool, item).await;
        assert_eq!(state, "failed", "an at-cap item is escalated, not requeued");
        assert_eq!(
            attempts, CAP,
            "at-cap attempt_count is the cap (no further bump)"
        );
        let exhausted: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM escalations WHERE reason = 'retries_exhausted' AND work_item_id = $1",
        )
        .bind(item)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            exhausted, 1,
            "exactly one retries_exhausted escalation at the cap"
        );
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            1,
            "exactly one escalation comment (only at the cap)"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            1,
            "exactly one escalation assignment (only at the cap)"
        );
    }

    /// F3 TOCTOU: a late or duplicate review verdict must NOT clobber a CR the
    /// coordinator has already advanced to `ready`/`merged`/`closed`. The verdict write
    /// is an atomic conditional UPDATE (`record_verdict_cr_state` →
    /// `set_change_request_state_if_not_terminal`), so a `ready` CR is left untouched,
    /// while an `approved`/`open` CR IS updated.
    #[tokio::test]
    async fn late_verdict_does_not_clobber_ready_cr() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (_manager, coder, reviewer), _gh, _state) =
            p4_fixture(&pool, "e2e-p4-toctou").await;

        // A CR that the coordinator has already handed off (factory_state = ready).
        let (cr_number, cr_row) = open_cr(&pool, repo, coder, &state_for(&pool), "body").await;
        record::set_change_request_state(&pool, cr_row.id, "ready")
            .await
            .unwrap();

        // A reviewer submits a LATE approve verdict for the same CR. The verdict path
        // must NOT drag `ready` back to `approved`.
        let reviewer_item = enqueue_item(
            &pool,
            repo,
            record::Kind::Reviewer,
            reviewer,
            &format!("cr#{cr_number}"),
        )
        .await;
        let reviewer_auth = task_auth(reviewer_item, repo, record::Kind::Reviewer);
        factory_tools::submit_review(
            &state_for(&pool),
            &reviewer_auth,
            cr_number,
            "approve",
            "late lgtm",
            None,
        )
        .await
        .unwrap();

        let after = record::change_request_by_number(&pool, repo, cr_number as i32)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after.factory_state, "ready",
            "a late verdict must not clobber a CR already marked ready"
        );

        // Contrast: an `approved`/`open` CR IS updated by a verdict. Drive the same CR
        // from a non-terminal state and confirm the verdict writes through.
        record::set_change_request_state(&pool, cr_row.id, "open")
            .await
            .unwrap();
        factory_tools::submit_review(
            &state_for(&pool),
            &reviewer_auth,
            cr_number,
            "approve",
            "lgtm",
            None,
        )
        .await
        .unwrap();
        let updated = record::change_request_by_number(&pool, repo, cr_number as i32)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            updated.factory_state, "approved",
            "a verdict on a non-terminal CR (open) updates its factory state"
        );
    }

    /// C2: a reviewer-requested-changes CR that is BELOW the loop cap but has NO linked
    /// issue cannot be reworked (there is no issue to re-dispatch a coder against).
    /// Rather than STRANDING the CR (no coder, no escalation — stuck forever), the
    /// coordinator escalates `needs_decision` exactly once.
    #[tokio::test]
    async fn changes_requested_without_linked_issue_escalates_needs_decision() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (_manager, coder, reviewer), gh, _state) =
            p4_fixture(&pool, "e2e-p4-nolink").await;

        // A CR with NO linked issue, in `changes_requested`, below the loop cap. Build it
        // directly: a coder item opens the CR row (linked_issue = None), give it a number,
        // and set its factory_state to changes_requested.
        let coder_item = enqueue_item(&pool, repo, record::Kind::Coder, coder, "issue#7").await;
        let cr = record::upsert_change_request_for_item(
            &pool,
            repo,
            coder_item,
            "factory/unlinked",
            None, // no linked issue — the C2 condition
        )
        .await
        .unwrap();
        record::set_change_request_number(&pool, cr.id, 42)
            .await
            .unwrap();
        record::set_change_request_state(&pool, cr.id, "changes_requested")
            .await
            .unwrap();

        // A succeeded reviewer item whose subject is that CR.
        let reviewer_id =
            enqueue_item(&pool, repo, record::Kind::Reviewer, reviewer, "cr#42").await;
        mark_succeeded(&pool, reviewer_id).await;

        coordinator_tick(&pool, gh.as_ref()).await.unwrap();

        // Exactly one needs_decision escalation against the reviewer item; the CR is NOT
        // reworked (no coder enqueued) and NOT left stranded.
        let needs: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM escalations WHERE reason = 'needs_decision' AND work_item_id = $1",
        )
        .bind(reviewer_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(needs, 1, "exactly one needs_decision escalation");
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            1,
            "exactly one escalation comment (delivered to the CR)"
        );
        // No coder rework was dispatched (there was no issue to rework against).
        let coders: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM work_items
              WHERE repo_id = $1 AND kind = 'coder' AND state IN ('queued', 'leased', 'running', 'blocked')
                AND id <> $2",
        )
        .bind(repo)
        .bind(coder_item)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(coders, 0, "no rework coder is enqueued for an unlinked CR");
    }

    /// F5: an already-`escalated` terminal item is a pure no-op react — the coordinator
    /// marks it reacted but creates NO new escalation and enqueues NO new work.
    #[tokio::test]
    async fn escalated_terminal_item_is_noop_react() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (_manager, coder, _reviewer), gh, _state) =
            p4_fixture(&pool, "e2e-p4-escalated").await;

        // A coder item for issue#1 already in the terminal `escalated` state.
        let item = enqueue_item(&pool, repo, record::Kind::Coder, coder, "issue#1").await;
        sqlx::query("UPDATE work_items SET state = 'escalated', updated_at = now() WHERE id = $1")
            .bind(item)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            coordinator_tick(&pool, gh.as_ref()).await.unwrap(),
            1,
            "the escalated item is reacted exactly once"
        );
        assert!(is_reacted(&pool, item).await, "the item is marked reacted");

        // No new escalation row for the item, and no new work item was enqueued.
        assert_eq!(
            record::count_escalations_for_item(&pool, item)
                .await
                .unwrap(),
            0,
            "a no-op react creates no escalation"
        );
        assert!(
            find_item(&pool, repo, record::Kind::Coder, "issue#1")
                .await
                .is_none(),
            "a no-op react enqueues no new work"
        );
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            0,
            "a no-op react posts no comment"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            0,
            "a no-op react assigns nobody"
        );
    }

    /// F5: an approved CR whose checks are NOT green (Pending, then Failure) is neither
    /// marked ready NOR escalated — it is left `approved` to be re-evaluated next tick.
    #[tokio::test]
    async fn approved_cr_with_non_green_checks_is_left_approved() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (_manager, coder, reviewer), _setup_gh, setup_state) =
            p4_fixture(&pool, "e2e-p4-notgreen").await;
        let (cr_number, _cr_row) = open_cr(&pool, repo, coder, &setup_state, "wip").await;
        let cr_subject = format!("cr#{cr_number}");

        // Approve the CR once (factory_state → approved); the reviewer item is re-used
        // across the two non-green check scenarios by clearing its react flag.
        let reviewer_id =
            enqueue_item(&pool, repo, record::Kind::Reviewer, reviewer, &cr_subject).await;
        let reviewer_auth = task_auth(reviewer_id, repo, record::Kind::Reviewer);
        factory_tools::submit_review(
            &state_for(&pool),
            &reviewer_auth,
            cr_number,
            "approve",
            "looks good",
            Some("rev-1"),
        )
        .await
        .unwrap();
        mark_succeeded(&pool, reviewer_id).await;

        for overall in [CheckConclusion::Pending, CheckConclusion::Failure] {
            // Reset the react claim so this tick re-reacts to the same reviewer item.
            sqlx::query("UPDATE work_items SET reacted = false WHERE id = $1")
                .bind(reviewer_id)
                .execute(&pool)
                .await
                .unwrap();
            let gh = Arc::new(FakeGitHub::new().with_checks(
                &format!("sha-{cr_number}"),
                CheckSummary {
                    overall,
                    checks: vec![("ci".into(), overall)],
                },
            ));
            coordinator_tick(&pool, gh.as_ref()).await.unwrap();

            let cr_row = record::change_request_by_number(&pool, repo, cr_number as i32)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                cr_row.factory_state,
                "approved",
                "a CR with {} checks stays approved (not ready)",
                overall.as_str()
            );
            assert_eq!(
                record::count_escalations_for_item(&pool, reviewer_id)
                    .await
                    .unwrap(),
                0,
                "non-green checks do not escalate ({})",
                overall.as_str()
            );
            assert_eq!(
                calls_with_prefix(&gh, "post_comment").len(),
                0,
                "no handoff comment when checks are not green ({})",
                overall.as_str()
            );
        }
    }

    /// Build a `ToolsState` over a throwaway `FakeGitHub` for a tool call whose GitHub
    /// effect the test does not assert on (e.g. recording an approval verdict).
    fn state_for(pool: &record::Pool) -> ToolsState {
        tools_state(pool, Arc::new(FakeGitHub::new()))
    }

    /// Whether a work item's `reacted` flag is set (the coordinator's per-item claim).
    async fn is_reacted(pool: &record::Pool, id: Uuid) -> bool {
        sqlx::query_scalar("SELECT reacted FROM work_items WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A work item's `(state, attempt_count, reacted)` — used by the retry test to
    /// assert the requeue resets state/attempts/reacted (C1).
    async fn item_state_attempts_reacted(pool: &record::Pool, id: Uuid) -> (String, i32, bool) {
        let row = sqlx::query("SELECT state, attempt_count, reacted FROM work_items WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        (
            row.get("state"),
            row.get("attempt_count"),
            row.get("reacted"),
        )
    }

    /// Force a work item to a Failed terminal state with `attempt_count` so the
    /// coordinator's retries-exhausted escalation path is exercised deterministically.
    async fn force_failed(pool: &record::Pool, id: Uuid, attempts: i32) {
        sqlx::query(
            "UPDATE work_items SET state = 'failed', attempt_count = $2, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(attempts)
        .execute(pool)
        .await
        .unwrap();
    }

    /// F1 crash-safety: the per-item claim (`mark_reacted`) and the DB decision commit
    /// **atomically**. A successful react leaves the item reacted AND its next step
    /// present together; a decision that ERRORS after the claim rolls the claim back, so
    /// the item stays terminal-but-unreacted and the NEXT tick retries it (rather than
    /// being lost forever — the F1 bug). We can't kill a process mid-transaction, so we
    /// assert both ends of the atomic boundary directly.
    #[tokio::test]
    async fn coordinator_react_is_crash_safe_decision_atomic() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (manager, _coder, _reviewer), gh, _state) =
            p4_fixture(&pool, "e2e-p4-atomic").await;

        // (A) Success: a manager-succeeded react both marks the item reacted AND
        // enqueues the coder + sets the issue in_progress — all committed together.
        let manager_item =
            enqueue_item(&pool, repo, record::Kind::Manager, manager, "issue#1").await;
        mark_succeeded(&pool, manager_item).await;
        assert_eq!(coordinator_tick(&pool, gh.as_ref()).await.unwrap(), 1);
        assert!(
            is_reacted(&pool, manager_item).await,
            "claim committed on success"
        );
        assert!(
            find_item(&pool, repo, record::Kind::Coder, "issue#1")
                .await
                .is_some(),
            "the decision (coder enqueue) committed atomically with the claim"
        );
        assert_eq!(
            issue_state(&pool, repo, 1).await.as_deref(),
            Some("in_progress"),
            "the issue-state write committed atomically with the claim"
        );

        // (B) Rollback on decision error: a manager whose subject issue number is out
        // of the issues table's i32 range makes the in-tx decision error AFTER the
        // claim. The whole transaction rolls back, so the item stays unreacted and a
        // later tick will retry it (no silent drop).
        let bad_issue = "issue#3000000000"; // > i32::MAX → in-tx decision error
        let bad_item = enqueue_item(&pool, repo, record::Kind::Manager, manager, bad_issue).await;
        mark_succeeded(&pool, bad_item).await;
        // The tick does not abort (per-item error isolation): it returns 0 reacted and
        // logs the failure, leaving the claim rolled back.
        assert_eq!(
            coordinator_tick(&pool, gh.as_ref()).await.unwrap(),
            0,
            "a decision error reacts to nothing this tick"
        );
        assert!(
            !is_reacted(&pool, bad_item).await,
            "the claim rolled back with the failed decision — the item is retryable"
        );
        assert!(
            find_item(&pool, repo, record::Kind::Coder, bad_issue)
                .await
                .is_none(),
            "no partial next-step was written when the decision errored"
        );
    }

    /// F4 per-issue serialization: two terminal items that both resolve to the SAME
    /// issue, processed by two coordinator ticks running concurrently, must not corrupt
    /// the per-issue decision. The advisory lock serializes the two reacts on the issue;
    /// combined with the atomic per-item claim, each item is reacted exactly once, the
    /// coder is enqueued exactly once, and exactly one escalation is recorded — no
    /// double-enqueue, no double-escalation, consistent issue state.
    #[tokio::test]
    async fn coordinator_serializes_per_issue() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let (_serial, repo, (manager, coder, _reviewer), gh, _state) =
            p4_fixture(&pool, "e2e-p4-serial").await;

        // Two terminal items, both resolving to issue#1: a succeeded manager (→ enqueue
        // a coder + set in_progress) and a failed coder with retries exhausted
        // (→ escalate retries_exhausted, assign issue#1). Distinct (kind, subject) so
        // both rows coexist; both reacts contend for the issue#1 advisory lock.
        let manager_item =
            enqueue_item(&pool, repo, record::Kind::Manager, manager, "issue#1").await;
        mark_succeeded(&pool, manager_item).await;
        let failed_coder = enqueue_item(&pool, repo, record::Kind::Coder, coder, "issue#1").await;
        force_failed(&pool, failed_coder, 3).await; // RETRY_CAP = 3 → escalate

        // Run two ticks concurrently against the same DB state. They race over the same
        // two terminal items; the per-item claim + per-issue lock keep every effect
        // exactly-once across both.
        let (gh_a, gh_b) = (gh.clone(), gh.clone());
        let (pool_a, pool_b) = (pool.clone(), pool.clone());
        let (a, b) = tokio::join!(
            tokio::spawn(async move { coordinator_tick(&pool_a, gh_a.as_ref()).await.unwrap() }),
            tokio::spawn(async move { coordinator_tick(&pool_b, gh_b.as_ref()).await.unwrap() }),
        );
        let total = a.unwrap() + b.unwrap();
        assert_eq!(
            total, 2,
            "each of the two terminal items is reacted exactly once across both ticks"
        );

        // Both items are reacted; the per-issue decision is consistent and exactly-once.
        assert!(is_reacted(&pool, manager_item).await);
        assert!(is_reacted(&pool, failed_coder).await);

        // Exactly one ACTIVE coder for issue#1 (the manager's enqueue, idempotent): the
        // failed coder is terminal, so a single queued coder remains.
        let active_coders: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM work_items
              WHERE repo_id = $1 AND kind = 'coder' AND subject_ref = 'issue#1'
                AND state IN ('queued', 'leased', 'running', 'blocked')",
        )
        .bind(repo)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active_coders, 1, "the coder is enqueued exactly once");

        assert_eq!(
            issue_state(&pool, repo, 1).await.as_deref(),
            Some("in_progress"),
            "the issue reaches a single consistent state"
        );

        // Exactly one retries-exhausted escalation for the failed coder, with exactly
        // one comment + one assignment (the lock + claim prevent a racing double-react
        // from re-running the human-facing side effects).
        assert_eq!(
            record::count_escalations_for_item(&pool, failed_coder)
                .await
                .unwrap(),
            1,
            "exactly one escalation for the failed item"
        );
        assert_eq!(
            calls_with_prefix(&gh, "post_comment").len(),
            1,
            "exactly one escalation comment across both concurrent ticks"
        );
        assert_eq!(
            calls_with_prefix(&gh, "assign_issue").len(),
            1,
            "exactly one escalation assignment across both concurrent ticks"
        );
    }

    /// Find a single non-terminal-or-any work item for `(repo, kind, subject)`.
    /// Used to assert the coordinator enqueued the expected next step.
    async fn find_item(
        pool: &record::Pool,
        repo: Uuid,
        kind: record::Kind,
        subject: &str,
    ) -> Option<Uuid> {
        sqlx::query_scalar(
            "SELECT id FROM work_items
              WHERE repo_id = $1 AND kind = $2 AND subject_ref = $3
                AND state IN ('queued', 'leased', 'running', 'blocked')
              ORDER BY created_at DESC LIMIT 1",
        )
        .bind(repo)
        .bind(kind.as_str())
        .bind(subject)
        .fetch_optional(pool)
        .await
        .unwrap()
    }

    /// The factory_state of an issue row, if it exists.
    async fn issue_state(pool: &record::Pool, repo: Uuid, number: i32) -> Option<String> {
        sqlx::query_scalar("SELECT factory_state FROM issues WHERE repo_id = $1 AND number = $2")
            .bind(repo)
            .bind(number)
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    // P5 — resilience
    #[test]
    #[ignore = "P5: implement needs-human-input blocked→resume"]
    fn needs_human_input_blocks_then_resumes_same_session_on_reply() {
        todo!("P5: worker hits high-stakes ambiguity → blocked + question on issue; a human reply resumes the SAME session")
    }

    #[test]
    #[ignore = "P5: implement checkpoint + claude --resume"]
    fn crash_resume_continues_the_same_session() {
        todo!("P5: kill a worker mid-run → the next dispatch resumes its same Claude Code session from checkpoint")
    }

    #[test]
    #[ignore = "P5: implement full retention (record purge + Blob lifecycle)"]
    fn retention_aging_removes_only_terminal_records() {
        todo!("P5: terminal records past the retention window are purged; active work is untouched (record purge exists in P1; Blob lifecycle in P5)")
    }
}
