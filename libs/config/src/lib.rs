//! Parsing, validation, and prompt-layering for the per-repo `.factory/config.yml`
//! "describe your team" file (see `docs/factory-technical-spec.md`).
//!
//! [`parse`] turns YAML into a validated [`Config`]. Validation enforces the
//! rules the spec requires: supported version, positive caps, unique worker
//! names, known triggers, parseable cron, non-empty `instructions_file`, and the
//! **manager-first** rule (only the manager may subscribe to `issue_labeled`).
//! [`resolve_prompt`] composes a worker's effective prompt from its layers.
#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A validated Factory team configuration.
///
/// The public types derive `Serialize`/`Deserialize` so a validated config can be
/// stored as jsonb in the record book and read back (the YAML on-disk form is
/// parsed separately via [`parse`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub version: u32,
    pub settings: Settings,
    pub limits: Limits,
    pub workers: Vec<Worker>,
}

/// Repo-wide settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Opt-in label; Factory acts only on work carrying it.
    pub label: String,
    /// Model used unless a worker overrides it.
    pub default_model: Option<String>,
    /// Guidance appended to every worker's prompt.
    pub instructions: Option<String>,
    /// Path prefixes whose changes alter the very checks that gate a merge. A CR
    /// touching any of these "weakens its own gating" and can never be marked ready
    /// (it is escalated as a conflict instead). Defaults to `[".github/workflows/"]`
    /// (GitHub Actions workflow definitions); a repo can extend it (e.g. `"ci/"`).
    /// `serde(default)` so a config jsonb stored before this field existed still
    /// deserializes (back-compat for already-connected repos).
    #[serde(default = "default_gating_paths")]
    pub gating_paths: Vec<String>,
    /// Retry ceiling: a Failed work item with this many attempts (or more) escalates
    /// `retries_exhausted` instead of being re-queued for another attempt. Per-repo
    /// (consistent with `gating_paths`); defaults to 3. `serde(default)` for back-compat
    /// with config jsonb stored before this field existed.
    #[serde(default = "default_retry_cap")]
    pub retry_cap: u32,
    /// Review-rework ceiling: at this many changes-requested cycles the loop escalates
    /// `loop_cap` instead of dispatching yet another coder. Per-repo; defaults to 5.
    /// `serde(default)` for back-compat with older stored config jsonb.
    #[serde(default = "default_loop_cap")]
    pub loop_cap: u32,
}

/// Global capacity limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Global ceiling across all workers in this repo.
    pub max_concurrent_workers: u32,
}

/// A single configured worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    pub name: String,
    pub role: Role,
    pub on: Vec<Trigger>,
    pub max_concurrent: Option<u32>,
    pub model: Option<String>,
    pub instructions: Option<String>,
    pub instructions_file: Option<String>,
}

/// The role a worker plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Manager,
    Coder,
    Reviewer,
    Tester,
}

/// What wakes a worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trigger {
    IssueLabeled,
    CrOpened,
    CrUpdated,
    CrChangesRequested,
    ReviewRequested,
    IssueComment,
    Manager,
    /// A timer; carries the cron expression (UTC).
    Schedule(String),
}

/// Errors produced while parsing or validating a config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to parse config YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("unsupported config version {0}; expected 1")]
    UnsupportedVersion(u32),
    #[error("limits.max_concurrent_workers must be greater than 0")]
    NonPositiveGlobalCap,
    #[error("settings.retry_cap must be greater than 0")]
    NonPositiveRetryCap,
    #[error("settings.loop_cap must be greater than 0")]
    NonPositiveLoopCap,
    #[error("worker '{0}': max_concurrent must be greater than 0")]
    NonPositiveWorkerCap(String),
    #[error("duplicate worker name '{0}'")]
    DuplicateWorkerName(String),
    #[error("worker '{worker}': '{trigger}' is not a known trigger")]
    UnknownTrigger { worker: String, trigger: String },
    #[error("worker '{worker}': invalid cron expression '{cron}'")]
    InvalidCron { worker: String, cron: String },
    #[error("worker '{worker}' (role {role:?}): only the manager may subscribe to issue_labeled (manager-first)")]
    NonManagerIssueLabeled { worker: String, role: Role },
    #[error("at most one worker may subscribe to issue_labeled (a labelled issue yields a single Manager work item); found both '{first}' and '{second}'")]
    MultipleIssueLabeledSubscribers { first: String, second: String },
    #[error("worker '{0}': instructions_file must be a non-empty path")]
    EmptyInstructionsFile(String),
}

