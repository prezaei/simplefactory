//! Factory Tools service (P3): the worker's ONLY channel to Factory, exposed as a
//! remote MCP server over HTTP. The worker holds a per-task bearer token; every
//! call is authorized against that token and scoped to the token's work item and
//! repo. There is no cross-task access — a token can only act on its own task.
//!
//! This module is the testable library: pure-ish `async fn`s, one per tool, each
//! taking `(&ToolsState, &TaskAuth, <args>)` and returning JSON. The Axum binary
//! (`main.rs`) is a thin wrapper that authorizes, dispatches, and writes the audit
//! event in one place so no tool can skip its audit row.
#![forbid(unsafe_code)]

pub mod mcp;

use std::str::FromStr;
use std::sync::Arc;

use factory_blob::TranscriptStore;
use factory_github::{
    CheckRef, CommentTarget, GitHubClient, OpenPrRequest, RepoRef, ReviewVerdict, Subject,
};
use factory_record::{self as record, AttemptStatus, IdempotencyClaim, State, TaskAuth};
use serde_json::{json, Value};

/// A tool-call error split by who is at fault, so the HTTP layer can map it to the
/// right status: a [`ToolError::BadRequest`] is the caller's fault (malformed or
/// invalid arguments) and becomes a `400` with the message echoed back; a
/// [`ToolError::Conflict`] is a transient idempotency race (another caller holds the
/// claim while its side effect is in flight) and becomes a `409` so the caller
/// retries; a [`ToolError::Internal`] is Factory's fault (DB / GitHub / blob I/O) and
/// becomes a `500` with a generic body, the real error logged server-side.
#[derive(Debug)]
pub enum ToolError {
    /// Invalid or missing arguments — the caller must fix the request (HTTP 400).
    BadRequest(String),
    /// A retryable conflict: a concurrent caller holds the idempotency claim while
    /// its side effect is in flight. The caller should retry (HTTP 409); the message
    /// is echoed back so the client knows it is in-progress, not a hard failure.
    Conflict(String),
    /// An internal failure (DB, GitHub, blob, …) — not the caller's fault (HTTP 500).
    Internal(anyhow::Error),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::BadRequest(msg) => write!(f, "{msg}"),
            ToolError::Conflict(msg) => write!(f, "{msg}"),
            ToolError::Internal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// Any plain `anyhow` error coming up through `?` (record / GitHub / blob I/O) is an
/// internal failure by default. Validation paths construct [`ToolError::BadRequest`]
/// explicitly instead of going through this conversion.
impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        ToolError::Internal(e)
    }
}

/// Record-book errors surfacing through `?` are internal failures.
impl From<record::RecordError> for ToolError {
    fn from(e: record::RecordError) -> Self {
        ToolError::Internal(e.into())
    }
}

/// A tool result: a JSON value on success, or a classified [`ToolError`].
type ToolResult = std::result::Result<Value, ToolError>;

/// Shared dependencies for every tool call: the record book, the GitHub client
/// (real or fake), and the transcript store.
#[derive(Clone)]
pub struct ToolsState {
    pub pool: record::Pool,
    pub gh: Arc<dyn GitHubClient>,
    pub blob: Arc<dyn TranscriptStore>,
}

/// Authorize a presented bearer token: hash it and resolve it to its task. `None`
/// means unauthorized (unknown / revoked / expired token). The HTTP layer turns
/// `None` into 401.
pub async fn authorize(pool: &record::Pool, bearer: &str) -> anyhow::Result<Option<TaskAuth>> {
    Ok(record::resolve_task_token(pool, &record::hash_token(bearer)).await?)
}

/// The set of tools the MCP surface exposes. The HTTP layer parses the `{tool}`
/// path segment into one of these and audits the call under this exact name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    GetTask,
    ReadIssue,
    ReadChangeRequest,
    OpenOrUpdateChangeRequest,
    PostComment,
    SubmitReview,
    GetCheckStatus,
    AppendTranscript,
    RecordOutcome,
    Heartbeat,
    Escalate,
}

impl Tool {
    /// Parse the `{tool}` path segment. Unknown names are rejected (404 at the
    /// HTTP layer) rather than silently ignored.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "get_task" => Tool::GetTask,
            "read_issue" => Tool::ReadIssue,
            "read_change_request" => Tool::ReadChangeRequest,
            "open_or_update_change_request" => Tool::OpenOrUpdateChangeRequest,
            "post_comment" => Tool::PostComment,
            "submit_review" => Tool::SubmitReview,
            "get_check_status" => Tool::GetCheckStatus,
            "append_transcript" => Tool::AppendTranscript,
            "record_outcome" => Tool::RecordOutcome,
            "heartbeat" => Tool::Heartbeat,
            "escalate" => Tool::Escalate,
            _ => return None,
        })
    }

    /// The stable name used as the audit `action` and in the route.
    pub fn as_str(self) -> &'static str {
        match self {
            Tool::GetTask => "get_task",
            Tool::ReadIssue => "read_issue",
            Tool::ReadChangeRequest => "read_change_request",
            Tool::OpenOrUpdateChangeRequest => "open_or_update_change_request",
            Tool::PostComment => "post_comment",
            Tool::SubmitReview => "submit_review",
            Tool::GetCheckStatus => "get_check_status",
            Tool::AppendTranscript => "append_transcript",
            Tool::RecordOutcome => "record_outcome",
            Tool::Heartbeat => "heartbeat",
            Tool::Escalate => "escalate",
        }
    }
}

