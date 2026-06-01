//! Worker harness binary (P3).
//!
//! Wires the live seams — [`GitCli`], [`ClaudeCli`], [`HttpTools`] — to the
//! pure orchestration in [`factory_worker::run_task`]. These implementations are
//! exercised only in live deployment; the harness logic itself is covered by the
//! pure fake-driven tests in `lib.rs`.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use factory_worker::{
    ClaudeRunner, HarnessConfig, HttpTools, ResumeInfo, ReviewVerdict, RunOutcome, Workspace,
};

/// Real git CLI driver. Shallow-clones the PLAIN repo URL and pushes with
/// `--force-with-lease` for idempotent re-runs. The install token is NEVER placed
/// in the URL (argv) nor persisted in the clone's `.git/config`; instead it is
/// injected for the duration of each command as an `http.extraheader`
/// `Authorization: Basic` header delivered through environment variables.
///
/// The token is held on the struct (set once from the harness config) rather than
/// persisted in the clone's `.git/config`, so `push_branch` — whose trait signature
/// carries no token — can re-inject the same env credential without the plain
/// `origin` ever storing it on disk.
struct GitCli {
    install_token: String,
}

#[async_trait::async_trait]
impl Workspace for GitCli {
    async fn shallow_clone(&self, repo_url: &str, token: &str, dir: &Path) -> anyhow::Result<()> {
        // Validate the URL transport and the token shape before doing anything.
        // The PLAIN url (no token) is what becomes `origin` in `.git/config`.
        let url = plain_repo_url(repo_url, token)?;
        // Log the host but never the credential env (it carries the token).
        tracing::info!(dir = %dir.display(), "GitCli: git clone --depth 1");
        let mut cmd = tokio::process::Command::new("git");
        cmd.arg("clone").arg("--depth").arg("1").arg(&url).arg(dir);
        apply_git_credential_env(&mut cmd, token);
        let output = cmd.output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(status = ?output.status.code(), stderr = %stderr, "git clone failed");
            anyhow::bail!("git clone failed ({}): {stderr}", output.status);
        }
        tracing::info!("GitCli: clone complete");
        Ok(())
    }

    async fn push_branch(&self, dir: &Path, branch: &str) -> anyhow::Result<()> {
        // The token must be a valid credential even though it is no longer placed
        // in a URL: reject a malformed token rather than emit a broken header.
        if !is_url_safe_token(&self.install_token) {
            // Do NOT include the token in the error: it must never be logged.
            anyhow::bail!("install token contains characters that are not URL-safe userinfo");
        }
        tracing::info!(dir = %dir.display(), branch, "GitCli: git push --force-with-lease");
        // `--force-with-lease` keeps re-runs idempotent: push the current HEAD to
        // the task branch even if a prior attempt already created it. `origin` is
        // the PLAIN url; the credential rides the env-delivered extraheader.
        let mut cmd = tokio::process::Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .arg("push")
            .arg("--force-with-lease")
            .arg("origin")
            .arg(push_refspec(branch));
        apply_git_credential_env(&mut cmd, &self.install_token);
        let output = cmd.output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(branch, status = ?output.status.code(), stderr = %stderr, "git push failed");
            anyhow::bail!("git push failed ({}): {stderr}", output.status);
        }
        tracing::info!(branch, "GitCli: push complete");
        Ok(())
    }
}

/// Validate `repo_url` and `token`, returning the PLAIN clone URL (no token in it).
///
/// Only `https://` URLs are accepted; `http://` and `ssh`/`git@` forms are
/// rejected so a token is never sent over a plaintext or non-matching transport.
///
/// Precondition: `token` must contain only URL-safe characters (GitHub
/// installation tokens are `[A-Za-z0-9_]`). A malformed token is rejected here so a
/// broken `Authorization` header is never constructed. The token is NOT placed in
/// the returned URL — the URL is plain, so `.git/config`'s `origin` never persists
/// the token on disk and the token never appears in git's argv.
fn plain_repo_url(repo_url: &str, token: &str) -> anyhow::Result<String> {
    let rest = repo_url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow::anyhow!("repo_url must be an https:// URL"))?;
    if rest.is_empty() {
        anyhow::bail!("repo_url has no host");
    }
    if !is_url_safe_token(token) {
        // Do NOT include the token in the error: it must never be logged.
        anyhow::bail!("install token contains characters that are not URL-safe userinfo");
    }
    Ok(repo_url.to_string())
}

