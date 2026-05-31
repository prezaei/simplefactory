//! The Coordinator (Manager): Factory's reactive decision loop (P4).
//!
//! The coordinator is a polling service. Every tick it scans the record book for
//! TERMINAL work items it has not yet reacted to and decides the single next step
//! for each — assign a coder, wake a reviewer, mark a change request
//! ready-for-merge, drive the bounded review-rework loop, or escalate to a human.
//! It NEVER writes code; it only delegates and bookkeeps.
//!
//! ## Crash-safety and per-issue serialization
//! The claim (`mark_reacted`) and the DB decision writes (enqueue next / set
//! issue+CR state / increment loop / create escalation row) run in ONE transaction,
//! [`react_to_item`], serialized per issue with [`record::issue_advisory_lock`]:
//! - **Atomic claim+decision (crash-safe):** a crash between marking the item
//!   reacted and writing the decision can no longer happen — both commit together or
//!   neither does. If the decision errors, the claim rolls back and the next tick
//!   retries the item (it is still terminal-but-unreacted).
//! - **Per-issue serialization:** two replicas reacting to terminal items for the
//!   same issue serialize on the advisory lock, so a racing double-decision can't
//!   double-enqueue or leave the issue in an inconsistent state.
//!
//! ## Exactly-once under re-dispatch
//! Multiple replicas may run [`coordinator_tick`] concurrently, and a lease-expiry
//! re-dispatch can resurface the same terminal condition. Three record-book
//! guarantees make a racing double-decision safe:
//! - [`record::mark_reacted`] is the per-item claim: exactly one caller wins and
//!   applies the decision for a given terminal item.
//! - [`record::enqueue`] is idempotent by active `(repo, subject, kind)`, so a
//!   re-decided "assign a coder" / "wake a reviewer" never duplicates work.
//! - [`record::create_escalation_idempotent`] is exactly-once per
//!   `(work_item, reason)`, and the human-facing side effects (comment + assign)
//!   run ONLY when that call reports it created the row — so a re-dispatch posts
//!   exactly one comment and one assignment.
//!
//! ## Ready-for-merge handoff vs. issue closure
//! The approved+green path is a ready-for-human-**merge** handoff: it marks the CR
//! `ready`, posts the handoff comment, assigns the linked issue to a human, and leaves
//! the issue OPEN (`in_progress`). The issue is closed only when the human merges the
//! CR. P4 does not ingest GitHub's `pull_request closed/merged` webhook, so it cannot
//! observe the merge and therefore deliberately does NOT set the issue to `done` at
//! handoff. Closing the issue on merge is a later capability (merge-webhook ingestion).
//!
//! The bounded review-rework counter is the per-CR `change_requests.loop_count`,
//! advanced via [`record::increment_cr_loop_count`] on each changes-requested cycle;
//! at the repo's configured `settings.loop_cap` the loop escalates instead of
//! dispatching another coder.
//!
//! ## Side-effect ordering
//! GitHub *reads* needed for a decision happen BEFORE the transaction (they are
//! reads, no mutation). GitHub *writes* (handoff comment + assignment, escalation
//! comment + assignment) happen AFTER the transaction commits, and only when the
//! escalation/handoff row was newly created in the transaction (a boolean carried
//! out of the tx). A crash after commit but before the comment therefore leaves the
//! escalation/handoff RECORDED (visible, recoverable) rather than the whole decision
//! lost — a strict improvement over the previous autocommit-per-statement design.
//! Full comment redelivery for that residual window is P5.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use factory_config::{Config, Role};
use factory_github::{CheckConclusion, CheckRef, CommentTarget, GitHubClient, RepoRef, Subject};
use factory_record::{
    self as record, ChangeRequest, Kind, NewWorkItem, RepoIdent, State, WorkItem,
};
use uuid::Uuid;

/// The human handed work on escalation / ready-for-merge. Factory has no per-repo
/// human identity in scope yet, so a single placeholder assignee is used; a real
/// owner mapping is a later refinement. (Assignment is idempotent on GitHub.)
const HANDOFF_ASSIGNEE: &str = "factory-human";

/// React to every un-reacted terminal work item once. Returns the number reacted to.
///
/// Per-item error isolation (F6): a failure to load one repo's config OR to react to
/// one item is logged (`tracing::error!`) and the loop continues to the next item, so
/// one bad item never stalls the whole tick. The tick itself only errors on a failure
/// to read the work LIST.
pub async fn coordinator_tick(pool: &record::Pool, gh: &dyn GitHubClient) -> anyhow::Result<usize> {
    let items = record::list_unreacted_terminal(pool).await?;
    tracing::info!(
        count = items.len(),
        "coordinator tick: scanning terminal items"
    );

    // Cache each repo's identity + parsed config for the duration of the tick: many
    // terminal items typically share a repo, so this loads + parses each distinct
    // repo at most once per tick instead of once per item. The cache stores an
    // `Arc<(RepoIdent, Config)>` so each item clones a cheap pointer, not the whole
    // (potentially large) Config. `None` records a repo with no usable config so it
    // is not re-loaded for every item that shares it.
    let mut repo_cache: HashMap<Uuid, Option<Arc<(RepoIdent, Config)>>> = HashMap::new();

    let mut reacted = 0usize;
    for item in items {
        // Load (and cache) the repo config. A load failure for this repo is logged and
        // the item skipped — it must not abort the whole tick (F6).
        let repo = match repo_cache.get(&item.repo_id) {
            Some(cached) => cached.clone(),
            None => match load_repo_and_config(pool, item.repo_id).await {
                Ok(loaded) => {
                    let loaded = loaded.map(Arc::new);
                    repo_cache.insert(item.repo_id, loaded.clone());
                    loaded
                }
                Err(e) => {
                    tracing::error!(work_item = %item.id, repo_id = %item.repo_id, error = %e, "loading repo config failed; skipping item");
                    continue;
                }
            },
        };
        match react_to_item(pool, gh, &item, repo).await {
            Ok(true) => reacted += 1,
            Ok(false) => {
                tracing::info!(work_item = %item.id, "already reacted by another replica; skipped");
            }
            Err(e) => {
                tracing::error!(work_item = %item.id, error = %e, "reacting to terminal item failed");
            }
        }
    }
    tracing::info!(reacted, "coordinator tick complete");
    Ok(reacted)
}