/// Dispatch a parsed tool to its implementation. Centralizing dispatch (and the
/// audit event written by the HTTP layer around it) guarantees every tool call is
/// authorized and recorded; no tool can be reached except through here.
pub async fn dispatch(state: &ToolsState, auth: &TaskAuth, tool: Tool, args: &Value) -> ToolResult {
    match tool {
        Tool::GetTask => get_task(state, auth).await,
        Tool::ReadIssue => read_issue(state, auth, arg_i64(args, "issue_number")?).await,
        Tool::ReadChangeRequest => {
            read_change_request(state, auth, arg_i64(args, "cr_number")?).await
        }
        Tool::OpenOrUpdateChangeRequest => {
            open_or_update_change_request(
                state,
                auth,
                arg_str(args, "title")?,
                arg_str(args, "body")?,
                arg_str(args, "head_branch")?,
                arg_str(args, "base")?,
                arg_opt_i64(args, "linked_issue"),
            )
            .await
        }
        Tool::PostComment => {
            post_comment(
                state,
                auth,
                arg_str(args, "target")?,
                arg_str(args, "body")?,
                arg_opt_str(args, "dedupe_key"),
            )
            .await
        }
        Tool::SubmitReview => {
            submit_review(
                state,
                auth,
                arg_i64(args, "cr_number")?,
                arg_str(args, "verdict")?,
                arg_str(args, "body")?,
                arg_opt_str(args, "dedupe_key"),
            )
            .await
        }
        Tool::GetCheckStatus => {
            get_check_status(
                state,
                auth,
                arg_opt_i64(args, "cr_number"),
                arg_opt_str(args, "sha"),
            )
            .await
        }
        Tool::AppendTranscript => append_transcript(state, auth, arg_str(args, "chunk")?).await,
        Tool::RecordOutcome => {
            record_outcome(
                state,
                auth,
                arg_str(args, "status")?,
                arg_str(args, "summary")?,
            )
            .await
        }
        Tool::Heartbeat => heartbeat(state, auth).await,
        Tool::Escalate => {
            escalate(
                state,
                auth,
                arg_str(args, "reason")?,
                arg_str(args, "context")?,
                arg_opt_str(args, "dedupe_key"),
            )
            .await
        }
    }
}

// ===========================================================================
// Tools. Each scopes ALL GitHub ops to the token's repo (resolve_repo) and acts
// only on the token's work item — never anything supplied by the caller.
// ===========================================================================

