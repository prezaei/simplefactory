//! Worker harness binary (P3).
//!
//! Wires the live seams — [`GitCli`], [`ClaudeCli`], [`HttpTools`] — to the
//! pure orchestration in [`factory_worker::run_task`]. These implementations are
//! exercised only in live deployment; the harness logic itself is covered by the
//! pure fake-driven tests in `lib.rs`.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use factory_worker::{ClaudeRunner, HarnessConfig, HttpTools, RunResult, Workspace};

/// Real git CLI driver. Shallow-clones with the install token embedded in the
/// URL and pushes with `--force-with-lease` for idempotent re-runs.
struct GitCli;

#[async_trait::async_trait]
impl Workspace for GitCli {
    async fn shallow_clone(&self, repo_url: &str, token: &str, dir: &Path) -> anyhow::Result<()> {
        let authed = authed_url(repo_url, token)?;
        // Log the host but never the authenticated URL (it carries the token).
        tracing::info!(dir = %dir.display(), "GitCli: git clone --depth 1");
        let output = tokio::process::Command::new("git")
            .arg("clone")
            .arg("--depth")
            .arg("1")
            .arg(&authed)
            .arg(dir)
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(status = ?output.status.code(), stderr = %stderr, "git clone failed");
            anyhow::bail!("git clone failed ({}): {stderr}", output.status);
        }
        tracing::info!("GitCli: clone complete");
        Ok(())
    }

    async fn push_branch(&self, dir: &Path, branch: &str) -> anyhow::Result<()> {
        tracing::info!(dir = %dir.display(), branch, "GitCli: git push --force-with-lease");
        // `--force-with-lease` keeps re-runs idempotent: push the current HEAD to
        // the task branch even if a prior attempt already created it.
        let output = tokio::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("push")
            .arg("--force-with-lease")
            .arg("origin")
            .arg(push_refspec(branch))
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(branch, status = ?output.status.code(), stderr = %stderr, "git push failed");
            anyhow::bail!("git push failed ({}): {stderr}", output.status);
        }
        tracing::info!(branch, "GitCli: push complete");
        Ok(())
    }
}

/// Build an authenticated clone URL by injecting the install token as the
/// `x-access-token` basic-auth user, the form GitHub accepts for installation
/// tokens: `https://x-access-token:<token>@<host>/<owner>/<repo>.git`.
///
/// Only `https://` URLs are accepted; `http://` and `ssh`/`git@` forms are
/// rejected so a token is never sent over a plaintext or non-matching transport.
/// The returned string contains the token and MUST NOT be logged.
fn authed_url(repo_url: &str, token: &str) -> anyhow::Result<String> {
    let rest = repo_url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow::anyhow!("repo_url must be an https:// URL"))?;
    if rest.is_empty() {
        anyhow::bail!("repo_url has no host");
    }
    Ok(format!("https://x-access-token:{token}@{rest}"))
}

/// The refspec that pushes the current HEAD to the named remote branch.
fn push_refspec(branch: &str) -> String {
    format!("HEAD:{branch}")
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
        // Register the Factory Tools service as a Streamable-HTTP MCP server so
        // Claude reaches Factory (open CRs, etc.) through the authenticated
        // endpoint. The config and args carry the token but are never logged.
        let mcp_config = mcp_config_json(tools_url, token);
        let args = claude_args(&mcp_config, prompt);
        let output = tokio::process::Command::new("claude")
            .current_dir(workdir)
            .args(&args)
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(status = ?output.status.code(), stderr = %stderr, "claude run failed");
            anyhow::bail!("claude run failed ({}): {stderr}", output.status);
        }
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let summary = if stdout.is_empty() {
            "claude run completed with no output".to_string()
        } else {
            stdout
        };
        tracing::info!("ClaudeCli: run completed");
        Ok(RunResult { summary })
    }
}