/// The git config env vars that inject an `http.extraheader` `Authorization: Basic`
/// credential for the duration of a single git command.
///
/// Returns `[(GIT_CONFIG_COUNT, "1"), (GIT_CONFIG_KEY_0, "http.extraheader"),
/// (GIT_CONFIG_VALUE_0, "Authorization: Basic <base64(x-access-token:token)>")]`.
///
/// The token rides ONLY inside the (base64-encoded) env value — never in argv (so
/// `ps`/`/proc/<pid>/cmdline` cannot see it) and never written to `.git/config` (so
/// the on-disk clone cannot leak it). The returned values carry the credential and
/// MUST NOT be logged.
fn git_credential_env(token: &str) -> Vec<(String, String)> {
    let basic = BASE64_STANDARD.encode(format!("x-access-token:{token}"));
    vec![
        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
        (
            "GIT_CONFIG_KEY_0".to_string(),
            "http.extraheader".to_string(),
        ),
        (
            "GIT_CONFIG_VALUE_0".to_string(),
            format!("Authorization: Basic {basic}"),
        ),
    ]
}

/// Apply [`git_credential_env`] to a [`tokio::process::Command`] so the credential
/// is delivered via the environment (not argv, not persisted) for that one command.
fn apply_git_credential_env(cmd: &mut tokio::process::Command, token: &str) {
    for (k, v) in git_credential_env(token) {
        cmd.env(k, v);
    }
}

/// Whether `token` is a valid credential value. GitHub installation tokens are
/// `[A-Za-z0-9_]`; anything else (empty, or containing `@`/`:`/`/`/etc.) is rejected
/// so a malformed credential is never used.
fn is_url_safe_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
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
        role: &str,
        prompt: &str,
        tools_url: &str,
        token: &str,
        session_id: &str,
        resume: bool,
    ) -> anyhow::Result<RunOutcome> {
        tracing::info!(workdir = %workdir.display(), role, tools_url, session_id, resume, "ClaudeCli: running claude headless");
        // Ensure the working directory exists before we `current_dir(workdir)` the
        // `claude` process. The Coder path creates it via `git clone`, but the Manager
        // and Reviewer roles skip cloning (no repo needed — they read the issue/CR over
        // MCP), so for them nothing has created `workdir` yet and the spawn would fail
        // with "No such file or directory". `create_dir_all` is idempotent, so it is a
        // no-op for the already-cloned Coder tree.
        tokio::fs::create_dir_all(workdir).await?;
        // Register the Factory Tools service as a Streamable-HTTP MCP server so
        // Claude reaches Factory (open CRs, etc.) through the authenticated
        // endpoint. The config carries the token (an Authorization header), so it is
        // written to a 0600 file and passed BY PATH — never on the command line,
        // where `ps`/`/proc/<pid>/cmdline` would expose it.
        //
        // The config file lives in a scratch directory *outside* `workdir` (the
        // cloned repo's working tree), NOT inside it. Claude runs headless with
        // `--dangerously-skip-permissions` and can run arbitrary git commands; if
        // the config were inside the tree, a `git add -A`/`git add .` + commit
        // would capture the token and the subsequent `push_branch` would publish it
        // to the remote. Keeping it outside the tree means git never sees it.
        let scratch_dir = mcp_scratch_dir();
        tokio::fs::create_dir_all(&scratch_dir).await?;

        // Run the token-bearing config write + claude invocation inside a scope that
        // ALWAYS cleans up the scratch dir afterward — including the config-WRITE
        // failure path. If cleanup ran only after a successful write+run, a write that
        // failed (e.g. a planted file at the path) would leave the scratch dir (and any
        // partial token file) on disk. cleanup_scratch runs on every exit path here.
        let result = run_claude_with_config(
            &scratch_dir,
            workdir,
            role,
            prompt,
            tools_url,
            token,
            session_id,
            resume,
        )
        .await;
        cleanup_scratch(&scratch_dir).await;
        result
    }
}

/// Write the MCP config into `scratch_dir` and run `claude` against it, returning the
/// parsed [`RunOutcome`]. Split out from [`ClaudeRunner::run`] so the scratch-dir
/// cleanup in the caller wraps EVERY exit path of this function — both the
/// config-write failure and the claude-run failure — guaranteeing the token-bearing
/// config never lingers on disk regardless of where this fails.
#[allow(clippy::too_many_arguments)]
async fn run_claude_with_config(
    scratch_dir: &Path,
    workdir: &Path,
    role: &str,
    prompt: &str,
    tools_url: &str,
    token: &str,
    session_id: &str,
    resume: bool,
) -> anyhow::Result<RunOutcome> {
    let config_path = scratch_dir.join(MCP_CONFIG_FILENAME);
    write_mcp_config_file(&config_path, tools_url, token).await?;

    let args = claude_args(&config_path, prompt, session_id, resume);
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
    tracing::info!(role, "ClaudeCli: run completed");
    // Shape the textual output into the role's structured outcome. The decision
    // signals (manager actionable?, reviewer verdict) are parsed from a trailing
    // marker line Claude is prompted to emit; absent a marker, conservative defaults
    // apply (manager → actionable, reviewer → comment). Richer parsing is a deploy
    // refinement; this seam runs only in live deployment (the harness logic is
    // covered by the fake-driven tests in lib.rs).
    Ok(outcome_for_role(role, summary))
}

