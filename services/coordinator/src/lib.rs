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
//! at [`LOOP_CAP`] the loop escalates instead of dispatching another coder.
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

use factory_config::{Config, Role};
use factory_github::{CheckConclusion, CheckRef, CommentTarget, GitHubClient, RepoRef, Subject};
use factory_record::{
    self as record, ChangeRequest, Kind, NewWorkItem, RepoIdent, State, WorkItem,
};
use uuid::Uuid;

/// Platform-default retry ceiling: a Failed item with this many attempts (or more)
/// escalates rather than being retried again.
const RETRY_CAP: i32 = 3;

/// Platform-default review-rework ceiling: at this many changes-requested cycles the
/// loop escalates instead of dispatching yet another coder.
const LOOP_CAP: i32 = 5;

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
    // repo at most once per tick instead of once per item. The cache value is itself
    // a `Result` so a config-load failure is isolated per item (it is replayed for
    // each item that shares the repo) without aborting the tick.
    let mut repo_cache: HashMap<Uuid, Option<(RepoIdent, Config)>> = HashMap::new();

    let mut reacted = 0usize;
    for item in items {
        // Load (and cache) the repo config. A load failure for this repo is logged and
        // the item skipped — it must not abort the whole tick (F6).
        let repo = match repo_cache.get(&item.repo_id) {
            Some(cached) => cached.clone(),
            None => match load_repo_and_config(pool, item.repo_id).await {
                Ok(loaded) => {
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
    repo_and_config: Option<(RepoIdent, Config)>,
) -> anyhow::Result<bool> {
    // Every decision needs the repo identity and its validated config (the coder and
    // reviewer worker names). A repo with no usable config can't be acted on. The
    // tick loads + parses this once per repo and hands it in (see `coordinator_tick`).
    let Some((repo_ident, config)) = repo_and_config else {
        tracing::warn!(work_item = %item.id, repo_id = %item.repo_id, "no usable repo config; skipping");
        return Ok(false);
    };
    let repo = RepoRef::new(
        repo_ident.owner,
        repo_ident.name,
        repo_ident.installation_id,
    );

    // GitHub reads needed for the decision happen BEFORE the claim/tx. Only the
    // reviewer-approved path reads GitHub (the can't-weaken guard + check status).
    let approval = if item.kind == Kind::Reviewer && item.state == State::Succeeded {
        load_approval_decision(pool, gh, &repo, item, &config).await?
    } else {
        None
    };

    // The per-issue serialization key for the advisory lock. Derived from the item's
    // subject (issue#N directly; cr#N via the CR's linked issue) so two replicas
    // reacting to different terminal items for the SAME issue serialize. When it can't
    // be derived, fall back to a stable key from the work-item id so two replicas
    // racing on the SAME item still serialize on the lock.
    let lock_key = issue_lock_key(pool, item).await?;

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
    // after the commit (a no-op for most paths).
    let post_commit = match (item.kind, item.state) {
        (Kind::Manager, State::Succeeded) => {
            decide_manager_succeeded(&mut tx, item, &config).await?
        }
        (Kind::Coder, State::Succeeded) => {
            decide_coder_succeeded(&mut tx, pool, item, &config).await?
        }
        (Kind::Reviewer, State::Succeeded) => {
            decide_reviewer_succeeded(&mut tx, pool, item, &config, approval).await?
        }
        (_, State::Failed) => decide_failed(&mut tx, pool, item).await?,
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
/// decision paths produce [`PostCommit::None`]; the handoff and escalation paths
/// carry the targets and the `created` flag so the comment + assignment run exactly
/// once (only when the underlying row was newly created in the tx).
enum PostCommit {
    /// No GitHub side effect.
    None,
    /// An approved CR is ready for merge: post the handoff comment on the CR and
    /// assign the linked issue (if any) to a human.
    ReadyHandoff {
        cr_number: i64,
        assign_issue: Option<i64>,
    },
    /// An escalation was recorded; run its human-facing side effects iff `created`.
    Escalation {
        created: bool,
        escalation_id: Uuid,
        comment_target: Option<CommentTarget>,
        assign_issue: Option<i64>,
        reason: &'static str,
        context: String,
    },
}

/// Run the post-commit GitHub side effects for a decision. Idempotent: escalation
/// effects run only when the row was newly created in the tx; a re-dispatch / racing
/// replica that hit the existing row carries `created = false` and posts nothing.
async fn run_post_commit(
    gh: &dyn GitHubClient,
    repo: &RepoRef,
    item: &WorkItem,
    post_commit: PostCommit,
) -> anyhow::Result<()> {
    match post_commit {
        PostCommit::None => Ok(()),
        PostCommit::ReadyHandoff {
            cr_number,
            assign_issue,
        } => {
            let body = format!(
                "Factory: change request #{cr_number} is approved and all checks are green. \
                 Ready for a human to merge; the linked issue is closed once you merge."
            );
            gh.post_comment(repo, CommentTarget::ChangeRequest(cr_number), &body)
                .await?;
            if let Some(issue) = assign_issue {
                gh.assign_issue(repo, issue, HANDOFF_ASSIGNEE).await?;
                tracing::info!(work_item = %item.id, cr_number, issue, "reviewer approved: CR ready, handed to human; issue stays open awaiting merge");
            } else {
                tracing::warn!(work_item = %item.id, cr_number, "approved CR has no linked issue; marked ready without issue handoff");
            }
            Ok(())
        }
        PostCommit::Escalation {
            created,
            escalation_id,
            comment_target,
            assign_issue,
            reason,
            context,
        } => {
            if !created {
                tracing::info!(work_item = %item.id, reason, %escalation_id, "escalation already exists; no second comment/assignment");
                return Ok(());
            }
            match comment_target {
                Some(target) => {
                    let body = format!("Factory escalation ({reason}): {context}");
                    gh.post_comment(repo, target, &body).await?;
                }
                None => {
                    tracing::warn!(work_item = %item.id, reason, %escalation_id, "escalation comment target unresolved; recorded escalation without a comment");
                }
            }
            if let Some(issue) = assign_issue {
                gh.assign_issue(repo, issue, HANDOFF_ASSIGNEE).await?;
            }
            tracing::info!(work_item = %item.id, reason, %escalation_id, "escalation created; side effects applied");
            Ok(())
        }
    }
}

/// The pre-claim decision for a Succeeded reviewer: read the CR and (if approved
/// and gating-safe) its check status. `None` means "no actionable approval"
/// (the CR isn't tracked, isn't approved, or the gating guard / checks block it).
/// The result is applied only after the react claim is won.
async fn load_approval_decision(
    pool: &record::Pool,
    gh: &dyn GitHubClient,
    repo: &RepoRef,
    item: &WorkItem,
    config: &Config,
) -> anyhow::Result<Option<ApprovalDecision>> {
    let Some(cr) = cr_for_reviewed_subject(pool, item).await? else {
        return Ok(None);
    };
    if cr.factory_state != "approved" {
        return Ok(None);
    }
    let Some(number) = cr.number else {
        tracing::warn!(work_item = %item.id, cr = %cr.id, "approved CR has no GitHub number; cannot finalize");
        return Ok(None);
    };

    // Can't-weaken-own-checks guard: read the CR's files. If any touches a configured
    // gating path, the change alters the very checks that gate it → never mark ready.
    // The gating prefixes are repo-configurable (`settings.gating_paths`, default
    // `[".github/workflows/"]`) so a repo's checks defined elsewhere are guarded too.
    let view = gh.read_change_request(repo, number as i64).await?;
    if view
        .files
        .iter()
        .any(|f| is_gating_path(f, &config.settings.gating_paths))
    {
        tracing::warn!(work_item = %item.id, cr_number = number, "CR alters its own gating checks; will escalate conflict");
        return Ok(Some(ApprovalDecision::WeakensGating { cr, number }));
    }

    // Checks must be green to hand to a human. Pending/Failure → leave approved.
    let checks = gh
        .check_status(repo, CheckRef::ChangeRequest(number as i64))
        .await?;
    if is_green(checks.overall) {
        Ok(Some(ApprovalDecision::Ready { cr, number }))
    } else {
        tracing::info!(work_item = %item.id, cr_number = number, overall = checks.overall.as_str(), "approved CR checks not green; leaving CR approved this tick");
        Ok(None)
    }
}

/// What a Succeeded reviewer's approval resolves to (computed pre-claim).
enum ApprovalDecision {
    /// Checks green, no gating-path changes: mark ready + hand to a human.
    Ready { cr: ChangeRequest, number: i32 },
    /// The CR weakens its own gating checks: escalate a conflict, never mark ready.
    WeakensGating { cr: ChangeRequest, number: i32 },
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

/// Coder succeeded → it opened a CR; wake a reviewer for that CR.
async fn decide_coder_succeeded(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    pool: &record::Pool,
    item: &WorkItem,
    config: &Config,
) -> anyhow::Result<PostCommit> {
    let Some(reviewer) = worker_with_role(config, Role::Reviewer) else {
        tracing::warn!(work_item = %item.id, "no reviewer worker in config; cannot request review");
        return Ok(PostCommit::None);
    };
    // Read the CR row for this coder item. A read against the pool is fine — the CR
    // was committed by the worker before the item went terminal.
    let Some(cr) = record::change_request_for_item(pool, item.id).await? else {
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

/// Reviewer succeeded → drive the CR forward by its factory_state.
async fn decide_reviewer_succeeded(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    pool: &record::Pool,
    item: &WorkItem,
    config: &Config,
    approval: Option<ApprovalDecision>,
) -> anyhow::Result<PostCommit> {
    // The approved path was pre-computed (it needed GitHub reads); apply it.
    if let Some(decision) = approval {
        return apply_approval(tx, item, decision).await;
    }

    // Otherwise re-read the CR's factory_state to handle changes_requested. (The
    // approved case already returned above; a non-approved read here is cheap.) The
    // read is against the pool, not the tx, which is fine — it is a plain read.
    let Some(cr) = cr_for_reviewed_subject(pool, item).await? else {
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
        ApprovalDecision::WeakensGating { cr, number } => {
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
            tracing::info!(work_item = %item.id, cr_number = number, "reviewer approved but CR weakens gating checks; escalated conflict");
            Ok(post)
        }
        ApprovalDecision::Ready { cr, number } => {
            record::set_change_request_state(&mut **tx, cr.id, "ready").await?;
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
            tracing::info!(work_item = %item.id, cr_number = number, "reviewer approved: CR marked ready for human merge; issue left in_progress");
            Ok(PostCommit::ReadyHandoff {
                cr_number: number as i64,
                assign_issue,
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
    let count = record::increment_cr_loop_count(&mut **tx, cr.id).await?;
    tracing::info!(work_item = %item.id, cr = %cr.id, loop_count = count, "changes requested: incremented loop count");

    if count >= LOOP_CAP {
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
                    "review-rework loop reached the cap ({LOOP_CAP}) without converging"
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
    let Some(issue) = cr.linked_issue else {
        tracing::warn!(work_item = %item.id, cr = %cr.id, "CR has no linked issue; cannot rework");
        return Ok(PostCommit::None);
    };
    let subject = Subject::Issue(issue as i64).as_ref_string();
    enqueue_next(&mut **tx, item, Kind::Coder, coder, &subject, "manager").await?;
    tracing::info!(work_item = %item.id, cr = %cr.id, issue, loop_count = count, coder, "changes requested: re-dispatched coder for rework");
    Ok(PostCommit::None)
}

/// Any kind Failed with attempts exhausted → escalate retries_exhausted.
async fn decide_failed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    pool: &record::Pool,
    item: &WorkItem,
) -> anyhow::Result<PostCommit> {
    if item.attempt_count < RETRY_CAP {
        tracing::info!(work_item = %item.id, attempt_count = item.attempt_count, cap = RETRY_CAP, "failed item under retry cap; no escalation");
        return Ok(PostCommit::None);
    }
    // When the failed item's subject is a CR (e.g. a failed reviewer, or any cr#N work
    // item), the CR's own subject carries no issue to assign. Resolve the CR's linked
    // issue so the escalation assigns a human, not just comments. A failed item with an
    // `issue#N` subject (e.g. a coder) resolves its issue directly from the subject.
    let cr = cr_for_failed_item(pool, item).await?;
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
    let (escalation_id, created) =
        record::create_escalation_idempotent(tx, item.id, esc.reason, &esc.context, Some("github"))
            .await?;
    Ok(PostCommit::Escalation {
        created,
        escalation_id,
        comment_target: esc.comment_target,
        assign_issue: esc.assign_issue,
        reason: esc.reason,
        context: esc.context,
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

/// The per-issue advisory-lock key for an item: the issue number this react serializes
/// on. A `manager`/`coder` item's subject is `issue#N` (use N directly); a `reviewer`
/// item's subject is `cr#N`, so resolve N via the CR's linked issue. When neither can
/// be derived, fall back to a stable key from the work-item id (its low 62 bits) so
/// two replicas racing on the SAME item still serialize on the lock. The key only
/// needs to be stable per logical issue, not equal to the GitHub issue number.
async fn issue_lock_key(pool: &record::Pool, item: &WorkItem) -> anyhow::Result<i64> {
    if let Ok(subject) = item.subject_ref.parse::<Subject>() {
        match subject {
            Subject::Issue(n) => return Ok(n),
            Subject::ChangeRequest(_) => {
                if let Some(cr) = cr_for_reviewed_subject(pool, item).await? {
                    if let Some(issue) = cr.linked_issue {
                        return Ok(issue as i64);
                    }
                }
            }
        }
    }
    // Fall back to a stable per-item key (low 62 bits of the uuid) so two replicas
    // racing on the same item still serialize, even when no issue can be derived.
    Ok((item.id.as_u128() as i64) & 0x3fff_ffff_ffff_ffff)
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

/// The CR a failed item is about, when its subject is `cr#N` (e.g. a failed reviewer
/// or any CR-subject work item); `None` for a non-CR subject (e.g. an `issue#N` coder),
/// which is normal here and not a warning. Lets the escalation assign the CR's linked
/// issue to a human rather than commenting with nobody assigned (F7).
async fn cr_for_failed_item(
    pool: &record::Pool,
    item: &WorkItem,
) -> anyhow::Result<Option<ChangeRequest>> {
    match item.subject_ref.parse::<Subject>() {
        Ok(Subject::ChangeRequest(n)) => {
            Ok(record::change_request_by_number(pool, item.repo_id, n as i32).await?)
        }
        _ => Ok(None),
    }
}

/// Find the CR the reviewer reviewed: its subject is `cr#N`.
async fn cr_for_reviewed_subject(
    pool: &record::Pool,
    item: &WorkItem,
) -> anyhow::Result<Option<ChangeRequest>> {
    let Ok(Subject::ChangeRequest(n)) = parse_subject(&item.subject_ref) else {
        tracing::warn!(work_item = %item.id, subject = %item.subject_ref, "reviewer subject is not a cr#N");
        return Ok(None);
    };
    Ok(record::change_request_by_number(pool, item.repo_id, n as i32).await?)
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
        assert_eq!(RETRY_CAP, 3);
        assert_eq!(LOOP_CAP, 5);
    }
}