/// Return this task's facts (role, subject, correlation). If the subject is an
/// issue or change request, the referenced GitHub object is read and embedded so
/// the worker gets the task in a single call.
pub async fn get_task(state: &ToolsState, auth: &TaskAuth) -> ToolResult {
    let item = record::work_item_by_id(&state.pool, auth.work_item_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("work item {} not found", auth.work_item_id))?;

    // Load the repo row ONCE: its config composes the prompt, its identity makes the
    // GitHub calls below. (Both used to load this same row separately.)
    let (repo_ident, repo_config) = record::repo_by_id_with_config(&state.pool, auth.repo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repo {} not found", auth.repo_id))?;
    let repo = RepoRef::new(
        repo_ident.owner,
        repo_ident.name,
        repo_ident.installation_id,
    );

    // Compose the worker's prompt from the repo's stored config: the built-in role
    // default, then the repo-wide instructions, then the matching worker's.
    let prompt = compose_prompt(auth, &item, repo_config);

    // Deterministic head branch from the subject so a re-dispatch pushes the SAME
    // branch (idempotent CR). e.g. `issue#7` -> `factory/issue-7`.
    let head_branch = format!("factory/{}", item.subject_ref.replace('#', "-"));
    // TODO: we don't yet track the repo's default branch; assume "main" until then.
    let base = "main";

    let mut out = json!({
        "work_item_id": auth.work_item_id.to_string(),
        "role": item.kind.as_str(),
        "subject_ref": item.subject_ref,
        "correlation_id": item.correlation_id.to_string(),
        "head_branch": head_branch,
        "base": base,
        "prompt": prompt,
    });

    match Subject::from_str(&item.subject_ref) {
        Ok(Subject::Issue(n)) => {
            let issue = state.gh.read_issue(&repo, n).await?;
            out["subject"] = json!({
                "type": "issue",
                "number": issue.number,
                "title": issue.title,
                "body": issue.body,
                "comments": issue.comments,
            });
        }
        Ok(Subject::ChangeRequest(n)) => {
            let cr = state.gh.read_change_request(&repo, n).await?;
            out["subject"] = json!({
                "type": "change_request",
                "number": cr.number,
                "head_sha": cr.head_sha,
                "diff": cr.diff,
                "files": cr.files,
                "reviews": cr.reviews,
                "checks": check_summary_json(&cr.checks),
            });
        }
        Err(_) => {
            // Schedule / other subjects carry no GitHub object; the subject_ref
            // string above is the whole answer.
            tracing::info!(subject = %item.subject_ref, "get_task: non-GitHub subject");
        }
    }

    tracing::info!(work_item = %auth.work_item_id, role = item.kind.as_str(), "get_task");
    Ok(out)
}

/// Read an issue in the token's repo.
pub async fn read_issue(state: &ToolsState, auth: &TaskAuth, issue_number: i64) -> ToolResult {
    let repo = resolve_repo(state, auth).await?;
    let issue = state.gh.read_issue(&repo, issue_number).await?;
    tracing::info!(work_item = %auth.work_item_id, issue_number, "read_issue");
    Ok(json!({
        "number": issue.number,
        "title": issue.title,
        "body": issue.body,
        "comments": issue.comments,
    }))
}

/// Read a change request in the token's repo.
pub async fn read_change_request(
    state: &ToolsState,
    auth: &TaskAuth,
    cr_number: i64,
) -> ToolResult {
    let repo = resolve_repo(state, auth).await?;
    let cr = state.gh.read_change_request(&repo, cr_number).await?;
    tracing::info!(work_item = %auth.work_item_id, cr_number, "read_change_request");
    Ok(json!({
        "number": cr.number,
        "head_sha": cr.head_sha,
        "diff": cr.diff,
        "files": cr.files,
        "reviews": cr.reviews,
        "checks": check_summary_json(&cr.checks),
    }))
}

/// Open (or update) the change request for this task. Idempotent two ways, BOTH keyed
/// by the deterministic head branch (`factory/issue-N`): the record-book CR row's
/// identity is `(repo_id, head_branch)` (one CR per branch — a re-dispatch OR a later
/// rework coder pushing the same branch reuses the SAME row, re-pointing its
/// `work_item_id` to the current owning coder), and the GitHub PR is also keyed by head
/// branch (re-open returns the same number). The number from GitHub is recorded back
/// onto the record-book row.
pub async fn open_or_update_change_request(
    state: &ToolsState,
    auth: &TaskAuth,
    title: &str,
    body: &str,
    head_branch: &str,
    base: &str,
    linked_issue: Option<i64>,
) -> ToolResult {
    let repo = resolve_repo(state, auth).await?;

    // One CR row per head branch — a re-dispatch OR a rework coder pushing the same
    // (deterministic) branch reuses the SAME row, re-pointing it to this work item.
    let cr_row = record::upsert_change_request_for_item(
        &state.pool,
        auth.repo_id,
        auth.work_item_id,
        head_branch,
        linked_issue.map(|n| n as i32),
    )
    .await?;

    let pr = state
        .gh
        .open_or_update_pr(
            &repo,
            &OpenPrRequest {
                title: title.to_owned(),
                body: body.to_owned(),
                head_branch: head_branch.to_owned(),
                base: base.to_owned(),
                linked_issue,
            },
        )
        .await?;

    // Persist the GitHub number onto our row (idempotent: same number on re-call).
    record::set_change_request_number(&state.pool, cr_row.id, pr.number as i32).await?;

    tracing::info!(
        work_item = %auth.work_item_id,
        cr_number = pr.number,
        head_branch,
        "open_or_update_change_request"
    );
    Ok(json!({ "cr_number": pr.number, "url": pr.url }))
}

/// Post a comment on the subject (`issue#N` or `cr#N`). When `dedupe_key` is
/// provided the call is idempotent via the claim-first pattern: the first caller
/// claims `(work_item, "comment", key)`, posts to GitHub, then records the result;
/// a concurrent caller that loses the claim while the post is in flight is told to
/// retry, and any later caller returns the stored result WITHOUT posting again.
/// Without a `dedupe_key` the comment is posted every time (no idempotency).
pub async fn post_comment(
    state: &ToolsState,
    auth: &TaskAuth,
    target: &str,
    body: &str,
    dedupe_key: Option<&str>,
) -> ToolResult {
    let repo = resolve_repo(state, auth).await?;
    let comment_target = parse_comment_target(target)?;

    // No dedupe_key: preserve the post-every-time behavior.
    let Some(key) = dedupe_key else {
        let comment_id = state.gh.post_comment(&repo, comment_target, body).await?;
        tracing::info!(work_item = %auth.work_item_id, ?target, "post_comment (no dedupe)");
        return Ok(json!({ "comment_id": comment_id }));
    };

    idempotent(
        state,
        auth,
        "comment",
        key,
        "a comment for this task is already in progress",
        || async {
            let comment_id = state
                .gh
                .post_comment(&repo, comment_target, body)
                .await
                .map_err(ToolError::Internal)?;
            tracing::info!(work_item = %auth.work_item_id, ?target, "post_comment");
            // Persist the posted body so a re-run can tell a genuine body change from
            // an identical re-dispatch (see the redo closure).
            Ok(json!({ "comment_id": comment_id, "body": body }))
        },
        |stored| async {
            // Spec: a re-run with the same dedupe_key UPDATES the same comment rather
            // than suppressing the new body. But an *identical* re-dispatch (same body)
            // must NOT re-PUT — that would overwrite a human's intervening edit of the
            // comment (and waste a GitHub write). Only update when the body changed.
            let Some(comment_id) = stored.get("comment_id").and_then(Value::as_i64) else {
                tracing::warn!(work_item = %auth.work_item_id, dedupe_key = key, "post_comment: stored result has no comment_id; cannot update");
                return Ok(stored);
            };
            let unchanged = stored.get("body").and_then(Value::as_str) == Some(body);
            if unchanged {
                tracing::info!(work_item = %auth.work_item_id, comment_id, "post_comment: identical re-dispatch; no update (preserves any human edit)");
                return Ok(stored);
            }
            state.gh.update_comment(&repo, comment_id, body).await?;
            tracing::info!(work_item = %auth.work_item_id, comment_id, "post_comment: body changed on re-run; updated existing comment");
            // Persist the body we just wrote so the equality check compares against the
            // LAST-written body, not the first one forever. Without this, a later
            // identical re-dispatch of this same body would re-PUT it — clobbering a
            // human edit made after this update. (`idempotency_complete` is an UPDATE of
            // the stored result; the Done branch of `idempotent` does not re-store on its
            // own, so we do it here.)
            //
            // BEST-EFFORT with bounded retries (mirrors the main `idempotent` Won-branch
            // pattern): the GitHub update already succeeded, so this re-persist must NOT
            // propagate (a `?` here would return a tool error AFTER the write, prompting the
            // caller to retry → re-run `update_comment` → the exact clobber we're preventing).
            // We retry a few times with a yield to ride out a transient DB blip and shrink
            // the staleness window, then give up. RESIDUAL RISK if every attempt fails: the
            // stored body stays at its previous value, so a LATER identical re-dispatch sees
            // a mismatch and re-PUTs once — which can overwrite a human edit made in between.
            // That window is the irreducible cost of returning success after the GitHub write
            // (the alternative — propagating — causes the immediate clobber-retry instead).
            let updated = json!({ "comment_id": comment_id, "body": body });
            let mut persisted = false;
            for attempt in 1..=IDEMPOTENCY_COMPLETE_ATTEMPTS {
                match record::idempotency_complete(
                    &state.pool,
                    auth.work_item_id,
                    "comment",
                    key,
                    &updated,
                )
                .await
                {
                    Ok(()) => {
                        persisted = true;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(work_item = %auth.work_item_id, comment_id, attempt, error = %e, "post_comment: re-persist of updated body failed; retrying");
                        tokio::task::yield_now().await;
                    }
                }
            }
            if !persisted {
                tracing::warn!(work_item = %auth.work_item_id, comment_id, attempts = IDEMPOTENCY_COMPLETE_ATTEMPTS, "post_comment: could not re-persist updated body after retries; stored body is stale, so a later identical re-dispatch may re-PUT and overwrite a human edit");
            }
            Ok(updated)
        },
    )
    .await
}

/// Submit a review verdict on a change request. Idempotent by `dedupe_key` under
/// scope `"review"` via the claim-first pattern: the claim winner submits the review
/// and records the result; a concurrent loser is told to retry; any later caller
/// returns the stored result and submits no second review. Without a `dedupe_key`
/// the review is submitted every time (no idempotency).
pub async fn submit_review(
    state: &ToolsState,
    auth: &TaskAuth,
    cr_number: i64,
    verdict: &str,
    body: &str,
    dedupe_key: Option<&str>,
) -> ToolResult {
    let repo = resolve_repo(state, auth).await?;
    let verdict = parse_verdict(verdict)?;

    // No dedupe_key: preserve the submit-every-time behavior.
    let Some(key) = dedupe_key else {
        let review_id = state
            .gh
            .submit_review(&repo, cr_number, verdict, body)
            .await?;
        record_verdict_cr_state(state, auth, cr_number, verdict).await?;
        tracing::info!(work_item = %auth.work_item_id, cr_number, verdict = verdict.as_str(), "submit_review (no dedupe)");
        return Ok(json!({ "review_id": review_id }));
    };

    idempotent(
        state,
        auth,
        "review",
        key,
        "a review for this task is already in progress",
        || async {
            let review_id = state
                .gh
                .submit_review(&repo, cr_number, verdict, body)
                .await
                .map_err(ToolError::Internal)?;
            record_verdict_cr_state(state, auth, cr_number, verdict).await?;
            tracing::info!(work_item = %auth.work_item_id, cr_number, verdict = verdict.as_str(), "submit_review");
            Ok(json!({ "review_id": review_id }))
        },
        |stored| async {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "submit_review: deduped (no-op)");
            Ok(stored)
        },
    )
    .await
}

