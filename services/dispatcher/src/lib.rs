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

/// Per-task lease/token TTL in seconds. The lease sweeper that reclaims expired
/// leases and revokes their tokens lands in P5; until then an expired lease is
/// NOT auto-reclaimed within this window.
const TASK_TTL_SECS: f64 = 900.0;

/// Default per-worker concurrency cap when a worker does not set `max_concurrent`.
const DEFAULT_PER_WORKER_CAP: i64 = 2;

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

/// Live launcher: runs a worker container via `docker run`, injecting the task
/// token and Tools URL as environment variables. Best-effort — this is the seam
/// that actually reaches Docker and is exercised in the live environment.
pub struct DockerLauncher {
    image: String,
}

impl DockerLauncher {
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
        }
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
        let args = docker_run_args(&self.image, spec);
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
fn docker_run_args(image: &str, spec: &LaunchSpec) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--detach".to_string(),
    ];
    for (key, value) in docker_env(spec) {
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
fn docker_env(spec: &LaunchSpec) -> Vec<(&'static str, String)> {
    vec![
        // Secrets — never logged.
        ("FACTORY_TASK_TOKEN", spec.raw_token.clone()),
        ("FACTORY_INSTALL_TOKEN", spec.install_token.clone()),
        // Config the worker needs to clone + reach Tools.
        ("FACTORY_REPO_URL", spec.repo_url.clone()),
        ("FACTORY_TOOLS_URL", spec.tools_url.clone()),
        // Retained for logging/correlation only (the worker reads head_branch/base
        // from get_task, not from env).
        ("FACTORY_WORK_ITEM", spec.work_item_id.to_string()),
        ("FACTORY_REPO_ID", spec.repo_id.to_string()),
        ("FACTORY_ROLE", spec.role.as_str().to_string()),
        ("FACTORY_WORKER_NAME", spec.worker_name.clone()),
    ]
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
        }
    }

    #[test]
    fn docker_env_carries_token_and_required_keys() {
        let env = docker_env(&spec());
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
    fn docker_run_args_pass_install_token_only_in_an_env_pair() {
        let args = docker_run_args("ghcr.io/acme/worker:latest", &spec());
        // The install token appears exactly once, in the FACTORY_INSTALL_TOKEN env value.
        let hits: Vec<&String> = args.iter().filter(|a| a.contains(INSTALL_TOKEN)).collect();
        assert_eq!(hits.len(), 1, "install token must appear exactly once");
        assert_eq!(hits[0], &format!("FACTORY_INSTALL_TOKEN={INSTALL_TOKEN}"));
        let idx = args.iter().position(|a| a.contains(INSTALL_TOKEN)).unwrap();
        assert_eq!(args[idx - 1], "-e");
    }

    #[test]
    fn docker_run_args_have_image_last_and_run_flags_first() {
        let args = docker_run_args("ghcr.io/acme/worker:latest", &spec());
        assert_eq!(args[0], "run");
        assert_eq!(args[1], "--rm");
        assert_eq!(args[2], "--detach");
        // Image is the final positional argument.
        assert_eq!(args.last().unwrap(), "ghcr.io/acme/worker:latest");
    }

    #[test]
    fn docker_run_args_pass_token_only_in_an_env_pair() {
        let args = docker_run_args("ghcr.io/acme/worker:latest", &spec());
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
        let args = docker_run_args("ghcr.io/acme/worker:latest", &spec());
        assert!(!args.last().unwrap().contains(TOKEN));
    }
}
