//! Intake service binary: receives GitHub webhooks, verifies the signature, and
//! hands them to the intake pipeline. Returns fast; all work goes onto the queue.
#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use factory_record::Pool;

struct AppState {
    pool: Pool,
    /// Webhook secret; empty means signature checking is disabled (local only).
    secret: Vec<u8>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("intake");

    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET")
        .unwrap_or_default()
        .into_bytes();
    if secret.is_empty() {
        tracing::warn!("GITHUB_WEBHOOK_SECRET unset — webhook signature checking disabled");
    }

    let pool = factory_record::connect(&database_url).await?;
    factory_record::migrate(&pool).await?;

    let state = Arc::new(AppState { pool, secret });
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/webhooks/github", post(webhook))
        .with_state(state);

    let addr = std::env::var("INTAKE_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "intake listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };

    let signature = header("x-hub-signature-256");
    if !state.secret.is_empty()
        && !factory_github::verify_signature(&state.secret, &body, &signature)
    {
        tracing::warn!("rejected webhook: bad signature");
        return StatusCode::UNAUTHORIZED;
    }

    let event_type = header("x-github-event");
    let delivery = header("x-github-delivery");
    let event = match factory_github::parse_event(&event_type, &body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, event_type, "unparseable webhook payload");
            return StatusCode::BAD_REQUEST;
        }
    };

    match factory_intake::ingest(&state.pool, &event, &delivery).await {
        Ok(outcome) => {
            tracing::info!(?outcome, event_type, delivery, "ingested webhook");
            StatusCode::ACCEPTED
        }
        Err(e) => {
            tracing::error!(error = %e, event_type, delivery, "intake failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
