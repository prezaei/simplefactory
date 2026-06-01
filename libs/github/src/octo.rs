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
//! can be constructed and every method is wired for real, including
//! [`OctocrabGitHub::check_status`]: it reads the Checks API via
//! `checks().list_check_runs_for_git_ref(..)` (which accepts a raw commit SHA as a
//! `Commitish`) AND folds in the legacy combined commit status for the same SHA, read
//! via a raw REST GET to `/repos/{owner}/{name}/commits/{sha}/status` (FIX B —
//! `combined_status_for_ref` only accepts a `heads/`/`tags/`-prefixed
//! `params::repos::Reference`, not a raw SHA). It uses that endpoint's server-computed
//! top-level `state` (authoritative over every context, pagination-immune) and merges it
//! with the check runs before the rollup, so an empty Checks API result is never treated
//! as green when a failing status exists.
//! `read_change_request` populates the changed-file list (the coordinator's
//! can't-weaken-own-gating guard depends on it) AND the unified diff (the Reviewer's
//! primary signal) — the diff is BEST-EFFORT (a fetch failure/406 doesn't fail the call,
//! so gating is never blocked by a diff problem) and size-capped. CR-view `checks` stay
//! `Pending` (the coordinator reads checks via `check_status` directly, off this path).
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
            private_key_pem: normalize_pem(&private_key_pem),
        })
    }

    /// Parse the App private key (PEM) into a `jsonwebtoken::EncodingKey`.
    fn encoding_key(&self) -> anyhow::Result<jsonwebtoken::EncodingKey> {
        jsonwebtoken::EncodingKey::from_rsa_pem(self.private_key_pem.as_bytes())
            .map_err(|e| anyhow::anyhow!("invalid GITHUB_APP_PRIVATE_KEY: {e:?}"))
    }

    /// Build the App JWT client (authenticates as the GitHub App itself).
    fn app_client(&self) -> anyhow::Result<octocrab::Octocrab> {
        octocrab::Octocrab::builder()
            .app(octocrab::models::AppId(self.app_id), self.encoding_key()?)
            .build()
            .map_err(|e| anyhow::anyhow!("building GitHub App client: {e:?}"))
    }

    /// Build an installation-scoped client (authenticates as the installation).
    async fn installation(&self, installation_id: i64) -> anyhow::Result<octocrab::Octocrab> {
        let installation_id = installation_id_u64(installation_id)?;
        let app = self.app_client()?;
        let (client, _token) = app
            .installation_and_token(octocrab::models::InstallationId(installation_id))
            .await
            .map_err(|e| anyhow::anyhow!("minting installation client: {e:?}"))?;
        Ok(client)
    }
}

/// Cap on the CR diff body carried to the reviewer (keeps a huge generated/lockfile
/// diff from blowing up the model context / MCP payload). 256 KiB is ample for a
/// review signal; larger is truncated with a marker.
const MAX_DIFF_BYTES: usize = 256 * 1024;

/// Cap the diff BODY at `max` bytes (truncating on a UTF-8 char boundary) plus a short
/// trailing truncation marker — so the returned string is `max` + the marker length, not
/// strictly `max`. Returns the diff unchanged when the body is within budget. The marker
/// length is a small constant, so this still bounds the worst case to ~`max`.
fn cap_diff(diff: String, max: usize) -> String {
    if diff.len() <= max {
        return diff;
    }
    let mut end = max;
    while end > 0 && !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n... [diff truncated at {max} bytes of {} total]",
        &diff[..end],
        diff.len()
    )
}

/// Normalize a PEM read from the environment. A multi-line PEM can't live on one
/// `env_file`/`KEY=VALUE` line, so the documented format (and how secret stores like
/// Key Vault often surface it) is a single line with literal `\n` escapes. Turn those
/// back into real newlines so `jsonwebtoken::from_rsa_pem` can parse it; a PEM that
/// already has real newlines is unchanged (it contains no literal backslash-n).
fn normalize_pem(raw: &str) -> String {
    raw.replace("\\n", "\n")
}

