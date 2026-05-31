//! The outbound GitHub surface (P3): the operations Factory performs *on* GitHub
//! — mint installation tokens, read issues/CRs, open/update change requests, post
//! comments, submit reviews, read check status, fetch files.
//!
//! It is a trait so the worker/tools/intake depend on behavior, not on a live
//! GitHub: [`FakeGitHub`] is the deterministic test double, and `OctocrabGitHub`
//! (see `octo.rs`) is the real, network-backed implementation.
use std::collections::HashMap;
use std::sync::Mutex;

use crate::RepoRef;

/// A read view of an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueView {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub comments: Vec<String>,
}

/// A read view of a change request (pull request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequestView {
    pub number: i64,
    pub head_sha: String,
    pub diff: String,
    pub files: Vec<String>,
    pub reviews: Vec<String>,
    pub checks: CheckSummary,
}

/// Request to open (or update, idempotently by head branch) a change request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPrRequest {
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub base: String,
    pub linked_issue: Option<i64>,
}

/// The opened/updated change request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrView {
    pub number: i64,
    pub url: String,
}

/// What a comment targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentTarget {
    Issue(i64),
    ChangeRequest(i64),
}

/// A reviewer's verdict on a change request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewVerdict::Approve => "approve",
            ReviewVerdict::RequestChanges => "request_changes",
            ReviewVerdict::Comment => "comment",
        }
    }
}

/// What to read check status for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckRef {
    Sha(String),
    ChangeRequest(i64),
}

/// The conclusion of a single check or the overall rollup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckConclusion {
    Success,
    Failure,
    Pending,
    Neutral,
}

impl CheckConclusion {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckConclusion::Success => "success",
            CheckConclusion::Failure => "failure",
            CheckConclusion::Pending => "pending",
            CheckConclusion::Neutral => "neutral",
        }
    }
}

/// Per-check conclusions plus the overall rollup for a ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckSummary {
    pub overall: CheckConclusion,
    pub checks: Vec<(String, CheckConclusion)>,
}

/// The outbound GitHub operations Factory performs. All are idempotent where the
/// spec requires it (a re-dispatch must not duplicate a CR/comment/review).
#[async_trait::async_trait]
pub trait GitHubClient: Send + Sync {
    /// Mint a short-lived installation token for cloning/pushing the repo.
    async fn installation_token(&self, installation_id: i64) -> anyhow::Result<String>;

    /// Read an issue.
    async fn read_issue(&self, repo: &RepoRef, number: i64) -> anyhow::Result<IssueView>;

    /// Read a change request (diff, files, reviews, check summary).
    async fn read_change_request(
        &self,
        repo: &RepoRef,
        number: i64,
    ) -> anyhow::Result<ChangeRequestView>;

    /// Open the change request for a head branch, or update it if one already
    /// exists for that branch. Idempotent by `head_branch`.
    async fn open_or_update_pr(
        &self,
        repo: &RepoRef,
        req: &OpenPrRequest,
    ) -> anyhow::Result<PrView>;

    /// Post a comment on an issue or change request. Returns the comment id.
    async fn post_comment(
        &self,
        repo: &RepoRef,
        target: CommentTarget,
        body: &str,
    ) -> anyhow::Result<i64>;

    /// Update an existing comment's body (used by `post_comment`'s idempotent re-run:
    /// the same dedupe_key updates the same comment rather than creating a new one).
    async fn update_comment(
        &self,
        repo: &RepoRef,
        comment_id: i64,
        body: &str,
    ) -> anyhow::Result<()>;

    /// Submit a review verdict on a change request. Returns the review id.
    async fn submit_review(
        &self,
        repo: &RepoRef,
        number: i64,
        verdict: ReviewVerdict,
        body: &str,
    ) -> anyhow::Result<i64>;

    /// Read required-check status for a ref.
    async fn check_status(&self, repo: &RepoRef, what: CheckRef) -> anyhow::Result<CheckSummary>;

    /// Fetch a file's UTF-8 content at a git ref. `None` if it doesn't exist.
    async fn get_file(
        &self,
        repo: &RepoRef,
        path: &str,
        git_ref: &str,
    ) -> anyhow::Result<Option<String>>;
}

