//! Dispatcher service binary: every tick, lease ready work under capacity caps
//! and launch a worker job per task via the live [`DockerLauncher`] (P3).
#![forbid(unsafe_code)]

use std::time::Duration;

use factory_dispatcher::{dispatch_once, DockerLauncher, WorkerRuntime};
use factory_github::OctocrabGitHub;
use uuid::Uuid;

/// How often the dispatcher scans the queue for leasable work.
const DISPATCH_TICK: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("dispatcher");

    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;
    let image = std::env::var("FACTORY_WORKER_IMAGE")
        .map_err(|_| anyhow::anyhow!("FACTORY_WORKER_IMAGE is required"))?;
    let tools_url = std::env::var("FACTORY_TOOLS_URL")
        .map_err(|_| anyhow::anyhow!("FACTORY_TOOLS_URL is required"))?;

    // Identify this replica for lease ownership: prefer the hostname, falling back
    // to a process-unique id so two replicas never collide.
    let lease_owner = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| format!("dispatcher-{}", Uuid::new_v4().simple()));

    let pool = factory_record::connect(&database_url).await?;
    factory_record::migrate(&pool).await?;

    // Forward the worker-container runtime wiring (Azure Blob connection string +
    // claude token) read from the dispatcher's env. A worker launched via `docker
    // run` isn't on the compose network, so it can't inherit these — they must be
    // injected explicitly per launch.
    let runtime = WorkerRuntime::from_env();
    // Fail loud-but-early-ish: every worker hard-requires the blob connection string
    // (no Fs fallback), so without it on the dispatcher each launched container would
    // boot-loop on startup. Warn once here so the cause is one clear dispatcher log,
    // not a stream of opaque per-worker boot failures.
    if runtime.blob_connection_string.is_none() {
        tracing::warn!(
            "FACTORY_BLOB_CONNECTION_STRING is not set on the dispatcher; launched workers \
             require it and will fail to start (no checkpoints / crash-resume)"
        );
    }
    let launcher = DockerLauncher::new(image.clone()).with_runtime(runtime);

    // The dispatcher mints a per-launch installation token via the GitHub App. If
    // the App credentials are absent we keep the loop alive but skip every tick:
    // a missing token means we cannot safely launch a worker (it could not clone).
    let gh = match OctocrabGitHub::from_env() {
        Ok(client) => Some(client),
        Err(e) => {
            tracing::warn!(error = %e, "GitHub App credentials absent; dispatch ticks will be skipped until configured");
            None
        }
    };

    tracing::info!(
        %lease_owner,
        worker_image = %image,
        tools_url = %tools_url,
        tick_secs = DISPATCH_TICK.as_secs(),
        "dispatcher starting"
    );

    let mut ticker = tokio::time::interval(DISPATCH_TICK);
    loop {
        ticker.tick().await;
        let Some(gh) = gh.as_ref() else {
            tracing::warn!("skipping dispatch tick: GitHub App credentials are not configured");
            continue;
        };
        match dispatch_once(&pool, &launcher, gh, &tools_url, &lease_owner).await {
            Ok(n) if n > 0 => tracing::info!(launched = n, "dispatch tick launched workers"),
            Ok(_) => tracing::info!("dispatch tick launched nothing"),
            Err(e) => tracing::error!(error = %e, "dispatch tick failed"),
        }
    }
}
