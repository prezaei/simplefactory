//! The real, network-backed [`GitHubClient`] (P3 live seam), built on octocrab.
//!
//! This is the one part of P3 that can only be exercised against a live GitHub
//! App + repo (it needs the App's private key and makes real API calls), so it is
//! not covered by the in-repo test suite — [`crate::FakeGitHub`] stands in there.
//!
//! Status of the octocrab wiring (octocrab 0.51):
//! Every data-plane operation funnels through a GitHub *App* JWT client, built
//! with `Octocrab::builder().app(AppId, jsonwebtoken::EncodingKey)`. `secrecy`
//! and `jsonwebtoken` are now direct dependencies, so the App/installation client
//! can be constructed and the methods are wired for real. The lone exception is
//! [`OctocrabGitHub::check_status`]: octocrab 0.51's `combined_status_for_ref`
//! only accepts `params::repos::Reference`, which has just `Branch`/`Tag`
//! variants (no `Commit`) and always prefixes the ref with `heads/`/`tags/`, so
//! a raw commit SHA cannot be passed through it cleanly. It therefore stays
//! `unwired` and `read_change_request` reports checks as `Pending`.
use secrecy::ExposeSecret;

use crate::client::{
    ChangeRequestView, CheckConclusion, CheckRef, CheckSummary, CommentTarget, GitHubClient,
    IssueView, OpenPrRequest, PrView, ReviewVerdict,
};
use crate::RepoRef;

/// Real GitHub client backed by a GitHub App installation (octocrab).
pub struct OctocrabGitHub {
    app_id: u64,
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

    /// Parse the App private key (PEM) into a `jsonwebtoken::EncodingKey`.
    fn encoding_key(&self) -> anyhow::Result<jsonwebtoken::EncodingKey> {
        jsonwebtoken::EncodingKey::from_rsa_pem(self.private_key_pem.as_bytes())
            .map_err(|e| anyhow::anyhow!("invalid GITHUB_APP_PRIVATE_KEY: {e}"))
    }

    /// Build the App JWT client (authenticates as the GitHub App itself).
    fn app_client(&self) -> anyhow::Result<octocrab::Octocrab> {
        octocrab::Octocrab::builder()
            .app(octocrab::models::AppId(self.app_id), self.encoding_key()?)
            .build()
            .map_err(|e| anyhow::anyhow!("building GitHub App client: {e}"))
    }

    /// Build an installation-scoped client (authenticates as the installation).
    async fn installation(&self, installation_id: i64) -> anyhow::Result<octocrab::Octocrab> {
        let app = self.app_client()?;
        let (client, _token) = app
            .installation_and_token(octocrab::models::InstallationId(installation_id as u64))
            .await
            .map_err(|e| anyhow::anyhow!("minting installation client: {e}"))?;
        Ok(client)
    }
}

/// Marker for the one operation that cannot be wired cleanly against octocrab
/// 0.51 (`check_status`; see the module docs). Keeps the live seam loud rather
/// than silently returning a wrong result.
fn unwired(op: &str) -> anyhow::Error {
    anyhow::anyhow!("OctocrabGitHub::{op} is not wired for octocrab 0.51 (see octo.rs)")
}

#[async_trait::async_trait]
impl GitHubClient for OctocrabGitHub {
    async fn installation_token(&self, installation_id: i64) -> anyhow::Result<String> {
        tracing::info!(installation_id, "minting GitHub installation token");
        let app = self.app_client()?;
        let (_client, token) = app
            .installation_and_token(octocrab::models::InstallationId(installation_id as u64))
            .await
            .map_err(|e| anyhow::anyhow!("minting installation token: {e}"))?;
        Ok(token.expose_secret().to_owned())
    }

    async fn read_issue(&self, repo: &RepoRef, number: i64) -> anyhow::Result<IssueView> {
        tracing::info!(owner = %repo.owner, name = %repo.name, number, "reading issue");
        let issue = self
            .installation(repo.installation_id)
            .await?
            .issues(&repo.owner, &repo.name)
            .get(number as u64)
            .await
            .map_err(|e| anyhow::anyhow!("reading issue #{number}: {e}"))?;
        Ok(IssueView {
            number: issue.number as i64,
            title: issue.title,
            body: issue.body.unwrap_or_default(),
            comments: vec![],
        })
    }

    async fn read_change_request(
        &self,
        repo: &RepoRef,
        number: i64,
    ) -> anyhow::Result<ChangeRequestView> {
        tracing::info!(owner = %repo.owner, name = %repo.name, number, "reading change request");
        let pr = self
            .installation(repo.installation_id)
            .await?
            .pulls(&repo.owner, &repo.name)
            .get(number as u64)
            .await
            .map_err(|e| anyhow::anyhow!("reading change request #{number}: {e}"))?;
        let head_sha = pr.head.sha.clone();
        // diff/files/reviews are best-effort empty; checks would come from
        // `check_status`, which is unwired on octocrab 0.51 (see module docs), so
        // report Pending.
        Ok(ChangeRequestView {
            number,
            head_sha,
            diff: String::new(),
            files: vec![],
            reviews: vec![],
            checks: CheckSummary {
                overall: CheckConclusion::Pending,
                checks: vec![],
            },
        })
    }

