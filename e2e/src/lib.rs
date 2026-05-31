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
