//! Factory Tools service binary (P3): the remote MCP server. It is the worker's
//! ONLY channel to Factory. Every request carries a per-task bearer token in the
//! `Authorization` header; the server resolves it to a task, dispatches the named
//! tool, and writes exactly one audit event per authorized call.
//!
//! All HTTP/auth/audit/dispatch logic lives in the library (`build_router`) so the
//! binary is just dependency wiring; tests drive the same router.
#![forbid(unsafe_code)]

use std::sync::Arc;

use factory_record::Pool;
use factory_tools::{build_router, ToolsState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("tools");

    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;
    let pool: Pool = factory_record::connect(&database_url).await?;
    factory_record::migrate(&pool).await?;

    // The real GitHub client needs App credentials. If they're absent (local dev),
    // log and continue with a client that errors on use — the server still serves
    // /healthz and the DB-only tools, and a misconfig is loud rather than silent.
    let gh: Arc<dyn factory_github::GitHubClient> = match factory_github::OctocrabGitHub::from_env()
    {
        Ok(client) => Arc::new(client),
        Err(e) => {
            tracing::warn!(error = %e, "GitHub credentials absent; GitHub-backed tools will error until configured");
            Arc::new(UnconfiguredGitHub)
        }
    };

    // Transcripts persist to Azure Blob (Azurite locally in P7, real Azure in P8) —
    // the SOLE production store, same Blob backend as worker checkpoints. Required:
    // there is no filesystem fallback (a dev/prod fork we deliberately removed).
    let blob_conn = std::env::var("FACTORY_BLOB_CONNECTION_STRING")
        .map_err(|_| anyhow::anyhow!("FACTORY_BLOB_CONNECTION_STRING is required"))?;
    let blob: Arc<dyn factory_blob::TranscriptStore> = Arc::new(
        factory_blob::AzureBlobTranscripts::connect(
            &blob_conn,
            factory_blob::DEFAULT_TRANSCRIPT_CONTAINER,
        )
        .await?,
    );

    let state = Arc::new(ToolsState { pool, gh, blob });
    let app = build_router(state);

    let addr = std::env::var("TOOLS_ADDR").unwrap_or_else(|_| "0.0.0.0:8081".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "tools listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// A GitHub client used only when App credentials are absent (local dev). Every
/// GitHub-backed call errors loudly rather than silently doing nothing; the DB-only
/// tools (heartbeat, record_outcome, append_transcript) and /healthz still work.
struct UnconfiguredGitHub;

fn no_creds(op: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "GitHub credentials absent: {op} unavailable (set GITHUB_APP_ID / GITHUB_APP_PRIVATE_KEY)"
    )
}

#[async_trait::async_trait]
impl factory_github::GitHubClient for UnconfiguredGitHub {
    async fn installation_token(&self, _installation_id: i64) -> anyhow::Result<String> {
        Err(no_creds("installation_token"))
    }
    async fn read_issue(
        &self,
        _repo: &factory_github::RepoRef,
        _number: i64,
    ) -> anyhow::Result<factory_github::IssueView> {
        Err(no_creds("read_issue"))
    }
    async fn read_change_request(
        &self,
        _repo: &factory_github::RepoRef,
        _number: i64,
    ) -> anyhow::Result<factory_github::ChangeRequestView> {
        Err(no_creds("read_change_request"))
    }
    async fn open_or_update_pr(
        &self,
        _repo: &factory_github::RepoRef,
        _req: &factory_github::OpenPrRequest,
    ) -> anyhow::Result<factory_github::PrView> {
        Err(no_creds("open_or_update_pr"))
    }
    async fn post_comment(
        &self,
        _repo: &factory_github::RepoRef,
        _target: factory_github::CommentTarget,
        _body: &str,
    ) -> anyhow::Result<i64> {
        Err(no_creds("post_comment"))
    }
    async fn update_comment(
        &self,
        _repo: &factory_github::RepoRef,
        _comment_id: i64,
        _body: &str,
    ) -> anyhow::Result<()> {
        Err(no_creds("update_comment"))
    }
    async fn assign_issue(
        &self,
        _repo: &factory_github::RepoRef,
        _issue_number: i64,
        _assignee: &str,
    ) -> anyhow::Result<()> {
        Err(no_creds("assign_issue"))
    }
    async fn submit_review(
        &self,
        _repo: &factory_github::RepoRef,
        _number: i64,
        _verdict: factory_github::ReviewVerdict,
        _body: &str,
    ) -> anyhow::Result<i64> {
        Err(no_creds("submit_review"))
    }
    async fn check_status(
        &self,
        _repo: &factory_github::RepoRef,
        _what: factory_github::CheckRef,
    ) -> anyhow::Result<factory_github::CheckSummary> {
        Err(no_creds("check_status"))
    }
    async fn get_file(
        &self,
        _repo: &factory_github::RepoRef,
        _path: &str,
        _git_ref: &str,
    ) -> anyhow::Result<Option<String>> {
        Err(no_creds("get_file"))
    }
}