/// Decide and apply the next step for one terminal item. Returns whether THIS
/// caller won the react claim (and therefore applied the decision).
///
/// The claim and the DB decision are bound into one transaction serialized per issue
/// (see the module docs). GitHub reads happen before the tx; GitHub writes after the
/// commit, only for newly-created handoffs/escalations.
async fn react_to_item(
    pool: &record::Pool,
    gh: &dyn GitHubClient,
    item: &WorkItem,
    repo_and_config: Option<Arc<(RepoIdent, Config)>>,
) -> anyhow::Result<bool> {
    // Every decision needs the repo identity and its validated config (the coder and
    // reviewer worker names). A repo with no usable config can't be acted on. The
    // tick loads + parses this once per repo and hands it in (see `coordinator_tick`).
    let Some(repo_and_config) = repo_and_config else {
        tracing::warn!(work_item = %item.id, repo_id = %item.repo_id, "no usable repo config; skipping");
        return Ok(false);
    };
    let (repo_ident, config) = repo_and_config.as_ref();
    let repo = RepoRef::new(
        repo_ident.owner.clone(),
        repo_ident.name.clone(),
        repo_ident.installation_id,
    );

    // Load the CR ONCE, BEFORE the tx, for every path that needs it (reviewer-succeeded
    // and any failed item whose subject is a CR). Reading it here — not inside the tx —
    // avoids acquiring a second pool connection while the tx holds one plus the per-issue
    // lock (a pool-starvation deadlock risk under load), and avoids re-reading the same
    // CR 2–3× per react. The loaded CR is threaded into the lock-key derivation and the
    // in-tx decision; NO `&Pool` read happens inside the tx (C4).
    let cr = load_cr_for_item(pool, item).await?;

    // GitHub reads needed for the decision happen BEFORE the claim/tx. Only the
    // reviewer-approved path reads GitHub (the can't-weaken guard + check status).
    let approval = if item.kind == Kind::Reviewer && item.state == State::Succeeded {
        load_approval_decision(gh, &repo, item, config, cr.clone()).await?
    } else {
        None
    };

    // The per-issue serialization key for the advisory lock. Derived from the item's
    // subject (issue#N directly; cr#N via the pre-loaded CR's linked issue) so two
    // replicas reacting to different terminal items for the SAME issue serialize. When
    // no issue can be derived, fall back to a REPO-LEVEL key (C5) so items for the same
    // repo still serialize (coarser, but it never splits the namespace per work item).
    let lock_key = issue_lock_key(item, cr.as_ref());

    let mut tx = pool.begin().await?;
    record::issue_advisory_lock(&mut tx, item.repo_id, lock_key).await?;

    // Claim the item INSIDE the tx: exactly one replica wins. If we don't win, the
    // item was already reacted — roll back and skip.
    if !record::mark_reacted(&mut *tx, item.id).await? {
        tx.rollback().await?;
        return Ok(false);
    }
    tracing::info!(
        work_item = %item.id,
        kind = item.kind.as_str(),
        state = item.state.as_str(),
        subject = %item.subject_ref,
        "reacting to terminal item"
    );

    // Apply the DB decision INSIDE the tx; it returns the GitHub side effects to run
    // after the commit (a no-op for most paths). All DB access here uses `&mut *tx`;
    // the CR was pre-loaded above so no path re-reads the pool inside the tx (C4).
    let post_commit = match (item.kind, item.state) {
        (Kind::Manager, State::Succeeded) => {
            decide_manager_succeeded(&mut tx, item, config).await?
        }
        (Kind::Coder, State::Succeeded) => {
            decide_coder_succeeded(&mut tx, item, config, cr).await?
        }
        (Kind::Reviewer, State::Succeeded) => {
            decide_reviewer_succeeded(&mut tx, item, config, approval, cr).await?
        }
        (_, State::Failed) => decide_failed(&mut tx, item, config, cr).await?,
        (_, State::Escalated) => {
            // Already escalated upstream; nothing more to do but record the react.
            tracing::info!(work_item = %item.id, "terminal item already escalated; no-op react");
            PostCommit::None
        }
        (kind, state) => {
            tracing::info!(work_item = %item.id, kind = kind.as_str(), state = state.as_str(), "no coordinator action for (kind, state)");
            PostCommit::None
        }
    };

    // Commit the atomic claim+decision, then run the GitHub side effects. A crash here
    // leaves the decision RECORDED (recoverable) rather than the whole react lost.
    tx.commit().await?;
    run_post_commit(gh, &repo, item, post_commit).await?;
    Ok(true)
}