// ---------------------------------------------------------------------------
// FakeGitHub — deterministic in-memory double for tests (dispatcher/worker/tools/
// intake all drive this instead of a live GitHub).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeState {
    /// Seeded issues, keyed by number.
    issues: HashMap<i64, IssueView>,
    /// Seeded files, keyed by (git_ref, path).
    files: HashMap<(String, String), String>,
    /// Seeded check summaries, keyed by ref string.
    checks: HashMap<String, CheckSummary>,
    /// PR number assigned per head branch (idempotency).
    pr_by_branch: HashMap<String, i64>,
    next_pr: i64,
    next_comment: i64,
    next_review: i64,
    /// Recorded mutating calls, for assertions.
    pub_calls: Vec<String>,
    /// One-shot failure injection: a method name whose NEXT call returns an error
    /// once (then clears), used to exercise the side-effect-failure recovery paths.
    fail_next: Option<String>,
}

/// In-memory [`GitHubClient`] for tests. Records mutating calls and serves seeded
/// reads. PR creation is idempotent by head branch (re-open returns the same number).
#[derive(Default)]
pub struct FakeGitHub {
    state: Mutex<FakeState>,
}

impl FakeGitHub {
    pub fn new() -> Self {
        let s = FakeState {
            next_pr: 100,
            next_comment: 1000,
            next_review: 5000,
            ..Default::default()
        };
        Self {
            state: Mutex::new(s),
        }
    }

    /// Seed an issue the worker/tools can read.
    pub fn with_issue(self, issue: IssueView) -> Self {
        self.state
            .lock()
            .unwrap()
            .issues
            .insert(issue.number, issue);
        self
    }

    /// Seed a file's content at a ref (for config-on-push and reads).
    pub fn with_file(self, git_ref: &str, path: &str, content: &str) -> Self {
        self.state
            .lock()
            .unwrap()
            .files
            .insert((git_ref.to_owned(), path.to_owned()), content.to_owned());
        self
    }

    /// Seed a check summary for a ref string.
    pub fn with_checks(self, git_ref: &str, checks: CheckSummary) -> Self {
        self.state
            .lock()
            .unwrap()
            .checks
            .insert(git_ref.to_owned(), checks);
        self
    }

    /// The mutating calls recorded so far (for assertions).
    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().pub_calls.clone()
    }

    /// Arm a one-shot failure: the NEXT call to `method` (e.g. `"post_comment"` or
    /// `"submit_review"`) returns `Err` once, then subsequent calls succeed. Used to
    /// reproduce a side effect that fails after the idempotency claim is won.
    pub fn with_fail_next(self, method: &str) -> Self {
        self.state.lock().unwrap().fail_next = Some(method.to_owned());
        self
    }

    /// If a one-shot failure is armed for `method`, consume it and return an error to
    /// inject; otherwise `None`. Caller holds the state lock.
    fn take_injected_failure(s: &mut FakeState, method: &str) -> Option<anyhow::Error> {
        if s.fail_next.as_deref() == Some(method) {
            s.fail_next = None;
            Some(anyhow::anyhow!("injected {method} failure"))
        } else {
            None
        }
    }
}

#[async_trait::async_trait]
impl GitHubClient for FakeGitHub {
    async fn installation_token(&self, installation_id: i64) -> anyhow::Result<String> {
        Ok(format!("ghs_fake_install_{installation_id}"))
    }

