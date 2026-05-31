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

    let mut seen = HashSet::new();
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
            if trigger == Trigger::IssueLabeled && w.role != Role::Manager {
                return Err(ConfigError::NonManagerIssueLabeled {
                    worker: w.name.clone(),
                    role: w.role,
                });
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

/// Lightweight cron shape check: 5 (standard) or 6 (with seconds) whitespace-
/// separated fields. Full expression validation lands with the scheduler (P2).
fn validate_cron(worker: &str, cron: &str) -> Result<(), ConfigError> {
    let fields = cron.split_whitespace().count();
    if (5..=6).contains(&fields) {
        Ok(())
    } else {
        Err(ConfigError::InvalidCron {
            worker: worker.to_owned(),
            cron: cron.to_owned(),
        })
    }
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
}

impl Default for RawSettings {
    fn default() -> Self {
        Self {
            label: default_label(),
            default_model: None,
            instructions: None,
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
// Low severity (the schedule value still round-trips); stricter trigger
// validation is a P2 follow-up when the scheduler also adds full cron parsing.
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
