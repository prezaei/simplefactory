//! Dispatcher core: poll the queue under capacity caps and launch a worker job
//! per leased task (P3).
//!
//! [`dispatch_once`] walks every configured repo, parses its team [`Config`], and
//! for each worker leases ready work up to the worker/global caps. For each leased
//! item it opens an attempt, mints a per-task scoped token (only the hash is
//! stored; the raw token is injected into the job), and asks a [`WorkerLauncher`]
//! to start the job with that token plus the Tools URL. Every launch is recorded
//! as an audit event.
//!
//! The launcher is a seam: [`FakeLauncher`] records launches for tests, while
//! [`DockerLauncher`] is the live implementation that spawns a container.
#![forbid(unsafe_code)]

use std::sync::Mutex;

use factory_config as config;
use factory_record as record;
use record::{Kind, LeaseRequest, Pool};
use serde_json::json;
use uuid::Uuid;

/// Per-task lease/token TTL in seconds. The coordinator's lease sweeper (P5) reclaims
/// leases that expire past this window, marking the attempt reclaimed and revoking the
/// token, so a crashed worker's slot is freed and the item re-dispatched.
const TASK_TTL_SECS: f64 = 900.0;

/// Default per-worker concurrency cap when a worker does not set `max_concurrent`.
const DEFAULT_PER_WORKER_CAP: i64 = 2;

/// How many times a work item may be resumed from a checkpoint before the dispatcher
/// gives up on the crashed session and launches a fresh attempt (P5). Bounds a
/// deterministic crash loop from resuming forever.
const DEFAULT_RESUME_CAP: u32 = 3;

/// A crash-resume launch: continue the prior Claude session from its worktree snapshot
/// instead of starting fresh. Injected into the worker's env by [`docker_env`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeLaunch {
    pub session_id: String,
    pub checkpoint_uri: String,
}

/// Decide whether a re-dispatch resumes the prior session or starts fresh: resume only
/// while a recoverable checkpoint exists AND the resume budget is not exhausted. A pure
/// function so the resume policy is unit-tested without a database.
pub fn decide_resume(
    prior: Option<&record::CheckpointInfo>,
    resume_cap: u32,
) -> Option<ResumeLaunch> {
    match prior {
        Some(info) if info.resume_count < u64::from(resume_cap) => Some(ResumeLaunch {
            session_id: info.session_id.clone(),
            checkpoint_uri: info.checkpoint_uri.clone(),
        }),
        _ => None,
    }
}

/// Everything a launcher needs to start one worker job.
///
/// `raw_token` and `install_token` are secrets injected into the job's
/// environment and are never persisted nor logged (only the task token's hash
/// lives in the record book; the install token is minted fresh per launch).
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub work_item_id: Uuid,
    pub repo_id: Uuid,
    pub role: Kind,
    pub raw_token: String,
    pub tools_url: String,
    pub worker_name: String,
    /// HTTPS clone URL of the repo (`https://github.com/{owner}/{name}.git`).
    pub repo_url: String,
    /// Short-lived GitHub installation token for clone + push. Secret.
    pub install_token: String,
    /// The config sha pinned on the work item at enqueue — passed to the worker so a
    /// mid-flight config change can't retarget an in-flight attempt (P5 config pinning).
    pub config_sha: String,
    /// When set, this launch resumes a crashed attempt's session (P5 crash-resume).
    pub resume: Option<ResumeLaunch>,
}

/// Starts a single worker job for a leased task. Implementations return a job
/// execution id (opaque) that callers can correlate with the attempt.
#[async_trait::async_trait]
pub trait WorkerLauncher: Send + Sync {
    async fn launch(&self, spec: &LaunchSpec) -> anyhow::Result<String>;
}

/// Map a config [`Role`](config::Role) to a record [`Kind`].
pub fn role_to_kind(role: config::Role) -> Kind {
    match role {
        config::Role::Manager => Kind::Manager,
        config::Role::Coder => Kind::Coder,
        config::Role::Reviewer => Kind::Reviewer,
        config::Role::Tester => Kind::Tester,
    }
}

