//! Worker harness binary (P3).
//!
//! Wires the live seams — [`GitCli`], [`ClaudeCli`], [`HttpTools`] — to the
//! pure orchestration in [`factory_worker::run_task`]. These implementations are
//! exercised only in live deployment; the harness logic itself is covered by the
//! pure fake-driven tests in `lib.rs`.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use factory_worker::{
    ClaudeRunner, CrRequest, CrResult, HarnessConfig, RunResult, TaskInfo, ToolsClient, Workspace,
};

/// Real git CLI driver. Shallow-clones with the install token embedded in the
/// URL and pushes with `--force-with-lease` for idempotent re-runs.
struct GitCli;

#[async_trait::async_trait]
impl Workspace for GitCli {
    async fn shallow_clone(&self, repo_url: &str, token: &str, dir: &Path) -> anyhow::Result<()> {
        let authed = authenticated_url(repo_url, token)?;
        tracing::info!(dir = %dir.display(), "GitCli: git clone --depth 1");
        let status = tokio::process::Command::new("git")
            .arg("clone")
            .arg("--depth")
            .arg("1")
            .arg(&authed)
            .arg(dir)
            .status()
            .await?;
        if !status.success() {
            anyhow::bail!("git clone failed: {status}");
        }
        Ok(())
    }

    async fn push_branch(&self, dir: &Path, branch: &str) -> anyhow::Result<()> {
        tracing::info!(dir = %dir.display(), branch, "GitCli: git push --force-with-lease");
        let status = tokio::process::Command::new("git")
            .current_dir(dir)
            .arg("push")
            .arg("--force-with-lease")
            .arg("origin")
            .arg(format!("HEAD:{branch}"))
            .status()
            .await?;
        if !status.success() {
            anyhow::bail!("git push failed: {status}");
        }
        Ok(())
    }
}

/// Build an authenticated clone URL by injecting the token as `x-access-token`.
fn authenticated_url(repo_url: &str, token: &str) -> anyhow::Result<String> {
    let rest = repo_url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow::anyhow!("repo_url must be https://: {repo_url}"))?;
    Ok(format!("https://x-access-token:{token}@{rest}"))
}

/// Real Claude Code driver: runs the `claude` binary headless with the Factory
/// Tools service registered as an MCP server.
struct ClaudeCli;

#[async_trait::async_trait]
impl ClaudeRunner for ClaudeCli {
    async fn run(
        &self,
        workdir: &Path,
        prompt: &str,
        tools_url: &str,
        token: &str,
    ) -> anyhow::Result<RunResult> {
        tracing::info!(workdir = %workdir.display(), tools_url, "ClaudeCli: running claude headless");
        // Register the Factory Tools service as an MCP HTTP server so Claude can
        // reach Factory (open CRs, etc.) through the authenticated endpoint.
        let mcp_config = serde_json::json!({
            "mcpServers": {
                "factory": {
                    "type": "http",
                    "url": tools_url,
                    "headers": { "Authorization": format!("Bearer {token}") }
                }
            }
        });
        let output = tokio::process::Command::new("claude")
            .current_dir(workdir)
            .arg("--print")
            .arg("--dangerously-skip-permissions")
            .arg("--mcp-config")
            .arg(mcp_config.to_string())
            .arg(prompt)
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("claude run failed ({}): {stderr}", output.status);
        }
        let summary = String::from_utf8_lossy(&output.stdout).trim().to_string();
        tracing::info!("ClaudeCli: run completed");
        Ok(RunResult { summary })
    }
}

/// Real Tools client over HTTP. Every call carries `Authorization: Bearer
/// <task_token>` and targets a path under the Tools base URL.
struct HttpTools {
    base_url: String,
    task_token: String,
    http: reqwest::Client,
}

