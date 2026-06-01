//! Worker harness — the throwaway workroom (P3 + P4.5).
//!
//! The worker has **no direct Postgres/Blob access**; its only channel back to
//! Factory is the Tools service over HTTP. [`run_task`] is **role-aware** (P4.5): it
//! runs Claude Code headless against the Factory MCP/Tools endpoint, then performs
//! the role's terminal action —
//! - **Coder**: short-lived GitHub installation token + fresh shallow clone → edits →
//!   push branch (`--force-with-lease`, so a re-run is idempotent) → open/update the
//!   change request → record success. The only role that produces a CR.
//! - **Manager**: triage → comment + record success (Coordinator assigns a Coder), or
//!   escalate a non-actionable issue. No clone/push/CR.
//! - **Reviewer**: review → `submit_review` verdict → record success. No clone/push/CR.
//! - **`tester`** (and any unknown role): no lifecycle → fails loudly at the run seam.
//!
//! Every external system is abstracted behind a trait ([`ToolsClient`],
//! [`Workspace`], [`ClaudeRunner`]) so the orchestration in [`run_task`] is
//! testable with fakes and never touches the network or a real git binary.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// What the worker needs to know about the task it was leased.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskInfo {
    /// The agent role to run as (one of `manager`, `coder`, `reviewer`, `tester`).
    pub role: String,
    /// Opaque reference to the subject of the task (issue, CR, etc.).
    pub subject_ref: String,
    /// Branch the worker pushes its work to.
    pub head_branch: String,
    /// Base branch the change request targets.
    pub base: String,
    /// The prompt handed to Claude Code.
    pub prompt: String,
}

/// Request to open (or, idempotently, update) a change request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrRequest {
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub base: String,
    pub linked_issue: Option<i64>,
}

/// Result of opening/updating a change request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrResult {
    pub cr_number: i64,
    pub url: String,
}

/// A reviewer's verdict on a change request. Maps to the Tools `submit_review`
/// `verdict` argument (`approve` / `request_changes` / `comment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewVerdict {
    /// The exact wire string the Tools `submit_review` verdict expects.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewVerdict::Approve => "approve",
            ReviewVerdict::RequestChanges => "request_changes",
            ReviewVerdict::Comment => "comment",
        }
    }
}

/// The structured outcome of a Claude Code run, by role. The harness maps each
/// variant to the role's terminal Tools action: only [`RunOutcome::Code`] leads to a
/// change request; [`RunOutcome::Triage`] drives the Manager's comment/escalate and
/// [`RunOutcome::Review`] drives the Reviewer's verdict. `summary` is always the
/// human-readable text recorded to the transcript / used as the comment/CR body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// Coder: edits are staged in the workdir; the harness pushes + opens a CR.
    Code { summary: String },
    /// Manager triage: `actionable` → comment + succeed (Coordinator assigns a coder);
    /// not actionable → escalate (terminal; no coder).
    Triage { actionable: bool, summary: String },
    /// Reviewer verdict on the CR.
    Review {
        verdict: ReviewVerdict,
        summary: String,
    },
}

/// The worker's only channel to Factory: the Tools service over HTTP.
///
/// Reads of the issue/CR are NOT on this trait: in the live flow Claude reads them
/// itself over MCP (`read_issue` / `read_change_request`), and the harness only
/// performs the role's *terminal* action — so adding harness read methods would be
/// dead code. The harness needs the terminal actions for all three roles:
/// `open_or_update_change_request` (Coder), `post_comment` + `escalate` (Manager),
/// `submit_review` (Reviewer), plus the shared `record_outcome` / `heartbeat` /
/// `append_transcript`.
#[async_trait::async_trait]
pub trait ToolsClient: Send + Sync {
    /// Fetch the leased task this worker should execute.
    async fn get_task(&self) -> anyhow::Result<TaskInfo>;
    /// Open the change request, or update it if it already exists (idempotent).
    async fn open_or_update_change_request(&self, req: &CrRequest) -> anyhow::Result<CrResult>;
    /// Comment on an issue/CR (Manager triage note); idempotent by `dedupe_key`.
    async fn post_comment(&self, target: &str, body: &str, dedupe_key: &str) -> anyhow::Result<()>;
    /// Submit a review verdict on a CR (Reviewer); idempotent by `dedupe_key`.
    async fn submit_review(
        &self,
        cr_number: i64,
        verdict: &str,
        body: &str,
        dedupe_key: &str,
    ) -> anyhow::Result<()>;
    /// Raise an escalation to a human (Manager on a non-actionable issue); idempotent
    /// by `dedupe_key`. Terminalizes the work item (the Coordinator parks it).
    async fn escalate(&self, reason: &str, context: &str, dedupe_key: &str) -> anyhow::Result<()>;
    /// Append a chunk to the run transcript (stored in Blob by Tools).
    async fn append_transcript(&self, chunk: &str) -> anyhow::Result<()>;
    /// Signal liveness so the lease is not reclaimed.
    async fn heartbeat(&self) -> anyhow::Result<()>;
    /// Record the terminal outcome of the run (`succeeded` / `failed` / `blocked`).
    async fn record_outcome(&self, status: &str, summary: &str) -> anyhow::Result<()>;
}

/// Git working tree operations against a fresh clone.
#[async_trait::async_trait]
pub trait Workspace: Send + Sync {
    /// Shallow-clone `repo_url` into `dir`, authenticating with `token`.
    async fn shallow_clone(&self, repo_url: &str, token: &str, dir: &Path) -> anyhow::Result<()>;
    /// Push `branch` from the working tree at `dir` (force-with-lease in the real impl).
    async fn push_branch(&self, dir: &Path, branch: &str) -> anyhow::Result<()>;
}

/// Runs Claude Code headless against the Factory MCP/Tools endpoint.
#[async_trait::async_trait]
pub trait ClaudeRunner: Send + Sync {
    /// Run Claude in `workdir` for `role`, with `prompt`, configured to reach the
    /// Tools service at `tools_url` authenticated with `token`. Returns the
    /// role-shaped [`RunOutcome`] (the implementation is contractually expected to
    /// return the variant matching `role`; the harness guards a mismatch).
    async fn run(
        &self,
        workdir: &Path,
        role: &str,
        prompt: &str,
        tools_url: &str,
        token: &str,
    ) -> anyhow::Result<RunOutcome>;
}

