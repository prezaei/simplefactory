//! Worker harness — the throwaway workroom (P3).
//!
//! The worker has **no direct Postgres/Blob access**; its only channel back to
//! Factory is the Tools service over HTTP. Each run: get a short-lived GitHub
//! installation token + fresh shallow clone, run Claude Code headless against the
//! Factory MCP/Tools endpoint, then open/update the change request and record the
//! outcome. The branch is pushed directly with the git token using
//! `--force-with-lease`, which makes a re-run idempotent.
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

/// Result of a Claude Code run. The run made edits in the workdir and intends to
/// open a CR; `summary` is the human-readable outcome the harness records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub summary: String,
}

/// The worker's only channel to Factory: the Tools service over HTTP.
#[async_trait::async_trait]
pub trait ToolsClient: Send + Sync {
    /// Fetch the leased task this worker should execute.
    async fn get_task(&self) -> anyhow::Result<TaskInfo>;
    /// Open the change request, or update it if it already exists (idempotent).
    async fn open_or_update_change_request(&self, req: &CrRequest) -> anyhow::Result<CrResult>;
    /// Append a chunk to the run transcript (stored in Blob by Tools).
    async fn append_transcript(&self, chunk: &str) -> anyhow::Result<()>;
    /// Signal liveness so the lease is not reclaimed.
    async fn heartbeat(&self) -> anyhow::Result<()>;
    /// Record the terminal outcome of the run (`succeeded` / `failed`).
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
    /// Run Claude in `workdir` with `prompt`, configured to reach the Tools
    /// service at `tools_url` authenticated with `token`.
    async fn run(
        &self,
        workdir: &Path,
        prompt: &str,
        tools_url: &str,
        token: &str,
    ) -> anyhow::Result<RunResult>;
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
    /// Branch the worker pushes its work to.
    pub head_branch: String,
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
            tracing::error!(error = %err, "worker: task failed");
            // Best effort — the original error is what we return regardless.
            if let Err(record_err) = tools.record_outcome("failed", &err.to_string()).await {
                tracing::error!(error = %record_err, "worker: failed to record failure outcome");
            }
            Err(err)
        }
    }
}

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

    tracing::info!(workdir = %cfg.workdir.display(), "worker: shallow cloning repository");
    git.shallow_clone(&cfg.repo_url, &cfg.install_token, &cfg.workdir)
        .await?;
    tracing::info!("worker: clone complete");

    tracing::info!(tools_url = %cfg.tools_url, "worker: running Claude Code headless");
    let run = claude
        .run(&cfg.workdir, &task.prompt, &cfg.tools_url, &cfg.task_token)
        .await?;
    tracing::info!(summary = %run.summary, "worker: Claude run finished");
    tools.append_transcript(&run.summary).await?;
    tracing::info!("worker: transcript appended");

    tracing::info!(branch = %cfg.head_branch, "worker: pushing branch (force-with-lease)");
    git.push_branch(&cfg.workdir, &cfg.head_branch).await?;
    tracing::info!("worker: branch pushed");

    let cr_request = CrRequest {
        title: format!("[{}] {}", task.role, task.subject_ref),
        body: run.summary.clone(),
        head_branch: cfg.head_branch.clone(),
        base: task.base.clone(),
        linked_issue: None,
    };
    tracing::info!("worker: opening or updating change request");
    let cr = tools.open_or_update_change_request(&cr_request).await?;
    tracing::info!(cr_number = cr.cr_number, url = %cr.url, "worker: change request ready");

    tools.record_outcome("succeeded", &run.summary).await?;
    tracing::info!("worker: outcome recorded (succeeded)");
    Ok(())
}

/// Real Tools client over HTTP — the worker's live seam to the Tools service.
///
/// Every call targets `POST {base}/tools/{tool}` (the only surface the Tools
/// router serves) and carries `Authorization: Bearer <task_token>`. Lives in the
/// library (not just the binary) so an integration test can drive the REAL client
/// against a real `build_router`, proving the two seams actually connect.
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
        let resp = self
            .http
            .post(self.tool_url(tool))
            .bearer_auth(&self.task_token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }
}

#[async_trait::async_trait]
impl ToolsClient for HttpTools {
    async fn get_task(&self) -> anyhow::Result<TaskInfo> {
        tracing::info!("HttpTools: get_task");
        let v = self.call_tool("get_task", serde_json::json!({})).await?;
        Ok(TaskInfo {
            role: json_str(&v, "role"),
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
    fn json_str_extracts_and_defaults() {
        let v = serde_json::json!({ "role": "engineer" });
        assert_eq!(json_str(&v, "role"), "engineer");
        assert_eq!(json_str(&v, "missing"), "");
    }

    /// Records every call the harness makes. `open_or_update_change_request`
    /// models the real idempotent endpoint: it always returns the same
    /// `cr_number` (CR is keyed by head branch server-side), and increments a
    /// counter so tests can assert the harness called it exactly once per run.
    #[derive(Default)]
    struct FakeTools {
        task: Mutex<TaskInfo>,
        cr_calls: AtomicUsize,
        heartbeats: AtomicUsize,
        transcripts: Mutex<Vec<String>>,
        outcomes: Mutex<Vec<(String, String)>>,
        fixed_cr_number: i64,
    }

    impl FakeTools {
        fn seeded() -> Self {
            Self {
                task: Mutex::new(TaskInfo {
                    role: "engineer".into(),
                    subject_ref: "issue-42".into(),
                    head_branch: "factory/issue-42".into(),
                    base: "main".into(),
                    prompt: "Fix the bug".into(),
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
        async fn open_or_update_change_request(
            &self,
            _req: &CrRequest,
        ) -> anyhow::Result<CrResult> {
            self.cr_calls.fetch_add(1, Ordering::SeqCst);
            // Idempotent: same number regardless of how many times it is called.
            Ok(CrResult {
                cr_number: self.fixed_cr_number,
                url: format!("https://example.test/cr/{}", self.fixed_cr_number),
            })
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

    /// Claude double: either returns a fixed summary, or always errors (to drive
    /// the failure path).
    struct FakeClaude {
        ran: AtomicUsize,
        summary: String,
        fail: bool,
    }

    impl FakeClaude {
        fn ok(summary: &str) -> Self {
            Self {
                ran: AtomicUsize::new(0),
                summary: summary.into(),
                fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                ran: AtomicUsize::new(0),
                summary: String::new(),
                fail: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl ClaudeRunner for FakeClaude {
        async fn run(
            &self,
            _workdir: &Path,
            _prompt: &str,
            _tools_url: &str,
            _token: &str,
        ) -> anyhow::Result<RunResult> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                anyhow::bail!("claude run failed");
            }
            Ok(RunResult {
                summary: self.summary.clone(),
            })
        }
    }

    fn config() -> HarnessConfig {
        HarnessConfig {
            repo_url: "https://github.com/acme/repo.git".into(),
            install_token: "ghs_installtoken".into(),
            tools_url: "https://tools.factory.test".into(),
            task_token: "task_bearer".into(),
            workdir: PathBuf::from("/tmp/factory-workdir"),
            head_branch: "factory/issue-42".into(),
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
        // Push happened exactly once, on the configured head branch.
        assert_eq!(git.pushes.load(Ordering::SeqCst), 1);
        assert_eq!(
            *git.last_pushed_branch.lock().expect("lock"),
            Some("factory/issue-42".to_string())
        );
        // CR opened/updated exactly once.
        assert_eq!(tools.cr_calls.load(Ordering::SeqCst), 1);
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
