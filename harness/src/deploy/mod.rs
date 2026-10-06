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
//! max_divergence = 0.05
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
    format_duration, plan_for_repo, plan_for_repo_checked, plan_for_repo_with_windows, DeployPlan,
    DeployStep, Enforcement, Precondition, PreconditionKind, StepKind,
};
pub use template::{
    DeployTemplate, Health, OnBreach, RiskClass, RiskSpec, RiskThresholds, Rollback, Rollout,
    Shadow, ShadowCompare, Strategy, Target, TargetKind, DEFAULT_MAX_DIVERGENCE,
};
pub use validate::{min_span, min_steps, strategy_allowed};

use crate::identity::config_dir_from;
use crate::windows::{WindowSet, WINDOWS_FILE_NAME};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Where the host keeps the availability windows deploys are gated on:
/// `windows.toml` inside `$NANNA_CONFIG_DIR`, else `$XDG_CONFIG_HOME/nanna`,
/// else `$HOME/.config/nanna`.
///
/// The file is never read from the target repository: agents can write
/// there, so a repository-local file would let an agent loosen the window it
/// is gated by. The repository's `deploy.toml` may only name a window.
///
/// ```
/// use harness::deploy::host_windows_path_from;
/// use std::path::PathBuf;
///
/// let lookup = |key: &str| (key == "NANNA_CONFIG_DIR").then(|| "/etc/nanna".into());
/// assert_eq!(host_windows_path_from(&lookup), Some(PathBuf::from("/etc/nanna").join("windows.toml")));
/// ```
pub fn host_windows_path_from(lookup: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    config_dir_from(lookup).map(|dir| dir.join(WINDOWS_FILE_NAME))
}