/// Guard the `installation_id` before casting it to the `u64` octocrab expects. A
/// non-positive id is never valid (installation ids are positive) and an unchecked
/// `as u64` would silently wrap a negative value into a huge id; reject it with a
/// clear error instead.
fn installation_id_u64(installation_id: i64) -> anyhow::Result<u64> {
    if installation_id <= 0 {
        return Err(anyhow::anyhow!(
            "invalid installation_id: {installation_id}"
        ));
    }
    u64::try_from(installation_id)
        .map_err(|_| anyhow::anyhow!("invalid installation_id: {installation_id}"))
}

/// Guard an externally-supplied GitHub object number / id (issue, PR, comment)
/// before casting it to the `u64` octocrab expects. A negative value would wrap
/// under an unchecked `as u64`; reject it with a clear error instead.
fn id_u64(n: i64) -> anyhow::Result<u64> {
    u64::try_from(n).map_err(|_| anyhow::anyhow!("invalid id: {n}"))
}

/// Map a GitHub check-run `conclusion` string to our [`CheckConclusion`]. A run with
/// no conclusion yet (queued/in_progress → `None`) is `Pending`. Per the GitHub
/// Checks API: success → Success; failure/timed_out/cancelled/action_required/
/// startup_failure → Failure (`startup_failure` is a terminal error — the run never
/// got off the ground); neutral/skipped → Neutral; `stale` → Pending (a stale run is
/// superseded by a newer run for the same ref, so it is not itself a terminal verdict);
/// anything else (including unknown future values) is conservatively `Pending` so an
/// unrecognized state can never be read as green.
fn map_conclusion(conclusion: Option<&str>) -> CheckConclusion {
    match conclusion {
        Some("success") => CheckConclusion::Success,
        Some("failure")
        | Some("timed_out")
        | Some("cancelled")
        | Some("action_required")
        | Some("startup_failure") => CheckConclusion::Failure,
        Some("neutral") | Some("skipped") => CheckConclusion::Neutral,
        _ => CheckConclusion::Pending,
    }
}

/// The minimal shape of the combined commit-status REST response
/// (`GET /repos/{owner}/{name}/commits/{sha}/status`). We read the SERVER-computed
/// top-level `state` (authoritative over EVERY context, so it cannot miss a failing
/// status on a later page — unlike paging the `statuses` array, which defaults to 30)
/// plus `total_count` to tell "no statuses at all" (which must stay green) apart from a
/// pending/failing rollup. The rest of the (large) payload is ignored (FIX B).
#[derive(serde::Deserialize)]
struct CombinedStatus {
    state: String,
    total_count: u64,
}

/// Map a combined commit-status `state` to our [`CheckConclusion`] (FIX B). The Statuses
/// API uses a small fixed vocabulary: success → Success; failure/error → Failure;
/// pending → Pending. Anything unrecognized is conservatively `Pending` so an unknown
/// state can never be read as green.
fn map_status_state(state: &str) -> CheckConclusion {
    match state {
        "success" => CheckConclusion::Success,
        "failure" | "error" => CheckConclusion::Failure,
        _ => CheckConclusion::Pending,
    }
}

/// True when an octocrab error is a GitHub **403 permission denial** — the App lacks a
/// permission for this read (e.g. `Commit statuses: read` →
/// "Resource not accessible by integration"). DISTINCT from a transient error: a 403
/// won't fix itself on retry, so the caller must DEGRADE rather than loop on it forever.
/// (`Error::GitHub`/`GitHubError` are `#[non_exhaustive]`, so this matches by shape — it
/// cannot be unit-tested by constructing one; the pure policy lives in
/// [`combined_status_contribution`].)
fn is_permission_denied(e: &octocrab::Error) -> bool {
    matches!(
        e,
        octocrab::Error::GitHub { source, .. } if source.status_code.as_u16() == 403
    )
}

