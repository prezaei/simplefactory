//! Dashboard API service binary (P6): a read-only Axum server that projects the
//! record book for humans and streams an eventually-consistent live view over SSE.
//!
//! All routing/query logic lives in the library (`build_router`); the binary is
//! just dependency wiring (telemetry, DB pool, listener), so tests drive the same
//! router via `tower::ServiceExt::oneshot`.
#![forbid(unsafe_code)]

use factory_dashboard_api::{build_router, sse_interval_from_env, DashboardState};
use factory_record::Pool;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("dashboard-api");

    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;
    let pool: Pool = factory_record::connect(&database_url).await?;
    // migrate() is idempotent; the dashboard never writes app rows, but it shares
    // the schema and migrating keeps a fresh DB usable for local runs.
    factory_record::migrate(&pool).await?;

    let sse_interval = sse_interval_from_env();
    tracing::info!(
        interval_ms = sse_interval.as_millis() as u64,
        "SSE snapshot cadence"
    );

    let state = DashboardState { pool, sse_interval };
    let app = build_router(state);

    let addr = std::env::var("DASHBOARD_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "dashboard-api listening (read-only)");
    axum::serve(listener, app).await?;
    Ok(())
}