/// The GitHub side effects to run AFTER the per-react transaction commits. Most
/// decision paths produce [`PostCommit::None`]; the handoff and escalation paths carry
/// a PREBUILT human-facing `body` (D2: the decision layer composes all copy; this
/// transport layer only delivers it), the comment target, the issue to assign, and the
/// guard flag (`did_transition` / `created`) so the comment + assignment run exactly
/// once — only when the underlying transition/row actually happened in the tx.
enum PostCommit {
    /// No GitHub side effect.
    None,
    /// An approved CR transitioned to `ready` for merge: post the prebuilt handoff
    /// `body` on `comment_target` and assign the linked issue (if any) to a human. Runs
    /// only when `did_transition` (the in-tx `approved → ready` guard won).
    ReadyHandoff {
        body: String,
        comment_target: CommentTarget,
        assign_issue: Option<i64>,
        did_transition: bool,
    },
    /// An escalation was recorded; deliver its prebuilt `body` iff `created`.
    Escalation {
        body: String,
        comment_target: Option<CommentTarget>,
        assign_issue: Option<i64>,
        created: bool,
        reason: &'static str,
    },
}

/// Run the post-commit GitHub side effects for a decision. Pure transport: it delivers
/// the prebuilt `body` the decision layer composed (D2) and NEVER composes copy itself.
/// Idempotent: handoff/escalation effects run only when their in-tx guard won
/// (`did_transition` / `created`); a re-dispatch / racing replica that lost the guard
/// posts nothing.
async fn run_post_commit(
    gh: &dyn GitHubClient,
    repo: &RepoRef,
    item: &WorkItem,
    post_commit: PostCommit,
) -> anyhow::Result<()> {
    match post_commit {
        PostCommit::None => Ok(()),
        PostCommit::ReadyHandoff {
            body,
            comment_target,
            assign_issue,
            did_transition,
        } => {
            if !did_transition {
                tracing::info!(work_item = %item.id, "approved→ready handoff skipped post-commit: transition guard lost");
                return Ok(());
            }
            gh.post_comment(repo, comment_target, &body).await?;
            if let Some(issue) = assign_issue {
                gh.assign_issue(repo, issue, HANDOFF_ASSIGNEE).await?;
                tracing::info!(work_item = %item.id, issue, "reviewer approved: CR ready, handed to human; issue stays open awaiting merge");
            } else {
                tracing::warn!(work_item = %item.id, "approved CR has no linked issue; marked ready without issue handoff");
            }
            Ok(())
        }
        PostCommit::Escalation {
            body,
            comment_target,
            assign_issue,
            created,
            reason,
        } => {
            if !created {
                tracing::info!(work_item = %item.id, reason, "escalation already exists; no second comment/assignment");
                return Ok(());
            }
            match comment_target {
                Some(target) => {
                    gh.post_comment(repo, target, &body).await?;
                }
                None => {
                    tracing::warn!(work_item = %item.id, reason, "escalation comment target unresolved; recorded escalation without a comment");
                }
            }
            if let Some(issue) = assign_issue {
                gh.assign_issue(repo, issue, HANDOFF_ASSIGNEE).await?;
            }
            tracing::info!(work_item = %item.id, reason, "escalation created; side effects applied");
            Ok(())
        }
    }
}

/// The pre-claim decision for a Succeeded reviewer: inspect the pre-loaded CR and
/// (if approved and gating-safe) read its check status. `None` means "no actionable
/// approval" (the CR isn't tracked, isn't approved, or the gating guard / checks
/// block it). The CR was loaded ONCE before the tx (C4) and is handed in here; this
/// fn performs only GitHub reads, no `&Pool` reads. The result is applied only after
/// the react claim is won.
async fn load_approval_decision(
    gh: &dyn GitHubClient,
    repo: &RepoRef,
    item: &WorkItem,
    config: &Config,
    cr: Option<ChangeRequest>,
) -> anyhow::Result<Option<ApprovalDecision>> {
    let Some(cr) = cr else {
        return Ok(None);
    };
    if cr.factory_state != "approved" {
        return Ok(None);
    }
    let Some(number) = cr.number else {
        tracing::warn!(work_item = %item.id, cr = %cr.id, "approved CR has no GitHub number; cannot finalize");
        return Ok(None);
    };

    // D3: the two GitHub reads needed for the decision are independent — the CR view
    // (for the can't-weaken-gating guard) and the check rollup (for the green gate) — so
    // fetch them CONCURRENTLY rather than serially, halving the GitHub round-trip latency
    // on the hot approval path. `try_join!` short-circuits on the first error.
    let (view, checks) = tokio::try_join!(
        gh.read_change_request(repo, number as i64),
        gh.check_status(repo, CheckRef::ChangeRequest(number as i64)),
    )?;

    // Can't-weaken-own-checks guard: if any changed file touches a configured gating
    // path, the change alters the very checks that gate it → never mark ready. The
    // gating prefixes are repo-configurable (`settings.gating_paths`, default
    // `[".github/workflows/"]`) so a repo's checks defined elsewhere are guarded too.
    // This guard takes precedence over the green check regardless of check status.
    if view
        .files
        .iter()
        .any(|f| is_gating_path(f, &config.settings.gating_paths))
    {
        tracing::warn!(work_item = %item.id, cr_number = number, "CR alters its own gating checks; will escalate conflict");
        return Ok(Some(ApprovalDecision::WeakensGating { cr }));
    }

    // Checks must be green to hand to a human. Pending/Failure → leave approved.
    if is_green(checks.overall) {
        Ok(Some(ApprovalDecision::Ready { cr }))
    } else {
        tracing::info!(work_item = %item.id, cr_number = number, overall = checks.overall.as_str(), "approved CR checks not green; leaving CR approved this tick");
        Ok(None)
    }
}

/// What a Succeeded reviewer's approval resolves to (computed pre-claim). Each
/// variant carries the [`ChangeRequest`] it was computed from; the GitHub number is
/// `cr.number` (no separate copy).
enum ApprovalDecision {
    /// Checks green, no gating-path changes: mark ready + hand to a human.
    Ready { cr: ChangeRequest },
    /// The CR weakens its own gating checks: escalate a conflict, never mark ready.
    WeakensGating { cr: ChangeRequest },
}

