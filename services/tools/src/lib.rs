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

use std::sync::Arc;

use factory_blob::TranscriptStore;
use factory_github::{
    CheckRef, CommentTarget, GitHubClient, OpenPrRequest, RepoRef, ReviewVerdict,
};
use factory_record::{self as record, AttemptStatus, State, TaskAuth};
use serde_json::{json, Value};

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
pub async fn dispatch(
    state: &ToolsState,
    auth: &TaskAuth,
    tool: Tool,
    args: &Value,
) -> anyhow::Result<Value> {
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
pub async fn get_task(state: &ToolsState, auth: &TaskAuth) -> anyhow::Result<Value> {
    let item = record::work_item_by_id(&state.pool, auth.work_item_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("work item {} not found", auth.work_item_id))?;

    let mut out = json!({
        "work_item_id": auth.work_item_id.to_string(),
        "role": item.kind.as_str(),
        "subject_ref": item.subject_ref,
        "correlation_id": item.correlation_id.to_string(),
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
pub async fn read_issue(
    state: &ToolsState,
    auth: &TaskAuth,
    issue_number: i64,
) -> anyhow::Result<Value> {
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
) -> anyhow::Result<Value> {
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
) -> anyhow::Result<Value> {
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
/// provided the call is idempotent: the first post is recorded under
/// `(work_item, "comment", key)` and a repeat returns the stored result WITHOUT
/// posting again.
pub async fn post_comment(
    state: &ToolsState,
    auth: &TaskAuth,
    target: &str,
    body: &str,
    dedupe_key: Option<&str>,
) -> anyhow::Result<Value> {
    let repo = resolve_repo(state, auth).await?;
    let comment_target = parse_comment_target(target)?;

    if let Some(key) = dedupe_key {
        if let Some(prev) =
            record::idempotency_get(&state.pool, auth.work_item_id, "comment", key).await?
        {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "post_comment: deduped (no-op)");
            return Ok(prev);
        }
    }

    let comment_id = state.gh.post_comment(&repo, comment_target, body).await?;
    let result = json!({ "comment_id": comment_id });

    let stored = match dedupe_key {
        Some(key) => {
            record::idempotency_get_or_set(&state.pool, auth.work_item_id, "comment", key, &result)
                .await?
        }
        None => result,
    };
    tracing::info!(work_item = %auth.work_item_id, ?target, "post_comment");
    Ok(stored)
}

/// Submit a review verdict on a change request. Idempotent by `dedupe_key` under
/// scope `"review"` — a repeat returns the stored result and submits no second
/// review.
pub async fn submit_review(
    state: &ToolsState,
    auth: &TaskAuth,
    cr_number: i64,
    verdict: &str,
    body: &str,
    dedupe_key: Option<&str>,
) -> anyhow::Result<Value> {
    let repo = resolve_repo(state, auth).await?;
    let verdict = parse_verdict(verdict)?;

    if let Some(key) = dedupe_key {
        if let Some(prev) =
            record::idempotency_get(&state.pool, auth.work_item_id, "review", key).await?
        {
            tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "submit_review: deduped (no-op)");
            return Ok(prev);
        }
    }

    let review_id = state
        .gh
        .submit_review(&repo, cr_number, verdict, body)
        .await?;
    let result = json!({ "review_id": review_id });

    let stored = match dedupe_key {
        Some(key) => {
            record::idempotency_get_or_set(&state.pool, auth.work_item_id, "review", key, &result)
                .await?
        }
        None => result,
    };
    tracing::info!(work_item = %auth.work_item_id, cr_number, verdict = verdict.as_str(), "submit_review");
    Ok(stored)
}

/// Read required-check status for either a CR number or an explicit sha. Exactly
/// one of the two must be supplied.
pub async fn get_check_status(
    state: &ToolsState,
    auth: &TaskAuth,
    cr_number: Option<i64>,
    sha: Option<&str>,
) -> anyhow::Result<Value> {
    let repo = resolve_repo(state, auth).await?;
    let what = match (cr_number, sha) {
        (Some(n), None) => CheckRef::ChangeRequest(n),
        (None, Some(s)) => CheckRef::Sha(s.to_owned()),
        (Some(_), Some(_)) => {
            anyhow::bail!("get_check_status: provide exactly one of cr_number or sha, not both")
        }
        (None, None) => anyhow::bail!("get_check_status: one of cr_number or sha is required"),
    };
    let summary = state.gh.check_status(&repo, what).await?;
    tracing::info!(work_item = %auth.work_item_id, overall = summary.overall.as_str(), "get_check_status");
    Ok(check_summary_json(&summary))
}

/// Append a chunk to this task's transcript. The transcript URI is derived from
/// the work item id, so a worker can never write to another task's transcript.
pub async fn append_transcript(
    state: &ToolsState,
    auth: &TaskAuth,
    chunk: &str,
) -> anyhow::Result<Value> {
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
) -> anyhow::Result<Value> {
    let (attempt_status, work_state) = parse_outcome(status)?;
    let attempt_id = auth
        .attempt_id
        .ok_or_else(|| anyhow::anyhow!("record_outcome: token has no attempt to finish"))?;

    let finished =
        record::finish_attempt(&state.pool, attempt_id, attempt_status, Some(summary)).await?;
    let state_changed = record::set_state(&state.pool, auth.work_item_id, work_state).await?;

    tracing::info!(
        work_item = %auth.work_item_id,
        attempt = %attempt_id,
        status,
        finished,
        state_changed,
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
pub async fn heartbeat(state: &ToolsState, auth: &TaskAuth) -> anyhow::Result<Value> {
    let expires =
        record::heartbeat_task(&state.pool, auth.work_item_id, HEARTBEAT_TTL_SECS).await?;
    tracing::info!(work_item = %auth.work_item_id, extended = expires.is_some(), "heartbeat");
    Ok(json!({ "lease_expires_at": expires }))
}

/// Escalate to a human — the single human channel. Idempotent per task under scope
/// `"escalate"`: one escalation row per task even if called repeatedly with the
/// same dedupe_key. On first call it records the escalation, posts the question on
/// the subject, and moves the work item to `escalated`.
pub async fn escalate(
    state: &ToolsState,
    auth: &TaskAuth,
    reason: &str,
    context: &str,
    dedupe_key: Option<&str>,
) -> anyhow::Result<Value> {
    // Escalation reason is constrained by the record book; reject bad values up
    // front with a clear message rather than surfacing a DB constraint error.
    validate_escalation_reason(reason)?;

    // One escalation per task. The dedupe key defaults to the reason when the
    // caller omits one, so a retry of the same escalation never double-files.
    let key = dedupe_key.unwrap_or(reason);
    if let Some(prev) =
        record::idempotency_get(&state.pool, auth.work_item_id, "escalate", key).await?
    {
        tracing::info!(work_item = %auth.work_item_id, dedupe_key = key, "escalate: deduped (no-op)");
        return Ok(prev);
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
    let stored =
        record::idempotency_get_or_set(&state.pool, auth.work_item_id, "escalate", key, &result)
            .await?;
    tracing::info!(work_item = %auth.work_item_id, reason, "escalate");
    Ok(stored)
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Default heartbeat lease extension (15 minutes), per the spec.
const HEARTBEAT_TTL_SECS: f64 = 900.0;

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

/// Parse a `post_comment` target argument (`issue#N` / `cr#N`).
fn parse_comment_target(target: &str) -> anyhow::Result<CommentTarget> {
    parse_subject(target)
        .map(|s| match s {
            Subject::Issue(n) => CommentTarget::Issue(n),
            Subject::ChangeRequest(n) => CommentTarget::ChangeRequest(n),
        })
        .ok_or_else(|| {
            anyhow::anyhow!("post_comment: target must be 'issue#N' or 'cr#N', got {target:?}")
        })
}

/// Parse a review verdict string.
fn parse_verdict(verdict: &str) -> anyhow::Result<ReviewVerdict> {
    Ok(match verdict {
        "approve" => ReviewVerdict::Approve,
        "request_changes" => ReviewVerdict::RequestChanges,
        "comment" => ReviewVerdict::Comment,
        other => anyhow::bail!(
            "submit_review: verdict must be approve|request_changes|comment, got {other:?}"
        ),
    })
}

/// Map a `record_outcome` status to (attempt status, work-item state).
fn parse_outcome(status: &str) -> anyhow::Result<(AttemptStatus, State)> {
    Ok(match status {
        "succeeded" => (AttemptStatus::Succeeded, State::Succeeded),
        "failed" => (AttemptStatus::Failed, State::Failed),
        "blocked" => (AttemptStatus::Blocked, State::Blocked),
        other => {
            anyhow::bail!("record_outcome: status must be succeeded|failed|blocked, got {other:?}")
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

fn validate_escalation_reason(reason: &str) -> anyhow::Result<()> {
    if ESCALATION_REASONS.contains(&reason) {
        Ok(())
    } else {
        anyhow::bail!(
            "escalate: reason must be one of {:?}, got {reason:?}",
            ESCALATION_REASONS
        )
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

fn arg_str<'a>(args: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing or non-string argument {key:?}"))
}

fn arg_opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn arg_i64(args: &Value, key: &str) -> anyhow::Result<i64> {
    args.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("missing or non-integer argument {key:?}"))
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

    // Audit the authorized call before running it: every attempt to use a tool is
    // recorded. The single place no tool call can escape.
    if let Err(e) = record::append_event(
        &state.pool,
        Some(auth.repo_id),
        Some(auth.work_item_id),
        None,
        "tools",
        tool.as_str(),
        json!({ "args": args }),
    )
    .await
    {
        tracing::error!(tool = tool.as_str(), error = %e, "audit write failed");
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "audit error");
    }

    match dispatch(&state, &auth, tool, &args).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => {
            tracing::error!(tool = tool.as_str(), work_item = %auth.work_item_id, error = %e, "tool failed");
            error_response(StatusCode::BAD_REQUEST, &e.to_string())
        }
    }
}

/// Extract the `Authorization: Bearer <token>` value.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
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