/// Everything the harness needs to execute one task, assembled by `main` from env.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    /// Clone URL of the repository to work in.
    pub repo_url: String,
    /// Short-lived GitHub installation token used for clone + push.
    pub install_token: String,
    /// Base URL of the Tools service.
    pub tools_url: String,
    /// Bearer token authenticating this worker to Tools.
    pub task_token: String,
    /// Directory the fresh clone lives in.
    pub workdir: PathBuf,
}

/// Drive one task end-to-end: clone → run Claude → push → open/update CR → record.
///
/// Logs every step. On any error, records a `failed` outcome via Tools (best
/// effort) and returns the original error.
pub async fn run_task(
    tools: &dyn ToolsClient,
    git: &dyn Workspace,
    claude: &dyn ClaudeRunner,
    cfg: &HarnessConfig,
) -> anyhow::Result<()> {
    match run_task_inner(tools, git, claude, cfg).await {
        Ok(()) => Ok(()),
        Err(err) => {
            // A contended idempotency claim ([`ClaimContended`]) means a CONCURRENT
            // attempt owns this work item's terminal action (a lease-expiry re-dispatch
            // overlap). This worker must NOT record `failed` — that would terminalize the
            // item and, because terminal states are final, prevent the owning attempt's
            // success from ever landing. Bow out without terminalizing; the owning attempt
            // completes the item (or the lease sweeper re-dispatches if it died).
            if err.downcast_ref::<ClaimContended>().is_some() {
                tracing::warn!(error = %err, "worker: idempotency claim held by a concurrent attempt; bowing out without terminalizing");
                return Err(err);
            }
            tracing::error!(error = %err, "worker: task failed");
            // Best effort — the original error is what we return regardless.
            if let Err(record_err) = tools.record_outcome("failed", &err.to_string()).await {
                tracing::error!(error = %record_err, "worker: failed to record failure outcome");
            }
            Err(err)
        }
    }
}

/// Error returned by [`HttpTools`] when a Tools call keeps getting 409 Conflict — a
/// concurrent attempt holds the idempotency claim past our retry budget. It is NOT a
/// task failure: [`run_task`] recognizes it and bows out without recording a terminal
/// `failed` outcome, leaving the work item for the owning attempt (or the lease sweeper).
#[derive(Debug)]
pub struct ClaimContended;

impl std::fmt::Display for ClaimContended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tools idempotency claim is held by a concurrent attempt (409 after retries)"
        )
    }
}

impl std::error::Error for ClaimContended {}

async fn run_task_inner(
    tools: &dyn ToolsClient,
    git: &dyn Workspace,
    claude: &dyn ClaudeRunner,
    cfg: &HarnessConfig,
) -> anyhow::Result<()> {
    tracing::info!(repo_url = %cfg.repo_url, "worker: fetching leased task");
    let task = tools.get_task().await?;
    tracing::info!(
        role = %task.role,
        subject_ref = %task.subject_ref,
        head_branch = %task.head_branch,
        base = %task.base,
        "worker: task acquired"
    );
    tools.heartbeat().await?;
    tracing::info!("worker: heartbeat sent");

    // Branch on the role: each role has a different terminal action, and ONLY the
    // Coder produces a change request (see the technical spec's "Worker lifecycle by
    // role"). `tester` is a valid role but has no lifecycle yet, so it — and any
    // unknown role — fails loudly here rather than silently running the coder default.
    match task.role.as_str() {
        "coder" => run_coder(tools, git, claude, cfg, &task).await,
        "manager" => run_manager(tools, claude, cfg, &task).await,
        "reviewer" => run_reviewer(tools, claude, cfg, &task).await,
        other => anyhow::bail!(
            "worker: role {other:?} has no run lifecycle (tester is reserved/unsupported)"
        ),
    }
}

/// Coder lifecycle (P3, unchanged): clone → Claude edits → push branch → open/update
/// the change request → record success. The only role that produces a CR.
async fn run_coder(
    tools: &dyn ToolsClient,
    git: &dyn Workspace,
    claude: &dyn ClaudeRunner,
    cfg: &HarnessConfig,
    task: &TaskInfo,
) -> anyhow::Result<()> {
    tracing::info!(workdir = %cfg.workdir.display(), "worker(coder): shallow cloning repository");
    git.shallow_clone(&cfg.repo_url, &cfg.install_token, &cfg.workdir)
        .await?;
    tracing::info!("worker(coder): clone complete");

    tracing::info!(tools_url = %cfg.tools_url, "worker(coder): running Claude Code headless");
    let outcome = claude
        .run(
            &cfg.workdir,
            "coder",
            &task.prompt,
            &cfg.tools_url,
            &cfg.task_token,
        )
        .await?;
    let RunOutcome::Code { summary } = outcome else {
        anyhow::bail!("worker(coder): Claude returned a non-Code outcome for a coder task");
    };
    tracing::info!(summary = %summary, "worker(coder): Claude run finished");
    tools.append_transcript(&summary).await?;
    tracing::info!("worker(coder): transcript appended");

    // The server (get_task) is the single source of truth for the branch name: push
    // and open the CR on exactly the branch it told us, never an env-supplied one.
    tracing::info!(branch = %task.head_branch, "worker(coder): pushing branch (force-with-lease)");
    git.push_branch(&cfg.workdir, &task.head_branch).await?;
    tracing::info!("worker(coder): branch pushed");

    // Derive the linked issue from the subject: `issue#N` → Some(N), anything else
    // (e.g. `cr#N`) → None. Inline parse, no extra dependency.
    let linked_issue = task
        .subject_ref
        .strip_prefix("issue#")
        .and_then(|n| n.parse::<i64>().ok());
    tracing::info!(
        subject_ref = %task.subject_ref,
        linked_issue = ?linked_issue,
        "worker(coder): derived linked issue from subject"
    );

    let cr_request = CrRequest {
        title: format!("[{}] {}", task.role, task.subject_ref),
        body: summary.clone(),
        head_branch: task.head_branch.clone(),
        base: task.base.clone(),
        linked_issue,
    };
    tracing::info!("worker(coder): opening or updating change request");
    let cr = tools.open_or_update_change_request(&cr_request).await?;
    tracing::info!(cr_number = cr.cr_number, url = %cr.url, "worker(coder): change request ready");

    tools.record_outcome("succeeded", &summary).await?;
    tracing::info!("worker(coder): outcome recorded (succeeded)");
    Ok(())
}