/// A recorded launch, captured by [`FakeLauncher`] for assertions. Holds the
/// scoped fields a test needs without retaining the raw token beyond the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRecord {
    pub work_item_id: Uuid,
    pub repo_id: Uuid,
    pub role: Kind,
    pub worker_name: String,
    pub tools_url: String,
    pub repo_url: String,
    pub install_token: String,
    pub config_sha: String,
    pub resume: Option<ResumeLaunch>,
}

/// Test launcher: records every launch and returns a synthetic job id. Never
/// spawns anything.
#[derive(Default)]
pub struct FakeLauncher {
    launched: Mutex<Vec<LaunchRecord>>,
}

impl FakeLauncher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of every launch recorded so far, in order.
    pub fn launched(&self) -> Vec<LaunchRecord> {
        // A poisoned lock means a prior launch panicked; recover the data so the
        // test can still assert rather than cascading the panic.
        match self.launched.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }
}

#[async_trait::async_trait]
impl WorkerLauncher for FakeLauncher {
    async fn launch(&self, spec: &LaunchSpec) -> anyhow::Result<String> {
        let record = LaunchRecord {
            work_item_id: spec.work_item_id,
            repo_id: spec.repo_id,
            role: spec.role,
            worker_name: spec.worker_name.clone(),
            tools_url: spec.tools_url.clone(),
            repo_url: spec.repo_url.clone(),
            install_token: spec.install_token.clone(),
            config_sha: spec.config_sha.clone(),
            resume: spec.resume.clone(),
        };
        let job_id = format!("fake-job-{}", Uuid::new_v4().simple());
        match self.launched.lock() {
            Ok(mut g) => g.push(record),
            Err(p) => p.into_inner().push(record),
        }
        tracing::info!(
            work_item = %spec.work_item_id,
            role = spec.role.as_str(),
            job = %job_id,
            "fake launcher recorded launch"
        );
        Ok(job_id)
    }
}

/// Host-side wiring injected into every worker container that the pure
/// [`docker_run_args`] can't derive from a [`LaunchSpec`]: the Azure Blob (Azurite
/// in P7) connection string the worker checkpoints to, and the `claude` auth token.
/// Read once from the dispatcher's environment in `main`, so the launch path stays
/// a pure, testable function. A worker launched via `docker run` is NOT on the
/// compose network, so it reaches Azurite/Tools via `host.docker.internal` — hence
/// these are passed explicitly rather than inherited from the compose env.
#[derive(Clone, Default)]
pub struct WorkerRuntime {
    /// Azure Blob connection string (Azurite in P7) the worker's `CheckpointStore`
    /// writes to, forwarded as `FACTORY_BLOB_CONNECTION_STRING`. Survives the
    /// container so a re-dispatched worker reads the dead attempt's checkpoint
    /// (crash-resume). Secret-ish (holds the account key for Azurite); never logged.
    /// `None` ⇒ not forwarded, and the worker (which has no Fs fallback) will fail
    /// fast — so a live dispatcher must always set `FACTORY_BLOB_CONNECTION_STRING`.
    pub blob_connection_string: Option<String>,
    /// `claude` setup-token, forwarded as `CLAUDE_CODE_OAUTH_TOKEN`. Secret; never
    /// logged. `None` ⇒ not forwarded (the image must supply auth another way).
    pub claude_oauth_token: Option<String>,
}

impl WorkerRuntime {
    /// Read the worker-container runtime wiring from the dispatcher's environment.
    /// Empty values are treated as unset so a blank env var can't inject an empty
    /// connection string or token.
    pub fn from_env() -> Self {
        Self {
            blob_connection_string: non_empty_env("FACTORY_BLOB_CONNECTION_STRING"),
            claude_oauth_token: non_empty_env("CLAUDE_CODE_OAUTH_TOKEN"),
        }
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    // Trim first so a whitespace-only value (common when copying env files) is treated
    // as unset rather than forwarded into a worker container, where it would surface as
    // a confusing parse/startup failure instead of "absent".
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Live launcher: runs a worker container via `docker run`, injecting the task
/// token and Tools URL as environment variables. Best-effort — this is the seam
/// that actually reaches Docker and is exercised in the live environment.
pub struct DockerLauncher {
    image: String,
    runtime: WorkerRuntime,
}

impl DockerLauncher {
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            runtime: WorkerRuntime::default(),
        }
    }

