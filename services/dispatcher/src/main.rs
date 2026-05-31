//! Dispatcher service binary: every tick, lease ready work under capacity caps
//! and launch a worker job per task via the live [`DockerLauncher`] (P3).
#![forbid(unsafe_code)]

use std::time::Duration;

use factory_dispatcher::{dispatch_once, DockerLauncher};
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

    let launcher = DockerLauncher::new(image.clone());

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
        match dispatch_once(&pool, &launcher, &tools_url, &lease_owner).await {
            Ok(n) if n > 0 => tracing::info!(launched = n, "dispatch tick launched workers"),
            Ok(_) => tracing::info!("dispatch tick launched nothing"),
            Err(e) => tracing::error!(error = %e, "dispatch tick failed"),
        }
    }
}