/// Manager lifecycle (P4.5): Claude triages the issue (reading it itself over MCP),
/// then — actionable → post a triage comment + record success (the Coordinator
/// assigns a Coder); not actionable → escalate (terminal; the Coordinator parks it).
/// NEVER clones, pushes, or opens a CR.
async fn run_manager(
    tools: &dyn ToolsClient,
    claude: &dyn ClaudeRunner,
    cfg: &HarnessConfig,
    task: &TaskInfo,
) -> anyhow::Result<()> {
    // Validate the subject BEFORE running Claude: a manager's success only makes the
    // Coordinator assign a Coder for an `issue#<number>` subject
    // (`decide_manager_succeeded` no-ops for any other subject, which would strand a
    // reacted item). Config/intake could enqueue a manager on a non-issue trigger, so
    // fail loudly at the seam — before spending a Claude run or any side effect —
    // rather than succeed into a dead end. Parse as a POSITIVE `i32`: malformed
    // (`issue#not-a-number`), non-positive (`issue#-1`/`issue#0`), AND out-of-i32-range
    // (`issue#2147483648`) all fail here. The i32 bound matches the record/coordinator
    // layer (issue numbers are stored as `int`), so a subject the coordinator would later
    // fail to narrow can't first run Claude + post + record success into a dead end.
    if task
        .subject_ref
        .strip_prefix("issue#")
        .and_then(|n| n.parse::<i32>().ok())
        .filter(|n| *n > 0)
        .is_none()
    {
        anyhow::bail!(
            "worker(manager): task subject {:?} is not an issue#<number>; a manager can only triage an issue",
            task.subject_ref
        );
    }
    tracing::info!(tools_url = %cfg.tools_url, "worker(manager): running Claude Code headless (triage)");
    let outcome = claude
        .run(
            &cfg.workdir,
            "manager",
            &task.prompt,
            &cfg.tools_url,
            &cfg.task_token,
        )
        .await?;
    let RunOutcome::Triage {
        actionable,
        summary,
    } = outcome
    else {
        anyhow::bail!("worker(manager): Claude returned a non-Triage outcome for a manager task");
    };
    tracing::info!(actionable, summary = %summary, "worker(manager): triage finished");
    tools.append_transcript(&summary).await?;
    tracing::info!("worker(manager): transcript appended");

    if actionable {
        // Leave a triage note on the issue, then succeed — a *succeeded* Manager is
        // what makes the Coordinator assign a Coder.
        tools
            .post_comment(&task.subject_ref, &summary, MANAGER_TRIAGE_DEDUPE_KEY)
            .await?;
        tracing::info!("worker(manager): posted triage comment");
        tools.record_outcome("succeeded", &summary).await?;
        tracing::info!(
            "worker(manager): outcome recorded (succeeded → Coordinator assigns a coder)"
        );
    } else {
        // Non-actionable: whether to act on the issue is a human judgment call, so
        // escalate with reason `conflict` — which maps to the TERMINAL `escalated`
        // state (unlike `needs_decision`/`blocked_input`, which are resumable
        // `blocked`). The Coordinator parks a terminal item and assigns no Coder, per
        // the spec's "non-actionable → escalate (terminal; no Coder)". `escalate` is the
        // terminal action, so there is no `record_outcome` (it would overwrite the
        // escalated state). The still-`running` attempt row and the task token are
        // reclaimed by the P5 lease sweeper (escalate stays retryable, so it does not
        // revoke the token itself); the token's bounded TTL caps the interim exposure.
        tools
            .escalate("conflict", &summary, MANAGER_TRIAGE_DEDUPE_KEY)
            .await?;
        tracing::info!(
            "worker(manager): non-actionable issue escalated as conflict (terminal; no coder)"
        );
    }
    Ok(())
}

/// Reviewer lifecycle (P4.5): Claude reviews the CR (reading it itself over MCP),
/// then submits a verdict and records success. NEVER clones, pushes, or opens a CR;
/// the verdict lands on the CR via `submit_review` and the Coordinator reacts to it.
async fn run_reviewer(
    tools: &dyn ToolsClient,
    claude: &dyn ClaudeRunner,
    cfg: &HarnessConfig,
    task: &TaskInfo,
) -> anyhow::Result<()> {
    // Validate the subject BEFORE running Claude: a reviewer can only act on a `cr#N`
    // subject. Resolving it up front means a bad subject (which non-CR triggers could
    // enqueue) fails at the seam without spending a Claude run or writing a transcript.
    // Parse as a POSITIVE `i32` (the Tools/record layer narrows CR numbers to `int`):
    // malformed, non-positive, and out-of-i32-range subjects all fail here, so a subject
    // that `submit_review` would later reject with `invalid cr_number` can't first spend a
    // Claude run and submit a GitHub review. Widened to i64 for the Tools call signature.
    let cr_number = task
        .subject_ref
        .strip_prefix("cr#")
        .and_then(|n| n.parse::<i32>().ok())
        .filter(|n| *n > 0)
        .map(i64::from)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "worker(reviewer): task subject {:?} is not a cr#<number> (positive, in range)",
                task.subject_ref
            )
        })?;
    tracing::info!(tools_url = %cfg.tools_url, "worker(reviewer): running Claude Code headless (review)");
    let outcome = claude
        .run(
            &cfg.workdir,
            "reviewer",
            &task.prompt,
            &cfg.tools_url,
            &cfg.task_token,
        )
        .await?;
    let RunOutcome::Review { verdict, summary } = outcome else {
        anyhow::bail!("worker(reviewer): Claude returned a non-Review outcome for a reviewer task");
    };
    tracing::info!(verdict = verdict.as_str(), summary = %summary, "worker(reviewer): review finished");
    tools.append_transcript(&summary).await?;
    tracing::info!("worker(reviewer): transcript appended");

    tools
        .submit_review(
            cr_number,
            verdict.as_str(),
            &summary,
            REVIEWER_VERDICT_DEDUPE_KEY,
        )
        .await?;
    tracing::info!(
        cr_number,
        verdict = verdict.as_str(),
        "worker(reviewer): verdict submitted"
    );

    tools.record_outcome("succeeded", &summary).await?;
    tracing::info!("worker(reviewer): outcome recorded (succeeded)");
    Ok(())
}