impl HttpTools {
    fn new(base_url: String, task_token: String) -> Self {
        Self {
            base_url,
            task_token,
            http: reqwest::Client::new(),
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }

    async fn post_json(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> anyhow::Result<reqwest::Response> {
        let resp = self
            .http
            .post(self.endpoint(path))
            .bearer_auth(&self.task_token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp)
    }
}

#[async_trait::async_trait]
impl ToolsClient for HttpTools {
    async fn get_task(&self) -> anyhow::Result<TaskInfo> {
        tracing::info!("HttpTools: GET task");
        let resp = self
            .http
            .get(self.endpoint("task"))
            .bearer_auth(&self.task_token)
            .send()
            .await?
            .error_for_status()?;
        let v: serde_json::Value = resp.json().await?;
        Ok(TaskInfo {
            role: json_str(&v, "role"),
            subject_ref: json_str(&v, "subject_ref"),
            head_branch: json_str(&v, "head_branch"),
            base: json_str(&v, "base"),
            prompt: json_str(&v, "prompt"),
        })
    }

    async fn open_or_update_change_request(&self, req: &CrRequest) -> anyhow::Result<CrResult> {
        tracing::info!(head_branch = %req.head_branch, "HttpTools: open/update change request");
        let resp = self
            .post_json(
                "change_request",
                serde_json::json!({
                    "title": req.title,
                    "body": req.body,
                    "head_branch": req.head_branch,
                    "base": req.base,
                    "linked_issue": req.linked_issue,
                }),
            )
            .await?;
        let v: serde_json::Value = resp.json().await?;
        Ok(CrResult {
            cr_number: v.get("cr_number").and_then(|n| n.as_i64()).unwrap_or(0),
            url: json_str(&v, "url"),
        })
    }

    async fn append_transcript(&self, chunk: &str) -> anyhow::Result<()> {
        tracing::info!("HttpTools: append transcript");
        self.post_json("transcript", serde_json::json!({ "chunk": chunk }))
            .await?;
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        tracing::info!("HttpTools: heartbeat");
        self.post_json("heartbeat", serde_json::json!({})).await?;
        Ok(())
    }

    async fn record_outcome(&self, status: &str, summary: &str) -> anyhow::Result<()> {
        tracing::info!(status, "HttpTools: record outcome");
        self.post_json(
            "outcome",
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

/// Read a required environment variable.
fn env_var(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|_| anyhow::anyhow!("missing required env var: {key}"))
}

fn load_config() -> anyhow::Result<HarnessConfig> {
    let workdir = match std::env::var("FACTORY_WORKDIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => std::env::temp_dir().join(format!("factory-worker-{}", uuid::Uuid::new_v4())),
    };
    Ok(HarnessConfig {
        repo_url: env_var("FACTORY_REPO_URL")?,
        install_token: env_var("FACTORY_INSTALL_TOKEN")?,
        tools_url: env_var("FACTORY_TOOLS_URL")?,
        task_token: env_var("FACTORY_TASK_TOKEN")?,
        workdir,
        head_branch: env_var("FACTORY_HEAD_BRANCH")?,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("worker");
    tracing::info!("worker: starting harness (P3)");

    let cfg = load_config()?;
    let tools = HttpTools::new(cfg.tools_url.clone(), cfg.task_token.clone());
    let git = GitCli;
    let claude = ClaudeCli;

    factory_worker::run_task(&tools, &git, &claude, &cfg).await?;
    tracing::info!("worker: harness finished");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_url_injects_token() {
        let url = authenticated_url("https://github.com/acme/repo.git", "ghs_tok").unwrap();
        assert_eq!(
            url,
            "https://x-access-token:ghs_tok@github.com/acme/repo.git"
        );
    }

    #[test]
    fn authenticated_url_rejects_non_https() {
        assert!(authenticated_url("git@github.com:acme/repo.git", "tok").is_err());
    }

    #[test]
    fn endpoint_joins_without_double_slash() {
        let t = HttpTools::new("https://tools.test/".into(), "tok".into());
        assert_eq!(t.endpoint("task"), "https://tools.test/task");
    }

    #[test]
    fn json_str_extracts_and_defaults() {
        let v = serde_json::json!({ "role": "engineer" });
        assert_eq!(json_str(&v, "role"), "engineer");
        assert_eq!(json_str(&v, "missing"), "");
    }
}
