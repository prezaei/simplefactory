//! GitHub webhook ingestion: HMAC-SHA256 signature verification and typed event
//! parsing (P2). This is the inbound half of the GitHub integration; the outbound
//! App-auth / installation-token / Checks-PR API client (octocrab) lands in P3,
//! when the worker first needs a token to clone and the Tools service posts to
//! GitHub — see `docs/factory-implementation-plan.md`.
#![forbid(unsafe_code)]

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

mod client;
pub use client::*;

mod octo;
pub use octo::OctocrabGitHub;

/// Errors from webhook handling.
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("failed to parse webhook payload: {0}")]
    Parse(#[from] serde_json::Error),
}

/// Verify a GitHub webhook signature (`X-Hub-Signature-256: sha256=<hex>`) against
/// the raw request body and the configured secret. Constant-time comparison.
pub fn verify_signature(secret: &[u8], body: &[u8], signature_header: &str) -> bool {
    let Some(hex_sig) = signature_header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(provided) = hex::decode(hex_sig) else {
        return false;
    };
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    // `verify_slice` is constant-time.
    mac.verify_slice(&provided).is_ok()
}

/// A repo a webhook refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
    pub installation_id: i64,
}

impl RepoRef {
    /// Construct a [`RepoRef`] from its three identity fields. The single place
    /// callers (coordinator, tools) build a `RepoRef` from a stored repo identity,
    /// so the field mapping lives here rather than being repeated at each seam.
    pub fn new(owner: impl Into<String>, name: impl Into<String>, installation_id: i64) -> Self {
        RepoRef {
            owner: owner.into(),
            name: name.into(),
            installation_id,
        }
    }
}

/// What a webhook event refers to, for subject-keying work items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Issue(i64),
    ChangeRequest(i64),
}

impl Subject {
    /// The `subject_ref` string used in the record book.
    pub fn as_ref_string(&self) -> String {
        match self {
            Subject::Issue(n) => format!("issue#{n}"),
            Subject::ChangeRequest(n) => format!("cr#{n}"),
        }
    }

    /// The [`CommentTarget`] for this subject. The single place a subject maps to a
    /// comment target, shared by the coordinator and the tools service.
    pub fn comment_target(self) -> CommentTarget {
        match self {
            Subject::Issue(n) => CommentTarget::Issue(n),
            Subject::ChangeRequest(n) => CommentTarget::ChangeRequest(n),
        }
    }
}

/// Parse a `subject_ref` string (the inverse of [`Subject::as_ref_string`]):
/// `issue#N` → [`Subject::Issue`], `cr#N` → [`Subject::ChangeRequest`]. Any other
/// form (e.g. a schedule subject) or a non-numeric suffix is an error.
impl std::str::FromStr for Subject {
    type Err = SubjectParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(n) = s.strip_prefix("issue#") {
            return n.parse().map(Subject::Issue).map_err(|_| SubjectParseError);
        }
        if let Some(n) = s.strip_prefix("cr#") {
            return n
                .parse()
                .map(Subject::ChangeRequest)
                .map_err(|_| SubjectParseError);
        }
        Err(SubjectParseError)
    }
}

/// A `subject_ref` string did not match the `issue#N` / `cr#N` scheme.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("subject must be 'issue#N' or 'cr#N'")]
pub struct SubjectParseError;

/// The webhook events Factory acts on (everything else parses to `Other`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookEvent {
    /// An issue gained a label.
    IssueLabeled {
        repo: RepoRef,
        issue: i64,
        label: String,
    },
    /// A change request was opened.
    CrOpened { repo: RepoRef, number: i64 },
    /// New commits pushed to a change request.
    CrUpdated { repo: RepoRef, number: i64 },
    /// A review asked for changes on a change request (rework trigger).
    CrChangesRequested { repo: RepoRef, number: i64 },
    /// A reviewer was requested on a change request.
    ReviewRequested { repo: RepoRef, number: i64 },
    /// A human commented on / edited an issue or change request.
    IssueComment { repo: RepoRef, subject: Subject },
    /// A push (used to detect `.factory/config.yml` changes).
    Push {
        repo: RepoRef,
        head_sha: String,
        touched_paths: Vec<String>,
    },
    /// The GitHub App was installed/uninstalled, or repos were added/removed from an
    /// existing installation. Drives repo onboarding/offboarding (P8): `added` repos are
    /// upserted into the record book with `installation_id` (so no manual seeding), and
    /// `removed` repos are deactivated. One `installation_id` per delivery; the repos are
    /// owner/name pairs.
    Installation {
        installation_id: i64,
        added: Vec<RepoCoord>,
        removed: Vec<RepoCoord>,
    },
    /// Anything Factory doesn't act on.
    Other,
}

