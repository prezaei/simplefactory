//! Worker harness: the throwaway workroom that drives Claude Code headless (P3),
//! with continuous checkpoint/resume (P5).
#![forbid(unsafe_code)]

fn main() {
    factory_telemetry::init("worker");
    tracing::info!("worker: P0 skeleton — clone + Claude Code + checkpoints arrive in P3/P5");
}
