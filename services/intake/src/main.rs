//! Intake service binary: receives GitHub webhooks, verifies the signature, and
//! hands them to the intake pipeline. Returns fast; all work goes onto the queue.
#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use chrono::Utc;
use factory_record::Pool;

/// How often the in-app scheduler scans for due timers. Replicas may all run it;
/// firing is exactly-once across them.
const SCHEDULER_TICK: Duration = Duration::from_secs(60);

struct AppState {
    pool: Pool,
    /// Webhook secret; empty means signature checking is disabled (local only).
    secret: Vec<u8>,
}

/// Resolve the webhook secret, failing closed. An empty/absent secret disables
/// signature checking, which is allowed ONLY when the operator explicitly opts
/// into the insecure local-dev path (`INTAKE_ALLOW_INSECURE=1`); otherwise this
/// errors so a production misconfiguration can't bring up an unauthenticated
/// webhook endpoint. Returns the secret bytes (empty = checking disabled).
fn resolve_secret(secret: Option<String>, allow_insecure: bool) -> anyhow::Result<Vec<u8>> {
    let bytes = secret.unwrap_or_default().into_bytes();
    if bytes.is_empty() && !allow_insecure {
        anyhow::bail!(
            "GITHUB_WEBHOOK_SECRET is required; set it, or set \
             INTAKE_ALLOW_INSECURE=1 to run without signature checking (local dev only)"
        );
    }
    Ok(bytes)
}

/// Build the webhook router. Extracted so tests can exercise the handler.
fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/webhooks/github", post(webhook))
        .with_state(state)
}