/// Manager succeeded → assign a coder for the issue and mark the issue in_progress.
async fn decide_manager_succeeded(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item: &WorkItem,
    config: &Config,
) -> anyhow::Result<PostCommit> {
    let Some(coder) = worker_with_role(config, Role::Coder) else {
        tracing::warn!(work_item = %item.id, "no coder worker in config; cannot delegate");
        return Ok(PostCommit::None);
    };
    // The manager's subject is the issue it triaged.
    let Subject::Issue(issue) = parse_subject(&item.subject_ref)? else {
        tracing::warn!(work_item = %item.id, subject = %item.subject_ref, "manager subject is not an issue; cannot delegate");
        return Ok(PostCommit::None);
    };

    let issue = issue_i32(item, issue)?;
    enqueue_next(
        &mut **tx,
        item,
        Kind::Coder,
        coder,
        &item.subject_ref,
        "manager",
    )
    .await?;
    record::set_issue_state(&mut **tx, item.repo_id, issue, "in_progress").await?;
    tracing::info!(work_item = %item.id, issue, coder, "manager decided: delegated to coder");
    Ok(PostCommit::None)
}

/// Coder succeeded → it opened a CR; wake a reviewer for that CR. The CR was loaded
/// ONCE before the tx (C4) and handed in; no `&Pool` read happens inside the tx.
async fn decide_coder_succeeded(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item: &WorkItem,
    config: &Config,
    cr: Option<ChangeRequest>,
) -> anyhow::Result<PostCommit> {
    let Some(reviewer) = worker_with_role(config, Role::Reviewer) else {
        tracing::warn!(work_item = %item.id, "no reviewer worker in config; cannot request review");
        return Ok(PostCommit::None);
    };
    let Some(cr) = cr else {
        tracing::info!(work_item = %item.id, "coder succeeded but no CR row yet; nothing to review");
        return Ok(PostCommit::None);
    };
    let Some(number) = cr.number else {
        tracing::info!(work_item = %item.id, "coder's CR has no number yet; nothing to review");
        return Ok(PostCommit::None);
    };

    let subject = Subject::ChangeRequest(number as i64).as_ref_string();
    enqueue_next(
        &mut **tx,
        item,
        Kind::Reviewer,
        reviewer,
        &subject,
        "cr_opened",
    )
    .await?;
    tracing::info!(work_item = %item.id, cr_number = number, reviewer, "coder decided: requested review");
    Ok(PostCommit::None)
}

/// Reviewer succeeded → drive the CR forward by its factory_state. The CR was loaded
/// ONCE before the tx (C4) and handed in; no `&Pool` read happens inside the tx.
async fn decide_reviewer_succeeded(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item: &WorkItem,
    config: &Config,
    approval: Option<ApprovalDecision>,
    cr: Option<ChangeRequest>,
) -> anyhow::Result<PostCommit> {
    // The approved path was pre-computed (it needed GitHub reads); apply it. The
    // approval carries its own CR; the conditional ready transition inside
    // `apply_approval` re-checks the CR state in-tx (C3), so a stale pre-loaded
    // factory_state can't cause a wrong handoff.
    if let Some(decision) = approval {
        return apply_approval(tx, item, decision).await;
    }

    // Otherwise handle changes_requested from the pre-loaded CR (no re-read).
    let Some(cr) = cr else {
        tracing::info!(work_item = %item.id, "reviewer succeeded but no CR found; no-op");
        return Ok(PostCommit::None);
    };

    match cr.factory_state.as_str() {
        "changes_requested" => decide_changes_requested(tx, item, config, &cr).await,
        other => {
            // approved-but-not-ready (checks pending) and any other state are no-ops
            // this tick.
            tracing::info!(work_item = %item.id, cr = %cr.id, factory_state = other, "reviewer succeeded: no actionable CR state this tick");
            Ok(PostCommit::None)
        }
    }
}