/// Reflect a review verdict onto the change request's factory state so the
/// coordinator can react: approve → `approved`, request_changes →
/// `changes_requested`, comment → no state change. No-op if the CR row isn't
/// tracked yet (number not recorded).
async fn record_verdict_cr_state(
    state: &ToolsState,
    auth: &TaskAuth,
    cr_number: i64,
    verdict: ReviewVerdict,
) -> Result<(), ToolError> {
    let new_state = match verdict {
        ReviewVerdict::Approve => "approved",
        ReviewVerdict::RequestChanges => "changes_requested",
        ReviewVerdict::Comment => return Ok(()),
    };
    let cr_number = i32::try_from(cr_number)
        .map_err(|_| ToolError::BadRequest(format!("invalid cr_number {cr_number}")))?;
    if let Some(cr) = record::change_request_by_number(&state.pool, auth.repo_id, cr_number).await?
    {
        // Don't clobber a CR the coordinator has already advanced past review: once a
        // CR is `ready`/`merged`/`closed` a late or duplicate verdict must not drag it
        // back to `approved`/`changes_requested` (that would re-open a settled handoff).
        // The guard is an ATOMIC conditional UPDATE (`... WHERE factory_state NOT IN
        // ('ready','merged','closed')`), NOT a read-then-write: the coordinator can move
        // a CR `approved → ready` in the window between a read and the write, so a
        // read-and-check would TOCTOU-clobber `ready`. The conditional write writes only
        // when the CR is still in an overwritable state.
        let wrote =
            record::set_change_request_state_if_not_terminal(&state.pool, cr.id, new_state).await?;
        if !wrote {
            tracing::info!(
                work_item = %auth.work_item_id,
                cr_number,
                "submit_review: CR already past review; not overwriting its factory state"
            );
        }
    }
    Ok(())
}

/// Read required-check status for either a CR number or an explicit sha. Exactly
/// one of the two must be supplied.
pub async fn get_check_status(
    state: &ToolsState,
    auth: &TaskAuth,
    cr_number: Option<i64>,
    sha: Option<&str>,
) -> ToolResult {
    let repo = resolve_repo(state, auth).await?;
    let what = match (cr_number, sha) {
        (Some(n), None) => CheckRef::ChangeRequest(n),
        (None, Some(s)) => CheckRef::Sha(s.to_owned()),
        (Some(_), Some(_)) => {
            return Err(ToolError::BadRequest(
                "get_check_status: provide exactly one of cr_number or sha, not both".to_owned(),
            ))
        }
        (None, None) => {
            return Err(ToolError::BadRequest(
                "get_check_status: one of cr_number or sha is required".to_owned(),
            ))
        }
    };
    let summary = state.gh.check_status(&repo, what).await?;
    tracing::info!(work_item = %auth.work_item_id, overall = summary.overall.as_str(), "get_check_status");
    Ok(check_summary_json(&summary))
}

/// Append a chunk to this task's transcript. The transcript URI is derived from
/// the work item id, so a worker can never write to another task's transcript.
pub async fn append_transcript(state: &ToolsState, auth: &TaskAuth, chunk: &str) -> ToolResult {
    let uri = transcript_uri(auth);
    state.blob.append(&uri, chunk).await?;
    tracing::info!(work_item = %auth.work_item_id, bytes = chunk.len(), "append_transcript");
    Ok(json!({ "uri": uri, "appended": chunk.len() }))
}

/// Complete this attempt: set the attempt's terminal status + summary and move the
/// work item to the mapped state. Idempotent on the attempt — `finish_attempt`
/// returns `false` if the attempt was already terminal. (The richer coordinator
/// transitions land in P4; here the slice simply completes.)
pub async fn record_outcome(
    state: &ToolsState,
    auth: &TaskAuth,
    status: &str,
    summary: &str,
) -> ToolResult {
    let (attempt_status, work_state) = parse_outcome(status)?;
    let attempt_id = auth.attempt_id.ok_or_else(|| {
        ToolError::BadRequest("record_outcome: token has no attempt to finish".to_owned())
    })?;

    let finished =
        record::finish_attempt(&state.pool, attempt_id, attempt_status, Some(summary)).await?;
    let state_changed = record::set_state(&state.pool, auth.work_item_id, work_state).await?;

    // The task has ended: revoke its tokens so the bearer can no longer act
    // (spec: "Tokens revoked when the task ends").
    let revoked = record::revoke_task_tokens(&state.pool, auth.work_item_id).await?;

    tracing::info!(
        work_item = %auth.work_item_id,
        attempt = %attempt_id,
        status,
        finished,
        state_changed,
        revoked,
        "record_outcome"
    );
    Ok(json!({
        "finished": finished,
        "attempt_id": attempt_id.to_string(),
        "state": work_state.as_str(),
        "state_changed": state_changed,
    }))
}

/// Extend this task's lease (default 15 min). Returns the new expiry, or null if
/// the item is no longer in flight (e.g. already terminal or reclaimed).
pub async fn heartbeat(state: &ToolsState, auth: &TaskAuth) -> ToolResult {
    let expires =
        record::heartbeat_task(&state.pool, auth.work_item_id, HEARTBEAT_TTL_SECS).await?;
    tracing::info!(work_item = %auth.work_item_id, extended = expires.is_some(), "heartbeat");
    Ok(json!({ "lease_expires_at": expires }))
}