/// Map a role + Claude's textual summary to a [`RunOutcome`]. Pure (no I/O) so it is
/// unit-tested. Unknown roles map to `Code` defensively — the harness's run-seam match
/// rejects any non-coder/manager/reviewer role before this is reached for a real run,
/// so this default is never exercised for an unsupported role in practice.
fn outcome_for_role(role: &str, summary: String) -> RunOutcome {
    match role {
        "manager" => {
            let actionable = parse_manager_actionable(&summary);
            // Strip the control marker so it never leaks into the user-facing triage
            // comment (run_manager posts `summary` as the issue comment). A terse
            // marker-only response leaves an empty body, which GitHub rejects — fall
            // back to a decision-derived note so the comment is always non-empty.
            let body = strip_marker_lines(&summary, "ACTIONABLE:");
            let body = non_empty_or(body, || {
                if actionable {
                    "Triaged: actionable — assigning a coder.".to_string()
                } else {
                    "Triaged: not actionable — escalating for a human decision.".to_string()
                }
            });
            RunOutcome::Triage {
                actionable,
                summary: body,
            }
        }
        "reviewer" => {
            let verdict = parse_reviewer_verdict(&summary);
            // Strip the control marker; fall back to a verdict-derived body if the
            // response was marker-only (GitHub rejects an empty review body).
            let body = strip_marker_lines(&summary, "VERDICT:");
            let body = non_empty_or(body, || format!("Review verdict: {}.", verdict.as_str()));
            RunOutcome::Review {
                verdict,
                summary: body,
            }
        }
        _ => RunOutcome::Code { summary },
    }
}

/// Return `s` if it has non-whitespace content, else the lazily-computed fallback.
/// Guards the user-facing comment/review body against an empty (marker-only) summary.
fn non_empty_or(s: String, fallback: impl FnOnce() -> String) -> String {
    if s.trim().is_empty() {
        fallback()
    } else {
        s
    }
}