/// The legacy-combined-commit-status contribution to the check rollup (FIX B). Pure so
/// the permission-degradation policy is unit-tested without real GitHub:
/// - `permission_denied` → `Some(Failure)`: the App can't read commit statuses, so we
///   CANNOT confirm there is no failing status. Contributing `Failure` keeps the CR off
///   the false-green path (the coordinator escalates it to a human) instead of the read
///   hard-failing the whole reaction on every tick forever.
/// - otherwise a status with ≥1 context folds its server-computed rollup `state`.
/// - a status with ZERO contexts contributes nothing — GitHub reports `pending` for a
///   SHA with no statuses, so the empty case must stay green (FIX 2).
fn combined_status_contribution(
    status: Option<&CombinedStatus>,
    permission_denied: bool,
) -> Option<CheckConclusion> {
    if permission_denied {
        return Some(CheckConclusion::Failure);
    }
    match status {
        Some(s) if s.total_count > 0 => Some(map_status_state(&s.state)),
        _ => None,
    }
}

#[async_trait::async_trait]
impl GitHubClient for OctocrabGitHub {
    async fn installation_token(&self, installation_id: i64) -> anyhow::Result<String> {
        tracing::info!(installation_id, "minting GitHub installation token");
        let installation_id = installation_id_u64(installation_id)?;
        let app = self.app_client()?;
        let (_client, token) = app
            .installation_and_token(octocrab::models::InstallationId(installation_id))
            .await
            .map_err(|e| anyhow::anyhow!("minting installation token: {e:?}"))?;
        Ok(token.expose_secret().to_owned())
    }