    async fn read_issue(&self, _repo: &RepoRef, number: i64) -> anyhow::Result<IssueView> {
        self.state
            .lock()
            .unwrap()
            .issues
            .get(&number)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("fake: no seeded issue #{number}"))
    }

    async fn read_change_request(
        &self,
        _repo: &RepoRef,
        number: i64,
    ) -> anyhow::Result<ChangeRequestView> {
        Ok(ChangeRequestView {
            number,
            head_sha: format!("sha-{number}"),
            diff: "--- fake diff ---".to_owned(),
            files: vec!["src/lib.rs".to_owned()],
            reviews: vec![],
            checks: CheckSummary {
                overall: CheckConclusion::Success,
                checks: vec![("ci".to_owned(), CheckConclusion::Success)],
            },
        })
    }

    async fn open_or_update_pr(
        &self,
        _repo: &RepoRef,
        req: &OpenPrRequest,
    ) -> anyhow::Result<PrView> {
        let mut s = self.state.lock().unwrap();
        let number = match s.pr_by_branch.get(&req.head_branch) {
            Some(n) => *n,
            None => {
                let n = s.next_pr;
                s.next_pr += 1;
                s.pr_by_branch.insert(req.head_branch.clone(), n);
                n
            }
        };
        s.pub_calls.push(format!(
            "open_or_update_pr branch={} -> #{number}",
            req.head_branch
        ));
        Ok(PrView {
            number,
            url: format!("https://github.test/pr/{number}"),
        })
    }

    async fn post_comment(
        &self,
        _repo: &RepoRef,
        target: CommentTarget,
        _body: &str,
    ) -> anyhow::Result<i64> {
        let mut s = self.state.lock().unwrap();
        if let Some(e) = Self::take_injected_failure(&mut s, "post_comment") {
            return Err(e);
        }
        let id = s.next_comment;
        s.next_comment += 1;
        s.pub_calls.push(format!("post_comment {target:?} -> {id}"));
        Ok(id)
    }

    async fn update_comment(
        &self,
        _repo: &RepoRef,
        comment_id: i64,
        _body: &str,
    ) -> anyhow::Result<()> {
        let mut s = self.state.lock().unwrap();
        if let Some(e) = Self::take_injected_failure(&mut s, "update_comment") {
            return Err(e);
        }
        s.pub_calls.push(format!("update_comment {comment_id}"));
        Ok(())
    }

    async fn submit_review(
        &self,
        _repo: &RepoRef,
        number: i64,
        verdict: ReviewVerdict,
        _body: &str,
    ) -> anyhow::Result<i64> {
        let mut s = self.state.lock().unwrap();
        if let Some(e) = Self::take_injected_failure(&mut s, "submit_review") {
            return Err(e);
        }
        let id = s.next_review;
        s.next_review += 1;
        s.pub_calls.push(format!(
            "submit_review cr#{number} {} -> {id}",
            verdict.as_str()
        ));
        Ok(id)
    }

    async fn check_status(&self, _repo: &RepoRef, what: CheckRef) -> anyhow::Result<CheckSummary> {
        let key = match what {
            CheckRef::Sha(s) => s,
            CheckRef::ChangeRequest(n) => format!("sha-{n}"),
        };
        Ok(self
            .state
            .lock()
            .unwrap()
            .checks
            .get(&key)
            .cloned()
            .unwrap_or(CheckSummary {
                overall: CheckConclusion::Pending,
                checks: vec![],
            }))
    }

    async fn get_file(
        &self,
        _repo: &RepoRef,
        path: &str,
        git_ref: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .files
            .get(&(git_ref.to_owned(), path.to_owned()))
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> RepoRef {
        RepoRef {
            owner: "acme".into(),
            name: "widgets".into(),
            installation_id: 1,
        }
    }

    #[tokio::test]
    async fn fake_pr_is_idempotent_by_branch() {
        let gh = FakeGitHub::new();
        let req = OpenPrRequest {
            title: "t".into(),
            body: "b".into(),
            head_branch: "factory/issue-1".into(),
            base: "main".into(),
            linked_issue: Some(1),
        };
        let a = gh.open_or_update_pr(&repo(), &req).await.unwrap();
        let b = gh.open_or_update_pr(&repo(), &req).await.unwrap();
        assert_eq!(a.number, b.number, "same branch reuses the PR number");
    }

    #[tokio::test]
    async fn fake_serves_seeded_issue_and_file() {
        let gh = FakeGitHub::new()
            .with_issue(IssueView {
                number: 7,
                title: "Bug".into(),
                body: "fix it".into(),
                comments: vec![],
            })
            .with_file("abc", ".factory/config.yml", "version: 1\n");
        assert_eq!(gh.read_issue(&repo(), 7).await.unwrap().title, "Bug");
        assert_eq!(
            gh.get_file(&repo(), ".factory/config.yml", "abc")
                .await
                .unwrap()
                .as_deref(),
            Some("version: 1\n")
        );
        assert!(gh
            .get_file(&repo(), "missing", "abc")
            .await
            .unwrap()
            .is_none());
    }
}
