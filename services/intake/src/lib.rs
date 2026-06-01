//! Intake logic: turn a verified GitHub webhook into work items, gated by the
//! repo's opt-in label and manager-first policy (P2). Pure functions over a
//! `Pool` so they're testable with fixtures; the HTTP server in `main.rs` is a
//! thin wrapper. Config-apply (validate + store) lives here too; wiring it to a
//! push event needs the GitHub file fetch (octocrab, P3) — `apply_config` is the
//! tested unit.
#![forbid(unsafe_code)]

use chrono::{DateTime, Utc};
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
    // Dedupe: skip if this delivery was already fully processed.
    if record::delivery_seen(pool, delivery_id).await? {
        return Ok(IngestOutcome::Duplicate);
    }

    let (repo_id, outcome) = process(pool, event).await?;

    // Audit every handled delivery — the product spec requires a transparent
    // play-by-play / audit trail of who did what, recorded in the events book and
    // not just in tracing. repo_id is None for events we couldn't attribute to a
    // connected repo.
    //
    // Concurrency note: dedupe here is read-act-mark across statements, not one
    // transaction. The *work item* is still exactly-once (enqueue's partial
    // unique index), so concurrent same-delivery handling can at worst write a
    // duplicate audit row — never a duplicate work item. GitHub delivers a given
    // X-GitHub-Delivery once and retries sequentially, so this race is not
    // reachable in practice; a transaction is deliberately avoided here so we
    // keep the mark-after-success ordering that makes lost-event redrive work.
    record::append_event(
        pool,
        repo_id,
        None,
        None,
        "intake",
        "webhook_ingested",
        serde_json::json!({ "outcome": format!("{outcome:?}") }),
    )
    .await?;

    // Record the delivery as handled ONLY for terminal outcomes. "Not ready"
    // outcomes (repo not connected yet / config not applied yet) are left
    // unmarked so a later GitHub redelivery re-drives them — nothing is lost.
    // The work itself is idempotent (enqueue's partial unique index; requeue is
    // a no-op when nothing is blocked), so if processing errors after some work,
    // the unmarked redelivery re-drives it without double-creating. (Errors
    // propagate before this point, so a failed delivery is never marked.)
    if !matches!(
        outcome,
        IngestOutcome::UnknownRepo | IngestOutcome::NoConfig
    ) {
        record::mark_delivery(pool, delivery_id).await?;
    }
    Ok(outcome)
}

/// Resolve the repo + config and act on the event. Idempotent. Returns the
/// resolved `repo_id` (for the audit trail) alongside the outcome.
async fn process(
    pool: &Pool,
    event: &WebhookEvent,
) -> anyhow::Result<(Option<Uuid>, IngestOutcome)> {
    let Some(repo_ref) = event_repo(event) else {
        return Ok((None, IngestOutcome::Ignored));
    };
    let Some(rc) = record::repo_by_name(pool, &repo_ref.owner, &repo_ref.name).await? else {
        return Ok((None, IngestOutcome::UnknownRepo));
    };
    let Some(config_json) = rc.config else {
        return Ok((Some(rc.id), IngestOutcome::NoConfig));
    };
    let config: Config = serde_json::from_value(config_json)?;
    let config_sha = rc.config_sha.unwrap_or_default();

    let outcome = match event {
        WebhookEvent::IssueLabeled { label, issue, .. } => {
            if *label != config.settings.label {
                IngestOutcome::Ignored
            } else {
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
                enqueued_or_ignored(n)
            }
        }
        // CR-keyed triggers: same shape, differing only in the Trigger variant.
        WebhookEvent::CrOpened { number, .. }
        | WebhookEvent::CrUpdated { number, .. }
        | WebhookEvent::CrChangesRequested { number, .. }
        | WebhookEvent::ReviewRequested { number, .. } => {
            let trigger = match event {
                WebhookEvent::CrOpened { .. } => Trigger::CrOpened,
                WebhookEvent::CrUpdated { .. } => Trigger::CrUpdated,
                WebhookEvent::CrChangesRequested { .. } => Trigger::CrChangesRequested,
                _ => Trigger::ReviewRequested,
            };
            cr_enqueue(pool, rc.id, &config, &config_sha, trigger, *number).await?
        }
        WebhookEvent::IssueComment { subject, .. } => {
            // A human reply resumes any blocked work on this subject.
            let n = record::requeue_blocked(pool, rc.id, &subject.as_ref_string()).await?;
            IngestOutcome::Resumed(n)
        }
        // Push → config-apply requires fetching the file content via the GitHub
        // API (octocrab, P3); `apply_config` below is the tested unit for it.
        WebhookEvent::Push { .. } | WebhookEvent::Other => IngestOutcome::Ignored,
    };
    Ok((Some(rc.id), outcome))
}