    async fn read_issue(&self, repo: &RepoRef, number: i64) -> anyhow::Result<IssueView> {
        tracing::info!(owner = %repo.owner, name = %repo.name, number, "reading issue");
        let issue = self
            .installation(repo.installation_id)
            .await?
            .issues(&repo.owner, &repo.name)
            .get(id_u64(number)?)
            .await
            .map_err(|e| anyhow::anyhow!("reading issue #{number}: {e:?}"))?;
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
        let client = self.installation(repo.installation_id).await?;
        let number_u64 = id_u64(number)?;
        let pr = client
            .pulls(&repo.owner, &repo.name)
            .get(number_u64)
            .await
            .map_err(|e| anyhow::anyhow!("reading change request #{number}: {e:?}"))?;
        let head_sha = pr.head.sha.clone();

        // FIX 5: populate the changed-file list for real. The coordinator's
        // can't-weaken-own-gating guard inspects `files`; with it stubbed empty the guard
        // was DEAD against real GitHub (a CR editing `.github/workflows/` was handed off as
        // green). `list_files` returns a `Page<DiffEntry>`, so PAGINATE with `all_pages` —
        // a gating file on page 2 must never be missed.
        let first = client
            .pulls(&repo.owner, &repo.name)
            .list_files(number_u64)
            .await
            .map_err(|e| anyhow::anyhow!("listing files for change request #{number}: {e:?}"))?;
        let files: Vec<String> = client
            .all_pages(first)
            .await
            .map_err(|e| anyhow::anyhow!("paginating files for change request #{number}: {e:?}"))?
            .into_iter()
            .map(|entry| entry.filename)
            .collect();
        tracing::info!(owner = %repo.owner, name = %repo.name, number, files = files.len(), "read change request files");

        // The unified diff is the Reviewer's PRIMARY signal (it reviews from the CR view,
        // not a local clone), so populate it. BEST-EFFORT, for two reasons: (1) the
        // coordinator's gating guard reads only `files`/`head_sha`, so a diff problem
        // must never fail the whole read and block approval; (2) GitHub returns 406 for
        // very large diffs. On any error, review/gate proceed without the diff body.
        // CAP the size so a huge (e.g. generated/lockfile) diff can't blow up the
        // reviewer's context / MCP payload.
        let diff = match client
            .pulls(&repo.owner, &repo.name)
            .get_diff(number_u64)
            .await
        {
            Ok(d) => cap_diff(d, MAX_DIFF_BYTES),
            Err(e) => {
                tracing::warn!(number, error = %e, "fetching CR diff failed; proceeding without it");
                String::new()
            }
        };
        tracing::info!(owner = %repo.owner, name = %repo.name, number, diff_bytes = diff.len(), "read change request diff");

        // `reviews` stays best-effort empty; `checks` stays the `Pending` stub here
        // because the coordinator reads checks via `check_status` directly (not this
        // CR-view rollup), so the CR-view checks are off the hot approval path. `files`
        // and `diff` are REAL — the gating guard depends on `files`, the reviewer on `diff`.
        Ok(ChangeRequestView {
            number,
            head_sha,
            head_branch: pr.head.ref_field.clone(),
            diff,
            files,
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
            .map_err(|e| anyhow::anyhow!("listing change requests: {e:?}"))?;
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
            .map_err(|e| anyhow::anyhow!("creating change request: {e:?}"))?;
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
            .create_comment(id_u64(number)?, body)
            .await
            .map_err(|e| anyhow::anyhow!("posting comment on #{number}: {e:?}"))?;
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
            .update_comment(octocrab::models::CommentId(id_u64(comment_id)?), body)
            .await
            .map_err(|e| anyhow::anyhow!("updating comment {comment_id}: {e:?}"))?;
        Ok(())
    }

    async fn assign_issue(
        &self,
        repo: &RepoRef,
        issue_number: i64,
        assignee: &str,
    ) -> anyhow::Result<()> {
        tracing::info!(owner = %repo.owner, name = %repo.name, issue_number, assignee, "assigning issue");
        self.installation(repo.installation_id)
            .await?
            .issues(&repo.owner, &repo.name)
            .add_assignees(id_u64(issue_number)?, &[assignee])
            .await
            .map_err(|e| anyhow::anyhow!("assigning issue #{issue_number} to {assignee}: {e:?}"))?;
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
        let number_u64 = id_u64(number)?;
        // The review must pin the head commit it reviewed.
        let pr = client
            .pulls(&repo.owner, &repo.name)
            .get(number_u64)
            .await
            .map_err(|e| anyhow::anyhow!("reading change request #{number} for review: {e:?}"))?;
        let head_sha = pr.head.sha.clone();
        use octocrab::models::pulls::ReviewAction;
        let event = match verdict {
            ReviewVerdict::Approve => ReviewAction::Approve,
            ReviewVerdict::RequestChanges => ReviewAction::RequestChanges,
            ReviewVerdict::Comment => ReviewAction::Comment,
        };
        // `pull_number().reviews().create_review(..)` is the only route octocrab 0.51
        // exposes for *creating* a review; `pull_number` carries a deprecation note but
        // its replacements only act on an existing review, so the allow is scoped tightly.
        let create = |evt: ReviewAction, text: String| {
            let client = client.clone();
            let (owner, name, sha) = (repo.owner.clone(), repo.name.clone(), head_sha.clone());
            async move {
                #[allow(deprecated)]
                client
                    .pulls(&owner, &name)
                    .pull_number(number_u64)
                    .reviews()
                    .create_review(sha, text, evt, vec![])
                    .await
            }
        };
        let review = match create(event, body.to_string()).await {
            Ok(r) => r,
            // GitHub forbids APPROVE/REQUEST_CHANGES on your OWN pull request, and the
            // Factory App authors the CR (as the coder) — so the reviewer (same App) can
            // only COMMENT. Fall back to a COMMENT review carrying the verdict in the body;
            // the structured verdict still reaches the coordinator via the CR factory_state.
            Err(e) if !matches!(verdict, ReviewVerdict::Comment) => {
                tracing::warn!(number, verdict = verdict.as_str(), error = %e, "review event rejected (likely self-review on the App's own PR); retrying as a COMMENT review with the verdict in the body");
                let annotated = format!("**Reviewer verdict: {}**\n\n{}", verdict.as_str(), body);
                create(ReviewAction::Comment, annotated)
                    .await
                    .map_err(|e2| {
                        anyhow::anyhow!("submitting review on #{number} (comment fallback): {e2}")
                    })?
            }
            Err(e) => return Err(anyhow::anyhow!("submitting review on #{number}: {e:?}")),
        };
        Ok(review.id.0 as i64)
    }

    async fn check_status(&self, repo: &RepoRef, what: CheckRef) -> anyhow::Result<CheckSummary> {
        tracing::info!(owner = %repo.owner, name = %repo.name, ?what, "reading check status");
        let client = self.installation(repo.installation_id).await?;
        // Resolve the commit SHA to read checks for. A `CheckRef::Sha` is used
        // directly; a `CheckRef::ChangeRequest(n)` is resolved to the PR head SHA.
        let sha = match what {
            CheckRef::Sha(s) => s,
            CheckRef::ChangeRequest(n) => {
                let pr = client
                    .pulls(&repo.owner, &repo.name)
                    .get(id_u64(n)?)
                    .await
                    .map_err(|e| anyhow::anyhow!("reading change request #{n} head sha: {e:?}"))?;
                pr.head.sha
            }
        };

        // octocrab 0.51's Checks API: list every check run for the commit ref. A raw
        // SHA is a valid `Commitish`, so this reads the check runs cleanly (unlike
        // `combined_status_for_ref`, which only accepts a `heads/`/`tags/` Reference).
        //
        // FIX 1: page through EVERY check run, not just the first page (~30). The
        // `check-runs` endpoint returns a `total_count` plus the page's `check_runs`;
        // it is NOT a `Page<T>` wrapper (no `next` link octocrab can follow with
        // `all_pages`), so we drive pagination ourselves: request 100 per page and keep
        // fetching successive pages until we have collected `total_count` runs. Rolling
        // up over only the first page could read a red check on page 2 as green.
        let mut checks: Vec<(String, CheckConclusion)> = Vec::new();
        let mut page: u32 = 1;
        loop {
            let runs = client
                .checks(&repo.owner, &repo.name)
                .list_check_runs_for_git_ref(octocrab::params::repos::Commitish(sha.clone()))
                .per_page(100u8)
                .page(page)
                .send()
                .await
                .map_err(|e| {
                    anyhow::anyhow!("listing check runs for {sha} (page {page}): {e:?}")
                })?;
            let total = runs.total_count;
            // Map each check run's conclusion to a `CheckConclusion`. A run with no
            // conclusion yet (queued/in_progress) is `Pending`; an unknown conclusion
            // string is treated conservatively as `Pending` so an unrecognized state can
            // never be mistaken for green.
            let page_empty = runs.check_runs.is_empty();
            checks.extend(
                runs.check_runs
                    .into_iter()
                    .map(|run| (run.name, map_conclusion(run.conclusion.as_deref()))),
            );
            // Stop once we have every run (or a short/empty page guards against a
            // mismatched total so this can never loop forever).
            if page_empty || checks.len() as u64 >= total {
                break;
            }
            page += 1;
        }

        // FIX B: the Checks API (above) does NOT see legacy *commit statuses* (the older
        // `/status` API many CI providers still post to). An empty Checks API result is
        // NOT "green" when failing statuses exist — so ALSO fold in the combined commit
        // status for the same SHA. `combined_status_for_ref` only accepts a `heads/`/
        // `tags/` Reference (not a raw SHA), so we read the REST endpoint directly. We use
        // the SERVER-computed top-level `state` (a single authoritative rollup over EVERY
        // status context) rather than paging the `statuses` array — so a failing status on
        // a later page can never be missed (the false-green a first-page-only read had).
        let route = format!("/repos/{}/{}/commits/{}/status", repo.owner, repo.name, sha);
        let status_result: Result<CombinedStatus, octocrab::Error> =
            client.get(&route, None::<&()>).await;
        // Degrade on a PERMISSION 403 (App missing `Commit statuses: read`): treat the
        // status as NOT-green so the coordinator escalates the CR to a human, rather than
        // (a) hard-failing the whole reaction every tick forever, or (b) silently
        // dropping the legacy-status gate and risking a false green. A non-permission
        // error (transient/5xx) still propagates so the next tick retries. See
        // [`combined_status_contribution`].
        let (status_opt, permission_denied) = match status_result {
            Ok(s) => (Some(s), false),
            Err(e) if is_permission_denied(&e) => {
                tracing::warn!(
                    owner = %repo.owner, name = %repo.name, %sha,
                    "combined commit status unreadable (403 — App missing 'Commit statuses: read'); \
                     treating as NOT-green so the CR is escalated for a human. Grant the \
                     permission to restore automatic green-gating."
                );
                (None, true)
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "reading combined commit status for {sha}: {e:?}"
                ))
            }
        };
        if let Some(conclusion) =
            combined_status_contribution(status_opt.as_ref(), permission_denied)
        {
            checks.push(("combined-status".to_owned(), conclusion));
        }

        // FIX 2 + FIX 6 + FIX B: roll up through the SHARED rollup over the UNION of
        // check-run conclusions AND the commit-status conclusion, so the green definition
        // lives in one place and the empty-is-green semantics match `FakeGitHub` exactly.
        // Zero checks AND zero statuses → Success (a ref with no gating is mergeable on
        // GitHub), so a no-CI repo is handed off rather than wedged forever.
        let summary = CheckSummary::rollup(checks);
        tracing::info!(owner = %repo.owner, name = %repo.name, %sha, overall = summary.overall.as_str(), count = summary.checks.len(), "read check status");
        Ok(summary)
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
            Err(e) => Err(anyhow::anyhow!("fetching file {path}@{git_ref}: {e:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- combined_status_contribution: legacy-commit-status fold + 403 degradation ----

    #[test]
    fn combined_status_permission_denied_contributes_failure() {
        // The bug this guards: a 403 reading commit statuses must NOT false-green the CR
        // and must NOT loop forever — it folds in Failure so the coordinator escalates.
        assert_eq!(
            combined_status_contribution(None, true),
            Some(CheckConclusion::Failure)
        );
        // Permission-denied dominates even if a (stale) status happens to be present.
        let green = CombinedStatus {
            state: "success".to_owned(),
            total_count: 3,
        };
        assert_eq!(
            combined_status_contribution(Some(&green), true),
            Some(CheckConclusion::Failure)
        );
    }

    #[test]
    fn combined_status_folds_state_only_when_contexts_exist() {
        let success = CombinedStatus {
            state: "success".to_owned(),
            total_count: 2,
        };
        assert_eq!(
            combined_status_contribution(Some(&success), false),
            Some(CheckConclusion::Success)
        );
        let failing = CombinedStatus {
            state: "failure".to_owned(),
            total_count: 1,
        };
        assert_eq!(
            combined_status_contribution(Some(&failing), false),
            Some(CheckConclusion::Failure)
        );
        // ZERO contexts → contribute nothing so the empty case stays green (FIX 2):
        // GitHub reports `pending` for a SHA with no statuses.
        let empty = CombinedStatus {
            state: "pending".to_owned(),
            total_count: 0,
        };
        assert_eq!(combined_status_contribution(Some(&empty), false), None);
        assert_eq!(combined_status_contribution(None, false), None);
    }

    // ---- map_conclusion: GitHub Checks API `conclusion` → CheckConclusion ----
    //
    // The load-bearing invariant: unknown/None must map to Pending (the conservative
    // "unknown → never green" rule), so an unrecognized state can never be read as green.

    #[test]
    fn map_conclusion_none_is_pending() {
        // queued/in_progress runs have no conclusion yet → Pending (never green).
        assert_eq!(map_conclusion(None), CheckConclusion::Pending);
    }

    #[test]
    fn map_conclusion_success_maps_to_success() {
        assert_eq!(map_conclusion(Some("success")), CheckConclusion::Success);
    }

    #[test]
    fn map_conclusion_failure_variants_map_to_failure() {
        for c in [
            "failure",
            "timed_out",
            "cancelled",
            "action_required",
            "startup_failure",
        ] {
            assert_eq!(
                map_conclusion(Some(c)),
                CheckConclusion::Failure,
                "conclusion {c:?} must map to Failure",
            );
        }
    }

    #[test]
    fn map_conclusion_neutral_and_skipped_map_to_neutral() {
        assert_eq!(map_conclusion(Some("neutral")), CheckConclusion::Neutral);
        assert_eq!(map_conclusion(Some("skipped")), CheckConclusion::Neutral);
    }

    #[test]
    fn map_conclusion_stale_is_pending() {
        // A stale run is superseded by a newer run, so it is not a terminal verdict.
        assert_eq!(map_conclusion(Some("stale")), CheckConclusion::Pending);
    }

    #[test]
    fn map_conclusion_unknown_is_pending() {
        // The conservative rule: any unrecognized (incl. future) value → Pending,
        // so an unknown state can never be mistaken for green.
        assert_eq!(
            map_conclusion(Some("some_future_value")),
            CheckConclusion::Pending,
        );
        assert_eq!(map_conclusion(Some("")), CheckConclusion::Pending);
    }

    // ---- map_status_state: combined commit-status `state` → CheckConclusion ----

    #[test]
    fn map_status_state_success_maps_to_success() {
        assert_eq!(map_status_state("success"), CheckConclusion::Success);
    }

    #[test]
    fn map_status_state_failure_and_error_map_to_failure() {
        assert_eq!(map_status_state("failure"), CheckConclusion::Failure);
        assert_eq!(map_status_state("error"), CheckConclusion::Failure);
    }

    #[test]
    fn map_status_state_pending_is_pending() {
        assert_eq!(map_status_state("pending"), CheckConclusion::Pending);
    }

    #[test]
    fn map_status_state_unknown_is_pending() {
        // Conservative: an unrecognized state can never be read as green.
        assert_eq!(
            map_status_state("some_future_state"),
            CheckConclusion::Pending,
        );
        assert_eq!(map_status_state(""), CheckConclusion::Pending);
    }

    // ---- installation_id_u64: positive-id guard ----

    #[test]
    fn installation_id_u64_zero_is_err() {
        // Installation ids are positive; 0 is never valid.
        assert!(installation_id_u64(0).is_err());
    }

    #[test]
    fn installation_id_u64_negative_is_err() {
        // A negative id must be rejected, not silently wrapped by `as u64`.
        assert!(installation_id_u64(-1).is_err());
        assert!(installation_id_u64(i64::MIN).is_err());
    }

    #[test]
    fn installation_id_u64_positive_is_ok() {
        assert_eq!(installation_id_u64(1).unwrap(), 1u64);
        assert_eq!(installation_id_u64(i64::MAX).unwrap(), i64::MAX as u64);
    }

    // ---- id_u64: non-negative object-id guard ----

    #[test]
    fn id_u64_negative_is_err() {
        // A negative number/id must be rejected, not wrapped under `as u64`.
        assert!(id_u64(-1).is_err());
        assert!(id_u64(i64::MIN).is_err());
    }

    #[test]
    fn id_u64_zero_and_positive_are_ok() {
        // Unlike installation ids, a plain object id of 0 is allowed (only negatives wrap).
        assert_eq!(id_u64(0).unwrap(), 0u64);
        assert_eq!(id_u64(42).unwrap(), 42u64);
        assert_eq!(id_u64(i64::MAX).unwrap(), i64::MAX as u64);
    }

    // ---- normalize_pem: \n-escaped env value → real PEM ----

    #[test]
    fn cap_diff_passes_small_and_truncates_large_on_char_boundary() {
        assert_eq!(cap_diff("small".to_string(), 1024), "small");
        // A multibyte char straddling the cap must not panic and must stay valid UTF-8.
        let big = "é".repeat(1000); // 2 bytes each = 2000 bytes
        let out = cap_diff(big, 101);
        assert!(out.starts_with('é') || out.starts_with("é"));
        assert!(out.contains("[diff truncated at 101 bytes of 2000 total]"));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn normalize_pem_unescapes_backslash_n() {
        let escaped =
            "-----BEGIN RSA PRIVATE KEY-----\\nMIIabc\\nMIIdef\\n-----END RSA PRIVATE KEY-----\\n";
        let out = normalize_pem(escaped);
        assert_eq!(
            out,
            "-----BEGIN RSA PRIVATE KEY-----\nMIIabc\nMIIdef\n-----END RSA PRIVATE KEY-----\n"
        );
        assert!(!out.contains("\\n"), "no literal backslash-n remains");
    }

    #[test]
    fn normalize_pem_leaves_real_newline_pem_unchanged() {
        let real = "-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----\n";
        assert_eq!(normalize_pem(real), real);
    }
}