/// Escalate to a human — the single human channel. The invariant is ONE escalation
/// per `(task, reason)`: idempotent under scope `"escalate"` via the claim-first
/// pattern, defaulting (when the caller passes NO `dedupe_key`) to a per-reason key
/// (`escalate:<reason>`). The FIRST escalate of a given reason to win the claim records
/// the escalation, posts the question on the subject, and moves the work item to the
/// state mapped from its `reason` (resumable `blocked` for needs-human-input, terminal
/// `escalated` otherwise), then stores the result. A LATER escalate of the SAME reason
/// with no key hits `Done` and is a no-op (no second row, no second comment). A later
/// escalate of a DIFFERENT reason (e.g. a task that was `blocked_input`, got unblocked,
/// then legitimately hit `retries_exhausted`) gets its own claim and escalates — it is
/// NOT swallowed. This matches the record layer's UNIQUE (work_item_id, reason) index.
/// A caller may still pass an explicit `dedupe_key` to override the default.
///
/// Escalation is ALSO idempotent at the record layer per `(work_item, reason)`: the
/// DB holds a UNIQUE (work_item_id, reason) index, so even an explicit distinct
/// `dedupe_key` that wins a fresh claim cannot create a second escalation row for the
/// same `(work_item, reason)`. In that case the write is a no-op success
/// (`created = false`): the original row/context is preserved, no second comment is
/// posted, and the work-item state is left as the first escalation set it.
pub async fn escalate(
    state: &ToolsState,
    auth: &TaskAuth,
    reason: &str,
    context: &str,
    dedupe_key: Option<&str>,
) -> ToolResult {
    // Escalation reason is constrained by the record book; reject bad values up
    // front with a clear message rather than surfacing a DB constraint error.
    validate_escalation_reason(reason)?;

    // ONE escalation per (task, reason). When the caller omits a dedupe_key, default it
    // to a per-reason key so a later escalate of the SAME reason dedupes to the first,
    // while a DIFFERENT reason gets its own claim and is not silently swallowed (this
    // mirrors the DB's UNIQUE (work_item_id, reason) index). An explicit caller-supplied
    // key overrides the default.
    let default_key = format!("{ESCALATE_DEFAULT_KEY_PREFIX}{reason}");
    let key = dedupe_key.unwrap_or(&default_key);

    idempotent(
        state,
        auth,
        "escalate",
        key,
        "an escalate for this task is already in progress",
        || async {
            let result = escalate_won(state, auth, reason, context).await?;
            tracing::info!(work_item = %auth.work_item_id, reason, "escalate");
            Ok(result)
        },
        |stored| async {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "escalate: deduped (no-op)");
            Ok(stored)
        },
    )
    .await
}

/// The side-effecting body of [`escalate`], run only by the claim winner. Records
/// the escalation, posts the human-facing question (best-effort), and moves the work
/// item to the state mapped from `reason` (resumable `blocked` vs terminal
/// `escalated`). Any `Err` propagates so the caller releases the idempotency claim.
async fn escalate_won(
    state: &ToolsState,
    auth: &TaskAuth,
    reason: &str,
    context: &str,
) -> ToolResult {
    let repo = resolve_repo(state, auth).await?;
    let item = record::work_item_by_id(&state.pool, auth.work_item_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("work item {} not found", auth.work_item_id))?;

    // Escalation is idempotent per (work_item, reason): the DB carries a UNIQUE
    // (work_item_id, reason) index (migration 0004), so a second escalate for the same
    // (work_item, reason) — e.g. via a distinct caller `dedupe_key` that bypasses the
    // task-level idempotency claim above — is a no-op success (`created = false`), not a
    // DB unique-violation. On conflict the original escalation row (and its context) is
    // preserved, matching the already-delivered human-facing question.
    let (escalation_id, created) =
        record::create_escalation_idempotent(&state.pool, auth.work_item_id, reason, context, None)
            .await?;

    // A repeat (work_item, reason) escalation already has a delivered question and a
    // settled work-item state; do not post a second comment or re-set the state.
    if !created {
        tracing::info!(
            work_item = %auth.work_item_id,
            reason,
            %escalation_id,
            "escalate: idempotent no-op (escalation already exists for this work_item+reason)"
        );
        let item_state = record::work_item_by_id(&state.pool, auth.work_item_id)
            .await?
            .map(|w| w.state)
            .unwrap_or(item.state);
        return Ok(json!({
            "escalation_id": escalation_id.to_string(),
            "delivered_comment_id": Option::<String>::None,
            "state": item_state.as_str(),
            "state_changed": false,
        }));
    }

    // Post the human-facing question on the subject (best-effort delivery target).
    let delivered = if let Some(target) = subject_comment_target(&item.subject_ref) {
        let body = format!("Factory needs a decision ({reason}):\n\n{context}");
        match state.gh.post_comment(&repo, target, &body).await {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(work_item = %auth.work_item_id, error = %e, "escalate: comment delivery failed");
                None
            }
        }
    } else {
        tracing::info!(subject = %item.subject_ref, "escalate: no GitHub subject to comment on");
        None
    };

    // Map the reason to the work-item state: needs-human-input is resumable (blocked,
    // re-queued on a human reply via requeue_blocked); the rest are terminal.
    let target_state = escalation_target_state(reason);
    let state_changed = record::set_state(&state.pool, auth.work_item_id, target_state).await?;

    Ok(json!({
        "escalation_id": escalation_id.to_string(),
        "delivered_comment_id": delivered,
        "state": target_state.as_str(),
        "state_changed": state_changed,
    }))
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Default heartbeat lease extension (15 minutes), per the spec.
const HEARTBEAT_TTL_SECS: f64 = 900.0;

/// Prefix for the per-reason default idempotency key used by [`escalate`] when the
/// caller supplies no explicit `dedupe_key`. The key is `escalate:<reason>`, so distinct
/// reasons each escalate once (mirroring the DB UNIQUE (work_item_id, reason) index)
/// while a repeated reason dedupes to the first.
const ESCALATE_DEFAULT_KEY_PREFIX: &str = "escalate:";

/// How many times [`idempotent`] retries `idempotency_complete` after a successful
/// side effect before falling back to release+warn. Complete-failures are usually
/// transient DB blips; a few retries with a yield between usually succeed and avoid a
/// duplicate side effect on the next request.
const IDEMPOTENCY_COMPLETE_ATTEMPTS: u32 = 3;

/// The built-in, role-specific base prompt. Repo-wide and per-worker instructions
/// are layered on top of this by [`compose_prompt`].
fn role_default_prompt(role: record::Kind) -> &'static str {
    match role {
        record::Kind::Manager => {
            "You are a manager. Triage the subject, decide the work, and delegate to coders."
        }
        record::Kind::Coder => {
            "You are a coder. Implement the task with tests, then open a change request."
        }
        record::Kind::Reviewer => {
            "You are a reviewer. Review the change request and submit a verdict."
        }
        record::Kind::Tester => {
            "You are a tester. Exercise the change and report whether the checks pass."
        }
    }
}

