//! Intake logic: turn a verified GitHub webhook into work items, gated by the
//! repo's opt-in label and manager-first policy (P2). Pure functions over a
//! `Pool` so they're testable with fixtures; the HTTP server in `main.rs` is a
//! thin wrapper. Config-apply (validate + store) lives here too; wiring it to a
//! push event needs the GitHub file fetch (octocrab, P3) — `apply_config` is the
//! tested unit.
#![forbid(unsafe_code)]

use factory_config::{Config, Role, Trigger};
use factory_github::{RepoRef, WebhookEvent};
use factory_record::{self as record, EnqueueOutcome, Kind, NewWorkItem, Pool};
use uuid::Uuid;

/// What `ingest` did with a webhook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Redelivered webhook (delivery id already seen) — no-op.
    Duplicate,
    /// The repo isn't connected to Factory.
    UnknownRepo,
    /// The repo has no validated config yet.
    NoConfig,
    /// Event didn't match the opt-in label or any subscribed worker.
    Ignored,
    /// `n` work items were enqueued.
    Enqueued(usize),
    /// `n` blocked items were re-queued (resume on reply).
    Resumed(u64),
}

/// What `apply_config` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    /// Invalid config; the previous good config remains in effect.
    RejectedKeptLastGood,
}

/// Ingest a verified webhook: dedupe, resolve the repo + its config, then enqueue
/// (or resume) per the opt-in label and the workers' triggers.
pub async fn ingest(
    pool: &Pool,
    event: &WebhookEvent,
    delivery_id: &str,
) -> anyhow::Result<IngestOutcome> {
    // Dedupe redelivered webhooks first.
    if !record::mark_delivery(pool, delivery_id).await? {
        return Ok(IngestOutcome::Duplicate);
    }

    let Some(repo_ref) = event_repo(event) else {
        return Ok(IngestOutcome::Ignored);
    };
    let Some(rc) = record::repo_by_name(pool, &repo_ref.owner, &repo_ref.name).await? else {
        return Ok(IngestOutcome::UnknownRepo);
    };
    let Some(config_json) = rc.config else {
        return Ok(IngestOutcome::NoConfig);
    };
    let config: Config = serde_json::from_value(config_json)?;
    let config_sha = rc.config_sha.unwrap_or_default();

    match event {
        WebhookEvent::IssueLabeled { label, issue, .. } => {
            if *label != config.settings.label {
                return Ok(IngestOutcome::Ignored);
            }
            let subject = format!("issue#{issue}");
            let n = enqueue_matching(
                pool,
                rc.id,
                &config,
                &config_sha,
                Trigger::IssueLabeled,
                &subject,
            )
            .await?;
            Ok(enqueued_or_ignored(n))
        }
        WebhookEvent::CrOpened { number, .. } => {
            let subject = format!("cr#{number}");
            let n = enqueue_matching(
                pool,
                rc.id,
                &config,
                &config_sha,
                Trigger::CrOpened,
                &subject,
            )
            .await?;
            Ok(enqueued_or_ignored(n))
        }
        WebhookEvent::CrUpdated { number, .. } => {
            let subject = format!("cr#{number}");
            let n = enqueue_matching(
                pool,
                rc.id,
                &config,
                &config_sha,
                Trigger::CrUpdated,
                &subject,
            )
            .await?;
            Ok(enqueued_or_ignored(n))
        }
        WebhookEvent::IssueComment { subject, .. } => {
            // A human reply resumes any blocked work on this subject.
            let n = record::requeue_blocked(pool, rc.id, &subject.as_ref_string()).await?;
            Ok(IngestOutcome::Resumed(n))
        }
        // Push → config-apply requires fetching the file content via the GitHub
        // API (octocrab, P3); `apply_config` below is the tested unit for it.
        WebhookEvent::Push { .. } | WebhookEvent::Other => Ok(IngestOutcome::Ignored),
    }
}

/// Validate a `.factory/config.yml` and, if valid, store it on the repo. Invalid
/// config is rejected and the previous good config is left untouched (fail-safe).
pub async fn apply_config(
    pool: &Pool,
    repo_id: Uuid,
    yaml: &str,
    config_sha: &str,
) -> anyhow::Result<ApplyOutcome> {
    match factory_config::parse(yaml) {
        Ok(cfg) => {
            let json = serde_json::to_value(&cfg)?;
            record::set_repo_config(pool, repo_id, &json, config_sha).await?;
            tracing::info!(%repo_id, config_sha, "applied config");
            Ok(ApplyOutcome::Applied)
        }
        Err(e) => {
            tracing::warn!(%repo_id, error = %e, "invalid .factory/config.yml; keeping last good");
            Ok(ApplyOutcome::RejectedKeptLastGood)
        }
    }
}

fn enqueued_or_ignored(n: usize) -> IngestOutcome {
    if n > 0 {
        IngestOutcome::Enqueued(n)
    } else {
        IngestOutcome::Ignored
    }
}

/// Enqueue one work item per worker whose `on` includes `trigger`.
async fn enqueue_matching(
    pool: &Pool,
    repo_id: Uuid,
    config: &Config,
    config_sha: &str,
    trigger: Trigger,
    subject: &str,
) -> anyhow::Result<usize> {
    let mut enqueued = 0;
    for worker in &config.workers {
        if worker.on.contains(&trigger) {
            let item = NewWorkItem {
                repo_id,
                kind: role_to_kind(worker.role),
                worker_name: worker.name.clone(),
                config_sha: config_sha.to_owned(),
                trigger: trigger_str(&trigger).to_owned(),
                subject_ref: subject.to_owned(),
                correlation_id: Uuid::now_v7(),
                priority: 0,
                available_at: None,
            };
            if let EnqueueOutcome::Inserted(_) = record::enqueue(pool, &item).await? {
                enqueued += 1;
            }
        }
    }
    Ok(enqueued)
}

fn role_to_kind(role: Role) -> Kind {
    match role {
        Role::Manager => Kind::Manager,
        Role::Coder => Kind::Coder,
        Role::Reviewer => Kind::Reviewer,
        Role::Tester => Kind::Tester,
    }
}

fn trigger_str(trigger: &Trigger) -> &'static str {
    match trigger {
        Trigger::IssueLabeled => "issue_labeled",
        Trigger::CrOpened => "cr_opened",
        Trigger::CrUpdated => "cr_updated",
        Trigger::CrChangesRequested => "cr_changes_requested",
        Trigger::ReviewRequested => "review_requested",
        Trigger::IssueComment => "issue_comment",
        Trigger::Manager => "manager",
        Trigger::Schedule(_) => "schedule",
    }
}

fn event_repo(event: &WebhookEvent) -> Option<&RepoRef> {
    match event {
        WebhookEvent::IssueLabeled { repo, .. }
        | WebhookEvent::CrOpened { repo, .. }
        | WebhookEvent::CrUpdated { repo, .. }
        | WebhookEvent::IssueComment { repo, .. }
        | WebhookEvent::Push { repo, .. } => Some(repo),
        WebhookEvent::Other => None,
    }
}
