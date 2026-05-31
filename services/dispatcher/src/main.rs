//! Dispatcher service: lease ready work under capacity caps, launch workers (P3).
#![forbid(unsafe_code)]

fn main() {
    factory_telemetry::init("dispatcher");
    tracing::info!("dispatcher: P0 skeleton — leasing + job launch arrive in P3");
}