    /// Attach the host-side runtime wiring (Azure Blob connection string + claude
    /// token) read from the environment. Without it, workers launch with no blob
    /// connection string and fail fast on startup (they require one — no Fs fallback).
    pub fn with_runtime(mut self, runtime: WorkerRuntime) -> Self {
        self.runtime = runtime;
        self
    }
}

#[async_trait::async_trait]
impl WorkerLauncher for DockerLauncher {
    async fn launch(&self, spec: &LaunchSpec) -> anyhow::Result<String> {
        // Secrets and config are passed via the environment, never on the command
        // line where they would leak into process listings. We log only the
        // work item / role / image — never the assembled argv (it holds the
        // token) — so tracing here is intentionally narrow.
        tracing::info!(
            work_item = %spec.work_item_id,
            role = spec.role.as_str(),
            image = %self.image,
            "DockerLauncher: docker run --rm --detach"
        );
        let args = docker_run_args(&self.image, &self.runtime, spec);
        let output = tokio::process::Command::new("docker")
            .args(&args)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(
                work_item = %spec.work_item_id,
                status = ?output.status.code(),
                stderr = %stderr,
                "docker run failed"
            );
            anyhow::bail!("docker run failed: {stderr}");
        }

        let job_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        tracing::info!(
            work_item = %spec.work_item_id,
            role = spec.role.as_str(),
            job = %job_id,
            "launched worker container"
        );
        Ok(job_id)
    }
}

/// Build the full `docker run` argv for a worker launch.
///
/// Secrets and config are injected as `-e KEY=VALUE` environment pairs (the only
/// place the raw token appears), and the image is the final positional argument
/// so `docker run` treats everything after it as the container's command. The
/// returned vec contains the token and MUST NOT be logged.
fn docker_run_args(image: &str, runtime: &WorkerRuntime, spec: &LaunchSpec) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--detach".to_string(),
    ];
    // A worker launched via `docker run` is NOT on the compose network, so it reaches
    // host-published services (Tools, webhooks) via `host.docker.internal`. Map it to
    // the host gateway so the name resolves on Linux too (Docker Desktop already does).
    args.push("--add-host".to_string());
    args.push("host.docker.internal:host-gateway".to_string());
    for (key, value) in docker_env(runtime, spec) {
        args.push("-e".to_string());
        args.push(format!("{key}={value}"));
    }
    // Image is last: docker treats trailing args as the container command.
    args.push(image.to_string());
    args
}

/// The environment passed into the worker container, as ordered (key, value)
/// pairs. The raw task token is delivered here (`FACTORY_TASK_TOKEN`) so it never
/// touches a logged argv string.
pub fn docker_env(runtime: &WorkerRuntime, spec: &LaunchSpec) -> Vec<(&'static str, String)> {
    let mut env = vec![
        // Secrets — never logged.
        ("FACTORY_TASK_TOKEN", spec.raw_token.clone()),
        ("FACTORY_INSTALL_TOKEN", spec.install_token.clone()),
        // Config the worker needs to clone + reach Tools.
        ("FACTORY_REPO_URL", spec.repo_url.clone()),
        ("FACTORY_TOOLS_URL", spec.tools_url.clone()),
        // The config sha pinned at enqueue — the worker logs it so an in-flight attempt
        // is traceable to the exact config it was dispatched under (P5 config pinning).
        ("FACTORY_CONFIG_SHA", spec.config_sha.clone()),
        // Retained for logging/correlation only (the worker reads head_branch/base
        // from get_task, not from env).
        ("FACTORY_WORK_ITEM", spec.work_item_id.to_string()),
        ("FACTORY_REPO_ID", spec.repo_id.to_string()),
        ("FACTORY_ROLE", spec.role.as_str().to_string()),
        ("FACTORY_WORKER_NAME", spec.worker_name.clone()),
    ];
    // Crash-resume: both vars present ⇒ the worker continues the prior session from its
    // worktree snapshot; absent ⇒ a fresh run (P5).
    if let Some(resume) = &spec.resume {
        env.push(("FACTORY_RESUME_SESSION_ID", resume.session_id.clone()));
        env.push((
            "FACTORY_RESUME_CHECKPOINT_URI",
            resume.checkpoint_uri.clone(),
        ));
    }
    // Point the worker's checkpoint/transcript stores at the shared Azure Blob
    // (Azurite in P7) so snapshots survive the container and a resume can read them.
    if let Some(conn) = &runtime.blob_connection_string {
        env.push(("FACTORY_BLOB_CONNECTION_STRING", conn.clone()));
    }
    // claude auth (setup-token) — secret, forwarded only as an env value.
    if let Some(token) = &runtime.claude_oauth_token {
        env.push(("CLAUDE_CODE_OAUTH_TOKEN", token.clone()));
    }
    env
}