/// Parse and validate a `.factory/config.yml` document.
pub fn parse(yaml: &str) -> Result<Config, ConfigError> {
    let raw: RawConfig = serde_yaml::from_str(yaml)?;

    if raw.version != 1 {
        return Err(ConfigError::UnsupportedVersion(raw.version));
    }
    if raw.limits.max_concurrent_workers == 0 {
        return Err(ConfigError::NonPositiveGlobalCap);
    }
    if raw.settings.retry_cap == 0 {
        return Err(ConfigError::NonPositiveRetryCap);
    }
    if raw.settings.loop_cap == 0 {
        return Err(ConfigError::NonPositiveLoopCap);
    }

    let mut seen = HashSet::new();
    // Tracks the single worker allowed to subscribe to issue_labeled. A labelled
    // issue must yield exactly one Manager work item; two subscribers would
    // collide on the (repo, subject, kind) index and silently drop one.
    let mut issue_labeled_subscriber: Option<&str> = None;
    let mut workers = Vec::with_capacity(raw.workers.len());
    for w in &raw.workers {
        if !seen.insert(w.name.as_str()) {
            return Err(ConfigError::DuplicateWorkerName(w.name.clone()));
        }
        if let Some(cap) = w.max_concurrent {
            if cap == 0 {
                return Err(ConfigError::NonPositiveWorkerCap(w.name.clone()));
            }
        }
        if let Some(file) = &w.instructions_file {
            if file.trim().is_empty() {
                return Err(ConfigError::EmptyInstructionsFile(w.name.clone()));
            }
        }

        let mut triggers = Vec::with_capacity(w.on.len());
        for raw_trigger in &w.on {
            let trigger = convert_trigger(&w.name, raw_trigger)?;
            // Manager-first: only the manager may be woken directly by a labelled issue.
            if trigger == Trigger::IssueLabeled {
                if w.role != Role::Manager {
                    return Err(ConfigError::NonManagerIssueLabeled {
                        worker: w.name.clone(),
                        role: w.role,
                    });
                }
                if let Some(first) = issue_labeled_subscriber {
                    return Err(ConfigError::MultipleIssueLabeledSubscribers {
                        first: first.to_owned(),
                        second: w.name.clone(),
                    });
                }
                issue_labeled_subscriber = Some(&w.name);
            }
            triggers.push(trigger);
        }

        workers.push(Worker {
            name: w.name.clone(),
            role: w.role,
            on: triggers,
            max_concurrent: w.max_concurrent,
            model: w.model.clone(),
            instructions: w.instructions.clone(),
            instructions_file: w.instructions_file.clone(),
        });
    }

    Ok(Config {
        version: raw.version,
        settings: Settings {
            label: raw.settings.label,
            default_model: raw.settings.default_model,
            instructions: raw.settings.instructions,
            gating_paths: raw.settings.gating_paths,
            retry_cap: raw.settings.retry_cap,
            loop_cap: raw.settings.loop_cap,
        },
        limits: Limits {
            max_concurrent_workers: raw.limits.max_concurrent_workers,
        },
        workers,
    })
}