/// An owner/name repo coordinate carried by an [`WebhookEvent::Installation`]. (Distinct
/// from [`RepoRef`], which also carries an `installation_id`; the installation id on an
/// installation webhook is per-delivery, not per-repo.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCoord {
    pub owner: String,
    pub name: String,
}

impl RepoCoord {
    /// Parse a GitHub `full_name` (`"owner/name"`). Returns `None` for a malformed value
    /// (no slash, or an empty owner/name) so a junk entry is skipped rather than onboarded.
    fn from_full_name(full_name: &str) -> Option<Self> {
        let (owner, name) = full_name.split_once('/')?;
        if owner.is_empty() || name.is_empty() {
            return None;
        }
        Some(RepoCoord {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }
}

/// Parse a webhook into a typed [`WebhookEvent`]. `event_type` is the
/// `X-GitHub-Event` header; `payload` is the raw JSON body.
pub fn parse_event(event_type: &str, payload: &[u8]) -> Result<WebhookEvent, GithubError> {
    match event_type {
        "issues" => {
            let p: IssuesPayload = serde_json::from_slice(payload)?;
            if p.action == "labeled" {
                if let (Some(issue), Some(label)) = (p.issue.as_ref(), p.label.as_ref()) {
                    return Ok(WebhookEvent::IssueLabeled {
                        repo: p.repository.to_ref(p.installation.as_ref().map(|i| i.id)),
                        issue: issue.number,
                        label: label.name.clone(),
                    });
                }
            }
            Ok(WebhookEvent::Other)
        }
        "pull_request" => {
            let p: PullRequestPayload = serde_json::from_slice(payload)?;
            let repo = p.repository.to_ref(p.installation.as_ref().map(|i| i.id));
            match p.action.as_str() {
                "opened" | "reopened" | "ready_for_review" => Ok(WebhookEvent::CrOpened {
                    repo,
                    number: p.number,
                }),
                "synchronize" | "edited" => Ok(WebhookEvent::CrUpdated {
                    repo,
                    number: p.number,
                }),
                "review_requested" => Ok(WebhookEvent::ReviewRequested {
                    repo,
                    number: p.number,
                }),
                _ => Ok(WebhookEvent::Other),
            }
        }
        "pull_request_review" => {
            let p: PullRequestReviewPayload = serde_json::from_slice(payload)?;
            let repo = p.repository.to_ref(p.installation.as_ref().map(|i| i.id));
            // Only a submitted "changes_requested" review drives rework; approvals
            // and plain comments are handled by the coordinator off the record book.
            if p.action == "submitted" && p.review.state.as_deref() == Some("changes_requested") {
                return Ok(WebhookEvent::CrChangesRequested {
                    repo,
                    number: p.pull_request.number,
                });
            }
            Ok(WebhookEvent::Other)
        }
        "issue_comment" => {
            let p: IssueCommentPayload = serde_json::from_slice(payload)?;
            if p.action == "created" || p.action == "edited" {
                let subject = if p.issue.pull_request.is_some() {
                    Subject::ChangeRequest(p.issue.number)
                } else {
                    Subject::Issue(p.issue.number)
                };
                return Ok(WebhookEvent::IssueComment {
                    repo: p.repository.to_ref(p.installation.as_ref().map(|i| i.id)),
                    subject,
                });
            }
            Ok(WebhookEvent::Other)
        }
        "push" => {
            let p: PushPayload = serde_json::from_slice(payload)?;
            let mut touched = Vec::new();
            for c in &p.commits {
                touched.extend(c.modified.iter().cloned());
                touched.extend(c.added.iter().cloned());
            }
            Ok(WebhookEvent::Push {
                repo: p.repository.to_ref(p.installation.as_ref().map(|i| i.id)),
                head_sha: p.after,
                touched_paths: touched,
            })
        }
        // App installed/uninstalled (whole-installation lifecycle). `repositories` lists
        // the selected repos. We model just two actions: ONBOARD (created/unsuspend) and
        // OFFBOARD (deleted/suspend). A GitHub `suspend` is deliberately treated as an
        // offboard (Factory stops acting on the repo, status → 'removed') rather than a
        // distinct lifecycle state; `unsuspend` re-onboards it (`upsert_repo` reactivates a
        // 'removed' repo). The schema's separate 'suspended' status is reserved for an
        // operator manually pausing Factory and is never written by these webhooks — so an
        // operator 'suspend' is never clobbered by a GitHub suspend/unsuspend round-trip.
        "installation" => {
            let p: InstallationPayload = serde_json::from_slice(payload)?;
            let coords = full_names_to_coords(&p.repositories);
            let (added, removed) = match p.action.as_str() {
                "created" | "unsuspend" => (coords, Vec::new()),
                "deleted" | "suspend" => (Vec::new(), coords),
                // e.g. "new_permissions_accepted" — no repo membership change.
                _ => (Vec::new(), Vec::new()),
            };
            Ok(WebhookEvent::Installation {
                installation_id: p.installation.id,
                added,
                removed,
            })
        }
        // Repos added to / removed from an existing installation.
        "installation_repositories" => {
            let p: InstallationPayload = serde_json::from_slice(payload)?;
            Ok(WebhookEvent::Installation {
                installation_id: p.installation.id,
                added: full_names_to_coords(&p.repositories_added),
                removed: full_names_to_coords(&p.repositories_removed),
            })
        }
        _ => Ok(WebhookEvent::Other),
    }
}

/// Map GitHub repo entries (`full_name`) to [`RepoCoord`]s, dropping malformed ones.
fn full_names_to_coords(repos: &[RepoLite]) -> Vec<RepoCoord> {
    repos
        .iter()
        .filter_map(|r| RepoCoord::from_full_name(&r.full_name))
        .collect()
}

// ---- Minimal payload shapes (only the fields Factory reads) ----

#[derive(Debug, Deserialize)]
struct Repository {
    name: String,
    owner: Owner,
    #[serde(default)]
    installation: Option<Installation>,
}

impl Repository {
    /// Build a [`RepoRef`]. GitHub carries `installation.id` at the payload top
    /// level; prefer that, falling back to a repo-nested installation, then 0
    /// (intake resolves the repo by owner/name, so 0 is fine there).
    fn to_ref(&self, top_level_installation: Option<i64>) -> RepoRef {
        RepoRef {
            owner: self.owner.login.clone(),
            name: self.name.clone(),
            installation_id: top_level_installation
                .or_else(|| self.installation.as_ref().map(|i| i.id))
                .unwrap_or(0),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Owner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct Installation {
    id: i64,
}

/// `installation` / `installation_repositories` webhook payload (only the fields onboarding
/// reads). One struct serves both events: `repositories` is populated on `installation`,
/// `repositories_added`/`repositories_removed` on `installation_repositories`.
#[derive(Debug, Deserialize)]
struct InstallationPayload {
    action: String,
    installation: Installation,
    #[serde(default)]
    repositories: Vec<RepoLite>,
    #[serde(default)]
    repositories_added: Vec<RepoLite>,
    #[serde(default)]
    repositories_removed: Vec<RepoLite>,
}

#[derive(Debug, Deserialize)]
struct RepoLite {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct Label {
    name: String,
}

#[derive(Debug, Deserialize)]
struct IssueRef {
    number: i64,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct IssuesPayload {
    action: String,
    #[serde(default)]
    issue: Option<IssueRef>,
    #[serde(default)]
    label: Option<Label>,
    repository: Repository,
    #[serde(default)]
    installation: Option<Installation>,
}

#[derive(Debug, Deserialize)]
struct PullRequestPayload {
    action: String,
    number: i64,
    repository: Repository,
    #[serde(default)]
    installation: Option<Installation>,
}

#[derive(Debug, Deserialize)]
struct PrRef {
    number: i64,
}

#[derive(Debug, Deserialize)]
struct Review {
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PullRequestReviewPayload {
    action: String,
    review: Review,
    pull_request: PrRef,
    repository: Repository,
    #[serde(default)]
    installation: Option<Installation>,
}

#[derive(Debug, Deserialize)]
struct IssueCommentPayload {
    action: String,
    issue: IssueRef,
    repository: Repository,
    #[serde(default)]
    installation: Option<Installation>,
}

#[derive(Debug, Deserialize)]
struct Commit {
    #[serde(default)]
    modified: Vec<String>,
    #[serde(default)]
    added: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PushPayload {
    after: String,
    repository: Repository,
    #[serde(default)]
    commits: Vec<Commit>,
    #[serde(default)]
    installation: Option<Installation>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn signature_accepts_valid_and_rejects_tampered() {
        let secret = b"s3cret";
        let body = br#"{"hello":"world"}"#;
        let good = sign(secret, body);
        assert!(verify_signature(secret, body, &good));
        // Wrong secret, tampered body, wrong scheme, and garbage all rejected.
        assert!(!verify_signature(b"other", body, &good));
        assert!(!verify_signature(secret, br#"{"hello":"mars"}"#, &good));
        assert!(!verify_signature(secret, body, "sha1=deadbeef"));
        assert!(!verify_signature(secret, body, "not-a-sig"));
    }

    #[test]
    fn parses_issue_labeled() {
        let payload = br#"{
            "action": "labeled",
            "issue": { "number": 123 },
            "label": { "name": "factory" },
            "repository": { "name": "repo", "owner": { "login": "acme" } },
            "installation": { "id": 42 }
        }"#;
        let ev = parse_event("issues", payload).unwrap();
        assert_eq!(
            ev,
            WebhookEvent::IssueLabeled {
                repo: RepoRef {
                    owner: "acme".into(),
                    name: "repo".into(),
                    installation_id: 42
                },
                issue: 123,
                label: "factory".into(),
            }
        );
    }

    #[test]
    fn non_labeled_issue_action_is_other() {
        let payload = br#"{
            "action": "opened",
            "issue": { "number": 1 },
            "repository": { "name": "r", "owner": { "login": "o" } }
        }"#;
        assert_eq!(parse_event("issues", payload).unwrap(), WebhookEvent::Other);
    }

    #[test]
    fn parses_pull_request_open_and_sync() {
        let opened =
            br#"{"action":"opened","number":7,"repository":{"name":"r","owner":{"login":"o"}}}"#;
        assert!(matches!(
            parse_event("pull_request", opened).unwrap(),
            WebhookEvent::CrOpened { number: 7, .. }
        ));
        let synced = br#"{"action":"synchronize","number":7,"repository":{"name":"r","owner":{"login":"o"}}}"#;
        assert!(matches!(
            parse_event("pull_request", synced).unwrap(),
            WebhookEvent::CrUpdated { number: 7, .. }
        ));
    }

    #[test]
    fn parses_review_requested_on_pull_request() {
        let payload = br#"{"action":"review_requested","number":11,"repository":{"name":"r","owner":{"login":"o"}}}"#;
        assert!(matches!(
            parse_event("pull_request", payload).unwrap(),
            WebhookEvent::ReviewRequested { number: 11, .. }
        ));
    }

    #[test]
    fn parses_pull_request_review_changes_requested() {
        let changes = br#"{
            "action": "submitted",
            "review": { "state": "changes_requested" },
            "pull_request": { "number": 13 },
            "repository": { "name": "r", "owner": { "login": "o" } }
        }"#;
        assert!(matches!(
            parse_event("pull_request_review", changes).unwrap(),
            WebhookEvent::CrChangesRequested { number: 13, .. }
        ));
        // An approving review is not a rework trigger.
        let approved = br#"{
            "action": "submitted",
            "review": { "state": "approved" },
            "pull_request": { "number": 13 },
            "repository": { "name": "r", "owner": { "login": "o" } }
        }"#;
        assert_eq!(
            parse_event("pull_request_review", approved).unwrap(),
            WebhookEvent::Other
        );
    }

    #[test]
    fn parses_issue_comment_on_issue_vs_cr() {
        let on_issue = br#"{"action":"created","issue":{"number":5},"repository":{"name":"r","owner":{"login":"o"}}}"#;
        assert!(matches!(
            parse_event("issue_comment", on_issue).unwrap(),
            WebhookEvent::IssueComment {
                subject: Subject::Issue(5),
                ..
            }
        ));
        let on_cr = br#"{"action":"created","issue":{"number":9,"pull_request":{"url":"x"}},"repository":{"name":"r","owner":{"login":"o"}}}"#;
        assert!(matches!(
            parse_event("issue_comment", on_cr).unwrap(),
            WebhookEvent::IssueComment {
                subject: Subject::ChangeRequest(9),
                ..
            }
        ));
    }

    #[test]
    fn parses_push_touched_paths_and_installation_id() {
        let payload = br#"{
            "after": "abc123",
            "repository": { "name": "r", "owner": { "login": "o" } },
            "installation": { "id": 99 },
            "commits": [
                { "modified": [".factory/config.yml"], "added": ["src/x.rs"] },
                { "modified": ["README.md"] }
            ]
        }"#;
        let ev = parse_event("push", payload).unwrap();
        let WebhookEvent::Push {
            repo,
            head_sha,
            touched_paths,
        } = ev
        else {
            panic!("expected push");
        };
        assert_eq!(head_sha, "abc123");
        // The installation id MUST survive parsing — config-auto-reload-on-push
        // fetches `.factory/config.yml` with this id, so dropping it (the prior
        // `to_ref(None)` bug) silently breaks auth on the live path.
        assert_eq!(repo.installation_id, 99);
        assert!(touched_paths.contains(&".factory/config.yml".to_string()));
        assert!(touched_paths.contains(&"src/x.rs".to_string()));
    }

    #[test]
    fn parses_installation_created_onboards_selected_repos() {
        let payload = br#"{
            "action": "created",
            "installation": { "id": 137 },
            "repositories": [
                { "full_name": "acme/widgets" },
                { "full_name": "acme/gadgets" }
            ]
        }"#;
        let ev = parse_event("installation", payload).unwrap();
        let WebhookEvent::Installation {
            installation_id,
            added,
            removed,
        } = ev
        else {
            panic!("expected installation");
        };
        assert_eq!(installation_id, 137);
        assert_eq!(
            added,
            vec![
                RepoCoord {
                    owner: "acme".into(),
                    name: "widgets".into()
                },
                RepoCoord {
                    owner: "acme".into(),
                    name: "gadgets".into()
                },
            ]
        );
        assert!(removed.is_empty());
    }

    #[test]
    fn parses_installation_deleted_offboards_repos() {
        let payload = br#"{
            "action": "deleted",
            "installation": { "id": 5 },
            "repositories": [ { "full_name": "acme/widgets" } ]
        }"#;
        let WebhookEvent::Installation { added, removed, .. } =
            parse_event("installation", payload).unwrap()
        else {
            panic!("expected installation");
        };
        assert!(added.is_empty());
        assert_eq!(
            removed,
            vec![RepoCoord {
                owner: "acme".into(),
                name: "widgets".into()
            }]
        );
    }

    #[test]
    fn parses_installation_repositories_added_and_removed() {
        let payload = br#"{
            "action": "added",
            "installation": { "id": 9 },
            "repositories_added": [ { "full_name": "o/added1" } ],
            "repositories_removed": [ { "full_name": "o/removed1" }, { "full_name": "bad-no-slash" } ]
        }"#;
        let WebhookEvent::Installation {
            installation_id,
            added,
            removed,
        } = parse_event("installation_repositories", payload).unwrap()
        else {
            panic!("expected installation");
        };
        assert_eq!(installation_id, 9);
        assert_eq!(
            added,
            vec![RepoCoord {
                owner: "o".into(),
                name: "added1".into()
            }]
        );
        // The malformed "bad-no-slash" entry is dropped, not onboarded.
        assert_eq!(
            removed,
            vec![RepoCoord {
                owner: "o".into(),
                name: "removed1".into()
            }]
        );
    }

    #[test]
    fn installation_new_permissions_accepted_changes_no_repos() {
        let payload = br#"{ "action": "new_permissions_accepted", "installation": { "id": 1 },
            "repositories": [ { "full_name": "o/r" } ] }"#;
        let WebhookEvent::Installation { added, removed, .. } =
            parse_event("installation", payload).unwrap()
        else {
            panic!("expected installation");
        };
        assert!(added.is_empty() && removed.is_empty());
    }

    #[test]
    fn unknown_event_is_other() {
        assert_eq!(parse_event("star", b"{}").unwrap(), WebhookEvent::Other);
    }

    #[test]
    fn subject_ref_strings() {
        assert_eq!(Subject::Issue(3).as_ref_string(), "issue#3");
        assert_eq!(Subject::ChangeRequest(8).as_ref_string(), "cr#8");
    }

    #[test]
    fn subject_from_str_roundtrips_and_rejects_other() {
        use std::str::FromStr;
        assert_eq!(Subject::from_str("issue#7"), Ok(Subject::Issue(7)));
        assert_eq!(Subject::from_str("cr#42"), Ok(Subject::ChangeRequest(42)));
        // Roundtrip against as_ref_string for both variants.
        for s in [Subject::Issue(3), Subject::ChangeRequest(8)] {
            assert_eq!(Subject::from_str(&s.as_ref_string()), Ok(s));
        }
        assert!(Subject::from_str("schedule@manager@x").is_err());
        assert!(Subject::from_str("issue#notanumber").is_err());
        assert!(Subject::from_str("pr/9").is_err());
    }
}
