//! Dashboard API service: read-only SSE projections of the record book (P6).
#![forbid(unsafe_code)]

fn main() {
    factory_telemetry::init("dashboard-api");
    tracing::info!("dashboard-api: P0 skeleton — read-only SSE view arrives in P6");
}