/// Remove any line whose trimmed text starts with `prefix` (case-insensitive) — the
/// internal control marker (`ACTIONABLE:` / `VERDICT:`) — from the user-facing summary,
/// trimming surrounding blank lines. The marker is a parsing contract, not content, so
/// it must not appear in the posted comment / review body.
fn strip_marker_lines(summary: &str, prefix: &str) -> String {
    summary
        .lines()
        .filter(|line| {
            line.trim()
                .get(..prefix.len())
                .map(|head| !head.eq_ignore_ascii_case(prefix))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Parse the manager's triage decision from a trailing `ACTIONABLE: yes|no` marker.
/// Defaults to actionable when no clear "no" marker is present (fail toward doing the
/// work rather than silently dropping an issue). Matches the FIRST token of the marker
/// value so a natural elaboration (`ACTIONABLE: no — out of scope`) still reads as
/// non-actionable rather than being missed by an exact-string compare.
fn parse_manager_actionable(summary: &str) -> bool {
    let not_actionable = last_marker(summary, "ACTIONABLE:")
        .and_then(|v| marker_first_token(&v))
        .map(|t| matches!(t.as_str(), "no" | "false" | "not" | "not_actionable"))
        .unwrap_or(false);
    !not_actionable
}

/// Parse the reviewer's verdict from a trailing `VERDICT: approve|request_changes|comment`
/// marker. Defaults to `comment` (the non-destructive verdict) when absent/unrecognized.
/// Matches the FIRST token so trailing rationale after the verdict is tolerated.
fn parse_reviewer_verdict(summary: &str) -> ReviewVerdict {
    match last_marker(summary, "VERDICT:")
        .and_then(|v| marker_first_token(&v))
        .as_deref()
    {
        Some("approve") => ReviewVerdict::Approve,
        Some("request_changes") => ReviewVerdict::RequestChanges,
        _ => ReviewVerdict::Comment,
    }
}

/// The first whitespace- or punctuation-delimited token of a marker value, lowercased.
/// e.g. `"no — out of scope"` → `"no"`, `"request_changes (add tests)"` →
/// `"request_changes"`. Returns `None` for an empty value.
fn marker_first_token(value: &str) -> Option<String> {
    value
        .split(|c: char| c.is_whitespace() || (c.is_ascii_punctuation() && c != '_'))
        .find(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
}

/// If the FINAL non-empty line starts with `prefix` (case-insensitive), return its
/// trimmed, lowercased value; otherwise `None`. The prompt requires the control marker
/// to be the last line, so only that line is consulted — a marker-looking line quoted
/// earlier in Claude's output (e.g. echoing issue text) cannot drive the decision.
fn last_marker(text: &str, prefix: &str) -> Option<String> {
    let last = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    let head = last.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        Some(last[prefix.len()..].trim().to_ascii_lowercase())
    } else {
        None
    }
}

/// Best-effort delete the entire MCP scratch directory (and its token-bearing config)
/// once the run is done — success OR failure — so the token never lingers on disk.
/// A failure to remove is logged but never propagated: it must not mask the run's own
/// outcome.
async fn cleanup_scratch(scratch_dir: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(scratch_dir).await {
        tracing::warn!(error = %e, "ClaudeCli: failed to remove MCP scratch dir");
    }
}

/// The MCP config filename. Dot-prefixed by convention.
const MCP_CONFIG_FILENAME: &str = ".factory-mcp.json";

/// A unique scratch directory under the system temp dir for the MCP config file.
/// Deliberately *outside* the cloned repo's working tree so the token-bearing
/// config can never be staged/committed by Claude and pushed to the remote. A
/// fresh UUID per run keeps concurrent workers from colliding.
fn mcp_scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("factory-mcp-{}", uuid::Uuid::new_v4()))
}

/// Write the MCP server config JSON to `path` *atomically* with Unix mode 0600
/// (owner read/write only) so the embedded token is never world-readable, not even
/// for a window. The token lives only in this file, never on the `claude` command
/// line.
///
/// The file is created with `create_new(true)` and `mode(0o600)` in a single
/// `open` syscall: the permission bits are applied at creation (no readable window
/// between create and chmod), and `create_new` fails if `path` already exists — so a
/// pre-planted symlink cannot redirect the write to another file. This crate targets
/// Unix.
async fn write_mcp_config_file(path: &Path, tools_url: &str, token: &str) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let json = mcp_config_json(tools_url, token);
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .await?;
    f.write_all(json.as_bytes()).await?;
    f.sync_all().await?;
    tracing::info!(path = %path.display(), "ClaudeCli: wrote MCP config (atomic create, mode 0600)");
    Ok(())
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
/// interactive permission prompts, attach the MCP config BY PATH, then the prompt.
///
/// The config is passed as a file path (not inline JSON), so the token it contains
/// never appears in argv (and thus never in `ps`/`/proc/<pid>/cmdline`).
fn claude_args(
    mcp_config_path: &Path,
    prompt: &str,
    session_id: &str,
    resume: bool,
) -> Vec<String> {
    // The worker owns the session id: a fresh run pins it with `--session-id` so the
    // checkpoint loop can record it; a crash-resume continues it with `--resume`.
    let (session_flag, session_value) = if resume {
        ("--resume", session_id)
    } else {
        ("--session-id", session_id)
    };
    vec![
        "--print".to_string(),
        "--dangerously-skip-permissions".to_string(),
        session_flag.to_string(),
        session_value.to_string(),
        "--mcp-config".to_string(),
        mcp_config_path.display().to_string(),
        prompt.to_string(),
    ]
}

/// Read a required environment variable.
fn env_var(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|_| anyhow::anyhow!("missing required env var: {key}"))
}

/// Default seconds between liveness heartbeats during a run (well inside the 15-min
/// lease so a single missed beat never lapses it).
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 60;
/// Default seconds between worktree/session checkpoints during a run.
const DEFAULT_CHECKPOINT_INTERVAL_SECS: u64 = 300;

/// Parse an optional positive-int env var, falling back to `default` when unset or
/// unparseable (a malformed override must not silently disable the loop).
fn env_secs(key: &str, default: u64) -> u64 {
    match std::env::var(key) {
        Ok(v) => v.parse::<u64>().ok().filter(|n| *n > 0).unwrap_or_else(|| {
            tracing::warn!(key, value = %v, default, "invalid interval env var; using default");
            default
        }),
        Err(_) => default,
    }
}