    async fn open_or_update_pr(
        &self,
        repo: &RepoRef,
        req: &OpenPrRequest,
    ) -> anyhow::Result<PrView> {
        tracing::info!(
            owner = %repo.owner,
            name = %repo.name,
            head_branch = %req.head_branch,
            base = %req.base,
            "opening or updating change request",
        );
        let client = self.installation(repo.installation_id).await?;
        // Idempotent by head branch: reuse an existing open PR for this branch.
        let existing = client
            .pulls(&repo.owner, &repo.name)
            .list()
            .state(octocrab::params::State::Open)
            .head(format!("{}:{}", repo.owner, req.head_branch))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("listing change requests: {e}"))?;
        if let Some(it) = existing.items.into_iter().next() {
            tracing::info!(number = it.number, "reusing existing change request");
            return Ok(PrView {
                number: it.number as i64,
                url: it.html_url.to_string(),
            });
        }
        let pr = client
            .pulls(&repo.owner, &repo.name)
            .create(&req.title, &req.head_branch, &req.base)
            .body(&req.body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("creating change request: {e}"))?;
        tracing::info!(number = pr.number, "created change request");
        Ok(PrView {
            number: pr.number as i64,
            url: pr.html_url.to_string(),
        })
    }

    async fn post_comment(
        &self,
        repo: &RepoRef,
        target: CommentTarget,
        body: &str,
    ) -> anyhow::Result<i64> {
        // PR numbers are valid issue numbers, so both targets go through the
        // issues comment endpoint.
        let number = match target {
            CommentTarget::Issue(n) => n,
            CommentTarget::ChangeRequest(n) => n,
        };
        tracing::info!(owner = %repo.owner, name = %repo.name, number, "posting comment");
        let comment = self
            .installation(repo.installation_id)
            .await?
            .issues(&repo.owner, &repo.name)
            .create_comment(number as u64, body)
            .await
            .map_err(|e| anyhow::anyhow!("posting comment on #{number}: {e}"))?;
        Ok(comment.id.0 as i64)
    }

    async fn update_comment(
        &self,
        repo: &RepoRef,
        comment_id: i64,
        body: &str,
    ) -> anyhow::Result<()> {
        tracing::info!(owner = %repo.owner, name = %repo.name, comment_id, "updating comment");
        self.installation(repo.installation_id)
            .await?
            .issues(&repo.owner, &repo.name)
            .update_comment(octocrab::models::CommentId(comment_id as u64), body)
            .await
            .map_err(|e| anyhow::anyhow!("updating comment {comment_id}: {e}"))?;
        Ok(())
    }

    async fn submit_review(
        &self,
        repo: &RepoRef,
        number: i64,
        verdict: ReviewVerdict,
        body: &str,
    ) -> anyhow::Result<i64> {
        tracing::info!(
            owner = %repo.owner,
            name = %repo.name,
            number,
            verdict = verdict.as_str(),
            "submitting review",
        );
        let client = self.installation(repo.installation_id).await?;
        // The review must pin the head commit it reviewed.
        let pr = client
            .pulls(&repo.owner, &repo.name)
            .get(number as u64)
            .await
            .map_err(|e| anyhow::anyhow!("reading change request #{number} for review: {e}"))?;
        let head_sha = pr.head.sha.clone();
        let event = match verdict {
            ReviewVerdict::Approve => octocrab::models::pulls::ReviewAction::Approve,
            ReviewVerdict::RequestChanges => octocrab::models::pulls::ReviewAction::RequestChanges,
            ReviewVerdict::Comment => octocrab::models::pulls::ReviewAction::Comment,
        };
        // `pull_number().reviews().create_review(..)` is the only route octocrab
        // 0.51 exposes for *creating* a review; `pull_number` carries a
        // deprecation note but its replacements only act on an existing review,
        // so the allow is scoped tightly to this call.
        #[allow(deprecated)]
        let review = client
            .pulls(&repo.owner, &repo.name)
            .pull_number(number as u64)
            .reviews()
            .create_review(head_sha, body, event, vec![])
            .await
            .map_err(|e| anyhow::anyhow!("submitting review on #{number}: {e}"))?;
        Ok(review.id.0 as i64)
    }

    async fn check_status(&self, repo: &RepoRef, what: CheckRef) -> anyhow::Result<CheckSummary> {
        // octocrab 0.51's `combined_status_for_ref` only accepts
        // `params::repos::Reference` (Branch|Tag, always prefixed `heads/`/
        // `tags/`), so a raw commit SHA cannot be passed cleanly. Left unwired
        // rather than silently querying the wrong ref (see module docs).
        tracing::info!(owner = %repo.owner, name = %repo.name, ?what, "check_status is unwired");
        Err(unwired("check_status"))
    }

    async fn get_file(
        &self,
        repo: &RepoRef,
        path: &str,
        git_ref: &str,
    ) -> anyhow::Result<Option<String>> {
        tracing::info!(owner = %repo.owner, name = %repo.name, path, git_ref, "fetching file");
        let result = self
            .installation(repo.installation_id)
            .await?
            .repos(&repo.owner, &repo.name)
            .get_content()
            .path(path)
            .r#ref(git_ref)
            .send()
            .await;
        match result {
            Ok(items) => match items.items.into_iter().next() {
                Some(item) => Ok(item.decoded_content()),
                None => Ok(None),
            },
            Err(octocrab::Error::GitHub { source, .. }) if source.status_code.as_u16() == 404 => {
                tracing::info!(path, git_ref, "file not found");
                Ok(None)
            }
            Err(e) => Err(anyhow::anyhow!("fetching file {path}@{git_ref}: {e}")),
        }
    }
}
