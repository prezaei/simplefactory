//! Cross-cutting telemetry initialization for every Factory service.
//!
//! P0 provides structured `tracing` with an env-driven filter (JSON in prod,
//! pretty locally). The OpenTelemetry → Azure Monitor exporter described in the
//! technical spec is wired in P7; the [`init`] seam stays stable so services
//! don't change when it lands.
#![forbid(unsafe_code)]

use std::sync::Once;

use tracing_subscriber::{fmt, EnvFilter};

static INIT: Once = Once::new();

/// Initialize process-wide telemetry for a service.
///
/// Idempotent: call once at startup. Repeated calls (e.g. across tests in one
/// binary) are no-ops, so this never panics on a double-init.
///
/// Filtering follows `RUST_LOG` (defaults to `info`). Set `FACTORY_LOG_JSON` to
/// emit JSON lines (production); otherwise output is human-readable.
pub fn init(service_name: &str) {
    INIT.call_once(|| {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let builder = fmt().with_env_filter(filter).with_target(true);
        if std::env::var("FACTORY_LOG_JSON").is_ok() {
            builder.json().init();
        } else {
            builder.init();
        }
        tracing::info!(service = service_name, "telemetry initialized");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent_and_emits() {
        // Two calls must not panic (the Once guard makes the second a no-op),
        // and emitting a span/event after init must succeed.
        init("test-a");
        init("test-b");
        let span = tracing::info_span!("smoke");
        let _enter = span.enter();
        tracing::info!(check = true, "telemetry smoke event");
    }
}