/// Generate a fresh raw task token. Two v4 UUIDs concatenated give 256 bits of
/// entropy; only its hash is ever stored.
fn new_raw_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// One dispatch pass: lease ready work under caps and launch a worker per task.
///
/// Returns the total number of workers launched across all repos. Per-repo and
/// per-worker failures are logged and skipped so one bad config can't stall the
/// whole pass.
pub async fn dispatch_once(
    pool: &Pool,
    launcher: &dyn WorkerLauncher,
    gh: &dyn factory_github::GitHubClient,
    tools_url: &str,
    lease_owner: &str,
) -> anyhow::Result<usize> {
    let repos = record::list_configured_repos(pool).await?;
    tracing::info!(
        repos = repos.len(),
        "dispatch pass: scanning configured repos"
    );

    let mut total_launched = 0usize;

    for (repo_id, config_sha, config_json) in repos {
        let cfg: config::Config = match serde_json::from_value(config_json) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(repo_id = %repo_id, config_sha = %config_sha, error = %e, "skipping repo: stored config is not a valid Config");
                continue;
            }
        };

        let global_cap = cfg.limits.max_concurrent_workers as i64;

        for worker in &cfg.workers {
            let per_worker_cap = worker
                .max_concurrent
                .map(|c| c as i64)
                .unwrap_or(DEFAULT_PER_WORKER_CAP);
            let role = role_to_kind(worker.role);

            tracing::info!(
                repo_id = %repo_id,
                worker = %worker.name,
                role = role.as_str(),
                per_worker_cap,
                global_cap,
                "leasing for worker"
            );

            // Lease until the queue is empty or a cap binds (lease_next returns
            // None when no item is available under the caps).
            loop {
                let req = LeaseRequest {
                    repo_id,
                    worker_name: worker.name.clone(),
                    lease_owner: lease_owner.to_owned(),
                    ttl_secs: TASK_TTL_SECS,
                    per_worker_cap,
                    global_cap,
                };

                let item = match record::lease_next(pool, &req).await {
                    Ok(Some(item)) => item,
                    Ok(None) => {
                        tracing::info!(repo_id = %repo_id, worker = %worker.name, "no more leasable work (queue empty or cap bound)");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(repo_id = %repo_id, worker = %worker.name, error = %e, "lease_next failed; stopping this worker");
                        break;
                    }
                };

                match launch_leased(pool, launcher, gh, tools_url, &item).await {
                    Ok(()) => total_launched += 1,
                    Err(e) => {
                        tracing::error!(
                            repo_id = %repo_id,
                            worker = %worker.name,
                            work_item = %item.id,
                            error = %e,
                            "failed to launch leased item; it stays leased until the P5 lease sweeper reclaims expired leases (no auto-reclaim in P3)"
                        );
                    }
                }
            }
        }
    }

    tracing::info!(launched = total_launched, "dispatch pass complete");
    Ok(total_launched)
}