/// Compose the worker's effective prompt: role default → repo-wide
/// (`settings.instructions`) → the matching worker's instructions. Missing config
/// layers are skipped; the role default is always present. Takes the repo's
/// already-loaded config snapshot (see [`get_task`]) so it never re-queries the row.
fn compose_prompt(auth: &TaskAuth, item: &record::WorkItem, repo_config: Option<Value>) -> String {
    let role_default = role_default_prompt(item.kind);

    let cfg = match repo_config {
        Some(value) => match serde_json::from_value::<factory_config::Config>(value) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(work_item = %auth.work_item_id, error = %e, "get_task: stored config is not a valid Config; using role default only");
                None
            }
        },
        None => {
            tracing::info!(work_item = %auth.work_item_id, "get_task: repo has no stored config; using role default only");
            None
        }
    };

    let repo_wide = cfg
        .as_ref()
        .and_then(|c| c.settings.instructions.as_deref());
    let worker = cfg.as_ref().and_then(|c| {
        c.workers
            .iter()
            .find(|w| w.name == item.worker_name)
            .and_then(|w| w.instructions.as_deref())
    });

    factory_config::resolve_prompt(role_default, repo_wide, worker)
}

/// The transcript object URI for a task. Derived solely from the work item id so a
/// token can only ever address its own transcript.
fn transcript_uri(auth: &TaskAuth) -> String {
    format!("{}/transcript.log", auth.work_item_id)
}

/// Resolve the token's repo to a GitHub [`RepoRef`]. ALL GitHub ops go through
/// this — a tool can never act on a repo other than the token's.
async fn resolve_repo(state: &ToolsState, auth: &TaskAuth) -> anyhow::Result<RepoRef> {
    let r = record::repo_by_id(&state.pool, auth.repo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repo {} not found", auth.repo_id))?;
    Ok(RepoRef::new(r.owner, r.name, r.installation_id))
}

/// Map a `subject_ref` to the comment target used for escalation questions.
fn subject_comment_target(subject_ref: &str) -> Option<CommentTarget> {
    Subject::from_str(subject_ref)
        .ok()
        .map(Subject::comment_target)
}

/// Parse a `post_comment` target argument (`issue#N` / `cr#N`). An unparseable
/// target is a bad request (HTTP 400).
fn parse_comment_target(target: &str) -> std::result::Result<CommentTarget, ToolError> {
    Subject::from_str(target)
        .map(Subject::comment_target)
        .map_err(|_| {
            ToolError::BadRequest(format!(
                "post_comment: target must be 'issue#N' or 'cr#N', got {target:?}"
            ))
        })
}

/// Parse a review verdict string. An unknown verdict is a bad request (HTTP 400).
fn parse_verdict(verdict: &str) -> std::result::Result<ReviewVerdict, ToolError> {
    Ok(match verdict {
        "approve" => ReviewVerdict::Approve,
        "request_changes" => ReviewVerdict::RequestChanges,
        "comment" => ReviewVerdict::Comment,
        other => {
            return Err(ToolError::BadRequest(format!(
                "submit_review: verdict must be approve|request_changes|comment, got {other:?}"
            )))
        }
    })
}

/// Map a `record_outcome` status to (attempt status, work-item state). An unknown
/// status is a bad request (HTTP 400).
fn parse_outcome(status: &str) -> std::result::Result<(AttemptStatus, State), ToolError> {
    Ok(match status {
        "succeeded" => (AttemptStatus::Succeeded, State::Succeeded),
        "failed" => (AttemptStatus::Failed, State::Failed),
        "blocked" => (AttemptStatus::Blocked, State::Blocked),
        other => {
            return Err(ToolError::BadRequest(format!(
                "record_outcome: status must be succeeded|failed|blocked, got {other:?}"
            )))
        }
    })
}

/// The shared claim→side-effect→complete / release-on-error idempotency state
/// machine used by `post_comment`, `submit_review`, and `escalate`. Routes the claim
/// outcome:
///
/// - **`Won`** (this caller owns the slot): run `run` (the side effect); on `Err`
///   best-effort [`release_claim`] the slot so the next retry re-claims immediately,
///   then propagate the error; on `Ok(result)` record it with `idempotency_complete`
///   and return it.
/// - **`Pending`** (a concurrent caller holds the slot): return the in-progress
///   error so the caller retries — message and variant are caller-supplied so each
///   tool keeps its exact wording.
/// - **`Done(stored)`** (the slot already has a result): run `on_redo(&stored)`
///   (post_comment updates the existing comment; the others pass a no-op) and return
///   the stored result unchanged.
///
/// `run` is only ever invoked once, by the single claim winner — exactly-once.
async fn idempotent<RunFut, RedoFut>(
    state: &ToolsState,
    auth: &TaskAuth,
    scope: &str,
    key: &str,
    in_progress_msg: &str,
    run: impl FnOnce() -> RunFut,
    on_redo: impl FnOnce(Value) -> RedoFut,
) -> ToolResult
where
    RunFut: std::future::Future<Output = ToolResult>,
    RedoFut: std::future::Future<Output = ToolResult>,
{
    match record::idempotency_claim(&state.pool, auth.work_item_id, scope, key).await? {
        IdempotencyClaim::Done(stored) => on_redo(stored).await,
        IdempotencyClaim::Pending => {
            // A concurrent caller holds the claim while its side effect is in flight.
            // This is a transient race, not a hard failure: classify it as a retryable
            // Conflict (HTTP 409) so the caller retries rather than treating it as 500.
            tracing::info!(work_item = %auth.work_item_id, scope, dedupe_key = key, "idempotency: claim pending; ask caller to retry");
            Err(ToolError::Conflict(in_progress_msg.to_owned()))
        }
        IdempotencyClaim::Won => match run().await {
            Ok(result) => {
                // The side effect SUCCEEDED. Record its result so later callers dedupe.
                // If idempotency_complete fails, we must NOT return via `?`: that would
                // strand the 'null' placeholder, wedging every retry as Pending for the
                // full stale window (CLAIM_STALE_SECS), after which the stale-reclaim
                // re-runs the side effect — a DUPLICATE. Instead release the claim so a
                // retry re-claims immediately.
                //
                // First, shrink the duplicate window: most complete-failures are
                // transient DB blips (a dropped connection, a momentary deadlock), so we
                // retry idempotency_complete a small bounded number of times with a yield
                // between attempts. A success here records the result and avoids the
                // duplicate entirely. Only after all attempts fail do we fall back to
                // release+warn below.
                let mut last_err = None;
                for attempt in 1..=IDEMPOTENCY_COMPLETE_ATTEMPTS {
                    match record::idempotency_complete(
                        &state.pool,
                        auth.work_item_id,
                        scope,
                        key,
                        &result,
                    )
                    .await
                    {
                        Ok(()) => return Ok(result),
                        Err(e) => {
                            tracing::warn!(work_item = %auth.work_item_id, scope, attempt, error = %e, "idempotency_complete failed; retrying");
                            last_err = Some(e);
                            // Yield so a transient blip (e.g. a pool slot freeing up) has
                            // a chance to clear before the next attempt; no new dep.
                            tokio::task::yield_now().await;
                        }
                    }
                }

                // Residual tradeoff: after the bounded retries still failed, the side
                // effect already ran but its result was never recorded, so a retry that
                // re-claims will run the side effect a SECOND time (a duplicate). This is
                // the irreducible dual-write window between the external side effect and
                // our record book — there is no way to make "do the side effect" and
                // "record that we did it" atomic across two systems. Releasing here trades
                // the old failure mode (wedge for CLAIM_STALE_SECS, THEN silently
                // double-fire) for a faster, logged one (retry MAY double-fire), which is
                // strictly better: bounded and visible. The retry loop above shrinks how
                // often we reach this fallback, but cannot eliminate the window.
                //
                // The loop only falls through here after every attempt returned Err, so
                // last_err is always Some; the match keeps us from fabricating an error.
                match last_err {
                    Some(e) => {
                        tracing::warn!(work_item = %auth.work_item_id, scope, attempts = IDEMPOTENCY_COMPLETE_ATTEMPTS, error = %e, "idempotency_complete failed after retries; releasing claim (a retry may duplicate the side effect)");
                        release_claim(state, auth, scope, key).await;
                        Err(ToolError::Internal(e.into()))
                    }
                    // Unreachable: the loop runs at least once and only exits via the
                    // Err arm (the Ok arm returns). Surface a defined error rather than
                    // panicking if the invariant is ever broken.
                    None => Err(ToolError::Internal(anyhow::anyhow!(
                        "idempotency_complete retry loop produced no result"
                    ))),
                }
            }
            Err(e) => {
                // Side effect failed: release the claim so the next retry re-claims
                // immediately rather than waiting out the stale window.
                release_claim(state, auth, scope, key).await;
                Err(e)
            }
        },
    }
}

/// Best-effort release of an idempotency claim whose side effect failed. Logs on
/// failure but never propagates: the worst case if the release itself fails is that
/// the placeholder waits out the stale window before the next retry re-claims it,
/// which is still eventually-once. We never want a release failure to mask the
/// original side-effect error.
async fn release_claim(state: &ToolsState, auth: &TaskAuth, scope: &str, key: &str) {
    if let Err(e) = record::idempotency_release(&state.pool, auth.work_item_id, scope, key).await {
        tracing::warn!(
            work_item = %auth.work_item_id,
            scope,
            dedupe_key = key,
            error = %e,
            "idempotency: release after side-effect failure failed (will wait out stale window)"
        );
    }
}

/// Map an escalation reason to the resulting work-item state. Needs-human-input is
/// *resumable*: it parks the item in `blocked` so a human reply re-queues it (via
/// `requeue_blocked`). The remaining reasons are *terminal*: the item is `escalated`
/// and never auto-resumes.
fn escalation_target_state(reason: &str) -> State {
    match reason {
        "blocked_input" | "needs_decision" => State::Blocked,
        // "retries_exhausted" | "loop_cap" | "conflict" (and any future terminal
        // reason already gated by validate_escalation_reason) → terminal.
        _ => State::Escalated,
    }
}

/// The escalation reasons the record book accepts (its CHECK constraint). Reject
/// anything else here with a clear error.
const ESCALATION_REASONS: [&str; 5] = [
    "retries_exhausted",
    "loop_cap",
    "needs_decision",
    "blocked_input",
    "conflict",
];

fn validate_escalation_reason(reason: &str) -> std::result::Result<(), ToolError> {
    if ESCALATION_REASONS.contains(&reason) {
        Ok(())
    } else {
        Err(ToolError::BadRequest(format!(
            "escalate: reason must be one of {ESCALATION_REASONS:?}, got {reason:?}"
        )))
    }
}

/// JSON-encode a check summary (overall + per-check conclusions).
fn check_summary_json(summary: &factory_github::CheckSummary) -> Value {
    json!({
        "overall": summary.overall.as_str(),
        "checks": summary
            .checks
            .iter()
            .map(|(name, c)| json!({ "name": name, "conclusion": c.as_str() }))
            .collect::<Vec<_>>(),
    })
}

// --- argument extraction (typed accessors over the JSON request body) ---

fn arg_str<'a>(args: &'a Value, key: &str) -> std::result::Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::BadRequest(format!("missing or non-string argument {key:?}")))
}

