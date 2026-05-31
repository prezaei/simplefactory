//! Intake service: GitHub webhook ingestion + the timer scheduler (P2).
#![forbid(unsafe_code)]

fn main() {
    factory_telemetry::init("intake");
    tracing::info!("intake: P0 skeleton — webhook intake + scheduler arrive in P2");
}