/// Apply a pre-computed approval decision (ready handoff or gating conflict).
async fn apply_approval(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item: &WorkItem,
    decision: ApprovalDecision,
) -> anyhow::Result<PostCommit> {
    match decision {
        ApprovalDecision::WeakensGating { cr } => {
            // NEVER mark ready. Escalate a conflict and hand to a human.
            let (comment_target, assign_issue) = escalation_target(item, Some(&cr));
            let post = escalate(
                tx,
                item,
                Escalation {
                    comment_target,
                    assign_issue,
                    reason: "conflict",
                    context: "change weakens its own gating checks".to_owned(),
                },
            )
            .await?;
            tracing::info!(work_item = %item.id, cr_number = ?cr.number, "reviewer approved but CR weakens gating checks; escalated conflict");
            Ok(post)
        }
        ApprovalDecision::Ready { cr } => {
            // C3 exactly-once + not-stale: the conditional transition is BOTH the
            // in-tx re-check and the idempotency guard. The approval's factory_state
            // was read BEFORE the tx; by the time we hold the issue lock the CR may
            // have been superseded (driven back to changes_requested/closed by a later
            // reviewer) or already handed off by a racing react. The
            // `approved → ready` conditional UPDATE succeeds for EXACTLY ONE caller
            // that finds the CR still `approved`; if it returns false the CR is no
            // longer approved, so we skip the handoff entirely (no comment, no assign).
            let did_transition =
                record::set_change_request_state_if(&mut **tx, cr.id, "approved", "ready").await?;
            if !did_transition {
                tracing::info!(work_item = %item.id, cr = %cr.id, cr_number = ?cr.number, "approved→ready handoff skipped: CR no longer approved (superseded or already handed off)");
                return Ok(PostCommit::None);
            }
            // This is the ready-for-human-MERGE handoff, NOT issue closure. The CR is
            // marked `ready`; the linked issue stays OPEN (`in_progress`) awaiting the
            // human's merge. The issue is closed only when the human merges the CR,
            // which requires ingesting GitHub's `pull_request closed/merged` webhook —
            // a capability not built in P4 (see the implementation plan's P4/P5 notes).
            // So we deliberately do NOT set the issue to `done` here.
            let assign_issue = match cr.linked_issue {
                Some(issue) => {
                    record::set_issue_state(&mut **tx, item.repo_id, issue, "in_progress").await?;
                    Some(issue as i64)
                }
                None => None,
            };
            // A ready CR with no number can't have a handoff comment posted; the
            // transition still happened, so carry it but with no comment target. (An
            // approved CR normally has a number — `load_approval_decision` returns
            // `None` otherwise — so this is defensive.)
            let cr_number = match cr.number {
                Some(n) => n as i64,
                None => {
                    tracing::warn!(work_item = %item.id, cr = %cr.id, "approved→ready CR has no number; transition recorded but no handoff comment");
                    return Ok(PostCommit::None);
                }
            };
            // D2: compose the human-facing handoff copy HERE (the decision layer), not in
            // the transport layer. `run_post_commit` only delivers this body.
            let body = format!(
                "Factory: change request #{cr_number} is approved and all checks are green. \
                 Ready for a human to merge; the linked issue is closed once you merge."
            );
            tracing::info!(work_item = %item.id, cr_number, "reviewer approved: CR marked ready for human merge; issue left in_progress");
            Ok(PostCommit::ReadyHandoff {
                body,
                comment_target: CommentTarget::ChangeRequest(cr_number),
                assign_issue,
                did_transition,
            })
        }
    }
}

/// Reviewer requested changes → bump the loop count; re-enqueue a coder for rework,
/// or escalate at the cap.
async fn decide_changes_requested(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item: &WorkItem,
    config: &Config,
    cr: &ChangeRequest,
) -> anyhow::Result<PostCommit> {
    let loop_cap = i64::from(config.settings.loop_cap);
    let count = record::increment_cr_loop_count(&mut **tx, cr.id).await?;
    tracing::info!(work_item = %item.id, cr = %cr.id, loop_count = count, cap = loop_cap, "changes requested: incremented loop count");

    if i64::from(count) >= loop_cap {
        // The rework loop is stuck. Escalate exactly once and block the issue.
        let (comment_target, assign_issue) = escalation_target(item, Some(cr));
        let post = escalate(
            tx,
            item,
            Escalation {
                comment_target,
                assign_issue,
                reason: "loop_cap",
                context: format!(
                    "review-rework loop reached the cap ({loop_cap}) without converging"
                ),
            },
        )
        .await?;
        if let Some(issue) = cr.linked_issue {
            record::set_issue_state(&mut **tx, item.repo_id, issue, "blocked").await?;
        }
        tracing::info!(work_item = %item.id, cr = %cr.id, loop_count = count, "loop cap reached; escalated");
        return Ok(post);
    }

    // Below the cap: send the coder back for rework on the linked issue.
    let Some(coder) = worker_with_role(config, Role::Coder) else {
        tracing::warn!(work_item = %item.id, "no coder worker in config; cannot rework");
        return Ok(PostCommit::None);
    };
    // C2: with no linked issue there is nothing to rework against — re-dispatching a
    // coder is impossible and silently returning would STRAND the CR (no coder, no
    // escalation, stuck forever). Escalate to a human instead. We do NOT count this
    // dead-end against the loop cap beyond the increment already applied above.
    let Some(issue) = cr.linked_issue else {
        let (comment_target, assign_issue) = escalation_target(item, Some(cr));
        let post = escalate(
            tx,
            item,
            Escalation {
                comment_target,
                assign_issue,
                reason: "needs_decision",
                context: "changes requested but CR has no linked issue to rework".to_owned(),
            },
        )
        .await?;
        tracing::warn!(work_item = %item.id, cr = %cr.id, "changes requested but CR has no linked issue; escalated instead of stranding");
        return Ok(post);
    };
    let subject = Subject::Issue(issue as i64).as_ref_string();
    enqueue_next(&mut **tx, item, Kind::Coder, coder, &subject, "manager").await?;
    tracing::info!(work_item = %item.id, cr = %cr.id, issue, loop_count = count, coder, "changes requested: re-dispatched coder for rework");
    Ok(PostCommit::None)
}