/// Compose a worker's effective prompt from its layers, in order:
/// role default → repo-wide (`settings.instructions`) → per-worker. Empty layers
/// are skipped; present layers are joined with a blank line.
pub fn resolve_prompt(role_default: &str, repo_wide: Option<&str>, worker: Option<&str>) -> String {
    [Some(role_default), repo_wide, worker]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|layer| !layer.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn convert_trigger(worker: &str, raw: &RawTrigger) -> Result<Trigger, ConfigError> {
    match raw {
        RawTrigger::Schedule { schedule } => {
            validate_cron(worker, schedule)?;
            Ok(Trigger::Schedule(schedule.clone()))
        }
        RawTrigger::Named(name) => match name.as_str() {
            "issue_labeled" => Ok(Trigger::IssueLabeled),
            "cr_opened" => Ok(Trigger::CrOpened),
            "cr_updated" => Ok(Trigger::CrUpdated),
            "cr_changes_requested" => Ok(Trigger::CrChangesRequested),
            "review_requested" => Ok(Trigger::ReviewRequested),
            "issue_comment" => Ok(Trigger::IssueComment),
            "manager" => Ok(Trigger::Manager),
            other => Err(ConfigError::UnknownTrigger {
                worker: worker.to_owned(),
                trigger: other.to_owned(),
            }),
        },
    }
}

/// Validate a worker's cron expression by fully parsing it (not just a shape
/// check), so an unschedulable expression is rejected at config-apply time.
fn validate_cron(worker: &str, cron: &str) -> Result<(), ConfigError> {
    parse_cron_schedule(cron)
        .map(|_| ())
        .ok_or_else(|| ConfigError::InvalidCron {
            worker: worker.to_owned(),
            cron: cron.to_owned(),
        })
}

/// Parse a cron expression into a schedule. We accept standard 5-field Unix cron
/// (`min hour dom mon dow`) by normalizing to the `cron` crate's 6-field form
/// (`sec min hour dom mon dow`, seconds = 0); 6- and 7-field forms pass through.
/// Returns `None` if the expression is unparseable.
fn parse_cron_schedule(cron: &str) -> Option<cron::Schedule> {
    let fields = cron.split_whitespace().count();
    let normalized = if fields == 5 {
        format!("0 {cron}")
    } else {
        cron.to_owned()
    };
    cron::Schedule::from_str(&normalized).ok()
}

/// The scheduled occurrences of `cron` in the half-open interval `(after, until]`
/// (UTC). Used by the intake scheduler to find ticks due since the last fire.
/// Bounded to avoid unbounded iteration on a pathological window.
pub fn schedule_due_between(
    cron: &str,
    after: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, ConfigError> {
    let schedule = parse_cron_schedule(cron).ok_or_else(|| ConfigError::InvalidCron {
        worker: "<scheduler>".to_owned(),
        cron: cron.to_owned(),
    })?;
    Ok(schedule
        .after(&after)
        .take_while(|dt| *dt <= until)
        .take(1000)
        .collect())
}

// ---- Raw (on-disk) shapes; converted to the validated types above. ----

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    #[serde(default)]
    settings: RawSettings,
    #[serde(default)]
    limits: RawLimits,
    #[serde(default)]
    workers: Vec<RawWorker>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSettings {
    #[serde(default = "default_label")]
    label: String,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default = "default_gating_paths")]
    gating_paths: Vec<String>,
    #[serde(default = "default_retry_cap")]
    retry_cap: u32,
    #[serde(default = "default_loop_cap")]
    loop_cap: u32,
}

