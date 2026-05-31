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
        factory_dispatcher::dispatch_once(&pool, &launcher, "http://tools.test", "e2e")
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
    #[test]
    #[ignore = "P4: implement manager + coder + reviewer loop"]
    fn worked_example_happy_path_issue_to_merge_handoff() {
        todo!("P4: labelled issue → Manager → Coder (tests+code) → CR → Reviewer approve → ready-for-human-merge → issue closed")
    }

    #[test]
    #[ignore = "P4: implement bounded review-rework loop + cap"]
    fn changes_requested_rework_loop_increments_and_caps() {
        todo!("P4: repeated changes-requested increments loop_count; at the cap the work escalates instead of looping forever")
    }

    #[test]
    #[ignore = "P4: implement exactly-once escalation"]
    fn exactly_once_escalation_under_redispatch() {
        todo!("P4: a lease-expiry re-dispatch during/after escalation yields exactly ONE escalation (one comment + one assignment)")
    }

    #[test]
    #[ignore = "P4: implement the can't-weaken-own-checks guard"]
    fn cannot_weaken_own_gating_checks_is_escalated() {
        todo!("P4: a change that weakens/disables the checks gating its own work is rejected → escalated, never marked ready")
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
