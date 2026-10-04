//! Per-repository deployment template (`.nanna/deploy.toml`).
//!
//! The template is human-authored infrastructure-as-code describing how a
//! repository's deployable is rolled out: the target registry and image, the
//! risk class of the system, the rollout strategy and its traffic steps, the
//! health gates, and what happens on a health breach. Loading it yields a
//! [`DeployTemplate`]; [`DeployTemplate::plan`] turns it into an ordered
//! [`DeployPlan`] whose steps carry their preconditions, ready for a rollout
//! executor.
//!
//! ```toml
//! [target]
//! kind = "container-registry+serverless"
//! registry = "registry.example.invalid/ns"
//! image = "app"
//! environments = ["sandbox", "staging", "production"]
//!
//! [risk]
//! class = "core"
//!
//! [rollout]
//! strategy = "gradual"
//! steps = [1, 5, 10, 25, 50, 75, 100]
//! min_step_duration = "1d"
//! windows = "business-hours"
//!
//! [health]
//! endpoints = ["/health/v1"]
//! error_rate_max = 0.01
//! latency_p99_max_ms = 800
//! bake_time = "30m"
//!
//! [rollback]
//! automatic = true
//! on_breach = "rollback"
//! retain_for = "2d"
//!
//! [shadow]
//! enabled = false
//! mirror_percent = 0
//! compare = ["status", "latency"]
//! ```
//!
//! The risk class sets a floor on how cautious the rollout must be:
//!
//! | class      | allowed strategies                         | minimum steps | minimum span |
//! |------------|--------------------------------------------|---------------|--------------|
//! | `unused`   | any                                        | 1             | none         |
//! | `internal` | `blue-green`, `gradual`, `shadow-then-gradual` | 1         | none         |
//! | `edge`     | `gradual`, `shadow-then-gradual`           | 3             | 1 day        |
//! | `core`     | `gradual`, `shadow-then-gradual`           | 7             | 7 days       |
//!
//! The span of a rollout is `steps × min_step_duration`. A more cautious
//! rollout than the class requires is always allowed.
//!
//! `class = "derived"` computes the class from the blast-radius score of the
//! change being deployed using `[risk.thresholds]`; see
//! [`DeployTemplate::resolve_risk`]. Such a template is validated against the
//! highest class its thresholds can produce.

mod init;
mod plan;
mod template;
mod validate;

pub use init::{init, starter_template};
pub use plan::{
    format_duration, plan_for_repo, plan_for_repo_checked, DeployPlan, DeployStep, Precondition,
    StepKind,
};
pub use template::{
    DeployTemplate, Health, OnBreach, RiskClass, RiskSpec, RiskThresholds, Rollback, Rollout,
    Shadow, ShadowCompare, Strategy, Target, TargetKind,
};
pub use validate::{min_span, min_steps, strategy_allowed};

use std::path::PathBuf;
use thiserror::Error;

/// Directory, relative to the repository root, holding Nanna's per-repo config.
pub const DEPLOY_DIR: &str = ".nanna";

/// File name of the deployment template inside [`DEPLOY_DIR`].
pub const DEPLOY_FILE_NAME: &str = "deploy.toml";

/// The environment name that requires availability windows and health gates.
pub const PRODUCTION_ENV: &str = "production";

/// True when `env` names the production environment, ignoring ASCII case.
///
/// A deploy template author who writes `environments = ["Production"]` means
/// the same thing as `"production"`; comparing case-sensitively would let a
/// capitalisation mismatch silently skip the window and health gates that
/// [`PRODUCTION_ENV`] exists to enforce.
///
/// ```
/// use harness::deploy::is_production_env;
///
/// assert!(is_production_env("production"));
/// assert!(is_production_env("Production"));
/// assert!(is_production_env("PRODUCTION"));
/// assert!(!is_production_env("staging"));
/// assert!(!is_production_env("prod"));
/// ```
pub fn is_production_env(env: &str) -> bool {
    env.eq_ignore_ascii_case(PRODUCTION_ENV)
}

/// Errors produced while loading, validating or planning a deployment template.
#[derive(Debug, Error)]
pub enum DeployError {
    /// The template file could not be read.
    #[error("failed to read {}: {source}", path.display())]
    Io {
        /// Path that was being read.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The template is not well-formed TOML for this schema.
    #[error("failed to parse {}: {source}", file.display())]
    Parse {
        /// Template that was being parsed.
        file: PathBuf,
        /// Underlying TOML error.
        #[source]
        source: toml::de::Error,
    },
    /// A field failed validation.
    #[error("{}: field `{field}` is invalid: {reason}", file.display())]
    InvalidField {
        /// Template that was being validated.
        file: PathBuf,
        /// Dotted TOML key that failed validation.
        field: &'static str,
        /// Human-readable explanation.
        reason: String,
    },
    /// The risk class is `derived` and no blast-radius score was supplied.
    #[error("risk class is derived: a blast-radius score is required to resolve it")]
    ScoreRequired,
    /// A plan was requested for an environment the template does not declare.
    #[error("unknown environment `{env}` (expected one of: {})", known.join(", "))]
    UnknownEnvironment {
        /// Environment that was requested.
        env: String,
        /// Environments the template declares.
        known: Vec<String>,
    },
    /// A starter template would overwrite an existing file.
    #[error("{} already exists; refusing to overwrite", path.display())]
    AlreadyExists {
        /// The existing template.
        path: PathBuf,
    },
    /// A co-located `windows.toml` exists but failed to load.
    #[error("failed to load {}: {source}", path.display())]
    WindowSet {
        /// Path of the window set that failed to load.
        path: PathBuf,
        /// Underlying window-loading failure.
        #[source]
        source: crate::windows::WindowError,
    },
}