impl Default for RawSettings {
    fn default() -> Self {
        Self {
            label: default_label(),
            default_model: None,
            instructions: None,
            gating_paths: default_gating_paths(),
            retry_cap: default_retry_cap(),
            loop_cap: default_loop_cap(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    #[serde(default = "default_global_cap")]
    max_concurrent_workers: u32,
}

impl Default for RawLimits {
    fn default() -> Self {
        Self {
            max_concurrent_workers: default_global_cap(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorker {
    name: String,
    role: Role,
    #[serde(default)]
    on: Vec<RawTrigger>,
    #[serde(default)]
    max_concurrent: Option<u32>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    instructions_file: Option<String>,
}

// NOTE: `#[serde(untagged)]` cannot enforce `deny_unknown_fields` inside its
// variants, so an extra key on a `{ schedule: "..." }` trigger (e.g.
// `{ schedule: "...", typo: 1 }`) is silently ignored rather than rejected.
// Low severity (the schedule value still round-trips, and the cron string
// itself is fully parsed/validated); rejecting the stray key is a later polish.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawTrigger {
    Named(String),
    Schedule { schedule: String },
}

fn default_label() -> String {
    "factory".to_owned()
}

fn default_global_cap() -> u32 {
    5
}

/// The default set of gating-check path prefixes: GitHub Actions workflow
/// definitions. A CR touching any of these alters the checks that gate its own
/// merge, so it can never be marked ready (see [`Settings::gating_paths`]).
fn default_gating_paths() -> Vec<String> {
    vec![".github/workflows/".to_owned()]
}

/// The platform-default retry ceiling (`settings.retry_cap`): a Failed work item with
/// this many attempts (or more) escalates rather than being re-queued.
fn default_retry_cap() -> u32 {
    3
}

/// The platform-default review-rework ceiling (`settings.loop_cap`): at this many
/// changes-requested cycles the loop escalates instead of dispatching another coder.
fn default_loop_cap() -> u32 {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
version: 1
settings:
  label: factory
  default_model: claude-opus-4-8
  instructions: |
    Repo-wide guidance.
limits:
  max_concurrent_workers: 5
workers:
  - name: manager
    role: manager
    on: [issue_labeled, cr_opened, manager]
    max_concurrent: 1
  - name: bug-fixer
    role: coder
    on:
      - manager
      - cr_changes_requested
    max_concurrent: 3
    instructions: |
      Always add regression tests.
  - name: nightly
    role: coder
    on:
      - schedule: "0 2 * * *"
    max_concurrent: 1
    instructions_file: .factory/prompts/nightly.md
"#;

    #[test]
    fn parses_a_valid_config() {
        let cfg = parse(VALID).expect("valid config should parse");
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.settings.label, "factory");
        assert_eq!(cfg.limits.max_concurrent_workers, 5);
        assert_eq!(cfg.workers.len(), 3);

        let manager = &cfg.workers[0];
        assert_eq!(manager.role, Role::Manager);
        assert!(manager.on.contains(&Trigger::IssueLabeled));

        let nightly = &cfg.workers[2];
        assert_eq!(nightly.on, vec![Trigger::Schedule("0 2 * * *".to_owned())]);
    }

    #[test]
    fn defaults_apply_when_blocks_omitted() {
        let cfg = parse("version: 1\n").expect("minimal config should parse");
        assert_eq!(cfg.settings.label, "factory");
        assert_eq!(cfg.limits.max_concurrent_workers, 5);
        assert!(cfg.workers.is_empty());
        // The gating-paths default applies when `settings.gating_paths` is omitted.
        assert_eq!(
            cfg.settings.gating_paths,
            vec![".github/workflows/".to_owned()]
        );
        // The retry/loop caps default when omitted (the platform defaults).
        assert_eq!(cfg.settings.retry_cap, 3);
        assert_eq!(cfg.settings.loop_cap, 5);
    }

    #[test]
    fn reads_custom_retry_and_loop_caps() {
        let cfg = parse("version: 1\nsettings:\n  label: factory\n  retry_cap: 7\n  loop_cap: 9\n")
            .expect("config with custom caps should parse");
        assert_eq!(cfg.settings.retry_cap, 7, "a custom retry_cap is read");
        assert_eq!(cfg.settings.loop_cap, 9, "a custom loop_cap is read");
    }

    #[test]
    fn rejects_zero_retry_cap() {
        let err = parse("version: 1\nsettings:\n  label: factory\n  retry_cap: 0\n");
        assert!(matches!(err, Err(ConfigError::NonPositiveRetryCap)));
    }

    #[test]
    fn rejects_zero_loop_cap() {
        let err = parse("version: 1\nsettings:\n  label: factory\n  loop_cap: 0\n");
        assert!(matches!(err, Err(ConfigError::NonPositiveLoopCap)));
    }

    #[test]
    fn reads_custom_gating_paths() {
        let cfg = parse(
            "version: 1\nsettings:\n  label: factory\n  gating_paths:\n    - \"ci/\"\n    - \".github/workflows/\"\n",
        )
        .expect("config with custom gating_paths should parse");
        assert_eq!(
            cfg.settings.gating_paths,
            vec!["ci/".to_owned(), ".github/workflows/".to_owned()],
            "a custom gating_paths list is read verbatim"
        );
    }

    #[test]
    fn rejects_unsupported_version() {
        assert!(matches!(
            parse("version: 2\n"),
            Err(ConfigError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn rejects_unknown_role() {
        let err = parse("version: 1\nworkers:\n  - name: x\n    role: wizard\n    on: []\n");
        assert!(matches!(err, Err(ConfigError::Yaml(_))));
    }

    #[test]
    fn rejects_zero_global_cap() {
        let err = parse("version: 1\nlimits:\n  max_concurrent_workers: 0\n");
        assert!(matches!(err, Err(ConfigError::NonPositiveGlobalCap)));
    }

    #[test]
    fn rejects_zero_worker_cap() {
        let err = parse(
            "version: 1\nworkers:\n  - name: c\n    role: coder\n    on: [manager]\n    max_concurrent: 0\n",
        );
        assert!(matches!(err, Err(ConfigError::NonPositiveWorkerCap(name)) if name == "c"));
    }

    #[test]
    fn rejects_duplicate_worker_names() {
        let err = parse(
            "version: 1\nworkers:\n  - name: dup\n    role: coder\n    on: [manager]\n  - name: dup\n    role: reviewer\n    on: [cr_opened]\n",
        );
        assert!(matches!(err, Err(ConfigError::DuplicateWorkerName(name)) if name == "dup"));
    }

    #[test]
    fn rejects_unknown_trigger() {
        let err =
            parse("version: 1\nworkers:\n  - name: c\n    role: coder\n    on: [on_full_moon]\n");
        assert!(matches!(
            err,
            Err(ConfigError::UnknownTrigger { trigger, .. }) if trigger == "on_full_moon"
        ));
    }

    #[test]
    fn rejects_bad_cron() {
        let err = parse(
            "version: 1\nworkers:\n  - name: c\n    role: coder\n    on:\n      - schedule: \"not a cron\"\n",
        );
        assert!(matches!(err, Err(ConfigError::InvalidCron { .. })));
    }

    #[test]
    fn rejects_out_of_range_cron_field() {
        // Right shape (5 fields) but minute 99 is out of range — real parsing
        // (not just a field-count check) must reject it.
        let err = parse(
            "version: 1\nworkers:\n  - name: c\n    role: coder\n    on:\n      - schedule: \"99 2 * * *\"\n",
        );
        assert!(matches!(err, Err(ConfigError::InvalidCron { .. })));
    }

    #[test]
    fn schedule_due_between_finds_the_minute_boundary() {
        use chrono::TimeZone;
        // Every day at 02:00 UTC. A window spanning 01:59→02:01 contains exactly
        // the 02:00 tick; a window before it contains none.
        let before = Utc.with_ymd_and_hms(2026, 1, 1, 1, 59, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 2, 1, 0).unwrap();
        let due = schedule_due_between("0 2 * * *", before, after).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0], Utc.with_ymd_and_hms(2026, 1, 1, 2, 0, 0).unwrap());

        let none = schedule_due_between(
            "0 2 * * *",
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap(),
        )
        .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn enforces_manager_first() {
        // A coder subscribed to issue_labeled is rejected.
        let err =
            parse("version: 1\nworkers:\n  - name: c\n    role: coder\n    on: [issue_labeled]\n");
        assert!(matches!(
            err,
            Err(ConfigError::NonManagerIssueLabeled { worker, role })
                if worker == "c" && role == Role::Coder
        ));
    }

    #[test]
    fn config_jsonb_without_gating_paths_still_deserializes() {
        // Back-compat: a config stored as jsonb before `gating_paths` existed must
        // still read back (the field defaults), so already-connected repos don't break.
        let stored = serde_json::json!({
            "version": 1,
            "settings": { "label": "factory", "default_model": null, "instructions": null },
            "limits": { "max_concurrent_workers": 5 },
            "workers": []
        });
        let cfg: Config = serde_json::from_value(stored).expect("old jsonb deserializes");
        assert_eq!(
            cfg.settings.gating_paths,
            vec![".github/workflows/".to_owned()]
        );
        // The retry/loop caps also default for jsonb stored before they existed.
        assert_eq!(cfg.settings.retry_cap, 3);
        assert_eq!(cfg.settings.loop_cap, 5);
    }

    #[test]
    fn rejects_multiple_issue_labeled_subscribers() {
        // Two managers both on issue_labeled would collide on the work-item index
        // and silently drop one — reject at validation.
        let err = parse(
            "version: 1\nworkers:\n  - name: m1\n    role: manager\n    on: [issue_labeled]\n  - name: m2\n    role: manager\n    on: [issue_labeled]\n",
        );
        assert!(matches!(
            err,
            Err(ConfigError::MultipleIssueLabeledSubscribers { first, second })
                if first == "m1" && second == "m2"
        ));
    }

    #[test]
    fn rejects_empty_instructions_file() {
        let err = parse(
            "version: 1\nworkers:\n  - name: c\n    role: coder\n    on: [manager]\n    instructions_file: \"   \"\n",
        );
        assert!(matches!(err, Err(ConfigError::EmptyInstructionsFile(name)) if name == "c"));
    }

    #[test]
    fn rejects_unknown_field() {
        let err = parse("version: 1\nbogus: true\n");
        assert!(matches!(err, Err(ConfigError::Yaml(_))));
    }

    #[test]
    fn resolves_prompt_layers_in_order() {
        let out = resolve_prompt("role default", Some("repo wide"), Some("worker specific"));
        assert_eq!(out, "role default\n\nrepo wide\n\nworker specific");
    }

    #[test]
    fn resolves_prompt_skips_empty_layers() {
        let out = resolve_prompt("role default", None, Some("   "));
        assert_eq!(out, "role default");
    }

    /// Regression guard for the YAML `on:` landmine: under YAML 1.1, the bare
    /// scalar `on` resolves to boolean `true`, which would make the `on:` key
    /// miss the struct field and silently drop all triggers. serde_yaml 0.9
    /// (YAML 1.2 core schema, pinned in Cargo.lock) reads it as the literal key,
    /// so triggers ARE captured. If a future YAML-parser swap reintroduces 1.1
    /// coercion, this test fails loudly.
    #[test]
    fn on_key_is_not_coerced_to_yaml_boolean() {
        let cfg = parse(
            "version: 1\nworkers:\n  - name: c\n    role: coder\n    on: [manager, cr_updated]\n",
        )
        .expect("config with an `on:` key should parse");
        assert_eq!(
            cfg.workers[0].on,
            vec![Trigger::Manager, Trigger::CrUpdated],
            "the `on:` key must capture triggers, not be dropped as a boolean key"
        );
    }
}