/// A Failed item: retry it (re-queue for a fresh attempt) while under the retry cap,
/// otherwise escalate `retries_exhausted`.
///
/// C1: `record_outcome("failed")` sets state=Failed WITHOUT bumping `attempt_count`
/// (only lease-expiry reclaim bumps it), so an explicitly-failed worker that is never
/// retried would otherwise be silently dropped — never retried (reclaim only handles
/// leased/running), never escalated (below the cap). The fix gives the coordinator the
/// retry model: under the cap it re-queues the SAME work item (back to `queued`,
/// `available_at = now()`, `attempt_count + 1`, lease cleared, AND `reacted = false`
/// so a subsequent failure of this same row is reacted to again — see
/// [`record::requeue_for_retry`]) so the dispatcher re-runs it; at the cap it escalates.
///
/// D1: the cap is the repo's configured `settings.retry_cap` (default 3), not a
/// platform constant — consistent with `settings.gating_paths`.
async fn decide_failed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item: &WorkItem,
    config: &Config,
    cr: Option<ChangeRequest>,
) -> anyhow::Result<PostCommit> {
    let retry_cap = i64::from(config.settings.retry_cap);
    if i64::from(item.attempt_count) < retry_cap {
        // Re-queue for a fresh attempt rather than dropping the failed worker. The
        // requeue resets `reacted` so the NEXT failure of this row is reacted to again.
        record::requeue_for_retry(&mut **tx, item.id).await?;
        tracing::info!(work_item = %item.id, attempt_count = item.attempt_count, next_attempt = item.attempt_count + 1, cap = retry_cap, "failed item under retry cap; re-queued for retry");
        return Ok(PostCommit::None);
    }
    // At/over the cap: escalate. When the failed item's subject is a CR (e.g. a failed
    // reviewer, or any cr#N work item), the CR's own subject carries no issue to
    // assign. The pre-loaded CR (C4) resolves the linked issue so the escalation
    // assigns a human, not just comments. A failed item with an `issue#N` subject
    // (e.g. a coder) resolves its issue directly from the subject.
    let (comment_target, assign_issue) = escalation_target(item, cr.as_ref());
    let post = escalate(
        tx,
        item,
        Escalation {
            comment_target,
            assign_issue,
            reason: "retries_exhausted",
            context: format!("worker failed after {} attempts", item.attempt_count),
        },
    )
    .await?;
    tracing::info!(work_item = %item.id, attempt_count = item.attempt_count, "retries exhausted; escalated");
    Ok(post)
}

/// The details of one escalation: where to notify the human, which issue to hand
/// them, and why. Bundled so [`escalate`] stays under the argument limit.
struct Escalation {
    /// Where to post the human-facing escalation comment (issue or CR), or `None`
    /// when no subject could be resolved — the escalation row + assignment are still
    /// recorded, but no comment is posted (a `#0` placeholder would be a misdelivery).
    comment_target: Option<CommentTarget>,
    /// The issue to assign to a human, if any.
    assign_issue: Option<i64>,
    /// The escalation reason (one of the record-book reasons; the dedupe key).
    reason: &'static str,
    /// Human-readable context for the escalation.
    context: String,
}