/// Enqueue work for a change-request-keyed trigger (`cr#<number>` subject).
async fn cr_enqueue(
    pool: &Pool,
    repo_id: Uuid,
    config: &Config,
    config_sha: &str,
    trigger: Trigger,
    number: i64,
) -> anyhow::Result<IngestOutcome> {
    let subject = format!("cr#{number}");
    let n = enqueue_matching(pool, repo_id, config, config_sha, trigger, &subject).await?;
    Ok(enqueued_or_ignored(n))
}

/// Validate a `.factory/config.yml` and, if valid, store it on the repo. Invalid
/// config is rejected and the previous good config is left untouched (fail-safe).
pub async fn apply_config(
    pool: &Pool,
    repo_id: Uuid,
    yaml: &str,
    config_sha: &str,
) -> anyhow::Result<ApplyOutcome> {
    // Every config decision is an auditable event (the "every decision logged" mandate),
    // not just a tracing line — a rejected-bad-config (last-good kept) is security-relevant
    // and must leave a queryable trail. The audit write is BEST-EFFORT: the config
    // decision (apply / reject-keep-last-good) is already authoritative, so a transient
    // failure writing the audit row must NOT flip the outcome to Err (which would mislabel
    // an applied config as failed). A failed audit write is itself logged at warn.
    match factory_config::parse(yaml) {
        Ok(cfg) => {
            let json = serde_json::to_value(&cfg)?;
            record::set_repo_config(pool, repo_id, &json, config_sha).await?;
            tracing::info!(%repo_id, config_sha, "applied config");
            audit_config_decision(
                pool,
                repo_id,
                "config_applied",
                serde_json::json!({ "config_sha": config_sha }),
            )
            .await;
            Ok(ApplyOutcome::Applied)
        }
        Err(e) => {
            tracing::warn!(%repo_id, error = %e, "invalid .factory/config.yml; keeping last good");
            audit_config_decision(
                pool,
                repo_id,
                "config_rejected_kept_last_good",
                serde_json::json!({ "config_sha": config_sha, "error": e.to_string() }),
            )
            .await;
            Ok(ApplyOutcome::RejectedKeptLastGood)
        }
    }
}

/// Best-effort audit of a config decision: append an `events` row, logging (not
/// propagating) any failure so a transient DB blip can't flip an already-authoritative
/// config outcome into an error.
async fn audit_config_decision(
    pool: &Pool,
    repo_id: Uuid,
    action: &str,
    detail: serde_json::Value,
) {
    if let Err(e) =
        record::append_event(pool, Some(repo_id), None, None, "intake", action, detail).await
    {
        tracing::warn!(%repo_id, action, error = %e, "failed to write config-decision audit event");
    }
}

/// Path of the per-repo config file Factory reloads on push.
const CONFIG_PATH: &str = ".factory/config.yml";