fn load_config() -> anyhow::Result<HarnessConfig> {
    let workdir = match std::env::var("FACTORY_WORKDIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => std::env::temp_dir().join(format!("factory-worker-{}", uuid::Uuid::new_v4())),
    };
    // Resume info is present (both vars) only when the dispatcher re-dispatched a
    // crashed attempt with a recoverable checkpoint; otherwise this is a fresh run.
    let resume = match (
        std::env::var("FACTORY_RESUME_SESSION_ID").ok(),
        std::env::var("FACTORY_RESUME_CHECKPOINT_URI").ok(),
    ) {
        (Some(session_id), Some(checkpoint_uri))
            if !session_id.is_empty() && !checkpoint_uri.is_empty() =>
        {
            tracing::info!(
                session_id,
                checkpoint_uri,
                "worker: launch is a crash-resume"
            );
            Some(ResumeInfo {
                session_id,
                checkpoint_uri,
            })
        }
        _ => None,
    };
    // Checkpoints persist to the local filesystem for parity; the Azure Blob swap is P7.
    let checkpoint_root = std::env::var("FACTORY_CHECKPOINT_ROOT").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("factory-checkpoints")
            .display()
            .to_string()
    });
    Ok(HarnessConfig {
        repo_url: env_var("FACTORY_REPO_URL")?,
        install_token: env_var("FACTORY_INSTALL_TOKEN")?,
        tools_url: env_var("FACTORY_TOOLS_URL")?,
        task_token: env_var("FACTORY_TASK_TOKEN")?,
        workdir,
        heartbeat_interval_secs: env_secs(
            "FACTORY_HEARTBEAT_INTERVAL_SECS",
            DEFAULT_HEARTBEAT_INTERVAL_SECS,
        ),
        checkpoint_interval_secs: env_secs(
            "FACTORY_CHECKPOINT_INTERVAL_SECS",
            DEFAULT_CHECKPOINT_INTERVAL_SECS,
        ),
        resume,
        checkpoint_store: std::sync::Arc::new(factory_blob::FsCheckpoints::new(checkpoint_root)),
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("worker");
    tracing::info!("worker: starting harness (P3)");

    let cfg = load_config()?;
    let tools = HttpTools::new(cfg.tools_url.clone(), cfg.task_token.clone());
    let git = GitCli {
        install_token: cfg.install_token.clone(),
    };
    let claude = ClaudeCli;

    factory_worker::run_task(&tools, &git, &claude, &cfg).await?;
    tracing::info!("worker: harness finished");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ghs_secrettoken";

    #[tokio::test]
    async fn write_mcp_config_file_creates_0600_with_token_contents() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("factory-mcp-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join(MCP_CONFIG_FILENAME);

        write_mcp_config_file(&path, "https://tools.factory.test", TOKEN)
            .await
            .unwrap();

        // The file exists with mode 0600 (owner read/write only).
        let meta = tokio::fs::metadata(&path).await.unwrap();
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "config file is owner-only"
        );
        // Its contents are the MCP config carrying the token in the Authorization header.
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(
            contents,
            mcp_config_json("https://tools.factory.test", TOKEN)
        );
        assert!(contents.contains(&format!("Bearer {TOKEN}")));

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn write_mcp_config_file_refuses_to_clobber_a_pre_existing_path() {
        // create_new(true) means a pre-planted file (or symlink) at the path is a hard
        // error rather than a silent overwrite — closing the symlink-redirect window.
        let dir = std::env::temp_dir().join(format!("factory-mcp-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join(MCP_CONFIG_FILENAME);
        tokio::fs::write(&path, "pre-existing").await.unwrap();

        let err = write_mcp_config_file(&path, "https://tools.factory.test", TOKEN)
            .await
            .expect_err("must refuse to clobber an existing path");
        assert_eq!(
            err.downcast_ref::<std::io::Error>().map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists),
            "create_new rejects a pre-existing path"
        );
        // The original file is untouched (the write never happened).
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "pre-existing"
        );

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn scratch_dir_is_cleaned_up_when_the_config_write_fails() {
        // Restructured cleanup (FIX 3): the scratch dir must be removed on EVERY exit
        // path after creation, including the config-WRITE failure path. We force a
        // write failure by pre-planting a file at the config path so create_new(true)
        // fails with AlreadyExists — this returns Err BEFORE claude is ever spawned —
        // then run the SAME sequence the trait method uses (run_claude_with_config then
        // cleanup_scratch) and assert the dir is gone and the error surfaced.
        let scratch_dir =
            std::env::temp_dir().join(format!("factory-mcp-cleanup-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&scratch_dir).await.unwrap();
        // Pre-plant the config file so the atomic create_new write fails.
        let config_path = scratch_dir.join(MCP_CONFIG_FILENAME);
        tokio::fs::write(&config_path, "pre-existing")
            .await
            .unwrap();

        let result = run_claude_with_config(
            &scratch_dir,
            Path::new("/tmp"),
            "coder",
            "prompt",
            "https://tools.factory.test",
            TOKEN,
            "ses-test",
            false,
        )
        .await;
        cleanup_scratch(&scratch_dir).await;

        // The write failed (claude was never spawned)...
        let err = result.expect_err("write into a clobbered path must fail");
        assert_eq!(
            err.downcast_ref::<std::io::Error>().map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists),
            "the failure is the create_new clobber refusal, before claude runs"
        );
        // ...and the scratch dir (with the partial/pre-planted token file) is gone.
        assert!(
            !tokio::fs::try_exists(&scratch_dir).await.unwrap(),
            "scratch dir must be removed even when the config write fails"
        );
    }

    #[test]
    fn plain_repo_url_returns_url_without_the_token() {
        // The clone/push URL is PLAIN: it must NOT carry the token (so the token
        // never reaches git's argv nor the persisted `.git/config` origin).
        let url = plain_repo_url("https://github.com/acme/repo.git", TOKEN).unwrap();
        assert_eq!(url, "https://github.com/acme/repo.git");
        assert!(!url.contains(TOKEN), "the URL must not contain the token");
    }

    #[test]
    fn plain_repo_url_rejects_http() {
        let err = plain_repo_url("http://github.com/acme/repo.git", TOKEN)
            .expect_err("http must be rejected");
        // The error must not leak the token.
        assert!(!err.to_string().contains(TOKEN));
    }

    #[test]
    fn plain_repo_url_rejects_ssh() {
        assert!(plain_repo_url("git@github.com:acme/repo.git", TOKEN).is_err());
        assert!(plain_repo_url("ssh://git@github.com/acme/repo.git", TOKEN).is_err());
    }

    #[test]
    fn plain_repo_url_rejects_empty_host() {
        assert!(plain_repo_url("https://", TOKEN).is_err());
    }

    #[test]
    fn git_credential_env_carries_token_only_base64_in_the_extraheader_value() {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine as _;

        let env = git_credential_env(TOKEN);
        // The exact env mechanism git reads: one config entry, http.extraheader.
        assert_eq!(
            env,
            vec![
                ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
                (
                    "GIT_CONFIG_KEY_0".to_string(),
                    "http.extraheader".to_string()
                ),
                (
                    "GIT_CONFIG_VALUE_0".to_string(),
                    format!(
                        "Authorization: Basic {}",
                        B64.encode(format!("x-access-token:{TOKEN}"))
                    ),
                ),
            ]
        );

        // The RAW token never appears anywhere in the env (key or value); it only
        // shows up base64-encoded inside the Authorization header value.
        let expected_basic = B64.encode(format!("x-access-token:{TOKEN}"));
        for (k, v) in &env {
            assert!(!k.contains(TOKEN), "raw token leaked into env key {k}");
            assert!(
                !v.contains(TOKEN),
                "raw token leaked into env value {v} (must be base64-only)"
            );
        }
        // The base64 form is present, and decodes back to the credential.
        let value = &env[2].1;
        assert_eq!(value, &format!("Authorization: Basic {expected_basic}"));
        let decoded = B64.decode(&expected_basic).unwrap();
        assert_eq!(
            String::from_utf8(decoded).unwrap(),
            format!("x-access-token:{TOKEN}")
        );
    }

    #[test]
    fn git_credential_env_rejected_tokens_still_never_appear_raw() {
        // Even for a token with unsafe chars, git_credential_env only emits the
        // base64 form; the raw token never appears in the value. (Validation that
        // rejects such tokens happens in plain_repo_url / push_branch.)
        let env = git_credential_env("ghs_AbC123_def");
        for (_k, v) in &env {
            assert!(!v.contains("ghs_AbC123_def") || v.starts_with("GIT_CONFIG"));
        }
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
    fn mcp_scratch_dir_is_outside_temp_dir_root_and_unique() {
        // The scratch dir lives under the system temp dir (never inside a repo
        // working tree), so the token-bearing config can't be committed/pushed.
        let a = mcp_scratch_dir();
        let b = mcp_scratch_dir();
        assert!(
            a.starts_with(std::env::temp_dir()),
            "scratch dir must be under the system temp dir, not a repo tree"
        );
        // A fresh UUID per call → concurrent workers never collide.
        assert_ne!(a, b, "each call yields a unique scratch dir");
    }

    #[test]
    fn claude_args_are_headless_with_config_path_and_prompt() {
        let path = Path::new("/work/.factory-mcp.json");
        let args = claude_args(path, "Fix the bug", "ses-123", false);
        assert_eq!(args[0], "--print");
        assert_eq!(args[1], "--dangerously-skip-permissions");
        // A fresh run pins the session id with --session-id.
        assert_eq!(args[2], "--session-id");
        assert_eq!(args[3], "ses-123");
        assert_eq!(args[4], "--mcp-config");
        // The config is referenced BY PATH, not as inline JSON.
        assert_eq!(args[5], "/work/.factory-mcp.json");
        // The prompt is the final positional argument.
        assert_eq!(args.last().unwrap(), "Fix the bug");
    }

    #[test]
    fn claude_args_resume_uses_resume_flag() {
        let path = Path::new("/work/.factory-mcp.json");
        let args = claude_args(path, "Continue", "ses-abc", true);
        // A crash-resume continues the same session with --resume <id>.
        assert_eq!(args[2], "--resume");
        assert_eq!(args[3], "ses-abc");
        assert!(!args.iter().any(|a| a == "--session-id"));
    }

    #[test]
    fn claude_args_never_carry_the_token() {
        // The args reference the config file path; the token (which lives only inside
        // that file) must not appear anywhere in argv.
        let path = Path::new("/work/.factory-mcp.json");
        let args = claude_args(path, "Fix the bug", "ses-123", false);
        for (i, a) in args.iter().enumerate() {
            assert!(!a.contains(TOKEN), "arg {i} leaked the token: {a}");
        }
    }

    #[test]
    fn plain_repo_url_rejects_tokens_with_unsafe_userinfo_chars() {
        // Tokens carrying characters outside [A-Za-z0-9_] are rejected outright so a
        // malformed credential is never used.
        for bad in [
            "tok@evil.example",
            "tok:secret",
            "tok/with/slash",
            "tok with space",
            "tok#frag",
            "",
        ] {
            let err = plain_repo_url("https://github.com/acme/repo.git", bad)
                .expect_err("unsafe token must be rejected");
            // The error must never leak the token value.
            assert!(!err.to_string().contains(bad) || bad.is_empty());
        }
    }

    #[test]
    fn plain_repo_url_accepts_normal_ghs_token() {
        // A normal GitHub installation token ([A-Za-z0-9_]) is accepted, and the
        // returned URL is plain (no token in it).
        let url = plain_repo_url("https://github.com/acme/repo.git", "ghs_AbC123_def").unwrap();
        assert_eq!(url, "https://github.com/acme/repo.git");
    }

    #[test]
    fn is_url_safe_token_classifies_correctly() {
        assert!(is_url_safe_token("ghs_AbC123_def"));
        assert!(is_url_safe_token("ABCdef0123456789_"));
        assert!(!is_url_safe_token(""));
        assert!(!is_url_safe_token("has@at"));
        assert!(!is_url_safe_token("has:colon"));
        assert!(!is_url_safe_token("has/slash"));
        assert!(!is_url_safe_token("has space"));
    }

    #[test]
    fn outcome_for_role_shapes_by_role() {
        // Coder → Code.
        assert!(matches!(
            outcome_for_role("coder", "did the work".into()),
            RunOutcome::Code { .. }
        ));
        // Manager → Triage; default actionable when no marker.
        assert!(matches!(
            outcome_for_role("manager", "in scope".into()),
            RunOutcome::Triage {
                actionable: true,
                ..
            }
        ));
        // Reviewer → Review; default comment when no marker.
        assert!(matches!(
            outcome_for_role("reviewer", "looks ok".into()),
            RunOutcome::Review {
                verdict: ReviewVerdict::Comment,
                ..
            }
        ));
        // The control marker is parsed for the decision but STRIPPED from the
        // user-facing summary (it must not appear in the posted comment / review body).
        let RunOutcome::Triage {
            actionable,
            summary,
        } = outcome_for_role("manager", "Out of scope.\nACTIONABLE: no".into())
        else {
            panic!("manager → Triage");
        };
        assert!(!actionable);
        assert_eq!(
            summary, "Out of scope.",
            "marker stripped from triage summary"
        );
        let RunOutcome::Review { verdict, summary } =
            outcome_for_role("reviewer", "Looks good.\nVERDICT: approve".into())
        else {
            panic!("reviewer → Review");
        };
        assert_eq!(verdict, ReviewVerdict::Approve);
        assert_eq!(
            summary, "Looks good.",
            "marker stripped from review summary"
        );
        // A marker-ONLY response strips to empty → must fall back to a non-empty,
        // decision-derived body (GitHub rejects an empty comment/review body).
        let RunOutcome::Triage { summary, .. } =
            outcome_for_role("manager", "ACTIONABLE: yes".into())
        else {
            panic!("manager → Triage");
        };
        assert!(
            !summary.trim().is_empty(),
            "marker-only manager response falls back to a non-empty body"
        );
        let RunOutcome::Review { summary, .. } =
            outcome_for_role("reviewer", "VERDICT: approve".into())
        else {
            panic!("reviewer → Review");
        };
        assert!(
            !summary.trim().is_empty(),
            "marker-only reviewer response falls back to a non-empty body"
        );
        // Unknown role defaults to Code (never reached for a real run — the harness
        // run-seam rejects unsupported roles before this).
        assert!(matches!(
            outcome_for_role("tester", "x".into()),
            RunOutcome::Code { .. }
        ));
    }

    #[test]
    fn parse_manager_actionable_reads_trailing_marker() {
        assert!(parse_manager_actionable("triaged\nACTIONABLE: yes"));
        assert!(parse_manager_actionable("triaged")); // default: actionable
        assert!(!parse_manager_actionable("out of scope\nACTIONABLE: no"));
        assert!(!parse_manager_actionable("ACTIONABLE: not_actionable"));
        // Case-insensitive on the marker; the LAST marker wins.
        assert!(!parse_manager_actionable("actionable: yes\nactionable: no"));
        // Elaboration after the decision token still reads correctly (first token wins).
        assert!(!parse_manager_actionable(
            "ACTIONABLE: no — out of scope, needs product"
        ));
        assert!(!parse_manager_actionable(
            "ACTIONABLE: not actionable; please clarify"
        ));
        assert!(parse_manager_actionable(
            "ACTIONABLE: yes, assigning a coder"
        ));
        // A marker quoted EARLIER (not the final line) is ignored → conservative
        // default (actionable). This stops echoed issue text from driving the decision.
        assert!(parse_manager_actionable(
            "The issue says 'ACTIONABLE: no'\nProceeding with the fix."
        ));
    }

    #[test]
    fn parse_reviewer_verdict_reads_trailing_marker() {
        assert_eq!(
            parse_reviewer_verdict("looks great\nVERDICT: approve"),
            ReviewVerdict::Approve
        );
        assert_eq!(
            parse_reviewer_verdict("needs work\nVERDICT: request_changes"),
            ReviewVerdict::RequestChanges
        );
        assert_eq!(
            parse_reviewer_verdict("just a note\nVERDICT: comment"),
            ReviewVerdict::Comment
        );
        // Absent/unrecognized → conservative `comment`.
        assert_eq!(parse_reviewer_verdict("no marker"), ReviewVerdict::Comment);
        assert_eq!(
            parse_reviewer_verdict("VERDICT: lgtm"),
            ReviewVerdict::Comment
        );
        // Trailing rationale after the verdict token is tolerated (first token wins).
        assert_eq!(
            parse_reviewer_verdict("VERDICT: request_changes (add tests)"),
            ReviewVerdict::RequestChanges
        );
        assert_eq!(
            parse_reviewer_verdict("VERDICT: approve — ship it"),
            ReviewVerdict::Approve
        );
    }

    #[test]
    fn strip_marker_lines_removes_the_control_marker_only() {
        assert_eq!(
            strip_marker_lines("Triaged it.\nACTIONABLE: no", "ACTIONABLE:"),
            "Triaged it."
        );
        // Case-insensitive on the marker; only marker lines are removed.
        assert_eq!(
            strip_marker_lines("line one\nverdict: approve\nline two", "VERDICT:"),
            "line one\nline two"
        );
        // No marker → unchanged (trimmed).
        assert_eq!(
            strip_marker_lines("just prose\n", "ACTIONABLE:"),
            "just prose"
        );
        // A marker-only summary collapses to empty.
        assert_eq!(strip_marker_lines("VERDICT: comment", "VERDICT:"), "");
    }

    #[test]
    fn marker_first_token_splits_on_space_and_punctuation() {
        assert_eq!(marker_first_token("no — out of scope"), Some("no".into()));
        assert_eq!(
            marker_first_token("request_changes (add tests)"),
            Some("request_changes".into()) // underscores are kept
        );
        assert_eq!(marker_first_token("approve"), Some("approve".into()));
        assert_eq!(marker_first_token("   "), None);
        assert_eq!(marker_first_token(""), None);
    }

    #[test]
    fn last_marker_only_reads_the_final_nonempty_line() {
        // The marker must be the FINAL non-empty line.
        assert_eq!(last_marker("kEy: Val", "KEY:"), Some("val".to_string()));
        assert_eq!(
            last_marker("a\nKEY: first\nKEY: second", "KEY:"),
            Some("second".to_string()),
            "the last line wins"
        );
        // Trailing blank lines are ignored; the last NON-EMPTY line is the marker.
        assert_eq!(
            last_marker("KEY: val\n\n  ", "KEY:"),
            Some("val".to_string())
        );
        // A marker quoted EARLIER does NOT count when the final line isn't a marker —
        // this is the contract that stops echoed issue/CR text from driving the decision.
        assert_eq!(last_marker("KEY: no\nsome closing prose", "KEY:"), None);
        assert_eq!(last_marker("nothing here", "KEY:"), None);
        // A short final line (shorter than the prefix) is None, not a panic.
        assert_eq!(last_marker("hi", "KEY:"), None);
        // Multibyte content doesn't panic the prefix slice.
        assert_eq!(last_marker("héllo", "KEY:"), None);
    }
}