/// Background loop: scan every configured repo for due scheduled work.
async fn run_scheduler(pool: Pool) {
    let mut ticker = tokio::time::interval(SCHEDULER_TICK);
    loop {
        ticker.tick().await;
        match factory_intake::scheduler_tick(&pool, Utc::now()).await {
            Ok(n) if n > 0 => tracing::info!(fired = n, "scheduler fired due work"),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "scheduler tick failed"),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("intake");

    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;
    let secret = resolve_secret(
        std::env::var("GITHUB_WEBHOOK_SECRET").ok(),
        std::env::var("INTAKE_ALLOW_INSECURE").as_deref() == Ok("1"),
    )?;
    if secret.is_empty() {
        tracing::warn!(
            "GITHUB_WEBHOOK_SECRET unset and INTAKE_ALLOW_INSECURE=1 — \
             signature checking DISABLED (local dev only)"
        );
    }

    let pool = factory_record::connect(&database_url).await?;
    factory_record::migrate(&pool).await?;

    // The scheduler runs alongside the webhook server; firing is exactly-once
    // across replicas, so every replica may run it.
    tokio::spawn(run_scheduler(pool.clone()));

    let state = Arc::new(AppState { pool, secret });
    let app = build_router(state);

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

    // Require the GitHub identity headers. A missing delivery id would otherwise
    // be normalized to "" and inserted into the dedupe table, so the next
    // header-less request would be silently treated as a redelivery. Reject up
    // front rather than poisoning dedupe state.
    let event_type = header("x-github-event");
    let delivery = header("x-github-delivery");
    if event_type.is_empty() || delivery.is_empty() {
        tracing::warn!(
            has_event = !event_type.is_empty(),
            has_delivery = !delivery.is_empty(),
            "rejected webhook: missing X-GitHub-Event or X-GitHub-Delivery"
        );
        return StatusCode::BAD_REQUEST;
    }

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
            // A push that touched the repo config triggers an auto-reload. This is
            // best-effort and side-channel to the queue: it must never change the
            // 202 we return for a successfully ingested webhook.
            if let factory_github::WebhookEvent::Push {
                repo,
                head_sha,
                touched_paths,
            } = &event
            {
                reload_config_on_push(&state.pool, repo, head_sha, touched_paths).await;
            }
            StatusCode::ACCEPTED
        }
        Err(e) => {
            tracing::error!(error = %e, event_type, delivery, "intake failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Best-effort config auto-reload for a push. Constructs the live GitHub client
/// from the environment; if credentials are absent (or the apply itself fails)
/// this logs and returns — the webhook ingest already succeeded and must still
/// 202. The file fetch is the live seam: `OctocrabGitHub::get_file` talks to
/// GitHub, so this path is exercised in tests via `FakeGitHub` against the
/// library function directly.
async fn reload_config_on_push(
    pool: &Pool,
    repo: &factory_github::RepoRef,
    head_sha: &str,
    touched_paths: &[String],
) {
    let gh = match factory_github::OctocrabGitHub::from_env() {
        Ok(gh) => gh,
        Err(e) => {
            tracing::warn!(error = %e, "config-on-push skipped: GitHub client unavailable");
            return;
        }
    };
    match factory_intake::apply_config_from_push(pool, &gh, repo, head_sha, touched_paths).await {
        Ok(Some(outcome)) => {
            tracing::info!(?outcome, owner = %repo.owner, repo = %repo.name, "config reloaded from push")
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!(error = %e, owner = %repo.owner, repo = %repo.name, "config-on-push apply failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt; // for `oneshot`

    /// A state whose pool is lazy and never connects — the rejection paths under
    /// test return before any DB access. `secret` empty disables the signature
    /// check (so the 400 paths don't need a valid signature).
    fn state(secret: &[u8]) -> Arc<AppState> {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused-intake-test/db")
            .expect("lazy pool builds without connecting");
        Arc::new(AppState {
            pool,
            secret: secret.to_vec(),
        })
    }

    async fn status_for(secret: &[u8], req: Request<Body>) -> StatusCode {
        build_router(state(secret))
            .oneshot(req)
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn bad_signature_is_401() {
        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-hub-signature-256", "sha256=00")
            .header("x-github-event", "issues")
            .header("x-github-delivery", "d1")
            .body(Body::from("{}"))
            .unwrap();
        // A non-empty secret means the (wrong) signature is checked → rejected.
        assert_eq!(status_for(b"secret", req).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_delivery_header_is_400() {
        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-github-event", "issues")
            // no x-github-delivery
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(status_for(b"", req).await, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn missing_event_header_is_400() {
        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-github-delivery", "d1")
            // no x-github-event
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(status_for(b"", req).await, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unparseable_payload_is_400() {
        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-github-event", "issues")
            .header("x-github-delivery", "d2")
            .body(Body::from("this is not json"))
            .unwrap();
        assert_eq!(status_for(b"", req).await, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn healthz_is_ok() {
        let req = Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_for(b"", req).await, StatusCode::OK);
    }

    #[test]
    fn resolve_secret_fails_closed_unless_opted_in() {
        // A present secret is always accepted.
        assert_eq!(
            resolve_secret(Some("s".into()), false).unwrap(),
            b"s".to_vec()
        );
        // Missing secret WITHOUT the opt-in → error (fail closed).
        assert!(resolve_secret(None, false).is_err());
        assert!(resolve_secret(Some(String::new()), false).is_err());
        // Missing secret WITH the explicit insecure opt-in → empty (checking off).
        assert!(resolve_secret(None, true).unwrap().is_empty());
    }

    fn sign(secret: &[u8], body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    /// A valid signature on a labelled-issue payload for a connected repo →
    /// 202 (exercises the signature-ACCEPT branch + the success path). DB-gated.
    #[tokio::test]
    async fn valid_signed_webhook_is_accepted() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            eprintln!("SKIP: DATABASE_URL not set");
            return;
        };
        let pool = factory_record::connect(&url).await.unwrap();
        factory_record::migrate(&pool).await.unwrap();

        let owner = "acme";
        let name = format!("handler-202-{}", uuid::Uuid::now_v7());
        let repo = factory_record::upsert_repo(&pool, owner, &name, 1)
            .await
            .unwrap();
        let yaml = "version: 1\nsettings:\n  label: factory\nworkers:\n  - name: manager\n    role: manager\n    on: [issue_labeled]\n";
        factory_intake::apply_config(&pool, repo, yaml, "sha")
            .await
            .unwrap();

        let secret = b"webhook-secret";
        let body = serde_json::to_vec(&serde_json::json!({
            "action": "labeled",
            "issue": { "number": 5 },
            "label": { "name": "factory" },
            "repository": { "name": name, "owner": { "login": owner } },
            "installation": { "id": 1 }
        }))
        .unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/github")
            .header("x-hub-signature-256", sign(secret, &body))
            .header("x-github-event", "issues")
            .header("x-github-delivery", format!("d-{}", uuid::Uuid::now_v7()))
            .body(Body::from(body))
            .unwrap();

        let state = Arc::new(AppState {
            pool,
            secret: secret.to_vec(),
        });
        let status = build_router(state).oneshot(req).await.unwrap().status();
        assert_eq!(status, StatusCode::ACCEPTED);
    }
}