fn arg_opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn arg_i64(args: &Value, key: &str) -> std::result::Result<i64, ToolError> {
    args.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| ToolError::BadRequest(format!("missing or non-integer argument {key:?}")))
}

fn arg_opt_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

// ===========================================================================
// HTTP layer. Lives here (not just in the binary) so integration tests can drive
// the full path — bearer extractor → authorize → audit → dispatch — via a tower
// `oneshot`. This is the single choke point that authorizes and audits EVERY call.
// ===========================================================================

use axum::extract::{Path, State as AxumState};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

/// Build the Tools router. Extracted so the binary and integration tests share the
/// exact same auth + audit + dispatch path.
pub fn build_router(state: Arc<ToolsState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/tools/:tool", post(handle_tool))
        // MCP Streamable-HTTP / JSON-RPC 2.0 endpoint (Claude Code & other MCP
        // clients). POST carries JSON-RPC requests; GET would open the SSE stream,
        // which Factory does not use (all tools are request/response).
        .route("/mcp", post(mcp::handle_post).get(mcp::handle_get))
        .with_state(state)
}

/// One JSON-in/JSON-out handler for every tool: authorize the bearer, parse the
/// tool name, AUDIT, then dispatch. Auditing around dispatch is the single choke
/// point guaranteeing every authorized call lands in the events book.
async fn handle_tool(
    AxumState(state): AxumState<Arc<ToolsState>>,
    Path(tool_name): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let Some(tool) = Tool::parse(&tool_name) else {
        tracing::warn!(tool = %tool_name, "unknown tool");
        return error_response(StatusCode::NOT_FOUND, "unknown tool");
    };

    let Some(bearer) = bearer_token(&headers) else {
        tracing::warn!(tool = %tool_name, "missing bearer token");
        return error_response(StatusCode::UNAUTHORIZED, "missing bearer token");
    };

    let auth = match authorize(&state.pool, &bearer).await {
        Ok(Some(auth)) => auth,
        Ok(None) => {
            tracing::warn!(tool = %tool_name, "unauthorized token");
            return error_response(StatusCode::UNAUTHORIZED, "unauthorized");
        }
        Err(e) => {
            tracing::error!(tool = %tool_name, error = %e, "token resolution failed");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "auth error");
        }
    };

    let args = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));

    match audit_and_dispatch(&state, &auth, tool, &args).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(ToolCallError::Audit(e)) => {
            tracing::error!(tool = tool.as_str(), error = %e, "audit write failed");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "audit error")
        }
        // A bad request is the caller's fault: 400 with the message echoed back.
        Err(ToolCallError::Tool(ToolError::BadRequest(msg))) => {
            tracing::warn!(tool = tool.as_str(), work_item = %auth.work_item_id, error = %msg, "tool rejected bad request");
            error_response(StatusCode::BAD_REQUEST, &msg)
        }
        // A retryable conflict (idempotency race): 409 with the in-progress message
        // echoed back so the caller knows to retry rather than treat it as a failure.
        Err(ToolCallError::Tool(ToolError::Conflict(msg))) => {
            tracing::info!(tool = tool.as_str(), work_item = %auth.work_item_id, error = %msg, "tool conflict; caller should retry");
            error_response(StatusCode::CONFLICT, &msg)
        }
        // An internal failure is Factory's fault: 500 with a generic body; the real
        // error is logged server-side, never leaked to the caller.
        Err(ToolCallError::Tool(ToolError::Internal(e))) => {
            tracing::error!(tool = tool.as_str(), work_item = %auth.work_item_id, error = %e, "tool failed");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// Outcome of running a tool through the audited choke point: either an
/// infrastructure failure writing the audit row, or a failure inside the tool
/// itself. The HTTP layer maps `Audit` → 500, `Tool(BadRequest)` → 400,
/// `Tool(Conflict)` → 409, and `Tool(Internal)` → 500; the MCP layer maps `Audit` → a
/// JSON-RPC internal error, `Tool(BadRequest)` → JSON-RPC invalid-params,
/// `Tool(Conflict)` → a JSON-RPC error with a retryable code, and `Tool(Internal)` →
/// an `isError` content result.
pub enum ToolCallError {
    /// Writing the mandatory audit event failed; the call did not run.
    Audit(anyhow::Error),
    /// The tool ran (or its arguments were rejected) and returned an error,
    /// classified as a bad request (caller's fault) or internal (Factory's fault).
    Tool(ToolError),
}

/// The single audited choke point shared by the REST and MCP surfaces: thread the
/// work item's correlation id onto the audit row, write the mandatory audit event
/// BEFORE running the tool (so no call can escape the events book), then dispatch.
///
/// Both transports call exactly this — there is one code path that authorizes,
/// audits, and dispatches a tool, so neither surface can skip an audit row.
pub async fn audit_and_dispatch(
    state: &ToolsState,
    auth: &TaskAuth,
    tool: Tool,
    args: &Value,
) -> Result<Value, ToolCallError> {
    // Thread the work item's correlation id onto the audit row so every tool call
    // joins the same causal chain as the dispatch that launched it.
    let correlation_id = match record::work_item_by_id(&state.pool, auth.work_item_id).await {
        Ok(Some(item)) => Some(item.correlation_id),
        Ok(None) => {
            tracing::warn!(work_item = %auth.work_item_id, "audit: work item not found for correlation");
            None
        }
        Err(e) => {
            tracing::error!(tool = tool.as_str(), error = %e, "audit: correlation lookup failed");
            return Err(ToolCallError::Audit(e.into()));
        }
    };

    // Audit the authorized call before running it: every attempt to use a tool is
    // recorded. The single place no tool call can escape. Per the spec data model,
    // the actor is `tool:<name>` and the action is the tool name.
    if let Err(e) = record::append_event(
        &state.pool,
        Some(auth.repo_id),
        Some(auth.work_item_id),
        correlation_id,
        &format!("tool:{}", tool.as_str()),
        tool.as_str(),
        json!({ "args": args }),
    )
    .await
    {
        return Err(ToolCallError::Audit(e.into()));
    }

    dispatch(state, auth, tool, args)
        .await
        .map_err(ToolCallError::Tool)
}

/// Extract the `Authorization: Bearer <token>` value.
pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_owned)
        .filter(|t| !t.is_empty())
}