/// Build the MCP server config JSON that registers the Factory Tools service as a
/// Streamable-HTTP MCP server. Claude POSTs to `{tools_url}/mcp` (the Tools
/// service's MCP endpoint) with `Authorization: Bearer <token>`.
///
/// The returned string contains the token and MUST NOT be logged.
fn mcp_config_json(tools_url: &str, token: &str) -> String {
    let mcp_url = format!("{}/mcp", tools_url.trim_end_matches('/'));
    let config = serde_json::json!({
        "mcpServers": {
            "factory": {
                "type": "http",
                "url": mcp_url,
                "headers": { "Authorization": format!("Bearer {token}") }
            }
        }
    });
    config.to_string()
}

/// The argv passed to the `claude` binary for a headless run: print mode, skip
/// interactive permission prompts, attach the MCP config, then the prompt.
fn claude_args(mcp_config: &str, prompt: &str) -> Vec<String> {
    vec![
        "--print".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--mcp-config".to_string(),
        mcp_config.to_string(),
        prompt.to_string(),
    ]
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

    const TOKEN: &str = "ghs_secrettoken";

    #[test]
    fn authed_url_injects_token_into_https() {
        let url = authed_url("https://github.com/acme/repo.git", TOKEN).unwrap();
        assert_eq!(
            url,
            "https://x-access-token:ghs_secrettoken@github.com/acme/repo.git"
        );
    }

    #[test]
    fn authed_url_rejects_http() {
        let err = authed_url("http://github.com/acme/repo.git", TOKEN)
            .expect_err("http must be rejected");
        // The error must not leak the token.
        assert!(!err.to_string().contains(TOKEN));
    }

    #[test]
    fn authed_url_rejects_ssh() {
        assert!(authed_url("git@github.com:acme/repo.git", TOKEN).is_err());
        assert!(authed_url("ssh://git@github.com/acme/repo.git", TOKEN).is_err());
    }

    #[test]
    fn authed_url_rejects_empty_host() {
        assert!(authed_url("https://", TOKEN).is_err());
    }

    #[test]
    fn push_refspec_targets_head() {
        assert_eq!(push_refspec("factory/issue-42"), "HEAD:factory/issue-42");
    }

    #[test]
    fn mcp_config_uses_mcp_endpoint_and_bearer_header() {
        let raw = mcp_config_json("https://tools.factory.test", TOKEN);
        let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        let server = &v["mcpServers"]["factory"];
        assert_eq!(server["type"], "http");
        // The MCP URL MUST be {tools_url}/mcp.
        assert_eq!(server["url"], "https://tools.factory.test/mcp");
        assert_eq!(
            server["headers"]["Authorization"],
            format!("Bearer {TOKEN}")
        );
    }

    #[test]
    fn mcp_config_trims_trailing_slash_before_mcp() {
        let raw = mcp_config_json("https://tools.factory.test/", TOKEN);
        let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(
            v["mcpServers"]["factory"]["url"],
            "https://tools.factory.test/mcp"
        );
    }

    #[test]
    fn mcp_config_carries_token_only_in_authorization_header() {
        let raw = mcp_config_json("https://tools.factory.test", TOKEN);
        // The token appears exactly once, inside the Authorization header value.
        assert_eq!(raw.matches(TOKEN).count(), 1);
        assert!(raw.contains(&format!("Bearer {TOKEN}")));
    }

    #[test]
    fn claude_args_are_headless_with_config_and_prompt() {
        let config = mcp_config_json("https://tools.factory.test", TOKEN);
        let args = claude_args(&config, "Fix the bug");
        assert_eq!(args[0], "--print");
        assert_eq!(args[1], "--dangerously-skip-permissions");
        assert_eq!(args[2], "--mcp-config");
        assert_eq!(args[3], config);
        // The prompt is the final positional argument.
        assert_eq!(args.last().unwrap(), "Fix the bug");
    }

    #[test]
    fn claude_args_keep_token_only_inside_the_config_arg() {
        let config = mcp_config_json("https://tools.factory.test", TOKEN);
        let args = claude_args(&config, "Fix the bug");
        // Only the --mcp-config payload (args[3]) carries the token.
        for (i, a) in args.iter().enumerate() {
            if i == 3 {
                assert!(a.contains(TOKEN));
            } else {
                assert!(!a.contains(TOKEN), "arg {i} leaked the token: {a}");
            }
        }
    }
}
