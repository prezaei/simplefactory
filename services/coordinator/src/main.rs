//! Coordinator service binary: the Manager (decide/delegate) reactive loop (P4).
//!
//! Every tick it reacts to terminal work items in the record book — assign a coder,
//! wake a reviewer, mark a change request ready-for-merge, drive the bounded
//! review-rework loop, or escalate to a human. It needs GitHub to post handoff
//! comments, assign issues, and read CR files/checks; if the App credentials are
//! absent it keeps the loop alive but skips every tick rather than acting blind.
#![forbid(unsafe_code)]

use std::time::Duration;

use factory_coordinator::{coordinator_tick, sweeper_tick};
use factory_github::OctocrabGitHub;

/// How often the coordinator scans for terminal work to react to.
const COORDINATE_TICK: Duration = Duration::from_secs(5);

/// How often the lease sweeper scans for expired leases to reclaim (P5).
const SWEEPER_TICK: Duration = Duration::from_secs(30);

/// Default absolute per-attempt wall-clock cap (seconds): a run going longer than this
/// is reclaimed even while heart-beating (a stuck-but-alive worker). 30 min — the
/// platform default in the technical spec (overridable via `FACTORY_WALL_CLOCK_CAP_SECS`).
const DEFAULT_WALL_CLOCK_CAP_SECS: f64 = 30.0 * 60.0;

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

    // The wall-clock cap is configurable so an operator can tighten/loosen the bound
    // without a rebuild; a malformed value falls back to the default rather than
    // disabling the bound.
    let wall_clock_cap_secs = match std::env::var("FACTORY_WALL_CLOCK_CAP_SECS") {
        Ok(v) => v
            .parse::<f64>()
            .ok()
            .filter(|n| *n > 0.0)
            .unwrap_or_else(|| {
                tracing::warn!(value = %v, default = DEFAULT_WALL_CLOCK_CAP_SECS, "invalid FACTORY_WALL_CLOCK_CAP_SECS; using default");
                DEFAULT_WALL_CLOCK_CAP_SECS
            }),
        Err(_) => DEFAULT_WALL_CLOCK_CAP_SECS,
    };

    tracing::info!(
        tick_secs = COORDINATE_TICK.as_secs(),
        sweeper_tick_secs = SWEEPER_TICK.as_secs(),
        wall_clock_cap_secs,
        "coordinator starting"
    );

    // One task drives both loops: the reactive coordinator tick and the lease sweeper.
    // Both need GitHub (the coordinator for handoffs; the sweeper for escalation
    // comments), so both are skipped together when credentials are absent.
    let mut coordinate = tokio::time::interval(COORDINATE_TICK);
    let mut sweep = tokio::time::interval(SWEEPER_TICK);
    loop {
        tokio::select! {
            _ = coordinate.tick() => {
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
            _ = sweep.tick() => {
                let Some(gh) = gh.as_ref() else {
                    tracing::warn!("skipping sweeper tick: GitHub App credentials are not configured");
                    continue;
                };
                match sweeper_tick(&pool, gh, wall_clock_cap_secs).await {
                    Ok((0, 0)) => tracing::info!("sweeper tick: no expired leases"),
                    Ok((reclaimed, escalated)) => {
                        tracing::info!(reclaimed, escalated, "sweeper tick reclaimed expired leases")
                    }
                    Err(e) => tracing::error!(error = %e, "sweeper tick failed"),
                }
            }
        }
    }
}
