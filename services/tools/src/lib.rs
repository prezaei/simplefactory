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

use std::sync::Arc;

use factory_blob::TranscriptStore;
use factory_github::{
    CheckRef, CommentTarget, GitHubClient, OpenPrRequest, RepoRef, ReviewVerdict,
};
use factory_record::{self as record, AttemptStatus, IdempotencyClaim, State, TaskAuth};
use serde_json::{json, Value};

/// A tool-call error split by who is at fault, so the HTTP layer can map it to the
/// right status: a [`ToolError::BadRequest`] is the caller's fault (malformed or
/// invalid arguments) and becomes a `400` with the message echoed back; a
/// [`ToolError::Internal`] is Factory's fault (DB / GitHub / blob I/O) and becomes a
/// `500` with a generic body, the real error logged server-side.
#[derive(Debug)]
pub enum ToolError {
    /// Invalid or missing arguments — the caller must fix the request (HTTP 400).
    BadRequest(String),
    /// An internal failure (DB, GitHub, blob, …) — not the caller's fault (HTTP 500).
    Internal(anyhow::Error),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::BadRequest(msg) => write!(f, "{msg}"),
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

    // Compose the worker's prompt from the repo's stored config: the built-in role
    // default, then the repo-wide instructions, then the matching worker's.
    let prompt = compose_prompt(state, auth, &item).await?;

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

    match parse_subject(&item.subject_ref) {
        Some(Subject::Issue(n)) => {
            let repo = resolve_repo(state, auth).await?;
            let issue = state.gh.read_issue(&repo, n).await?;
            out["subject"] = json!({
                "type": "issue",
                "number": issue.number,
                "title": issue.title,
                "body": issue.body,
                "comments": issue.comments,
            });
        }
        Some(Subject::ChangeRequest(n)) => {
            let repo = resolve_repo(state, auth).await?;
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
        None => {
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

/// Open (or update) the change request for this task. Idempotent two ways: the
/// record-book row is keyed by work item (one CR per task, survives re-dispatch),
/// and the GitHub PR is keyed by head branch (re-open returns the same number).
/// The number from GitHub is recorded back onto the record-book row.
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

    // One CR row per work item — a re-dispatch returns the SAME row.
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

    match record::idempotency_claim(&state.pool, auth.work_item_id, "comment", key).await? {
        IdempotencyClaim::Done(v) => {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "post_comment: deduped (no-op)");
            Ok(v)
        }
        IdempotencyClaim::Pending => {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "post_comment: claim pending; ask caller to retry");
            Err(ToolError::Internal(anyhow::anyhow!(
                "a comment for this task is already in progress"
            )))
        }
        IdempotencyClaim::Won => {
            let comment_id = state.gh.post_comment(&repo, comment_target, body).await?;
            let result = json!({ "comment_id": comment_id });
            record::idempotency_complete(&state.pool, auth.work_item_id, "comment", key, &result)
                .await?;
            tracing::info!(work_item = %auth.work_item_id, ?target, "post_comment");
            Ok(result)
        }
    }
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
        tracing::info!(work_item = %auth.work_item_id, cr_number, verdict = verdict.as_str(), "submit_review (no dedupe)");
        return Ok(json!({ "review_id": review_id }));
    };

    match record::idempotency_claim(&state.pool, auth.work_item_id, "review", key).await? {
        IdempotencyClaim::Done(v) => {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "submit_review: deduped (no-op)");
            Ok(v)
        }
        IdempotencyClaim::Pending => {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "submit_review: claim pending; ask caller to retry");
            Err(ToolError::Internal(anyhow::anyhow!(
                "a review for this task is already in progress"
            )))
        }
        IdempotencyClaim::Won => {
            let review_id = state
                .gh
                .submit_review(&repo, cr_number, verdict, body)
                .await?;
            let result = json!({ "review_id": review_id });
            record::idempotency_complete(&state.pool, auth.work_item_id, "review", key, &result)
                .await?;
            tracing::info!(work_item = %auth.work_item_id, cr_number, verdict = verdict.as_str(), "submit_review");
            Ok(result)
        }
    }
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

