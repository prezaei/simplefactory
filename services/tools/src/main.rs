//! Factory Tools service: the MCP server that is the worker's only channel (P3).
#![forbid(unsafe_code)]

fn main() {
    factory_telemetry::init("tools");
    tracing::info!("tools: P0 skeleton — MCP tool surface arrives in P3/P4");
}