/// React to a push: if it touched `.factory/config.yml`, fetch the file at the
/// pushed head and (re)apply it. Returns `Ok(None)` when there's nothing to do
/// (push didn't touch the config, the repo isn't connected, or the file vanished)
/// and `Ok(Some(outcome))` when [`apply_config`] ran. Fail-safe: an invalid
/// config is rejected and the last good config stays in effect (see
/// [`apply_config`]).
pub async fn apply_config_from_push(
    pool: &Pool,
    gh: &dyn factory_github::GitHubClient,
    repo: &RepoRef,
    head_sha: &str,
    touched_paths: &[String],
) -> anyhow::Result<Option<ApplyOutcome>> {
    if !touched_paths.iter().any(|p| p == CONFIG_PATH) {
        tracing::info!(
            owner = %repo.owner,
            repo = %repo.name,
            head_sha,
            "push did not touch config; nothing to reload"
        );
        return Ok(None);
    }

    let Some(rc) = record::repo_by_name(pool, &repo.owner, &repo.name).await? else {
        tracing::info!(
            owner = %repo.owner,
            repo = %repo.name,
            head_sha,
            "config push for unconnected repo; ignoring"
        );
        return Ok(None);
    };

    // Fetch the config using the STORED installation_id, not the one on the inbound
    // webhook's RepoRef: a repository webhook payload (e.g. via `gh webhook forward`)
    // carries no `installation` object, so `repo.installation_id` is 0 there. The repos
    // row is the authoritative source of the installation to mint a token with.
    let Some(ident) = record::repo_by_id(pool, rc.id).await? else {
        // The row resolved by name a line ago; absence here means it was just removed.
        tracing::info!(owner = %repo.owner, repo = %repo.name, "repo removed mid-apply; ignoring");
        return Ok(None);
    };
    let repo_ident = RepoRef::new(ident.owner, ident.name, ident.installation_id);
    let Some(yaml) = gh.get_file(&repo_ident, CONFIG_PATH, head_sha).await? else {
        tracing::info!(
            owner = %repo.owner,
            repo = %repo.name,
            head_sha,
            "config push but file is absent at head; nothing to apply"
        );
        return Ok(None);
    };

    let outcome = apply_config(pool, rc.id, &yaml, head_sha).await?;
    tracing::info!(
        owner = %repo.owner,
        repo = %repo.name,
        repo_id = %rc.id,
        head_sha,
        ?outcome,
        "reloaded config from push"
    );
    Ok(Some(outcome))
}

/// How far back the scheduler scans for a due tick when a `(repo, worker)` has
/// never fired. Slightly larger than the tick cadence to tolerate jitter; a
/// missed window only delays a fire, never duplicates one.
pub const SCHEDULER_WINDOW_SECS: i64 = 90;

/// Fire any scheduled work due for one repo's config as of `now`, returning the
/// number fired. Exactly-once across replicas — the fire is committed under a
/// per-`(repo, worker)` advisory lock that re-checks the last fired slot (see
/// [`record::record_scheduled_fire`]).
pub async fn scheduler_tick_for_repo(
    pool: &Pool,
    repo_id: Uuid,
    config: &Config,
    config_sha: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    let mut fired = 0;
    for worker in &config.workers {
        for trigger in &worker.on {
            let Trigger::Schedule(cron) = trigger else {
                continue;
            };
            let floor = now - chrono::Duration::seconds(SCHEDULER_WINDOW_SECS);
            let after = match record::last_scheduled_fire(pool, repo_id, &worker.name).await? {
                Some(last) => last.max(floor),
                None => floor,
            };
            let due = factory_config::schedule_due_between(cron, after, now)?;
            // Fire only the most recent due slot — after downtime we resume, we
            // don't stampede one work item per missed tick.
            let Some(slot) = due.last().copied() else {
                continue;
            };
            let item = NewWorkItem {
                repo_id,
                kind: role_to_kind(worker.role),
                worker_name: worker.name.clone(),
                config_sha: config_sha.to_owned(),
                trigger: "schedule".to_owned(),
                subject_ref: format!("schedule@{}@{}", worker.name, slot.to_rfc3339()),
                correlation_id: Uuid::now_v7(),
                priority: 0,
                available_at: None,
            };
            if let EnqueueOutcome::Inserted(_) =
                record::record_scheduled_fire(pool, &item, slot).await?
            {
                fired += 1;
            }
        }
    }
    Ok(fired)
}

/// One scheduler pass over every configured repo. Returns total schedules fired.
pub async fn scheduler_tick(pool: &Pool, now: DateTime<Utc>) -> anyhow::Result<usize> {
    let mut fired = 0;
    for (repo_id, config_sha, config_json) in record::list_configured_repos(pool).await? {
        let config: Config = match serde_json::from_value(config_json) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(%repo_id, error = %e, "skipping repo with unparseable stored config");
                continue;
            }
        };
        fired += scheduler_tick_for_repo(pool, repo_id, &config, &config_sha, now).await?;
    }
    Ok(fired)
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
        | WebhookEvent::CrChangesRequested { repo, .. }
        | WebhookEvent::ReviewRequested { repo, .. }
        | WebhookEvent::IssueComment { repo, .. }
        | WebhookEvent::Push { repo, .. } => Some(repo),
        WebhookEvent::Other => None,
    }
}