/// A uniform JSON error body with a status code.
fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_parse_roundtrips_every_variant() {
        for t in [
            Tool::GetTask,
            Tool::ReadIssue,
            Tool::ReadChangeRequest,
            Tool::OpenOrUpdateChangeRequest,
            Tool::PostComment,
            Tool::SubmitReview,
            Tool::GetCheckStatus,
            Tool::AppendTranscript,
            Tool::RecordOutcome,
            Tool::Heartbeat,
            Tool::Escalate,
        ] {
            assert_eq!(Tool::parse(t.as_str()), Some(t), "roundtrip {}", t.as_str());
        }
        assert_eq!(Tool::parse("nope"), None);
    }

    #[test]
    fn parse_outcome_maps_all_states() {
        assert_eq!(
            parse_outcome("succeeded").unwrap(),
            (AttemptStatus::Succeeded, State::Succeeded)
        );
        assert_eq!(
            parse_outcome("failed").unwrap(),
            (AttemptStatus::Failed, State::Failed)
        );
        assert_eq!(
            parse_outcome("blocked").unwrap(),
            (AttemptStatus::Blocked, State::Blocked)
        );
        assert!(parse_outcome("weird").is_err());
    }

    #[test]
    fn parse_verdict_maps_all_and_rejects_unknown() {
        assert_eq!(parse_verdict("approve").unwrap(), ReviewVerdict::Approve);
        assert_eq!(
            parse_verdict("request_changes").unwrap(),
            ReviewVerdict::RequestChanges
        );
        assert_eq!(parse_verdict("comment").unwrap(), ReviewVerdict::Comment);
        assert!(parse_verdict("lgtm").is_err());
    }

    #[test]
    fn escalation_reason_validation() {
        for r in ESCALATION_REASONS {
            assert!(validate_escalation_reason(r).is_ok());
        }
        assert!(validate_escalation_reason("because").is_err());
    }

    #[test]
    fn bearer_token_extraction() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer abc123".parse().unwrap(),
        );
        assert_eq!(bearer_token(&h).as_deref(), Some("abc123"));

        let mut empty = HeaderMap::new();
        empty.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer ".parse().unwrap(),
        );
        assert_eq!(bearer_token(&empty), None);

        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn comment_target_parsing() {
        assert_eq!(
            parse_comment_target("issue#3").unwrap(),
            CommentTarget::Issue(3)
        );
        assert_eq!(
            parse_comment_target("cr#9").unwrap(),
            CommentTarget::ChangeRequest(9)
        );
        assert!(parse_comment_target("pr/9").is_err());
    }
}
