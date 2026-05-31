//! Coordinator service binary: the Manager (decide/delegate) reactive loop (P4).
//!
//! Every tick it reacts to terminal work items in the record book — assign a coder,
//! wake a reviewer, mark a change request ready-for-merge, drive the bounded
//! review-rework loop, or escalate to a human. It needs GitHub to post handoff
//! comments, assign issues, and read CR files/checks; if the App credentials are
//! absent it keeps the loop alive but skips every tick rather than acting blind.
#![forbid(unsafe_code)]

use std::time::Duration;

use factory_coordinator::coordinator_tick;
use factory_github::OctocrabGitHub;

/// How often the coordinator scans for terminal work to react to.
const COORDINATE_TICK: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("coordinator");

    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;

    let pool = factory_record::connect(&database_url).await?;
    factory_record::migrate(&pool).await?;

    // The coordinator's side effects (handoff comments, assignments) and its
    // approval-path reads go through the GitHub App. If credentials are absent we
    // keep the loop alive but skip every tick: acting without GitHub would drop the
    // human handoff and the can't-weaken guard's file read.
    let gh = match OctocrabGitHub::from_env() {
        Ok(client) => Some(client),
        Err(e) => {
            tracing::warn!(error = %e, "GitHub App credentials absent; coordinator ticks will be skipped until configured");
            None
        }
    };

    tracing::info!(
        tick_secs = COORDINATE_TICK.as_secs(),
        "coordinator starting"
    );

    let mut ticker = tokio::time::interval(COORDINATE_TICK);
    loop {
        ticker.tick().await;
        let Some(gh) = gh.as_ref() else {
            tracing::warn!("skipping coordinator tick: GitHub App credentials are not configured");
            continue;
        };
        match coordinator_tick(&pool, gh).await {
            Ok(n) if n > 0 => {
                tracing::info!(reacted = n, "coordinator tick reacted to terminal items")
            }
            Ok(_) => tracing::info!("coordinator tick: nothing to react to"),
            Err(e) => tracing::error!(error = %e, "coordinator tick failed"),
        }
    }
}