/// Load the host's window set through `lookup`; `None` when the host has no
/// configuration directory or no windows file.
pub fn host_windows_from(
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Option<WindowSet>, DeployError> {
    let Some(path) = host_windows_path_from(lookup).filter(|p| p.is_file()) else {
        return Ok(None);
    };
    WindowSet::load(&path)
        .map(Some)
        .map_err(|source| DeployError::WindowSet { path, source })
}

/// [`host_windows_from`] over the process environment.
pub fn host_windows() -> Result<Option<WindowSet>, DeployError> {
    host_windows_from(&|key| std::env::var_os(key))
}

/// The `owner/name` of a git remote URL, or `None` when it names no
/// repository. Handles `https://host/owner/name(.git)`, `ssh://git@host/owner/name`
/// and scp-style `git@host:owner/name.git`.
///
/// ```
/// use harness::deploy::repo_slug_from_remote;
///
/// assert_eq!(repo_slug_from_remote("git@github.com:Org/repo.git").as_deref(), Some("Org/repo"));
/// assert_eq!(repo_slug_from_remote("https://github.com/Org/repo/").as_deref(), Some("Org/repo"));
/// assert_eq!(repo_slug_from_remote("/srv/git/repo"), None);
/// ```
pub fn repo_slug_from_remote(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.split_once(':')?.1,
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.rsplit('/');
    let name = parts.next().filter(|p| !p.is_empty())?;
    let owner = parts.next().filter(|p| !p.is_empty())?;
    Some(format!("{owner}/{name}"))
}

/// The repository identity of the checkout at `repo`: the `owner/name` of
/// its `origin` remote. `None` when it has no such remote.
pub fn repo_identity(repo: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    repo_slug_from_remote(&String::from_utf8_lossy(&output.stdout))
}

/// Directory, relative to the repository root, holding Nanna's per-repo config.
pub const DEPLOY_DIR: &str = ".nanna";

/// File name of the deployment template inside [`DEPLOY_DIR`].
pub const DEPLOY_FILE_NAME: &str = "deploy.toml";

/// The environment name that requires availability windows and health gates.
pub const PRODUCTION_ENV: &str = "production";

/// Environment names that are explicitly not production. Any name outside
/// this list is treated as production so a misspelt or aliased production
/// environment fails closed.
pub const NON_PRODUCTION_ENVS: &[&str] = &[
    "sandbox",
    "staging",
    "stage",
    "dev",
    "development",
    "test",
    "testing",
    "qa",
    "preview",
    "local",
];

/// True unless `env` is on the [`NON_PRODUCTION_ENVS`] allowlist, ignoring
/// ASCII case.
///
/// A name such as `prod` or `live` that is not on the allowlist therefore
/// still gets the availability window and health gates that
/// [`PRODUCTION_ENV`] exists to enforce.
///
/// ```
/// use harness::deploy::is_production_env;
///
/// assert!(is_production_env("production"));
/// assert!(is_production_env("Production"));
/// assert!(is_production_env("PRODUCTION"));
/// assert!(is_production_env("prod"));
/// assert!(is_production_env("eu-west-1"));
/// assert!(!is_production_env("staging"));
/// assert!(!is_production_env("Sandbox"));
/// ```
pub fn is_production_env(env: &str) -> bool {
    !NON_PRODUCTION_ENVS
        .iter()
        .any(|known| env.eq_ignore_ascii_case(known))
}

#[cfg(test)]
mod env_tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn allowlisted_names_are_not_production() {
        for env in NON_PRODUCTION_ENVS {
            assert!(!is_production_env(env), "{env}");
            assert!(!is_production_env(&env.to_ascii_uppercase()), "{env}");
        }
    }

    #[test]
    fn production_aliases_and_blank_are_production() {
        for env in ["production", "prod", "prd", "live", "", " staging"] {
            assert!(is_production_env(env), "{env:?}");
        }
    }

    proptest! {
        #[test]
        fn only_allowlisted_names_escape_production(env in "[a-zA-Z0-9 _-]{0,12}") {
            let listed = NON_PRODUCTION_ENVS
                .iter()
                .any(|k| k.eq_ignore_ascii_case(&env));
            prop_assert_eq!(is_production_env(&env), !listed);
        }
    }
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
    /// A production plan names a window but the host has no window set.
    #[error("window `{window}` is required but the host has no windows.toml (set NANNA_CONFIG_DIR); refusing")]
    HostWindowsMissing {
        /// Window the template requests.
        window: String,
    },
    /// The host's `windows.toml` exists but failed to load.
    #[error("failed to load {}: {source}", path.display())]
    WindowSet {
        /// Path of the window set that failed to load.
        path: PathBuf,
        /// Underlying window-loading failure.
        #[source]
        source: crate::windows::WindowError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_reduce_to_owner_and_name() {
        for (url, slug) in [
            ("https://github.com/Org/repo.git", Some("Org/repo")),
            ("https://github.com/Org/repo", Some("Org/repo")),
            ("https://github.com/Org/repo/\n", Some("Org/repo")),
            ("ssh://git@host:22/Org/repo.git", Some("Org/repo")),
            ("git@github.com:Org/repo.git", Some("Org/repo")),
            ("https://host/group/sub/repo.git", Some("sub/repo")),
            ("https://host", None),
            ("https://host/repo", None),
            ("https://host//repo", None),
            ("/srv/git/repo", None),
            ("", None),
        ] {
            assert_eq!(repo_slug_from_remote(url).as_deref(), slug, "{url:?}");
        }
    }

    #[test]
    fn host_windows_are_absent_without_a_config_dir_or_file() {
        assert!(host_windows_from(&|_| None).unwrap().is_none());
        let empty = tempfile::tempdir().unwrap();
        let lookup = |key: &str| (key == "NANNA_CONFIG_DIR").then(|| empty.path().into());
        assert!(host_windows_from(&lookup).unwrap().is_none());
    }

    #[test]
    fn a_checkout_without_an_origin_has_no_identity() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(repo_identity(dir.path()), None);
        assert_eq!(repo_identity(&dir.path().join("missing")), None);
    }
}
