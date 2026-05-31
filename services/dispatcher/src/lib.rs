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

/// Per-task lease/token TTL in seconds. A leased item that the worker never
/// completes within this window is reclaimed by the record layer's sweeper.
const TASK_TTL_SECS: f64 = 900.0;

/// Default per-worker concurrency cap when a worker does not set `max_concurrent`.
const DEFAULT_PER_WORKER_CAP: i64 = 2;

/// Everything a launcher needs to start one worker job.
///
/// `raw_token` is the un-hashed task token; it is injected into the job's
/// environment and is never persisted (only its hash lives in the record book).
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub work_item_id: Uuid,
    pub repo_id: Uuid,
    pub role: Kind,
    pub raw_token: String,
    pub tools_url: String,
    pub worker_name: String,
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
        // Pass secrets/URL via the environment; never on the command line where
        // they would leak into process listings.
        let output = tokio::process::Command::new("docker")
            .arg("run")
            .arg("--rm")
            .arg("--detach")
            .arg("-e")
            .arg(format!("FACTORY_TASK_TOKEN={}", spec.raw_token))
            .arg("-e")
            .arg(format!("FACTORY_TOOLS_URL={}", spec.tools_url))
            .arg("-e")
            .arg(format!("FACTORY_WORK_ITEM_ID={}", spec.work_item_id))
            .arg("-e")
            .arg(format!("FACTORY_REPO_ID={}", spec.repo_id))
            .arg("-e")
            .arg(format!("FACTORY_ROLE={}", spec.role.as_str()))
            .arg("-e")
            .arg(format!("FACTORY_WORKER_NAME={}", spec.worker_name))
            .arg(&self.image)
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

                match launch_leased(pool, launcher, tools_url, &item).await {
                    Ok(()) => total_launched += 1,
                    Err(e) => {
                        tracing::error!(
                            repo_id = %repo_id,
                            worker = %worker.name,
                            work_item = %item.id,
                            error = %e,
                            "failed to launch leased item; leaving it leased for reclaim"
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
    tools_url: &str,
    item: &record::WorkItem,
) -> anyhow::Result<()> {
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