/// Open an attempt, mint a scoped token, launch the job, and record the event for
/// a single already-leased work item.
async fn launch_leased(
    pool: &Pool,
    launcher: &dyn WorkerLauncher,
    gh: &dyn factory_github::GitHubClient,
    tools_url: &str,
    item: &record::WorkItem,
) -> anyhow::Result<()> {
    // Resolve the repo's GitHub identity so we can mint an installation token and
    // build the clone URL the worker needs.
    let repo = record::repo_by_id(pool, item.repo_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repo {} not found for leased item", item.repo_id))?;
    let repo_url = format!("https://github.com/{}/{}.git", repo.owner, repo.name);

    // Mint a short-lived installation token (secret — never logged).
    let install_token = gh.installation_token(repo.installation_id).await?;
    tracing::info!(
        repo_id = %item.repo_id,
        installation_id = repo.installation_id,
        "minted installation token for worker clone"
    );

    // record_outcome requires the attempt id, and the dispatcher guarantees it: the
    // task token is always minted with `Some(attempt_id)` from the attempt opened
    // here, so the worker can always record its terminal outcome against it.
    // Decide resume-vs-fresh BEFORE opening the new attempt, so the resume budget
    // counts only the PRIOR attempts that carried a session (not the one we open now).
    let prior_checkpoint = record::last_checkpoint_for_item(pool, item.id).await?;
    let resume = decide_resume(prior_checkpoint.as_ref(), DEFAULT_RESUME_CAP);
    tracing::info!(
        work_item = %item.id,
        resuming = resume.is_some(),
        "dispatcher: resume decision for re-dispatch"
    );

    let attempt_id = record::start_attempt(pool, item.id, None).await?;

    let raw_token = new_raw_token();
    record::mint_task_token(
        pool,
        &record::hash_token(&raw_token),
        item.id,
        item.repo_id,
        item.kind,
        Some(attempt_id),
        TASK_TTL_SECS,
    )
    .await?;

    let spec = LaunchSpec {
        work_item_id: item.id,
        repo_id: item.repo_id,
        role: item.kind,
        raw_token,
        tools_url: tools_url.to_owned(),
        worker_name: item.worker_name.clone(),
        repo_url,
        install_token,
        config_sha: item.config_sha.clone(),
        resume,
    };
    let job_id = launcher.launch(&spec).await?;

    record::append_event(
        pool,
        Some(item.repo_id),
        Some(item.id),
        Some(item.correlation_id),
        "dispatcher",
        "launched",
        json!({
            "attempt_id": attempt_id.to_string(),
            "job_execution_id": job_id,
            "role": item.kind.as_str(),
            "worker_name": item.worker_name,
            "subject_ref": item.subject_ref,
        }),
    )
    .await?;

    tracing::info!(
        work_item = %item.id,
        attempt = %attempt_id,
        role = item.kind.as_str(),
        job = %job_id,
        "dispatched worker"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "rawtoken-supersecret";
    const INSTALL_TOKEN: &str = "ghs_install-supersecret";
    const REPO_URL: &str = "https://github.com/acme/widgets.git";

    fn spec() -> LaunchSpec {
        LaunchSpec {
            work_item_id: Uuid::nil(),
            repo_id: Uuid::nil(),
            role: Kind::Coder,
            raw_token: TOKEN.to_string(),
            tools_url: "http://tools.local:8080".to_string(),
            worker_name: "coder-1".to_string(),
            repo_url: REPO_URL.to_string(),
            install_token: INSTALL_TOKEN.to_string(),
            config_sha: "cfgsha123".to_string(),
            resume: None,
        }
    }

    #[test]
    fn docker_env_carries_token_and_required_keys() {
        let env = docker_env(&WorkerRuntime::default(), &spec());
        let token = env
            .iter()
            .find(|(k, _)| *k == "FACTORY_TASK_TOKEN")
            .expect("token env present");
        assert_eq!(token.1, TOKEN);

        let install = env
            .iter()
            .find(|(k, _)| *k == "FACTORY_INSTALL_TOKEN")
            .expect("install token env present");
        assert_eq!(install.1, INSTALL_TOKEN);

        let repo_url = env
            .iter()
            .find(|(k, _)| *k == "FACTORY_REPO_URL")
            .expect("repo url env present");
        assert_eq!(repo_url.1, REPO_URL);

        let keys: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"FACTORY_TOOLS_URL"));
        assert!(keys.contains(&"FACTORY_WORK_ITEM"));
        assert!(keys.contains(&"FACTORY_ROLE"));
        // FACTORY_HEAD_BRANCH is gone: the worker reads the branch from get_task.
        assert!(!keys.contains(&"FACTORY_HEAD_BRANCH"));

        let role = env
            .iter()
            .find(|(k, _)| *k == "FACTORY_ROLE")
            .expect("role env present");
        assert_eq!(role.1, "coder");
    }

    #[test]
    fn docker_env_carries_the_pinned_config_sha() {
        let env = docker_env(&WorkerRuntime::default(), &spec());
        let sha = env
            .iter()
            .find(|(k, _)| *k == "FACTORY_CONFIG_SHA")
            .expect("config sha env present");
        assert_eq!(
            sha.1, "cfgsha123",
            "the pinned config sha reaches the worker"
        );
    }

    #[test]
    fn docker_env_omits_resume_vars_for_a_fresh_launch() {
        let keys: Vec<&str> = docker_env(&WorkerRuntime::default(), &spec())
            .iter()
            .map(|(k, _)| *k)
            .collect();
        assert!(!keys.contains(&"FACTORY_RESUME_SESSION_ID"));
        assert!(!keys.contains(&"FACTORY_RESUME_CHECKPOINT_URI"));
    }

    #[test]
    fn docker_env_injects_resume_vars_for_a_resume_launch() {
        let mut s = spec();
        s.resume = Some(ResumeLaunch {
            session_id: "ses-9".into(),
            checkpoint_uri: "checkpoints/ses-9.snapshot".into(),
        });
        let env = docker_env(&WorkerRuntime::default(), &s);
        let sid = env
            .iter()
            .find(|(k, _)| *k == "FACTORY_RESUME_SESSION_ID")
            .expect("resume session env present");
        assert_eq!(sid.1, "ses-9");
        let uri = env
            .iter()
            .find(|(k, _)| *k == "FACTORY_RESUME_CHECKPOINT_URI")
            .expect("resume uri env present");
        assert_eq!(uri.1, "checkpoints/ses-9.snapshot");
    }

    fn checkpoint(resume_count: u64) -> record::CheckpointInfo {
        record::CheckpointInfo {
            session_id: "ses-1".into(),
            checkpoint_uri: "checkpoints/ses-1.snapshot".into(),
            resume_count,
        }
    }

    #[test]
    fn decide_resume_is_fresh_without_a_checkpoint() {
        assert_eq!(decide_resume(None, DEFAULT_RESUME_CAP), None);
    }

    #[test]
    fn decide_resume_resumes_under_the_cap() {
        let info = checkpoint(2);
        assert_eq!(
            decide_resume(Some(&info), 3),
            Some(ResumeLaunch {
                session_id: "ses-1".into(),
                checkpoint_uri: "checkpoints/ses-1.snapshot".into(),
            })
        );
    }

    #[test]
    fn decide_resume_is_fresh_at_and_over_the_cap() {
        // At the cap and beyond, the crashed session is abandoned for a fresh attempt.
        assert_eq!(decide_resume(Some(&checkpoint(3)), 3), None);
        assert_eq!(decide_resume(Some(&checkpoint(5)), 3), None);
    }

    #[test]
    fn docker_run_args_pass_install_token_only_in_an_env_pair() {
        let args = docker_run_args(
            "ghcr.io/acme/worker:latest",
            &WorkerRuntime::default(),
            &spec(),
        );
        // The install token appears exactly once, in the FACTORY_INSTALL_TOKEN env value.
        let hits: Vec<&String> = args.iter().filter(|a| a.contains(INSTALL_TOKEN)).collect();
        assert_eq!(hits.len(), 1, "install token must appear exactly once");
        assert_eq!(hits[0], &format!("FACTORY_INSTALL_TOKEN={INSTALL_TOKEN}"));
        let idx = args.iter().position(|a| a.contains(INSTALL_TOKEN)).unwrap();
        assert_eq!(args[idx - 1], "-e");
    }

    #[test]
    fn docker_run_args_have_image_last_and_run_flags_first() {
        let args = docker_run_args(
            "ghcr.io/acme/worker:latest",
            &WorkerRuntime::default(),
            &spec(),
        );
        assert_eq!(args[0], "run");
        assert_eq!(args[1], "--rm");
        assert_eq!(args[2], "--detach");
        // Image is the final positional argument.
        assert_eq!(args.last().unwrap(), "ghcr.io/acme/worker:latest");
    }

    #[test]
    fn docker_run_args_pass_token_only_in_an_env_pair() {
        let args = docker_run_args(
            "ghcr.io/acme/worker:latest",
            &WorkerRuntime::default(),
            &spec(),
        );
        // The token appears exactly once, in the FACTORY_TASK_TOKEN env value.
        let token_args: Vec<&String> = args.iter().filter(|a| a.contains(TOKEN)).collect();
        assert_eq!(token_args.len(), 1, "token must appear exactly once");
        assert_eq!(token_args[0], &format!("FACTORY_TASK_TOKEN={TOKEN}"));
        // It is preceded by a `-e` flag (it is an env injection, not a bare arg).
        let idx = args.iter().position(|a| a.contains(TOKEN)).unwrap();
        assert_eq!(args[idx - 1], "-e");
    }

    #[test]
    fn docker_run_args_never_put_token_in_the_image_position() {
        let args = docker_run_args(
            "ghcr.io/acme/worker:latest",
            &WorkerRuntime::default(),
            &spec(),
        );
        assert!(!args.last().unwrap().contains(TOKEN));
    }

    #[test]
    fn docker_run_args_always_map_host_gateway() {
        // The worker (a standalone container, not on the compose network) must be able
        // to resolve host.docker.internal to reach host-published Tools.
        let args = docker_run_args("img", &WorkerRuntime::default(), &spec());
        let idx = args
            .iter()
            .position(|a| a == "host.docker.internal:host-gateway")
            .expect("host-gateway mapping present");
        assert_eq!(args[idx - 1], "--add-host");
    }

    #[test]
    fn docker_env_forwards_the_blob_connection_string_when_configured() {
        let runtime = WorkerRuntime {
            blob_connection_string: Some(
                "BlobEndpoint=http://host.docker.internal:10000/x".to_string(),
            ),
            ..WorkerRuntime::default()
        };
        let conn = docker_env(&runtime, &spec())
            .into_iter()
            .find(|(k, _)| *k == "FACTORY_BLOB_CONNECTION_STRING");
        assert_eq!(
            conn,
            Some((
                "FACTORY_BLOB_CONNECTION_STRING",
                "BlobEndpoint=http://host.docker.internal:10000/x".to_string()
            ))
        );
    }

    #[test]
    fn docker_env_omits_blob_connection_string_when_unset() {
        let keys: Vec<&str> = docker_env(&WorkerRuntime::default(), &spec())
            .iter()
            .map(|(k, _)| *k)
            .collect();
        assert!(!keys.contains(&"FACTORY_BLOB_CONNECTION_STRING"));
    }

    #[test]
    fn docker_run_args_forward_claude_token_only_as_an_env_pair() {
        let runtime = WorkerRuntime {
            claude_oauth_token: Some("sk-claude-secret".to_string()),
            ..WorkerRuntime::default()
        };
        let args = docker_run_args("img", &runtime, &spec());
        let hits: Vec<&String> = args
            .iter()
            .filter(|a| a.contains("sk-claude-secret"))
            .collect();
        assert_eq!(hits.len(), 1, "claude token must appear exactly once");
        assert_eq!(hits[0], "CLAUDE_CODE_OAUTH_TOKEN=sk-claude-secret");
        let idx = args
            .iter()
            .position(|a| a.contains("sk-claude-secret"))
            .unwrap();
        assert_eq!(args[idx - 1], "-e");
        // Never in the image (trailing) position.
        assert!(!args.last().unwrap().contains("sk-claude-secret"));
    }

    #[test]
    fn docker_env_omits_claude_token_when_unset() {
        let keys: Vec<&str> = docker_env(&WorkerRuntime::default(), &spec())
            .iter()
            .map(|(k, _)| *k)
            .collect();
        assert!(!keys.contains(&"CLAUDE_CODE_OAUTH_TOKEN"));
    }
}
