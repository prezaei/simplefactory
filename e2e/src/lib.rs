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
        let state = std::sync::Arc::new(factory_tools::ToolsState {
            pool: pool.clone(),
            gh,
            blob: std::sync::Arc::new(factory_blob::InMemoryTranscripts::new()),
        });

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

    /// `coordinator_tick` scans the WHOLE record book (it is a global service), so in
    /// a shared test DB the P4 tests — which assert exact per-tick react counts and
    /// exact FakeGitHub call counts against their OWN `FakeGitHub` — must not overlap
    /// with each other or react to each other's leftover terminal items. This mutex
    /// serializes the four P4 coordinator tests; each one [`drain_terminal`]s any
    /// pre-existing unreacted terminal items (through a throwaway client) before it
    /// begins, so its own `FakeGitHub` only ever records effects for its own repo.
    static P4_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// React to (and discard the side effects of) every currently-unreacted terminal
    /// item, so a subsequent scoped tick sees only freshly-created ones. Uses a
    /// throwaway `FakeGitHub` so the test's own client stays clean.
    async fn drain_terminal(pool: &record::Pool) {
        let sink = FakeGitHub::new();
        // A couple of passes settle any items a drain itself enqueues-then-terminates.
        for _ in 0..3 {
            if coordinator_tick(pool, &sink).await.unwrap() == 0 {
                break;
            }
        }
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

    #[tokio::test]
    async fn worked_example_happy_path_issue_to_merge_handoff() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let _serial = P4_SERIAL.lock().await;
        drain_terminal(&pool).await;
        let name = unique("e2e-p4-happy");
        let repo = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();
        let (manager, _coder, _reviewer) = setup_three_role_repo(&pool, repo).await;

        let gh = Arc::new(FakeGitHub::new());
        let state = tools_state(&pool, gh.clone());

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
        // and the issue reached its closed end state.
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
            Some("done"),
            "the issue reaches its closed end state"
        );
    }

    #[tokio::test]
    async fn changes_requested_rework_loop_increments_and_caps() {
        let Some(pool) = pool_or_skip().await else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let _serial = P4_SERIAL.lock().await;
        drain_terminal(&pool).await;
        let name = unique("e2e-p4-rework");
        let repo = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();
        let (_manager, coder, reviewer) = setup_three_role_repo(&pool, repo).await;

        let gh = Arc::new(FakeGitHub::new());
        let state = tools_state(&pool, gh.clone());

        // Bootstrap a CR on issue#1: one coder opens it.
        let first_coder = enqueue_item(&pool, repo, record::Kind::Coder, coder, "issue#1").await;
        let coder_auth = task_auth(first_coder, repo, record::Kind::Coder);
        let cr = factory_tools::open_or_update_change_request(
            &state,
            &coder_auth,
            "[coder] issue#1",
            "first attempt",
            "factory/issue-1",
            "main",
            Some(1),
        )
        .await
        .unwrap();
        let cr_number = cr["cr_number"].as_i64().unwrap();
        let cr_subject = format!("cr#{cr_number}");
        let cr_row = record::change_request_by_number(&pool, repo, cr_number as i32)
            .await
            .unwrap()
            .unwrap();
        // The bootstrap coder finished opening the CR: mark it terminal AND reacted
        // so it neither occupies the active (repo, issue#1, coder) slot (which would
        // make the coordinator's rework dispatch dedupe against it) nor shows up as
        // an extra item for the coordinator to react to in the cycles below.
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
        let _serial = P4_SERIAL.lock().await;
        drain_terminal(&pool).await;
        let name = unique("e2e-p4-once");
        let repo = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();
        let (_manager, coder, reviewer) = setup_three_role_repo(&pool, repo).await;

        let gh = Arc::new(FakeGitHub::new());
        let state = tools_state(&pool, gh.clone());

        // A CR already at the loop cap (loop_count = LOOP_CAP - 1, so the next
        // changes-requested reaches it). Open the CR via the coder seam.
        let coder_item = enqueue_item(&pool, repo, record::Kind::Coder, coder, "issue#1").await;
        let coder_auth = task_auth(coder_item, repo, record::Kind::Coder);
        let cr = factory_tools::open_or_update_change_request(
            &state,
            &coder_auth,
            "[coder] issue#1",
            "attempt",
            "factory/issue-1",
            "main",
            Some(1),
        )
        .await
        .unwrap();
        let cr_number = cr["cr_number"].as_i64().unwrap();
        let cr_subject = format!("cr#{cr_number}");
        let cr_row = record::change_request_by_number(&pool, repo, cr_number as i32)
            .await
            .unwrap()
            .unwrap();
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
        let _serial = P4_SERIAL.lock().await;
        drain_terminal(&pool).await;
        let name = unique("e2e-p4-gating");
        let repo = record::upsert_repo(&pool, "acme", &name, 1).await.unwrap();
        let (_manager, coder, reviewer) = setup_three_role_repo(&pool, repo).await;

        // First open the CR on a default FakeGitHub so we learn its number, then
        // build the assertion FakeGitHub seeded with a gating file list + green
        // checks for that CR (green checks prove the guard, not the checks, blocks it).
        let setup_gh = Arc::new(FakeGitHub::new());
        let setup_state = tools_state(&pool, setup_gh.clone());
        let coder_item = enqueue_item(&pool, repo, record::Kind::Coder, coder, "issue#1").await;
        let coder_auth = task_auth(coder_item, repo, record::Kind::Coder);
        let cr = factory_tools::open_or_update_change_request(
            &setup_state,
            &coder_auth,
            "[coder] issue#1",
            "disables CI",
            "factory/issue-1",
            "main",
            Some(1),
        )
        .await
        .unwrap();
        let cr_number = cr["cr_number"].as_i64().unwrap();
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