/// Stable dedupe key for the Manager's triage comment / escalation, so a lease-expiry
/// re-dispatch updates the same comment / dedupes the same escalation.
const MANAGER_TRIAGE_DEDUPE_KEY: &str = "manager-triage";
/// Stable dedupe key for the Reviewer's verdict, so a re-dispatch submits one review.
const REVIEWER_VERDICT_DEDUPE_KEY: &str = "reviewer-verdict";

/// Max attempts for a Tools call that returns 409 Conflict (a concurrent attempt
/// holds the idempotency claim). The first try plus retries; after this many 409s we
/// give up and surface the error.
const TOOLS_CONFLICT_MAX_ATTEMPTS: u32 = 6;

/// Bounded exponential backoff before retrying a 409 Conflict: 50ms, 100ms, 200ms, …
/// capped at 1s. `attempt` is 1-based (the attempt that just got a 409).
fn tools_conflict_backoff(attempt: u32) -> std::time::Duration {
    let ms = 50u64.saturating_mul(1u64 << attempt.saturating_sub(1).min(5));
    std::time::Duration::from_millis(ms.min(1000))
}

/// Real Tools client over HTTP — the worker's live seam to the Tools service.
///
/// Every call targets `POST {base}/tools/{tool}` (the only surface the Tools
/// router serves) and carries `Authorization: Bearer <task_token>`. Lives in the
/// library (not just the binary) so an integration test can drive the REAL client
/// against a real `build_router`, proving the two seams actually connect. A 409
/// Conflict (idempotency claim held by a concurrent attempt) is retried with bounded
/// backoff — see [`call_tool`](HttpTools::call_tool).
pub struct HttpTools {
    base_url: String,
    task_token: String,
    http: reqwest::Client,
}

impl HttpTools {
    pub fn new(base_url: String, task_token: String) -> Self {
        Self {
            base_url,
            task_token,
            http: reqwest::Client::new(),
        }
    }

    /// The URL for a tool: every call targets `POST {base}/tools/{tool}`, which is
    /// the only surface the Tools router serves.
    pub fn tool_url(&self, tool: &str) -> String {
        format!("{}/tools/{}", self.base_url.trim_end_matches('/'), tool)
    }

    /// POST a JSON body to a tool and return the parsed response. All tools (even
    /// no-argument ones like get_task/heartbeat) are POSTed with a JSON object.
    async fn call_tool(
        &self,
        tool: &str,
        body: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let url = self.tool_url(tool);
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&self.task_token)
                .json(&body)
                .send()
                .await?;
            // Tools returns 409 Conflict when a CONCURRENT attempt holds the same
            // idempotency claim (a lease-expiry re-dispatch overlapping the original
            // run). That is a transient race the claim is meant to absorb — NOT a task
            // failure. Retry a bounded number of times so the re-dispatched worker waits
            // for the in-flight action to complete/dedupe. Applies to every Tools call
            // (open CR, comment, review, escalate).
            if resp.status() == reqwest::StatusCode::CONFLICT {
                if attempt < TOOLS_CONFLICT_MAX_ATTEMPTS {
                    tracing::info!(
                        tool,
                        attempt,
                        "HttpTools: 409 conflict (idempotency claim in flight); retrying"
                    );
                    tokio::time::sleep(tools_conflict_backoff(attempt)).await;
                    continue;
                }
                // Still contended after the retry budget: surface a typed
                // [`ClaimContended`] so `run_task` bows out WITHOUT terminalizing the
                // item as `failed` — the attempt that owns the claim will complete it,
                // and terminal states are final, so a spurious `failed` here would block
                // that success forever.
                tracing::warn!(tool, attempt, "HttpTools: 409 conflict persisted past retry budget; treating as claim-contended (non-terminal)");
                return Err(ClaimContended.into());
            }
            let resp = resp.error_for_status()?;
            return Ok(resp.json().await?);
        }
    }
}

#[async_trait::async_trait]
impl ToolsClient for HttpTools {
    async fn get_task(&self) -> anyhow::Result<TaskInfo> {
        tracing::info!("HttpTools: get_task");
        let v = self.call_tool("get_task", serde_json::json!({})).await?;
        // Validate the role at the seam: an unknown role fails loudly here rather
        // than letting an invalid worker role propagate into the run.
        let role = validate_role(&json_str(&v, "role"))?;
        Ok(TaskInfo {
            role,
            subject_ref: json_str(&v, "subject_ref"),
            head_branch: json_str(&v, "head_branch"),
            base: json_str(&v, "base"),
            prompt: json_str(&v, "prompt"),
        })
    }

    async fn open_or_update_change_request(&self, req: &CrRequest) -> anyhow::Result<CrResult> {
        tracing::info!(head_branch = %req.head_branch, "HttpTools: open_or_update_change_request");
        let v = self
            .call_tool(
                "open_or_update_change_request",
                serde_json::json!({
                    "title": req.title,
                    "body": req.body,
                    "head_branch": req.head_branch,
                    "base": req.base,
                    "linked_issue": req.linked_issue,
                }),
            )
            .await?;
        Ok(CrResult {
            cr_number: v.get("cr_number").and_then(|n| n.as_i64()).unwrap_or(0),
            url: json_str(&v, "url"),
        })
    }

    async fn post_comment(&self, target: &str, body: &str, dedupe_key: &str) -> anyhow::Result<()> {
        tracing::info!(target, "HttpTools: post_comment");
        self.call_tool(
            "post_comment",
            serde_json::json!({ "target": target, "body": body, "dedupe_key": dedupe_key }),
        )
        .await?;
        Ok(())
    }

    async fn submit_review(
        &self,
        cr_number: i64,
        verdict: &str,
        body: &str,
        dedupe_key: &str,
    ) -> anyhow::Result<()> {
        tracing::info!(cr_number, verdict, "HttpTools: submit_review");
        self.call_tool(
            "submit_review",
            serde_json::json!({
                "cr_number": cr_number,
                "verdict": verdict,
                "body": body,
                "dedupe_key": dedupe_key,
            }),
        )
        .await?;
        Ok(())
    }

    async fn escalate(&self, reason: &str, context: &str, dedupe_key: &str) -> anyhow::Result<()> {
        tracing::info!(reason, "HttpTools: escalate");
        self.call_tool(
            "escalate",
            serde_json::json!({ "reason": reason, "context": context, "dedupe_key": dedupe_key }),
        )
        .await?;
        Ok(())
    }

