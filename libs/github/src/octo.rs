//! The real, network-backed [`GitHubClient`] (P3 live seam), built on octocrab.
//!
//! This is the one part of P3 that can only be exercised against a live GitHub
//! App + repo (it needs the App's private key and makes real API calls), so it is
//! not covered by the in-repo test suite — [`crate::FakeGitHub`] stands in there.
//! The octocrab method bodies are wired during live integration; until then each
//! returns a clear error rather than silently doing nothing.
use crate::client::{
    ChangeRequestView, CheckRef, CheckSummary, CommentTarget, GitHubClient, IssueView,
    OpenPrRequest, PrView, ReviewVerdict,
};
use crate::RepoRef;

/// Real GitHub client backed by a GitHub App installation (octocrab).
pub struct OctocrabGitHub {
    #[allow(dead_code)]
    app_id: u64,
    #[allow(dead_code)]
    private_key_pem: String,
}

impl OctocrabGitHub {
    /// Build from the App credentials in the environment:
    /// `GITHUB_APP_ID` and `GITHUB_APP_PRIVATE_KEY` (PEM).
    pub fn from_env() -> anyhow::Result<Self> {
        let app_id: u64 = std::env::var("GITHUB_APP_ID")
            .map_err(|_| anyhow::anyhow!("GITHUB_APP_ID is required for the real GitHub client"))?
            .parse()
            .map_err(|_| anyhow::anyhow!("GITHUB_APP_ID must be an integer"))?;
        let private_key_pem = std::env::var("GITHUB_APP_PRIVATE_KEY")
            .map_err(|_| anyhow::anyhow!("GITHUB_APP_PRIVATE_KEY (PEM) is required"))?;
        Ok(Self {
            app_id,
            private_key_pem,
        })
    }
}

/// Marker returned by every method until the octocrab calls are wired for the
/// target GitHub deployment. Keeps the live seam loud rather than silent.
fn unwired(op: &str) -> anyhow::Error {
    anyhow::anyhow!("OctocrabGitHub::{op} is wired during live integration (see octo.rs)")
}

#[async_trait::async_trait]
impl GitHubClient for OctocrabGitHub {
    async fn installation_token(&self, _installation_id: i64) -> anyhow::Result<String> {
        Err(unwired("installation_token"))
    }
    async fn read_issue(&self, _repo: &RepoRef, _number: i64) -> anyhow::Result<IssueView> {
        Err(unwired("read_issue"))
    }
    async fn read_change_request(
        &self,
        _repo: &RepoRef,
        _number: i64,
    ) -> anyhow::Result<ChangeRequestView> {
        Err(unwired("read_change_request"))
    }
    async fn open_or_update_pr(
        &self,
        _repo: &RepoRef,
        _req: &OpenPrRequest,
    ) -> anyhow::Result<PrView> {
        Err(unwired("open_or_update_pr"))
    }
    async fn post_comment(
        &self,
        _repo: &RepoRef,
        _target: CommentTarget,
        _body: &str,
    ) -> anyhow::Result<i64> {
        Err(unwired("post_comment"))
    }
    async fn submit_review(
        &self,
        _repo: &RepoRef,
        _number: i64,
        _verdict: ReviewVerdict,
        _body: &str,
    ) -> anyhow::Result<i64> {
        Err(unwired("submit_review"))
    }
    async fn check_status(&self, _repo: &RepoRef, _what: CheckRef) -> anyhow::Result<CheckSummary> {
        Err(unwired("check_status"))
    }
    async fn get_file(
        &self,
        _repo: &RepoRef,
        _path: &str,
        _git_ref: &str,
    ) -> anyhow::Result<Option<String>> {
        Err(unwired("get_file"))
    }
}
