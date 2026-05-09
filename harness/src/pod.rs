//! Pod bring-up for the user-facing `nanna` binary.
//!
//! When `nanna chat` / `nanna agent` / `nanna health` runs on a host where
//! `nanna-coder-pod` (Ollama at :11434) isn't already up, this module brings
//! it up via `nix run .#start-pod` so users don't have to remember to run
//! `scripts/install.sh` separately. When the binary itself runs *inside*
//! that pod (the install.sh-registered MCP entry at scripts/install.sh:407
//! does this), the in-container guard short-circuits to avoid recursion.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info};

const OLLAMA_PROBE_URL: &str = "http://localhost:11434/api/tags";
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
const HEALTH_WAIT_BUDGET: Duration = Duration::from_secs(60);
const HEALTH_WAIT_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct EnsureConfig {
    pub disabled: bool,
    pub probe_url: String,
    pub health_wait_budget: Duration,
}

impl Default for EnsureConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            probe_url: OLLAMA_PROBE_URL.to_string(),
            health_wait_budget: HEALTH_WAIT_BUDGET,
        }
    }
}

impl EnsureConfig {
    /// Honour `--no-ensure-pod` and `NANNA_NO_ENSURE_POD=1`.
    pub fn from_env_and_flag(no_ensure_pod_flag: bool) -> Self {
        let env_disabled = std::env::var("NANNA_NO_ENSURE_POD")
            .ok()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        Self {
            disabled: no_ensure_pod_flag || env_disabled,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// Probe succeeded immediately; nothing to do.
    AlreadyUp,
    /// Pod was down; we ran `nix run .#start-pod` and it came up.
    BroughtUp,
    /// Skipped because the binary is running inside a container, or because
    /// the caller passed `--no-ensure-pod` / `NANNA_NO_ENSURE_POD=1`.
    Skipped(SkipReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    InsideContainer,
    Disabled,
}

#[derive(Error, Debug)]
pub enum PodError {
    #[error("pod bring-up failed: `{cmd}` exited {status}: {stderr}")]
    BringUpFailed {
        cmd: String,
        status: String,
        stderr: String,
    },
    #[error("pod is not up after {budget:?} of waiting on {url}")]
    HealthTimeout { url: String, budget: Duration },
    #[error("could not spawn `{cmd}`: {source}")]
    Spawn {
        cmd: String,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "ollama not reachable at {url} and no bring-up command available. \
         Run `scripts/install.sh` (or `nix run .#start-pod`) and re-try, \
         or pass --no-ensure-pod to skip this check."
    )]
    NoBringUpCommand { url: String },
}

/// Detect whether the current process is running inside a container.
fn is_inside_container() -> bool {
    if Path::new("/run/.containerenv").exists() {
        return true;
    }
    if Path::new("/.dockerenv").exists() {
        return true;
    }
    if std::env::var_os("container").is_some() {
        return true;
    }
    false
}

/// Probe Ollama's `/api/tags` endpoint with a short timeout.
fn probe_ollama(url: &str) -> bool {
    // Use blocking reqwest to keep the call site simple; this is one HTTP
    // GET on startup, not a hot path.
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(url)
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Choose a bring-up command. Prefer `nix run .#start-pod`; fall back to
/// `podman play kube ${NANNA_POD_CONFIG}` when the env var is set.
fn bring_up_command() -> Option<(String, Vec<String>)> {
    if which::which("nix").is_ok() {
        return Some((
            "nix".to_string(),
            vec!["run".to_string(), ".#start-pod".to_string()],
        ));
    }
    if let Ok(pod_config) = std::env::var("NANNA_POD_CONFIG") {
        if which::which("podman").is_ok() {
            return Some((
                "podman".to_string(),
                vec!["play".to_string(), "kube".to_string(), pod_config],
            ));
        }
    }
    None
}

fn wait_for_ollama(url: &str, budget: Duration) -> Result<(), PodError> {
    let start = Instant::now();
    loop {
        if probe_ollama(url) {
            return Ok(());
        }
        if start.elapsed() >= budget {
            return Err(PodError::HealthTimeout {
                url: url.to_string(),
                budget,
            });
        }
        std::thread::sleep(HEALTH_WAIT_INTERVAL);
    }
}

/// Ensure the nanna pod is reachable; bring it up if it isn't.
pub fn ensure_running(cfg: &EnsureConfig) -> Result<EnsureOutcome, PodError> {
    if cfg.disabled {
        debug!("pod ensure: skipped (--no-ensure-pod / NANNA_NO_ENSURE_POD)");
        return Ok(EnsureOutcome::Skipped(SkipReason::Disabled));
    }
    if is_inside_container() {
        debug!("pod ensure: skipped (running inside container)");
        return Ok(EnsureOutcome::Skipped(SkipReason::InsideContainer));
    }
    if probe_ollama(&cfg.probe_url) {
        debug!("pod ensure: ollama already reachable at {}", cfg.probe_url);
        return Ok(EnsureOutcome::AlreadyUp);
    }

    let (cmd, args) = bring_up_command().ok_or_else(|| PodError::NoBringUpCommand {
        url: cfg.probe_url.clone(),
    })?;
    info!(
        "pod ensure: ollama not reachable; running `{} {}`",
        cmd,
        args.join(" ")
    );
    let output = Command::new(&cmd)
        .args(&args)
        .output()
        .map_err(|e| PodError::Spawn {
            cmd: format!("{} {}", cmd, args.join(" ")),
            source: e,
        })?;
    if !output.status.success() {
        return Err(PodError::BringUpFailed {
            cmd: format!("{} {}", cmd, args.join(" ")),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    wait_for_ollama(&cfg.probe_url, cfg.health_wait_budget)?;
    info!("pod ensure: ollama healthy at {}", cfg.probe_url);
    Ok(EnsureOutcome::BroughtUp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_is_enabled() {
        let cfg = EnsureConfig::default();
        assert!(!cfg.disabled);
        assert_eq!(cfg.probe_url, OLLAMA_PROBE_URL);
    }

    #[test]
    fn config_flag_disables() {
        let cfg = EnsureConfig::from_env_and_flag(true);
        assert!(cfg.disabled);
    }

    #[test]
    fn config_env_disables() {
        // We can't safely set/unset env in parallel tests; emulate the flag
        // path which has the same effect.
        let cfg = EnsureConfig {
            disabled: true,
            ..EnsureConfig::default()
        };
        let outcome = ensure_running(&cfg).unwrap();
        assert_eq!(outcome, EnsureOutcome::Skipped(SkipReason::Disabled));
    }

    #[test]
    fn disabled_short_circuits_without_probing() {
        // Use a port that nothing should be listening on so the test fails
        // loudly if the disabled path accidentally probes.
        let cfg = EnsureConfig {
            disabled: true,
            probe_url: "http://127.0.0.1:1/never".to_string(),
            ..EnsureConfig::default()
        };
        let outcome = ensure_running(&cfg).unwrap();
        assert_eq!(outcome, EnsureOutcome::Skipped(SkipReason::Disabled));
    }

    #[test]
    fn probe_ollama_returns_false_for_unreachable() {
        // Reserved-for-discard port; no server should answer here.
        assert!(!probe_ollama("http://127.0.0.1:1/api/tags"));
    }
}
