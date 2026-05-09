//! Pod bring-up for the user-facing `nanna` binary.
//!
//! When `nanna chat` / `nanna agent` / `nanna health` runs on a host where
//! `nanna-coder-pod` (Ollama at :11434) isn't already up, this module brings
//! it up via `nix run .#start-pod` so users don't have to remember to run
//! `scripts/install.sh` separately. When the binary itself runs *inside*
//! that pod (the install.sh-registered MCP entry at scripts/install.sh:407
//! does this), the in-container guard short-circuits to avoid recursion.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::process::Command;
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
    /// Optional override of the working directory used to look for
    /// `flake.nix`. Defaults to the process cwd when `None`. Tests use this
    /// to assert the flake-presence guard without mutating global state.
    pub flake_lookup_dir: Option<PathBuf>,
}

impl Default for EnsureConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            probe_url: OLLAMA_PROBE_URL.to_string(),
            health_wait_budget: HEALTH_WAIT_BUDGET,
            flake_lookup_dir: None,
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
///
/// `async` so the surrounding `ensure_running` does not block the Tokio
/// executor. Uses the `reqwest` async client.
async fn probe_ollama(url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(url).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// Choose a bring-up command. Prefer `nix run .#start-pod` *only when a
/// `flake.nix` is reachable from `flake_lookup_dir`* (`nix run .#start-pod`
/// resolves the flake from CWD; a `nanna agent --work-dir <arbitrary>` run
/// would otherwise fail with "flake not found"). Fall back to
/// `podman play kube ${NANNA_POD_CONFIG}` when the env var is set.
fn bring_up_command(flake_lookup_dir: &Path) -> Option<(String, Vec<String>)> {
    if which::which("nix").is_ok() && flake_lookup_dir.join("flake.nix").is_file() {
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

async fn wait_for_ollama(url: &str, budget: Duration) -> Result<(), PodError> {
    let start = Instant::now();
    loop {
        if probe_ollama(url).await {
            return Ok(());
        }
        if start.elapsed() >= budget {
            return Err(PodError::HealthTimeout {
                url: url.to_string(),
                budget,
            });
        }
        tokio::time::sleep(HEALTH_WAIT_INTERVAL).await;
    }
}

/// Ensure the nanna pod is reachable; bring it up if it isn't.
///
/// `async` because the network probe and the post-bring-up health wait both
/// run on the Tokio executor. Callers from `#[tokio::main]` can simply
/// `.await` this; non-async callers can wrap it in `tokio::runtime::Handle`.
pub async fn ensure_running(cfg: &EnsureConfig) -> Result<EnsureOutcome, PodError> {
    if cfg.disabled {
        debug!("pod ensure: skipped (--no-ensure-pod / NANNA_NO_ENSURE_POD)");
        return Ok(EnsureOutcome::Skipped(SkipReason::Disabled));
    }
    if is_inside_container() {
        debug!("pod ensure: skipped (running inside container)");
        return Ok(EnsureOutcome::Skipped(SkipReason::InsideContainer));
    }
    if probe_ollama(&cfg.probe_url).await {
        debug!("pod ensure: ollama already reachable at {}", cfg.probe_url);
        return Ok(EnsureOutcome::AlreadyUp);
    }

    let flake_dir = match &cfg.flake_lookup_dir {
        Some(p) => p.clone(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let (cmd, args) = bring_up_command(&flake_dir).ok_or_else(|| PodError::NoBringUpCommand {
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
        .await
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
    wait_for_ollama(&cfg.probe_url, cfg.health_wait_budget).await?;
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
        assert!(cfg.flake_lookup_dir.is_none());
    }

    #[test]
    fn config_flag_disables() {
        let cfg = EnsureConfig::from_env_and_flag(true);
        assert!(cfg.disabled);
    }

    #[tokio::test]
    async fn config_env_disables() {
        // We can't safely set/unset env in parallel tests; emulate the flag
        // path which has the same effect.
        let cfg = EnsureConfig {
            disabled: true,
            ..EnsureConfig::default()
        };
        let outcome = ensure_running(&cfg).await.unwrap();
        assert_eq!(outcome, EnsureOutcome::Skipped(SkipReason::Disabled));
    }

    #[tokio::test]
    async fn disabled_short_circuits_without_probing() {
        // Use a port that nothing should be listening on so the test fails
        // loudly if the disabled path accidentally probes.
        let cfg = EnsureConfig {
            disabled: true,
            probe_url: "http://127.0.0.1:1/never".to_string(),
            ..EnsureConfig::default()
        };
        let outcome = ensure_running(&cfg).await.unwrap();
        assert_eq!(outcome, EnsureOutcome::Skipped(SkipReason::Disabled));
    }

    #[tokio::test]
    async fn probe_ollama_returns_false_for_unreachable() {
        // Reserved-for-discard port; no server should answer here.
        assert!(!probe_ollama("http://127.0.0.1:1/api/tags").await);
    }

    #[tokio::test]
    async fn wait_for_ollama_times_out_quickly_on_unreachable() {
        // Sub-budget timeout against a guaranteed-unreachable endpoint
        // should produce HealthTimeout, not block forever.
        let started = Instant::now();
        let result =
            wait_for_ollama("http://127.0.0.1:1/api/tags", Duration::from_millis(50)).await;
        let elapsed = started.elapsed();
        match result {
            Err(PodError::HealthTimeout { .. }) => {}
            other => panic!("expected HealthTimeout, got {:?}", other),
        }
        // Assert the timeout actually fires near-instantly rather than
        // blocking on a 60s default — this is the whole point of the async
        // refactor. Generous bound to avoid CI-host flakiness.
        assert!(
            elapsed < Duration::from_secs(5),
            "wait_for_ollama took too long: {:?}",
            elapsed
        );
    }

    #[test]
    fn bring_up_command_requires_flake_for_nix_path() {
        // Non-existent dir → no flake.nix → nix path NOT chosen even when
        // `nix` is on PATH. (The fallback may still kick in via
        // NANNA_POD_CONFIG; we only assert the nix path's guard, which the
        // returned Option/cmd selection makes observable.)
        let nowhere = std::env::temp_dir().join("nanna_pod_test_no_flake_xyz_12345");
        let cmd = bring_up_command(&nowhere);
        // If nix is NOT on PATH and NANNA_POD_CONFIG is unset, this is
        // None. If NANNA_POD_CONFIG is set, this might be the podman path.
        // Either way it must NOT be the nix path because flake.nix doesn't
        // exist under the lookup dir.
        if let Some((c, _)) = cmd {
            assert_ne!(
                c, "nix",
                "nix bring-up path chosen without a flake.nix in lookup dir"
            );
        }
    }

    #[test]
    fn bring_up_command_picks_nix_when_flake_present() {
        // Skip if `nix` is not on PATH (CI without nix shouldn't false-fail).
        if which::which("nix").is_err() {
            eprintln!("nix not on PATH; skipping bring_up_command_picks_nix_when_flake_present");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("flake.nix"), "{}").unwrap();
        let cmd = bring_up_command(dir.path()).expect("expected a bring-up command");
        assert_eq!(cmd.0, "nix");
        assert_eq!(cmd.1, vec!["run".to_string(), ".#start-pod".to_string()]);
    }

    #[test]
    fn skip_reason_is_inside_container_does_not_panic() {
        // Just exercise the helper for coverage. Behaviour depends on the
        // host so we only assert the call doesn't panic and returns a bool.
        let _: bool = is_inside_container();
    }

    #[test]
    fn pod_error_display_includes_url_and_budget() {
        let err = PodError::HealthTimeout {
            url: "http://example/api/tags".to_string(),
            budget: Duration::from_secs(7),
        };
        let s = format!("{err}");
        assert!(s.contains("example"), "rendered error missing url: {s}");
        assert!(s.contains("7s") || s.contains("7"), "missing budget: {s}");
    }

    #[test]
    fn pod_error_no_bring_up_command_renders_helpful_text() {
        let err = PodError::NoBringUpCommand {
            url: "http://x/y".to_string(),
        };
        let s = format!("{err}");
        assert!(
            s.contains("scripts/install.sh"),
            "missing install hint: {s}"
        );
        assert!(s.contains("--no-ensure-pod"), "missing skip hint: {s}");
    }
}
