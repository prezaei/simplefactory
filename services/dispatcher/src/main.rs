//! Dispatcher service binary: every tick, lease ready work under capacity caps and
//! launch a worker job per task. The launcher is selected by `FACTORY_WORKER_RUNTIME`:
//! `docker` (default, local — [`DockerLauncher`]) or `container_apps` (Azure, P8 —
//! [`ContainerAppsJobLauncher`]).
#![forbid(unsafe_code)]

use std::time::Duration;

use factory_dispatcher::{
    dispatch_once, ContainerAppsJobLauncher, DockerLauncher, WorkerLauncher, WorkerRuntime,
};
use factory_github::OctocrabGitHub;
use uuid::Uuid;

/// How often the dispatcher scans the queue for leasable work.
const DISPATCH_TICK: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("dispatcher");

    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;
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

    // Select the worker runtime. `container_apps` (Azure, P8): start a Container Apps Job
    // per task — secrets (blob/claude) come from the Job template's Key Vault, so the
    // dispatcher injects only per-task env. Otherwise `docker` (local): inject the blob
    // connection string + claude token per `docker run` (a local worker isn't on the
    // compose network and can't inherit them).
    let launcher: Box<dyn WorkerLauncher> = match std::env::var("FACTORY_WORKER_RUNTIME")
        .ok()
        .as_deref()
    {
        Some("container_apps") => {
            let require = |k: &str| {
                std::env::var(k)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("{k} is required for the container_apps runtime")
                    })
            };
            let subscription = require("FACTORY_AZURE_SUBSCRIPTION_ID")?;
            let resource_group = require("FACTORY_AZURE_RESOURCE_GROUP")?;
            let job_name = require("FACTORY_WORKER_JOB_NAME")?;
            let mi_client_id = std::env::var("FACTORY_MANAGED_IDENTITY_CLIENT_ID")
                .ok()
                .filter(|v| !v.trim().is_empty());
            tracing::info!(%subscription, %resource_group, %job_name, "dispatcher: Azure Container Apps Jobs runtime");
            Box::new(ContainerAppsJobLauncher::new(
                subscription,
                resource_group,
                job_name,
                mi_client_id,
            ))
        }
        _ => {
            let image = std::env::var("FACTORY_WORKER_IMAGE").map_err(|_| {
                anyhow::anyhow!("FACTORY_WORKER_IMAGE is required for the docker runtime")
            })?;
            let runtime = WorkerRuntime::from_env();
            // Every worker hard-requires the blob connection string (no Fs fallback);
            // warn once here so a missing one is one clear dispatcher log, not a stream
            // of opaque per-worker boot failures.
            if runtime.blob_connection_string.is_none() {
                tracing::warn!(
                    "FACTORY_BLOB_CONNECTION_STRING is not set on the dispatcher; launched \
                         workers require it and will fail to start (no checkpoints / crash-resume)"
                );
            }
            tracing::info!(worker_image = %image, "dispatcher: local Docker runtime");
            Box::new(DockerLauncher::new(image).with_runtime(runtime))
        }
    };

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
        match dispatch_once(&pool, launcher.as_ref(), gh, &tools_url, &lease_owner).await {
            Ok(n) if n > 0 => tracing::info!(launched = n, "dispatch tick launched workers"),
            Ok(_) => tracing::info!("dispatch tick launched nothing"),
            Err(e) => tracing::error!(error = %e, "dispatch tick failed"),
        }
    }
}