/// Escalate to a human — the single human channel. Idempotent per task under scope
/// `"escalate"` via the claim-first pattern: the claim winner records the escalation,
/// posts the question on the subject, moves the work item to `escalated`, then stores
/// the result. Even when the caller passes NO `dedupe_key` the key defaults to a
/// per-task constant, so escalate is idempotent per work item by default — exactly one
/// escalation row per task no matter how many concurrent or retried calls arrive.
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

    // One escalation per task: when the caller omits a dedupe_key, fall back to a
    // per-task constant so escalate is idempotent per work item by default.
    let key = dedupe_key.unwrap_or(ESCALATE_DEFAULT_KEY);

    match record::idempotency_claim(&state.pool, auth.work_item_id, "escalate", key).await? {
        IdempotencyClaim::Done(v) => {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "escalate: deduped (no-op)");
            return Ok(v);
        }
        IdempotencyClaim::Pending => {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "escalate: claim pending; ask caller to retry");
            return Err(ToolError::Internal(anyhow::anyhow!(
                "an escalate for this task is already in progress"
            )));
        }
        IdempotencyClaim::Won => {}
    }

    let repo = resolve_repo(state, auth).await?;
    let item = record::work_item_by_id(&state.pool, auth.work_item_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("work item {} not found", auth.work_item_id))?;

    let escalation_id =
        record::create_escalation(&state.pool, auth.work_item_id, reason, context, None).await?;

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

    let state_changed = record::set_state(&state.pool, auth.work_item_id, State::Escalated).await?;

    let result = json!({
        "escalation_id": escalation_id.to_string(),
        "delivered_comment_id": delivered,
        "state_changed": state_changed,
    });
    record::idempotency_complete(&state.pool, auth.work_item_id, "escalate", key, &result).await?;
    tracing::info!(work_item = %auth.work_item_id, reason, "escalate");
    Ok(result)
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Default heartbeat lease extension (15 minutes), per the spec.
const HEARTBEAT_TTL_SECS: f64 = 900.0;

/// The dedupe key used for `escalate` when the caller supplies none. Escalation is
/// "one per task", so the default key is a per-task constant — every escalate for a
/// work item shares this slot and only the first runs the side effect.
const ESCALATE_DEFAULT_KEY: &str = "task";

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
/// layers are skipped; the role default is always present.
async fn compose_prompt(
    state: &ToolsState,
    auth: &TaskAuth,
    item: &record::WorkItem,
) -> anyhow::Result<String> {
    let role_default = role_default_prompt(item.kind);

    let cfg = match record::repo_config_by_id(&state.pool, auth.repo_id).await? {
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

    Ok(factory_config::resolve_prompt(
        role_default,
        repo_wide,
        worker,
    ))
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
    Ok(RepoRef {
        owner: r.owner,
        name: r.name,
        installation_id: r.installation_id,
    })
}

/// A parsed `subject_ref`.
enum Subject {
    Issue(i64),
    ChangeRequest(i64),
}

/// Parse a `subject_ref` like `issue#12` / `cr#34`. Other forms (e.g. schedule
/// subjects) return `None`.
fn parse_subject(subject_ref: &str) -> Option<Subject> {
    if let Some(n) = subject_ref.strip_prefix("issue#") {
        return n.parse().ok().map(Subject::Issue);
    }
    if let Some(n) = subject_ref.strip_prefix("cr#") {
        return n.parse().ok().map(Subject::ChangeRequest);
    }
    None
}

/// Map a `subject_ref` to the comment target used for escalation questions.
fn subject_comment_target(subject_ref: &str) -> Option<CommentTarget> {
    match parse_subject(subject_ref)? {
        Subject::Issue(n) => Some(CommentTarget::Issue(n)),
        Subject::ChangeRequest(n) => Some(CommentTarget::ChangeRequest(n)),
    }
}

/// Parse a `post_comment` target argument (`issue#N` / `cr#N`). An unparseable
/// target is a bad request (HTTP 400).
fn parse_comment_target(target: &str) -> std::result::Result<CommentTarget, ToolError> {
    parse_subject(target)
        .map(|s| match s {
            Subject::Issue(n) => CommentTarget::Issue(n),
            Subject::ChangeRequest(n) => CommentTarget::ChangeRequest(n),
        })
        .ok_or_else(|| {
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
/// itself. The HTTP layer maps `Audit` → 500, `Tool(BadRequest)` → 400, and
/// `Tool(Internal)` → 500; the MCP layer maps `Audit` → a JSON-RPC internal error,
/// `Tool(BadRequest)` → JSON-RPC invalid-params, and `Tool(Internal)` → an `isError`
/// content result.
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
    fn parse_subject_handles_issue_cr_and_other() {
        assert!(matches!(parse_subject("issue#7"), Some(Subject::Issue(7))));
        assert!(matches!(
            parse_subject("cr#42"),
            Some(Subject::ChangeRequest(42))
        ));
        assert!(parse_subject("schedule@manager@x").is_none());
        assert!(parse_subject("issue#notanumber").is_none());
    }

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
