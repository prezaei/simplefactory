//! Coordinator service: the Manager (decide/delegate) + the lease sweeper (P4/P5).
#![forbid(unsafe_code)]

fn main() {
    factory_telemetry::init("coordinator");
    tracing::info!("coordinator: P0 skeleton — manager + sweeper arrive in P4/P5");
}
