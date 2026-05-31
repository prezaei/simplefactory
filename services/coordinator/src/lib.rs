//! The Coordinator (Manager): Factory's reactive decision loop (P4).
//!
//! The coordinator is a polling service. Every tick it scans the record book for
//! TERMINAL work items it has not yet reacted to and decides the single next step
//! for each — assign a coder, wake a reviewer, mark a change request
//! ready-for-merge, drive the bounded review-rework loop, or escalate to a human.
//! It NEVER writes code; it only delegates and bookkeeps.
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
//! The bounded review-rework counter is the per-CR `change_requests.loop_count`,
//! advanced via [`record::increment_cr_loop_count`] on each changes-requested cycle;
//! at [`LOOP_CAP`] the loop escalates instead of dispatching another coder.
//!
//! GitHub *reads* needed for a decision happen BEFORE the claim; the DB next-step
//! writes and side effects happen AFTER winning it.
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
/// Errors from a single item's decision are logged and skipped so one bad item
/// never stalls the whole tick; the tick itself only errors on a failure to read
/// the work list.
pub async fn coordinator_tick(pool: &record::Pool, gh: &dyn GitHubClient) -> anyhow::Result<usize> {
    let items = record::list_unreacted_terminal(pool).await?;
    tracing::info!(
        count = items.len(),
        "coordinator tick: scanning terminal items"
    );

    // Cache each repo's identity + parsed config for the duration of the tick: many
    // terminal items typically share a repo, so this loads + parses each distinct
    // repo at most once per tick instead of once per item.
    let mut repo_cache: HashMap<Uuid, Option<(RepoIdent, Config)>> = HashMap::new();

    let mut reacted = 0usize;
    for item in items {
        let repo = match repo_cache.get(&item.repo_id) {
            Some(cached) => cached.clone(),
            None => {
                let loaded = load_repo_and_config(pool, item.repo_id).await?;
                repo_cache.insert(item.repo_id, loaded.clone());
                loaded
            }
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

    // GitHub reads needed for the decision happen BEFORE the claim. Only the
    // reviewer-approved path reads GitHub (the can't-weaken guard + check status);
    // it is computed here so the claim is held for as short a time as possible.
    let approval = if item.kind == Kind::Reviewer && item.state == State::Succeeded {
        load_approval_decision(pool, gh, &repo, item).await?
    } else {
        None
    };

    // Claim the item: exactly one replica wins and applies the decision.
    if !record::mark_reacted(pool, item.id).await? {
        return Ok(false);
    }
    tracing::info!(
        work_item = %item.id,
        kind = item.kind.as_str(),
        state = item.state.as_str(),
        subject = %item.subject_ref,
        "reacting to terminal item"
    );

    match (item.kind, item.state) {
        (Kind::Manager, State::Succeeded) => decide_manager_succeeded(pool, item, &config).await?,
        (Kind::Coder, State::Succeeded) => decide_coder_succeeded(pool, item, &config).await?,
        (Kind::Reviewer, State::Succeeded) => {
            decide_reviewer_succeeded(pool, gh, &repo, item, &config, approval).await?
        }
        (_, State::Failed) => decide_failed(pool, gh, &repo, item).await?,
        (_, State::Escalated) => {
            // Already escalated upstream; nothing more to do but record the react.
            tracing::info!(work_item = %item.id, "terminal item already escalated; no-op react");
        }
        (kind, state) => {
            tracing::info!(work_item = %item.id, kind = kind.as_str(), state = state.as_str(), "no coordinator action for (kind, state)");
        }
    }
    Ok(true)
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

    // Can't-weaken-own-checks guard: read the CR's files. If any touches a gating
    // path, the change alters the very checks that gate it → never mark ready.
    let view = gh.read_change_request(repo, number as i64).await?;
    if view.files.iter().any(|f| is_gating_path(f)) {
        tracing::warn!(work_item = %item.id, cr_number = number, "CR alters its own gating checks; will escalate conflict");
        return Ok(Some(ApprovalDecision::WeakensGating { cr, number }));
    }

    // Checks must be green to hand to a human. Pending/Failure → leave approved.
    let checks = gh
        .check_status(repo, CheckRef::ChangeRequest(number as i64))
        .await?;
    if checks.overall == CheckConclusion::Success {
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
    pool: &record::Pool,
    item: &WorkItem,
    config: &Config,
) -> anyhow::Result<()> {
    let Some(coder) = worker_with_role(config, Role::Coder) else {
        tracing::warn!(work_item = %item.id, "no coder worker in config; cannot delegate");
        return Ok(());
    };
    // The manager's subject is the issue it triaged.
    let Subject::Issue(issue) = parse_subject(&item.subject_ref)? else {
        tracing::warn!(work_item = %item.id, subject = %item.subject_ref, "manager subject is not an issue; cannot delegate");
        return Ok(());
    };

    let issue = issue_i32(item, issue)?;
    enqueue_next(pool, item, Kind::Coder, coder, &item.subject_ref, "manager").await?;
    record::set_issue_state(pool, item.repo_id, issue, "in_progress").await?;
    tracing::info!(work_item = %item.id, issue, coder, "manager decided: delegated to coder");
    Ok(())
}

/// Coder succeeded → it opened a CR; wake a reviewer for that CR.
async fn decide_coder_succeeded(
    pool: &record::Pool,
    item: &WorkItem,
    config: &Config,
) -> anyhow::Result<()> {
    let Some(reviewer) = worker_with_role(config, Role::Reviewer) else {
        tracing::warn!(work_item = %item.id, "no reviewer worker in config; cannot request review");
        return Ok(());
    };
    let Some(cr) = record::change_request_for_item(pool, item.id).await? else {
        tracing::info!(work_item = %item.id, "coder succeeded but no CR row yet; nothing to review");
        return Ok(());
    };
    let Some(number) = cr.number else {
        tracing::info!(work_item = %item.id, "coder's CR has no number yet; nothing to review");
        return Ok(());
    };

    let subject = Subject::ChangeRequest(number as i64).as_ref_string();
    enqueue_next(pool, item, Kind::Reviewer, reviewer, &subject, "cr_opened").await?;
    tracing::info!(work_item = %item.id, cr_number = number, reviewer, "coder decided: requested review");
    Ok(())
}

/// Reviewer succeeded → drive the CR forward by its factory_state.
async fn decide_reviewer_succeeded(
    pool: &record::Pool,
    gh: &dyn GitHubClient,
    repo: &RepoRef,
    item: &WorkItem,
    config: &Config,
    approval: Option<ApprovalDecision>,
) -> anyhow::Result<()> {
    // The approved path was pre-computed (it needed GitHub reads); apply it.
    if let Some(decision) = approval {
        return apply_approval(pool, gh, repo, item, decision).await;
    }

    // Otherwise re-read the CR's factory_state to handle changes_requested. (The
    // approved case already returned above; a non-approved read here is cheap.)
    let Some(cr) = cr_for_reviewed_subject(pool, item).await? else {
        tracing::info!(work_item = %item.id, "reviewer succeeded but no CR found; no-op");
        return Ok(());
    };

    match cr.factory_state.as_str() {
        "changes_requested" => decide_changes_requested(pool, gh, repo, item, config, &cr).await,
        other => {
            // approved-but-not-ready (checks pending) and any other state are no-ops
            // this tick.
            tracing::info!(work_item = %item.id, cr = %cr.id, factory_state = other, "reviewer succeeded: no actionable CR state this tick");
            Ok(())
        }
    }
}

/// Apply a pre-computed approval decision (ready handoff or gating conflict).
async fn apply_approval(
    pool: &record::Pool,
    gh: &dyn GitHubClient,
    repo: &RepoRef,
    item: &WorkItem,
    decision: ApprovalDecision,
) -> anyhow::Result<()> {
    match decision {
        ApprovalDecision::WeakensGating { cr, number } => {
            // NEVER mark ready. Escalate a conflict and hand to a human.
            let (comment_target, assign_issue) = escalation_target(item, Some(&cr));
            escalate(
                pool,
                gh,
                repo,
                item,
                Escalation {
                    comment_target,
                    assign_issue,
                    reason: "conflict",
                    context: "change weakens its own gating checks",
                },
            )
            .await?;
            tracing::info!(work_item = %item.id, cr_number = number, "reviewer approved but CR weakens gating checks; escalated conflict");
            Ok(())
        }
        ApprovalDecision::Ready { cr, number } => {
            record::set_change_request_state(pool, cr.id, "ready").await?;
            let body = format!(
                "Factory: change request #{number} is approved and all checks are green. \
                 Ready for a human to merge; once merged the linked issue is closed."
            );
            gh.post_comment(repo, CommentTarget::ChangeRequest(number as i64), &body)
                .await?;
            // Hand the linked issue to a human and mark it done (the worked-example
            // end state is "issue closed": this P4 slice marks the issue done at the
            // ready+handoff point; a live deployment would close it on the human's
            // merge). See the module docs.
            if let Some(issue) = cr.linked_issue {
                gh.assign_issue(repo, issue as i64, HANDOFF_ASSIGNEE)
                    .await?;
                record::set_issue_state(pool, item.repo_id, issue, "done").await?;
                tracing::info!(work_item = %item.id, cr_number = number, issue, "reviewer approved: CR ready, handed to human, issue closed");
            } else {
                tracing::warn!(work_item = %item.id, cr_number = number, "approved CR has no linked issue; marked ready without issue handoff");
            }
            Ok(())
        }
    }
}

/// Reviewer requested changes → bump the loop count; re-enqueue a coder for rework,
/// or escalate at the cap.
async fn decide_changes_requested(
    pool: &record::Pool,
    gh: &dyn GitHubClient,
    repo: &RepoRef,
    item: &WorkItem,
    config: &Config,
    cr: &ChangeRequest,
) -> anyhow::Result<()> {
    let count = record::increment_cr_loop_count(pool, cr.id).await?;
    tracing::info!(work_item = %item.id, cr = %cr.id, loop_count = count, "changes requested: incremented loop count");

    if count >= LOOP_CAP {
        // The rework loop is stuck. Escalate exactly once and block the issue.
        let (comment_target, assign_issue) = escalation_target(item, Some(cr));
        escalate(
            pool,
            gh,
            repo,
            item,
            Escalation {
                comment_target,
                assign_issue,
                reason: "loop_cap",
                context: &format!(
                    "review-rework loop reached the cap ({LOOP_CAP}) without converging"
                ),
            },
        )
        .await?;
        if let Some(issue) = cr.linked_issue {
            record::set_issue_state(pool, item.repo_id, issue, "blocked").await?;
        }
        tracing::info!(work_item = %item.id, cr = %cr.id, loop_count = count, "loop cap reached; escalated");
        return Ok(());
    }

    // Below the cap: send the coder back for rework on the linked issue.
    let Some(coder) = worker_with_role(config, Role::Coder) else {
        tracing::warn!(work_item = %item.id, "no coder worker in config; cannot rework");
        return Ok(());
    };
    let Some(issue) = cr.linked_issue else {
        tracing::warn!(work_item = %item.id, cr = %cr.id, "CR has no linked issue; cannot rework");
        return Ok(());
    };
    let subject = Subject::Issue(issue as i64).as_ref_string();
    enqueue_next(pool, item, Kind::Coder, coder, &subject, "manager").await?;
    tracing::info!(work_item = %item.id, cr = %cr.id, issue, loop_count = count, coder, "changes requested: re-dispatched coder for rework");
    Ok(())
}

/// Any kind Failed with attempts exhausted → escalate retries_exhausted.
async fn decide_failed(
    pool: &record::Pool,
    gh: &dyn GitHubClient,
    repo: &RepoRef,
    item: &WorkItem,
) -> anyhow::Result<()> {
    if item.attempt_count < RETRY_CAP {
        tracing::info!(work_item = %item.id, attempt_count = item.attempt_count, cap = RETRY_CAP, "failed item under retry cap; no escalation");
        return Ok(());
    }
    let (comment_target, assign_issue) = escalation_target(item, None);
    escalate(
        pool,
        gh,
        repo,
        item,
        Escalation {
            comment_target,
            assign_issue,
            reason: "retries_exhausted",
            context: &format!("worker failed after {} attempts", item.attempt_count),
        },
    )
    .await?;
    tracing::info!(work_item = %item.id, attempt_count = item.attempt_count, "retries exhausted; escalated");
    Ok(())
}

/// The details of one escalation: where to notify the human, which issue to hand
/// them, and why. Bundled so [`escalate`] stays under the argument limit.
struct Escalation<'a> {
    /// Where to post the human-facing escalation comment (issue or CR), or `None`
    /// when no subject could be resolved — the escalation row + assignment are still
    /// recorded, but no comment is posted (a `#0` placeholder would be a misdelivery).
    comment_target: Option<CommentTarget>,
    /// The issue to assign to a human, if any.
    assign_issue: Option<i64>,
    /// The escalation reason (one of the record-book reasons; the dedupe key).
    reason: &'a str,
    /// Human-readable context for the escalation.
    context: &'a str,
}

/// Record an escalation exactly once per `(work_item, reason)`, and ONLY when this
/// call created the row, run the human-facing side effects (one comment + one
/// assignment). A re-dispatch / racing replica that hits the existing escalation
/// performs no second comment or assignment.
async fn escalate(
    pool: &record::Pool,
    gh: &dyn GitHubClient,
    repo: &RepoRef,
    item: &WorkItem,
    esc: Escalation<'_>,
) -> anyhow::Result<()> {
    let (escalation_id, created) = record::create_escalation_idempotent(
        pool,
        item.id,
        esc.reason,
        esc.context,
        Some("github"),
    )
    .await?;
    if !created {
        tracing::info!(work_item = %item.id, reason = esc.reason, %escalation_id, "escalation already exists; no second comment/assignment");
        return Ok(());
    }

    match esc.comment_target {
        Some(target) => {
            let body = format!("Factory escalation ({}): {}", esc.reason, esc.context);
            gh.post_comment(repo, target, &body).await?;
        }
        None => {
            tracing::warn!(work_item = %item.id, reason = esc.reason, %escalation_id, "escalation comment target unresolved; recorded escalation without a comment");
        }
    }
    if let Some(issue) = esc.assign_issue {
        gh.assign_issue(repo, issue, HANDOFF_ASSIGNEE).await?;
    }
    tracing::info!(work_item = %item.id, reason = esc.reason, %escalation_id, "escalation created; side effects applied");
    Ok(())
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Enqueue the next-step work item (idempotent by active `(repo, subject, kind)`).
async fn enqueue_next(
    pool: &record::Pool,
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
    record::enqueue(pool, &next).await?;
    Ok(())
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

/// Whether a changed path is part of the gating-check definition. A change touching
/// these alters the checks that gate its own merge.
fn is_gating_path(path: &str) -> bool {
    path.starts_with(".github/workflows/") || path == ".github/workflows"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gating_paths_are_detected() {
        assert!(is_gating_path(".github/workflows/ci.yml"));
        assert!(is_gating_path(".github/workflows/release.yaml"));
        assert!(is_gating_path(".github/workflows"));
        assert!(!is_gating_path("src/lib.rs"));
        assert!(!is_gating_path(".github/dependabot.yml"));
        assert!(!is_gating_path("docs/.github/workflows/x.yml"));
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
    fn caps_are_platform_defaults() {
        assert_eq!(RETRY_CAP, 3);
        assert_eq!(LOOP_CAP, 5);
    }
}