    async fn append_transcript(&self, chunk: &str) -> anyhow::Result<()> {
        tracing::info!("HttpTools: append_transcript");
        self.call_tool("append_transcript", serde_json::json!({ "chunk": chunk }))
            .await?;
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        tracing::info!("HttpTools: heartbeat");
        self.call_tool("heartbeat", serde_json::json!({})).await?;
        Ok(())
    }

    async fn record_outcome(&self, status: &str, summary: &str) -> anyhow::Result<()> {
        tracing::info!(status, "HttpTools: record_outcome");
        self.call_tool(
            "record_outcome",
            serde_json::json!({ "status": status, "summary": summary }),
        )
        .await?;
        Ok(())
    }
}

/// Extract a string field from a JSON object, defaulting to empty.
fn json_str(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|f| f.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Validate the role string returned by `get_task`. Only the four real agent roles
/// are accepted; anything else is an error so an invalid role fails loudly at the
/// seam rather than running with an unknown role. Returns the role unchanged on
/// success (kept as a `String` for downstream use).
fn validate_role(role: &str) -> anyhow::Result<String> {
    match role {
        "manager" | "coder" | "reviewer" | "tester" => Ok(role.to_string()),
        other => anyhow::bail!("get_task returned an unknown role: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[test]
    fn tool_url_targets_tools_namespace_without_double_slash() {
        let t = HttpTools::new("https://tools.test/".into(), "tok".into());
        assert_eq!(t.tool_url("get_task"), "https://tools.test/tools/get_task");
        let t2 = HttpTools::new("https://tools.test".into(), "tok".into());
        assert_eq!(
            t2.tool_url("record_outcome"),
            "https://tools.test/tools/record_outcome"
        );
    }

    #[test]
    fn tools_conflict_backoff_grows_then_caps() {
        // 1-based attempts: 50, 100, 200, 400, 800, then capped at 1000ms.
        assert_eq!(tools_conflict_backoff(1).as_millis(), 50);
        assert_eq!(tools_conflict_backoff(2).as_millis(), 100);
        assert_eq!(tools_conflict_backoff(3).as_millis(), 200);
        assert_eq!(tools_conflict_backoff(5).as_millis(), 800);
        // Capped (and no overflow at large attempt counts).
        assert_eq!(tools_conflict_backoff(6).as_millis(), 1000);
        assert_eq!(tools_conflict_backoff(100).as_millis(), 1000);
    }

    #[test]
    fn json_str_extracts_and_defaults() {
        let v = serde_json::json!({ "role": "engineer" });
        assert_eq!(json_str(&v, "role"), "engineer");
        assert_eq!(json_str(&v, "missing"), "");
    }

    #[test]
    fn validate_role_accepts_every_real_role() {
        for role in ["manager", "coder", "reviewer", "tester"] {
            assert_eq!(validate_role(role).expect("real role accepted"), role);
        }
    }

    #[test]
    fn validate_role_rejects_unknown_role() {
        let err = validate_role("engineer").expect_err("unknown role must be rejected");
        assert!(
            err.to_string().contains("engineer"),
            "error names the bad role: {err}"
        );
        assert!(validate_role("").is_err(), "empty role is rejected");
    }

    /// Records every call the harness makes. `open_or_update_change_request`
    /// models the real idempotent endpoint: it always returns the same
    /// `cr_number` (CR is keyed by head branch server-side), and increments a
    /// counter so tests can assert the harness called it exactly once per run.
    /// `comments` / `reviews` / `escalations` capture the per-role terminal actions
    /// so tests can assert, e.g., that a manager opened NO CR but posted a comment.
    #[derive(Default)]
    struct FakeTools {
        task: Mutex<TaskInfo>,
        cr_calls: AtomicUsize,
        heartbeats: AtomicUsize,
        transcripts: Mutex<Vec<String>>,
        outcomes: Mutex<Vec<(String, String)>>,
        last_cr_request: Mutex<Option<CrRequest>>,
        comments: Mutex<Vec<(String, String, String)>>, // (target, body, dedupe_key)
        reviews: Mutex<Vec<(i64, String, String, String)>>, // (cr, verdict, body, dedupe_key)
        escalations: Mutex<Vec<(String, String, String)>>, // (reason, context, dedupe_key)
        /// When set, open_or_update_change_request returns ClaimContended (models a
        /// concurrent attempt holding the idempotency claim past the retry budget).
        contend_cr: bool,
        fixed_cr_number: i64,
    }

    impl FakeTools {
        fn seeded() -> Self {
            Self::seeded_role("coder", "issue#42", "factory/issue-42")
        }

        fn seeded_role(role: &str, subject_ref: &str, head_branch: &str) -> Self {
            Self {
                task: Mutex::new(TaskInfo {
                    role: role.into(),
                    subject_ref: subject_ref.into(),
                    head_branch: head_branch.into(),
                    base: "main".into(),
                    prompt: "do the task".into(),
                }),
                fixed_cr_number: 7,
                ..Default::default()
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolsClient for FakeTools {
        async fn get_task(&self) -> anyhow::Result<TaskInfo> {
            Ok(self.task.lock().expect("lock").clone())
        }
        async fn open_or_update_change_request(&self, req: &CrRequest) -> anyhow::Result<CrResult> {
            self.cr_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_cr_request.lock().expect("lock") = Some(req.clone());
            if self.contend_cr {
                return Err(ClaimContended.into());
            }
            // Idempotent: same number regardless of how many times it is called.
            Ok(CrResult {
                cr_number: self.fixed_cr_number,
                url: format!("https://example.test/cr/{}", self.fixed_cr_number),
            })
        }
        async fn post_comment(
            &self,
            target: &str,
            body: &str,
            dedupe_key: &str,
        ) -> anyhow::Result<()> {
            self.comments.lock().expect("lock").push((
                target.into(),
                body.into(),
                dedupe_key.into(),
            ));
            Ok(())
        }
        async fn submit_review(
            &self,
            cr_number: i64,
            verdict: &str,
            body: &str,
            dedupe_key: &str,
        ) -> anyhow::Result<()> {
            self.reviews.lock().expect("lock").push((
                cr_number,
                verdict.into(),
                body.into(),
                dedupe_key.into(),
            ));
            Ok(())
        }
        async fn escalate(
            &self,
            reason: &str,
            context: &str,
            dedupe_key: &str,
        ) -> anyhow::Result<()> {
            self.escalations.lock().expect("lock").push((
                reason.into(),
                context.into(),
                dedupe_key.into(),
            ));
            Ok(())
        }
        async fn append_transcript(&self, chunk: &str) -> anyhow::Result<()> {
            self.transcripts.lock().expect("lock").push(chunk.into());
            Ok(())
        }
        async fn heartbeat(&self) -> anyhow::Result<()> {
            self.heartbeats.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn record_outcome(&self, status: &str, summary: &str) -> anyhow::Result<()> {
            self.outcomes
                .lock()
                .expect("lock")
                .push((status.into(), summary.into()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeGit {
        clones: AtomicUsize,
        pushes: AtomicUsize,
        last_pushed_branch: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl Workspace for FakeGit {
        async fn shallow_clone(
            &self,
            _repo_url: &str,
            _token: &str,
            _dir: &Path,
        ) -> anyhow::Result<()> {
            self.clones.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn push_branch(&self, _dir: &Path, branch: &str) -> anyhow::Result<()> {
            self.pushes.fetch_add(1, Ordering::SeqCst);
            *self.last_pushed_branch.lock().expect("lock") = Some(branch.into());
            Ok(())
        }
    }

    /// Claude double: returns a seeded [`RunOutcome`], or always errors (to drive the
    /// failure path). Records the `role` it was last invoked with so tests can assert
    /// the harness passed the right role.
    struct FakeClaude {
        ran: AtomicUsize,
        outcome: Option<RunOutcome>,
        fail: bool,
        last_role: Mutex<Option<String>>,
    }

    impl FakeClaude {
        fn with(outcome: RunOutcome) -> Self {
            Self {
                ran: AtomicUsize::new(0),
                outcome: Some(outcome),
                fail: false,
                last_role: Mutex::new(None),
            }
        }
        /// Coder convenience: a Code outcome with the given summary.
        fn ok(summary: &str) -> Self {
            Self::with(RunOutcome::Code {
                summary: summary.into(),
            })
        }
        fn failing() -> Self {
            Self {
                ran: AtomicUsize::new(0),
                outcome: None,
                fail: true,
                last_role: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl ClaudeRunner for FakeClaude {
        async fn run(
            &self,
            _workdir: &Path,
            role: &str,
            _prompt: &str,
            _tools_url: &str,
            _token: &str,
        ) -> anyhow::Result<RunOutcome> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            *self.last_role.lock().expect("lock") = Some(role.into());
            if self.fail {
                anyhow::bail!("claude run failed");
            }
            Ok(self.outcome.clone().expect("seeded outcome"))
        }
    }

    fn config() -> HarnessConfig {
        HarnessConfig {
            repo_url: "https://github.com/acme/repo.git".into(),
            install_token: "ghs_installtoken".into(),
            tools_url: "https://tools.factory.test".into(),
            task_token: "task_bearer".into(),
            workdir: PathBuf::from("/tmp/factory-workdir"),
        }
    }

    #[tokio::test]
    async fn run_task_drives_full_sequence() {
        let tools = FakeTools::seeded();
        let git = FakeGit::default();
        let claude = FakeClaude::ok("Implemented the fix");
        let cfg = config();

        run_task(&tools, &git, &claude, &cfg)
            .await
            .expect("run_task should succeed");

        // Clone happened exactly once.
        assert_eq!(git.clones.load(Ordering::SeqCst), 1);
        // Claude ran exactly once.
        assert_eq!(claude.ran.load(Ordering::SeqCst), 1);
        // Push happened exactly once, on the branch from get_task (NOT from cfg/env).
        assert_eq!(git.pushes.load(Ordering::SeqCst), 1);
        assert_eq!(
            *git.last_pushed_branch.lock().expect("lock"),
            Some("factory/issue-42".to_string()),
            "pushes the branch the server (get_task) chose"
        );
        // CR opened/updated exactly once, with the server's branch/base and the
        // linked issue derived from the `issue#42` subject.
        assert_eq!(tools.cr_calls.load(Ordering::SeqCst), 1);
        let cr = tools
            .last_cr_request
            .lock()
            .expect("lock")
            .clone()
            .expect("a CR request was made");
        assert_eq!(
            cr.head_branch, "factory/issue-42",
            "CR opens on task.head_branch"
        );
        assert_eq!(cr.base, "main", "CR targets task.base");
        assert_eq!(
            cr.linked_issue,
            Some(42),
            "linked_issue derived from the issue#42 subject"
        );
        // Heartbeat was sent.
        assert_eq!(tools.heartbeats.load(Ordering::SeqCst), 1);
        // Transcript chunk appended.
        assert_eq!(tools.transcripts.lock().expect("lock").len(), 1);
        // Exactly one outcome, and it is "succeeded".
        let outcomes = tools.outcomes.lock().expect("lock");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, "succeeded");
        assert_eq!(outcomes[0].1, "Implemented the fix");
    }

    #[tokio::test]
    async fn coder_on_a_cr_subject_has_no_linked_issue() {
        // A coder working a `cr#N` subject (the rework loop) opens/updates a CR with
        // NO linked issue — only an `issue#N` subject derives one.
        let tools = FakeTools::seeded_role("coder", "cr#9", "factory/cr-9");
        let git = FakeGit::default();
        let claude = FakeClaude::ok("Reworked the fix");
        let cfg = config();

        run_task(&tools, &git, &claude, &cfg)
            .await
            .expect("run_task should succeed");

        assert_eq!(
            *claude.last_role.lock().expect("lock"),
            Some("coder".into())
        );
        assert_eq!(
            *git.last_pushed_branch.lock().expect("lock"),
            Some("factory/cr-9".to_string())
        );
        let cr = tools
            .last_cr_request
            .lock()
            .expect("lock")
            .clone()
            .expect("a CR request was made");
        assert_eq!(cr.linked_issue, None, "a cr# subject has no linked issue");
    }

    #[tokio::test]
    async fn manager_actionable_posts_comment_and_records_success_with_no_cr() {
        // Manager triage, actionable → post a triage comment + record success (which is
        // what makes the Coordinator assign a Coder). NO clone/push/CR, NO escalation.
        let tools = FakeTools::seeded_role("manager", "issue#7", "factory/issue-7");
        let git = FakeGit::default();
        let claude = FakeClaude::with(RunOutcome::Triage {
            actionable: true,
            summary: "In scope; assigning a coder".into(),
        });
        let cfg = config();

        run_task(&tools, &git, &claude, &cfg)
            .await
            .expect("manager run should succeed");

        assert_eq!(
            *claude.last_role.lock().expect("lock"),
            Some("manager".into())
        );
        assert_eq!(
            git.clones.load(Ordering::SeqCst),
            0,
            "manager does not clone"
        );
        assert_eq!(
            git.pushes.load(Ordering::SeqCst),
            0,
            "manager does not push"
        );
        assert_eq!(
            tools.cr_calls.load(Ordering::SeqCst),
            0,
            "manager opens NO change request"
        );
        let comments = tools.comments.lock().expect("lock");
        assert_eq!(comments.len(), 1, "exactly one triage comment");
        assert_eq!(comments[0].0, "issue#7", "comment targets the issue");
        assert_eq!(comments[0].2, MANAGER_TRIAGE_DEDUPE_KEY);
        let outcomes = tools.outcomes.lock().expect("lock");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, "succeeded");
        assert_eq!(
            tools.escalations.lock().expect("lock").len(),
            0,
            "an actionable issue is not escalated"
        );
    }

    #[tokio::test]
    async fn manager_non_actionable_escalates_with_no_cr_and_no_success() {
        // Manager triage, non-actionable → escalate (terminal). NO CR, NO triage
        // comment, and NO success outcome (escalate is the terminal action).
        let tools = FakeTools::seeded_role("manager", "issue#8", "factory/issue-8");
        let git = FakeGit::default();
        let claude = FakeClaude::with(RunOutcome::Triage {
            actionable: false,
            summary: "Out of scope; a human should decide".into(),
        });
        let cfg = config();

        run_task(&tools, &git, &claude, &cfg)
            .await
            .expect("manager run should succeed");

        assert_eq!(git.clones.load(Ordering::SeqCst), 0);
        assert_eq!(
            tools.cr_calls.load(Ordering::SeqCst),
            0,
            "manager opens NO change request"
        );
        assert_eq!(
            tools.comments.lock().expect("lock").len(),
            0,
            "no triage comment on a non-actionable issue"
        );
        let esc = tools.escalations.lock().expect("lock");
        assert_eq!(esc.len(), 1, "exactly one escalation");
        assert_eq!(
            esc[0].0, "conflict",
            "non-actionable escalates with the terminal `conflict` reason (a human judgment call)"
        );
        assert_eq!(esc[0].2, MANAGER_TRIAGE_DEDUPE_KEY);
        assert_eq!(
            tools.outcomes.lock().expect("lock").len(),
            0,
            "escalate is terminal; no record_outcome that would overwrite the escalated state"
        );
    }

    #[tokio::test]
    async fn reviewer_submits_verdict_with_no_cr() {
        // Reviewer → submit a verdict on the CR + record success. NO clone/push/CR.
        let tools = FakeTools::seeded_role("reviewer", "cr#12", "factory/cr-12");
        let git = FakeGit::default();
        let claude = FakeClaude::with(RunOutcome::Review {
            verdict: ReviewVerdict::RequestChanges,
            summary: "Please add regression tests".into(),
        });
        let cfg = config();

        run_task(&tools, &git, &claude, &cfg)
            .await
            .expect("reviewer run should succeed");

        assert_eq!(
            *claude.last_role.lock().expect("lock"),
            Some("reviewer".into())
        );
        assert_eq!(
            git.clones.load(Ordering::SeqCst),
            0,
            "reviewer does not clone"
        );
        assert_eq!(
            git.pushes.load(Ordering::SeqCst),
            0,
            "reviewer does not push"
        );
        assert_eq!(
            tools.cr_calls.load(Ordering::SeqCst),
            0,
            "reviewer opens NO change request"
        );
        let reviews = tools.reviews.lock().expect("lock");
        assert_eq!(reviews.len(), 1, "exactly one verdict");
        assert_eq!(reviews[0].0, 12, "verdict targets the cr# from the subject");
        assert_eq!(reviews[0].1, "request_changes", "exact verdict wire string");
        assert_eq!(reviews[0].3, REVIEWER_VERDICT_DEDUPE_KEY);
        let outcomes = tools.outcomes.lock().expect("lock");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, "succeeded");
    }

    #[tokio::test]
    async fn tester_and_unknown_roles_fail_loudly_at_the_run_seam() {
        // `tester` is a VALID role (validate_role accepts it) but has no lifecycle, so
        // reaching run_task with it — or with any unknown role — fails loudly rather
        // than running the coder default. No clone/push/CR/review/comment happens.
        for role in ["tester", "engineer"] {
            let tools = FakeTools::seeded_role(role, "issue#1", "factory/issue-1");
            let git = FakeGit::default();
            let claude = FakeClaude::ok("noop");
            let cfg = config();

            let err = run_task(&tools, &git, &claude, &cfg)
                .await
                .expect_err("an unsupported role must fail loudly");
            assert!(
                err.to_string().contains(role),
                "error names the role: {err}"
            );

            assert_eq!(git.clones.load(Ordering::SeqCst), 0, "{role}: no clone");
            assert_eq!(git.pushes.load(Ordering::SeqCst), 0, "{role}: no push");
            assert_eq!(tools.cr_calls.load(Ordering::SeqCst), 0, "{role}: no CR");
            assert_eq!(
                tools.reviews.lock().expect("lock").len(),
                0,
                "{role}: no review"
            );
            assert_eq!(
                tools.comments.lock().expect("lock").len(),
                0,
                "{role}: no comment"
            );
            // The run_task error wrapper records a best-effort "failed" outcome.
            let outcomes = tools.outcomes.lock().expect("lock");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].0, "failed");
        }
    }

    #[tokio::test]
    async fn role_outcome_mismatch_fails_loudly_and_opens_no_cr() {
        // A runner that returns the WRONG outcome variant for the role (a broken
        // ClaudeRunner contract) must bail loudly, not silently mishandle it. Here a
        // coder task gets a Triage outcome → run_coder bails; no CR, "failed" recorded.
        let tools = FakeTools::seeded_role("coder", "issue#42", "factory/issue-42");
        let git = FakeGit::default();
        let claude = FakeClaude::with(RunOutcome::Triage {
            actionable: true,
            summary: "wrong variant".into(),
        });
        let cfg = config();

        let err = run_task(&tools, &git, &claude, &cfg)
            .await
            .expect_err("a mismatched outcome variant must fail loudly");
        assert!(
            err.to_string().contains("non-Code"),
            "error explains the mismatch: {err}"
        );
        assert_eq!(
            tools.cr_calls.load(Ordering::SeqCst),
            0,
            "no CR on mismatch"
        );
        let outcomes = tools.outcomes.lock().expect("lock");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, "failed");
    }

    #[tokio::test]
    async fn reviewer_with_non_cr_subject_fails_loudly() {
        // A reviewer subject must be a POSITIVE `cr#<number>`. A non-cr subject — or a
        // non-positive `cr#-1`/`cr#0` — bails loudly BEFORE running Claude (validation is
        // up front), so it costs no Claude run, writes no transcript, submits no verdict.
        for bad_subject in [
            "issue#5",
            "cr#-1",
            "cr#0",
            "cr#not-a-number",
            "cr#2147483648",
        ] {
            let tools = FakeTools::seeded_role("reviewer", bad_subject, "factory/x");
            let git = FakeGit::default();
            let claude = FakeClaude::with(RunOutcome::Review {
                verdict: ReviewVerdict::Approve,
                summary: "lgtm".into(),
            });
            let cfg = config();

            let err = run_task(&tools, &git, &claude, &cfg)
                .await
                .expect_err("a reviewer with a bad subject must fail loudly");
            assert!(
                err.to_string().contains("cr#"),
                "{bad_subject}: error explains the bad subject: {err}"
            );
            assert_eq!(
                claude.ran.load(Ordering::SeqCst),
                0,
                "{bad_subject}: Claude not run (validation up front)"
            );
            assert_eq!(
                tools.transcripts.lock().expect("lock").len(),
                0,
                "{bad_subject}: no transcript"
            );
            assert_eq!(
                tools.reviews.lock().expect("lock").len(),
                0,
                "{bad_subject}: no verdict"
            );
            let outcomes = tools.outcomes.lock().expect("lock");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].0, "failed");
        }
    }

    #[tokio::test]
    async fn manager_with_non_issue_subject_fails_loudly_before_running_claude() {
        // A manager's success only delegates a coder for an `issue#<number>` subject (the
        // coordinator no-ops otherwise, stranding a reacted item). A non-issue subject —
        // including a malformed `issue#not-a-number` — must fail at the seam BEFORE Claude
        // runs: no comment, no escalate, no success.
        for bad_subject in [
            "cr#9",
            "issue#not-a-number",
            "schedule@nightly",
            "issue#-1",
            "issue#0",
            "issue#2147483648", // > i32::MAX
        ] {
            let tools = FakeTools::seeded_role("manager", bad_subject, "factory/x");
            let git = FakeGit::default();
            let claude = FakeClaude::with(RunOutcome::Triage {
                actionable: true,
                summary: "should never run".into(),
            });
            let cfg = config();

            let err = run_task(&tools, &git, &claude, &cfg)
                .await
                .expect_err("a manager with a non-issue subject must fail loudly");
            assert!(
                err.to_string().contains("issue#"),
                "{bad_subject}: error explains the bad subject: {err}"
            );
            // Claude never ran (validation is before the run); no side effects.
            assert_eq!(
                claude.ran.load(Ordering::SeqCst),
                0,
                "{bad_subject}: Claude not run"
            );
            assert_eq!(
                tools.comments.lock().expect("lock").len(),
                0,
                "{bad_subject}: no comment"
            );
            assert_eq!(
                tools.escalations.lock().expect("lock").len(),
                0,
                "{bad_subject}: no escalation"
            );
            let outcomes = tools.outcomes.lock().expect("lock");
            assert_eq!(outcomes.len(), 1);
            assert_eq!(outcomes[0].0, "failed");
        }
    }

    #[tokio::test]
    async fn claim_contended_bows_out_without_recording_failed() {
        // A contended idempotency claim (another attempt owns the terminal action) must
        // NOT terminalize the work item as `failed` — run_task returns the error but
        // records NO outcome, so the owning attempt's eventual success can still land.
        let tools = FakeTools {
            contend_cr: true,
            ..FakeTools::seeded()
        };
        let git = FakeGit::default();
        let claude = FakeClaude::ok("Implemented");
        let cfg = config();

        let err = run_task(&tools, &git, &claude, &cfg)
            .await
            .expect_err("a contended claim surfaces an error");
        assert!(
            err.downcast_ref::<ClaimContended>().is_some(),
            "the error is ClaimContended: {err}"
        );
        assert_eq!(
            tools.outcomes.lock().expect("lock").len(),
            0,
            "must NOT record a (failed) outcome — the owning attempt completes the item"
        );
    }

    #[tokio::test]
    async fn rerun_is_idempotent_same_cr() {
        let tools = FakeTools::seeded();
        let git = FakeGit::default();
        let claude = FakeClaude::ok("Implemented the fix");
        let cfg = config();

        // First run.
        let first = tools.fixed_cr_number;
        run_task(&tools, &git, &claude, &cfg)
            .await
            .expect("first run should succeed");
        // Second run with the SAME FakeTools (models the idempotent endpoint).
        run_task(&tools, &git, &claude, &cfg)
            .await
            .expect("second run should succeed");

        // open_or_update was called once per run (2 total), but the server-side
        // CR is the same one: the returned number never changes, so no second CR
        // is created.
        assert_eq!(tools.cr_calls.load(Ordering::SeqCst), 2);
        let second = tools.fixed_cr_number;
        assert_eq!(first, second, "re-run must yield the same cr_number");

        // Both runs succeeded.
        let outcomes = tools.outcomes.lock().expect("lock");
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|(status, _)| status == "succeeded"));
    }

    #[tokio::test]
    async fn failure_records_failed_outcome_and_returns_err() {
        let tools = FakeTools::seeded();
        let git = FakeGit::default();
        let claude = FakeClaude::failing();
        let cfg = config();

        let result = run_task(&tools, &git, &claude, &cfg).await;
        assert!(result.is_err(), "run_task must propagate the error");

        // No CR was opened because Claude failed before push/CR.
        assert_eq!(tools.cr_calls.load(Ordering::SeqCst), 0);
        // A "failed" outcome was recorded.
        let outcomes = tools.outcomes.lock().expect("lock");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, "failed");
        assert!(
            outcomes[0].1.contains("claude run failed"),
            "failure summary should carry the error: {}",
            outcomes[0].1
        );
    }
}