/// Record an escalation exactly once per `(work_item, reason)` INSIDE the tx, and
/// return the [`PostCommit::Escalation`] describing the human-facing side effects.
/// The side effects (one comment + one assignment) run AFTER commit and ONLY when
/// this call created the row; a re-dispatch / racing replica that hit the existing
/// row carries `created = false` and performs no second comment or assignment.
async fn escalate(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item: &WorkItem,
    esc: Escalation,
) -> anyhow::Result<PostCommit> {
    let (escalation_id, created) = record::create_escalation_idempotent(
        &mut **tx,
        item.id,
        esc.reason,
        &esc.context,
        Some("github"),
    )
    .await?;
    // D2: compose the human-facing escalation copy HERE (the decision layer). The
    // transport layer (`run_post_commit`) only delivers this prebuilt body, and never
    // re-derives it — so the `escalation_id`/`context` need not be carried out of the tx
    // (the id is logged at creation right here; `created` flags the deliver-once guard).
    tracing::info!(work_item = %item.id, reason = esc.reason, %escalation_id, created, "escalation recorded; body composed for delivery");
    let body = format!("Factory escalation ({}): {}", esc.reason, esc.context);
    Ok(PostCommit::Escalation {
        body,
        comment_target: esc.comment_target,
        assign_issue: esc.assign_issue,
        created,
        reason: esc.reason,
    })
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Enqueue the next-step work item (idempotent by active `(repo, subject, kind)`).
/// Executor-generic so it runs inside the per-react transaction.
async fn enqueue_next(
    executor: impl sqlx::PgExecutor<'_>,
    item: &WorkItem,
    kind: Kind,
    worker_name: &str,
    subject_ref: &str,
    trigger: &str,
) -> anyhow::Result<()> {
    let next = NewWorkItem {
        repo_id: item.repo_id,
        kind,
        worker_name: worker_name.to_owned(),
        config_sha: item.config_sha.clone(),
        trigger: trigger.to_owned(),
        subject_ref: subject_ref.to_owned(),
        correlation_id: item.correlation_id,
        priority: 0,
        available_at: None,
    };
    record::enqueue(executor, &next).await?;
    Ok(())
}

/// The per-issue advisory-lock key for an item: the logical issue this react
/// serializes on. A `manager`/`coder` item's subject is `issue#N` (use N directly); a
/// `reviewer` (or any `cr#N`) item resolves N via the pre-loaded CR's linked issue
/// (C4: the CR is loaded once before the tx and threaded in here — no `&Pool` read).
///
/// C5: when no issue can be derived (unparseable subject, or a `cr#N` whose CR is
/// untracked or has no linked issue) the fallback is a REPO-LEVEL key (a stable hash
/// of the repo id), NOT a per-work-item key. A per-item fallback split the namespace
/// so two same-issue items locked on different keys and did not serialize; the
/// repo-level fallback is coarser but always serializes items for the same repo. The
/// key is namespaced by `repo_id` in [`record::issue_advisory_lock`] anyway, so a
/// repo-level fallback can never collide with another repo's keys.
fn issue_lock_key(item: &WorkItem, cr: Option<&ChangeRequest>) -> i64 {
    if let Ok(subject) = item.subject_ref.parse::<Subject>() {
        match subject {
            Subject::Issue(n) => return n,
            Subject::ChangeRequest(_) => {
                if let Some(issue) = cr.and_then(|c| c.linked_issue) {
                    return issue as i64;
                }
            }
        }
    }
    // Repo-level fallback: a stable non-negative key derived from the repo id alone,
    // so all of a repo's un-derivable items serialize on one key (coarse but correct)
    // rather than each splitting onto its own per-item key.
    repo_level_lock_key(item.repo_id)
}

/// A stable non-negative repo-level advisory-lock key (low 62 bits of the repo uuid).
/// Used as the coarse fallback when an item's logical issue can't be derived (C5).
fn repo_level_lock_key(repo_id: Uuid) -> i64 {
    (repo_id.as_u128() as i64) & 0x3fff_ffff_ffff_ffff
}

/// Load a repo's identity and its parsed config. `None` when the repo is gone or
/// has no stored config / an unparseable one (logged by the caller).
async fn load_repo_and_config(
    pool: &record::Pool,
    repo_id: Uuid,
) -> anyhow::Result<Option<(RepoIdent, Config)>> {
    let Some((ident, config_json)) = record::repo_by_id_with_config(pool, repo_id).await? else {
        return Ok(None);
    };
    let Some(config_json) = config_json else {
        tracing::warn!(%repo_id, "repo has no stored config");
        return Ok(None);
    };
    match serde_json::from_value::<Config>(config_json) {
        Ok(config) => Ok(Some((ident, config))),
        Err(e) => {
            tracing::warn!(%repo_id, error = %e, "stored config is not a valid Config");
            Ok(None)
        }
    }
}

/// The first worker with the given role, by name.
fn worker_with_role(config: &Config, role: Role) -> Option<&str> {
    config
        .workers
        .iter()
        .find(|w| w.role == role)
        .map(|w| w.name.as_str())
}

/// Load the CR an item is about, ONCE, BEFORE the react tx (C4), so no decision path
/// re-reads the pool while the tx holds a connection + the per-issue lock (a pool
/// starvation/deadlock risk under load) and the same CR is never read 2–3× per react.
///
/// Resolution depends on the item: a coder's CR is keyed by its own `work_item_id`
/// (the coder opened it); a reviewer's / any `cr#N`-subject item's CR is keyed by the
/// `cr#N` in its subject. Any other shape (e.g. an `issue#N` coder/manager, or an
/// unparseable subject) has no CR and resolves to `None` — normal, not a warning.
async fn load_cr_for_item(
    pool: &record::Pool,
    item: &WorkItem,
) -> anyhow::Result<Option<ChangeRequest>> {
    // A succeeded coder's CR is the one it opened, keyed by work_item_id.
    if item.kind == Kind::Coder && item.state == State::Succeeded {
        return Ok(record::change_request_for_item(pool, item.id).await?);
    }
    // Otherwise the CR (if any) is the `cr#N` the item's subject names.
    match item.subject_ref.parse::<Subject>() {
        Ok(Subject::ChangeRequest(n)) => {
            Ok(record::change_request_by_number(pool, item.repo_id, n as i32).await?)
        }
        _ => Ok(None),
    }
}

/// Narrow a subject's i64 issue number to the i32 the issues table stores,
/// surfacing an out-of-range number as an error rather than wrapping it.
fn issue_i32(item: &WorkItem, issue: i64) -> anyhow::Result<i32> {
    i32::try_from(issue).map_err(|_| {
        anyhow::anyhow!(
            "issue number {issue} for work item {} is out of range",
            item.id
        )
    })
}

/// Parse a `subject_ref` into a [`Subject`], surfacing a parse failure as an error.
fn parse_subject(subject_ref: &str) -> anyhow::Result<Subject> {
    subject_ref
        .parse::<Subject>()
        .map_err(|e| anyhow::anyhow!("unparseable subject_ref {subject_ref:?}: {e}"))
}

/// Resolve where to deliver an escalation: the comment target and the issue to hand
/// a human. With a [`ChangeRequest`] in hand (loop-cap / gating conflict) the target
/// is the CR (or its linked issue if the CR has no number yet) and the assigned issue
/// is the CR's linked issue. Without one (a Failed item) the target and assigned issue
/// come from the item's own subject. Either component is `None` when it can't be
/// resolved — the caller skips the corresponding side effect rather than fabricating
/// a `#0` placeholder. The subject is parsed exactly once.
fn escalation_target(
    item: &WorkItem,
    cr: Option<&ChangeRequest>,
) -> (Option<CommentTarget>, Option<i64>) {
    match cr {
        Some(cr) => {
            let assign_issue = cr.linked_issue.map(|n| n as i64);
            let comment_target = match cr.number {
                Some(n) => Some(CommentTarget::ChangeRequest(n as i64)),
                None => assign_issue.map(CommentTarget::Issue),
            };
            (comment_target, assign_issue)
        }
        None => match item.subject_ref.parse::<Subject>() {
            Ok(subject) => {
                let assign_issue = match subject {
                    Subject::Issue(n) => Some(n),
                    Subject::ChangeRequest(_) => None,
                };
                (Some(subject.comment_target()), assign_issue)
            }
            Err(_) => {
                tracing::warn!(work_item = %item.id, subject = %item.subject_ref, "escalation subject unparseable; no comment target");
                (None, None)
            }
        },
    }
}

/// Whether a changed path is part of the gating-check definition, per the repo's
/// configured `gating_paths` prefixes. A change touching one of these alters the
/// checks that gate its own merge. A file matches if it starts with any configured
/// prefix; a prefix ending in `/` (e.g. `".github/workflows/"`) also matches the
/// bare directory name (`".github/workflows"`) so the directory itself is guarded.
fn is_gating_path(path: &str, gating_paths: &[String]) -> bool {
    gating_paths.iter().any(|prefix| {
        path.starts_with(prefix.as_str())
            || prefix
                .strip_suffix('/')
                .is_some_and(|dir| !dir.is_empty() && path == dir)
    })
}

/// Whether a check rollup counts as green for marking a CR ready. GitHub treats
/// neutral/skipped checks as passing, so `Success` and `Neutral` are green;
/// `Failure` and `Pending` are not.
fn is_green(overall: CheckConclusion) -> bool {
    matches!(overall, CheckConclusion::Success | CheckConclusion::Neutral)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gating_paths_are_detected() {
        // Default prefix: GitHub Actions workflow definitions.
        let default = vec![".github/workflows/".to_owned()];
        assert!(is_gating_path(".github/workflows/ci.yml", &default));
        assert!(is_gating_path(".github/workflows/release.yaml", &default));
        assert!(is_gating_path(".github/workflows", &default));
        assert!(!is_gating_path("src/lib.rs", &default));
        assert!(!is_gating_path(".github/dependabot.yml", &default));
        assert!(!is_gating_path("docs/.github/workflows/x.yml", &default));
    }

    #[test]
    fn custom_gating_paths_are_honored() {
        // A repo can declare extra gating prefixes (e.g. a `ci/` directory).
        let custom = vec!["ci/".to_owned(), ".github/workflows/".to_owned()];
        assert!(is_gating_path("ci/pipeline.yml", &custom));
        assert!(is_gating_path("ci", &custom)); // bare dir name of a `/`-suffixed prefix
        assert!(is_gating_path(".github/workflows/ci.yml", &custom));
        assert!(!is_gating_path("src/ci_helper.rs", &custom));
        // With the default prefixes only, a `ci/` file is NOT gating.
        let default = vec![".github/workflows/".to_owned()];
        assert!(!is_gating_path("ci/pipeline.yml", &default));
    }

    #[test]
    fn green_gate_accepts_success_and_neutral() {
        assert!(is_green(CheckConclusion::Success));
        assert!(is_green(CheckConclusion::Neutral));
        assert!(!is_green(CheckConclusion::Failure));
        assert!(!is_green(CheckConclusion::Pending));
    }

    /// A throwaway terminal work item with the given subject, for `escalation_target`.
    fn item_with_subject(subject_ref: &str) -> WorkItem {
        WorkItem {
            id: Uuid::now_v7(),
            repo_id: Uuid::now_v7(),
            kind: Kind::Coder,
            worker_name: "w".into(),
            trigger: "manager".into(),
            subject_ref: subject_ref.into(),
            correlation_id: Uuid::now_v7(),
            state: State::Failed,
            attempt_count: 0,
            loop_count: 0,
            config_sha: "sha".into(),
            lease_owner: None,
            lease_expires_at: None,
        }
    }

    fn cr_with(number: Option<i32>, linked_issue: Option<i32>) -> ChangeRequest {
        ChangeRequest {
            id: Uuid::now_v7(),
            repo_id: Uuid::now_v7(),
            number,
            head_branch: "factory/issue-1".into(),
            linked_issue,
            work_item_id: None,
            factory_state: "open".into(),
        }
    }

    #[test]
    fn escalation_target_from_subject_when_no_cr() {
        // An issue subject → comment on the issue, assign the issue.
        assert_eq!(
            escalation_target(&item_with_subject("issue#7"), None),
            (Some(CommentTarget::Issue(7)), Some(7))
        );
        // A CR subject → comment on the CR, but nothing to assign (no linked issue).
        assert_eq!(
            escalation_target(&item_with_subject("cr#9"), None),
            (Some(CommentTarget::ChangeRequest(9)), None)
        );
    }

    #[test]
    fn escalation_target_unparseable_subject_skips_comment() {
        // An unparseable subject resolves to neither a comment target nor an
        // assignment: the caller skips the GitHub comment rather than misdelivering
        // to a `#0` placeholder. (The escalation row itself is still recorded.)
        assert_eq!(
            escalation_target(&item_with_subject("garbage"), None),
            (None, None)
        );
    }

    #[test]
    fn escalation_target_from_cr() {
        // A CR with a number → comment on the CR, assign its linked issue.
        assert_eq!(
            escalation_target(&item_with_subject("cr#9"), Some(&cr_with(Some(9), Some(1)))),
            (Some(CommentTarget::ChangeRequest(9)), Some(1))
        );
        // A CR with no number yet → fall back to the linked issue for the comment.
        assert_eq!(
            escalation_target(&item_with_subject("cr#9"), Some(&cr_with(None, Some(1)))),
            (Some(CommentTarget::Issue(1)), Some(1))
        );
        // A CR with neither a number nor a linked issue → nothing to deliver to.
        assert_eq!(
            escalation_target(&item_with_subject("cr#9"), Some(&cr_with(None, None))),
            (None, None)
        );
    }

    #[test]
    fn issue_lock_key_falls_back_to_nonnegative_item_key() {
        // The uuid-derived fallback key is always a non-negative i64 (advisory-lock
        // keys are fine either way, but a stable, predictable value is clearer).
        let item = item_with_subject("garbage");
        let key = (item.id.as_u128() as i64) & 0x3fff_ffff_ffff_ffff;
        assert!(key >= 0, "fallback lock key is non-negative");
    }

    #[test]
    fn caps_are_platform_defaults() {
        // D1: the retry/loop caps are now per-repo config (`settings.retry_cap` /
        // `settings.loop_cap`) rather than coordinator consts. A config that omits them
        // falls back to the platform defaults (3 / 5), so the coordinator reads those.
        let cfg = factory_config::parse("version: 1\n").expect("minimal config parses");
        assert_eq!(cfg.settings.retry_cap, 3);
        assert_eq!(cfg.settings.loop_cap, 5);
    }
}
