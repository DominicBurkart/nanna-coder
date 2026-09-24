//! Per-task service sidecars.
//!
//! A project profile can declare services the dev container needs at runtime
//! (first: Postgres). Each task gets its own container network, its own
//! sidecar containers on that network and environment variables (such as
//! `DATABASE_URL`) exported into the dev container. Sidecars live exactly as
//! long as the task workspace: [`SidecarSet`] removes its containers and the
//! network when dropped.
//!
//! Every runtime call goes through [`CommandRunner`] so the orchestration is
//! testable without a container runtime; [`SystemRunner`] is the production
//! implementation.

use crate::container::{exec_in_container, ContainerHandle, ContainerRuntime};
use rand::distr::Alphanumeric;
use rand::Rng;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::warn;

/// Postgres image started for the database sidecar.
pub const POSTGRES_IMAGE: &str = "docker.io/library/postgres:16";
/// Hostname under which the dev container reaches the Postgres sidecar.
pub const POSTGRES_ALIAS: &str = "postgres";
/// Port the Postgres sidecar listens on inside the task network.
pub const POSTGRES_PORT: u16 = 5432;
/// Superuser created by the Postgres image.
pub const POSTGRES_USER: &str = "postgres";
/// Environment variable exported into the dev container.
pub const DATABASE_URL_VAR: &str = "DATABASE_URL";
const MAX_IDENTIFIER_LEN: usize = 63;
const PASSWORD_LEN: usize = 24;

/// Errors from sidecar orchestration.
#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("no container runtime available for sidecars")]
    NoRuntime,
    #[error("could not spawn `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("creating network {name} failed: {stderr}")]
    NetworkCreate { name: String, stderr: String },
    #[error("starting sidecar {name} failed: {stderr}")]
    Start { name: String, stderr: String },
    #[error("sidecar {name} not ready after {budget:?}")]
    NotReady { name: String, budget: Duration },
}

/// Result of one runtime command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Executes container-runtime commands (`podman network create ...`).
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[String]) -> std::io::Result<RunOutput>;
}

/// [`CommandRunner`] that spawns real processes.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> std::io::Result<RunOutput> {
        let output = Command::new(program).args(args).output()?;
        Ok(RunOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// A service container to start next to the dev container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarSpec {
    /// Container name, unique per task.
    pub name: String,
    /// Network alias the dev container uses as hostname.
    pub alias: String,
    /// Image reference.
    pub image: String,
    /// Environment passed to the sidecar container.
    pub env: Vec<(String, String)>,
    /// Port the service listens on inside the task network.
    pub port: u16,
    /// Command executed inside the sidecar until it exits successfully.
    pub readiness: Vec<String>,
    /// Environment exported into the dev container once the sidecar is ready.
    pub exports: Vec<(String, String)>,
}

impl SidecarSpec {
    /// Arguments for `<runtime> run` that start this sidecar on `network`.
    pub fn run_args(&self, network: &str) -> Vec<String> {
        let mut args: Vec<String> = [
            "run",
            "-d",
            "--rm",
            "--name",
            &self.name,
            "--network",
            network,
            "--network-alias",
            &self.alias,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for (key, value) in &self.env {
            args.push("-e".to_string());
            args.push(format!("{key}={value}"));
        }
        args.push(self.image.clone());
        args
    }
}

/// A per-task Postgres database.
///
/// ```
/// use harness::sidecar::PostgresSidecar;
///
/// let pg = PostgresSidecar::with_password("Task-42", "secret");
/// assert_eq!(pg.database, "task_task_42");
/// assert_eq!(pg.database_url(), "postgres://postgres:secret@postgres:5432/task_task_42");
/// assert_ne!(PostgresSidecar::database_name("a"), PostgresSidecar::database_name("b"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresSidecar {
    pub task_id: String,
    pub user: String,
    pub password: String,
    pub database: String,
    pub image: String,
}

impl PostgresSidecar {
    /// Sidecar for `task_id` with a freshly generated password.
    pub fn for_task(task_id: &str) -> Self {
        Self::with_password(task_id, random_password())
    }

    /// Sidecar for `task_id` with an explicit password.
    pub fn with_password(task_id: &str, password: impl Into<String>) -> Self {
        Self {
            task_id: task_id.to_string(),
            user: POSTGRES_USER.to_string(),
            password: password.into(),
            database: Self::database_name(task_id),
            image: POSTGRES_IMAGE.to_string(),
        }
    }

    /// Database name derived from the task id: `task_` followed by the id
    /// lower-cased with every non-alphanumeric character replaced by `_`,
    /// truncated to the Postgres identifier limit.
    pub fn database_name(task_id: &str) -> String {
        let sanitized: String = task_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let mut name = format!("task_{sanitized}");
        name.truncate(MAX_IDENTIFIER_LEN);
        name
    }

    /// Container name of the sidecar for `task_id`.
    pub fn container_name(task_id: &str) -> String {
        format!("nanna-task-{task_id}-postgres")
    }

    /// Connection URL as seen from the dev container.
    pub fn database_url(&self) -> String {
        format!(
            "postgres://{}:{}@{POSTGRES_ALIAS}:{POSTGRES_PORT}/{}",
            self.user, self.password, self.database
        )
    }

    /// The sidecar specification: the Postgres image creates `database` at
    /// first start and `pg_isready` gates readiness.
    pub fn spec(&self) -> SidecarSpec {
        SidecarSpec {
            name: Self::container_name(&self.task_id),
            alias: POSTGRES_ALIAS.to_string(),
            image: self.image.clone(),
            env: vec![
                ("POSTGRES_USER".to_string(), self.user.clone()),
                ("POSTGRES_PASSWORD".to_string(), self.password.clone()),
                ("POSTGRES_DB".to_string(), self.database.clone()),
            ],
            port: POSTGRES_PORT,
            readiness: vec![
                "pg_isready".to_string(),
                "-U".to_string(),
                self.user.clone(),
                "-d".to_string(),
                self.database.clone(),
            ],
            exports: vec![(DATABASE_URL_VAR.to_string(), self.database_url())],
        }
    }
}

fn random_password() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(PASSWORD_LEN)
        .map(char::from)
        .collect()
}

/// Name of the per-task container network.
pub fn task_network_name(task_id: &str) -> String {
    format!("nanna-task-{task_id}-net")
}

/// A per-task container network, removed on drop.
pub struct TaskNetwork {
    name: String,
    runtime: ContainerRuntime,
    runner: Arc<dyn CommandRunner>,
}

impl TaskNetwork {
    /// Create the network for `task_id`.
    pub fn create(
        runtime: ContainerRuntime,
        runner: Arc<dyn CommandRunner>,
        task_id: &str,
    ) -> Result<Self, SidecarError> {
        let name = task_network_name(task_id);
        let args = vec!["network".to_string(), "create".to_string(), name.clone()];
        let output = run_or_spawn_error(runner.as_ref(), runtime.command(), &args)?;
        if !output.success {
            return Err(SidecarError::NetworkCreate {
                name,
                stderr: output.stderr,
            });
        }
        Ok(Self {
            name,
            runtime,
            runner,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Debug for TaskNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskNetwork")
            .field("name", &self.name)
            .field("runtime", &self.runtime)
            .finish()
    }
}

impl Drop for TaskNetwork {
    fn drop(&mut self) {
        let args = vec!["network".to_string(), "rm".to_string(), self.name.clone()];
        let removed = matches!(self.runner.run(self.runtime.command(), &args), Ok(o) if o.success);
        if !removed {
            warn!("could not remove task network {}", self.name);
        }
    }
}

fn run_or_spawn_error(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[String],
) -> Result<RunOutput, SidecarError> {
    runner.run(program, args).map_err(|e| SidecarError::Spawn {
        command: format!("{program} {}", args.join(" ")),
        source: e,
    })
}

/// A started sidecar and the specification it was started from.
#[derive(Debug)]
pub struct RunningSidecar {
    pub handle: ContainerHandle,
    pub spec: SidecarSpec,
}

/// How long to wait for a sidecar's readiness command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessConfig {
    pub budget: Duration,
    pub interval: Duration,
}

impl Default for ReadinessConfig {
    fn default() -> Self {
        Self {
            budget: Duration::from_secs(60),
            interval: Duration::from_secs(1),
        }
    }
}

/// The sidecars of one task together with their network.
///
/// Fields drop in declaration order, so the containers are removed before
/// the network they are attached to.
#[derive(Debug)]
pub struct SidecarSet {
    sidecars: Vec<RunningSidecar>,
    network: TaskNetwork,
    exports: Vec<(String, String)>,
}

impl SidecarSet {
    /// Create the task network, start every spec on it and wait until each
    /// readiness command succeeds. On any failure everything started so far
    /// is torn down before the error is returned.
    pub async fn start(
        runtime: ContainerRuntime,
        runner: Arc<dyn CommandRunner>,
        task_id: &str,
        specs: &[SidecarSpec],
        readiness: ReadinessConfig,
    ) -> Result<Self, SidecarError> {
        if runtime == ContainerRuntime::None {
            return Err(SidecarError::NoRuntime);
        }
        let network = TaskNetwork::create(runtime.clone(), Arc::clone(&runner), task_id)?;
        let mut sidecars = Vec::with_capacity(specs.len());
        for spec in specs {
            let args = spec.run_args(network.name());
            let output = run_or_spawn_error(runner.as_ref(), runtime.command(), &args)?;
            if !output.success {
                return Err(SidecarError::Start {
                    name: spec.name.clone(),
                    stderr: output.stderr,
                });
            }
            let handle = ContainerHandle {
                name: spec.name.clone(),
                runtime: runtime.clone(),
                port: None,
                needs_cleanup: true,
            };
            wait_ready(&handle, &spec.readiness, readiness).await?;
            sidecars.push(RunningSidecar {
                handle,
                spec: spec.clone(),
            });
        }
        let exports = specs
            .iter()
            .flat_map(|s| s.exports.iter().cloned())
            .collect();
        Ok(Self {
            sidecars,
            network,
            exports,
        })
    }

    /// Name of the task network the dev container must join.
    pub fn network_name(&self) -> &str {
        self.network.name()
    }

    /// Environment to inject into the dev container.
    pub fn exports(&self) -> &[(String, String)] {
        &self.exports
    }

    /// Extra `<runtime> run` arguments that attach the dev container to the
    /// task network.
    pub fn container_args(&self) -> Vec<String> {
        vec![format!("--network={}", self.network.name())]
    }

    pub fn sidecars(&self) -> &[RunningSidecar] {
        &self.sidecars
    }
}

async fn wait_ready(
    handle: &ContainerHandle,
    readiness: &[String],
    cfg: ReadinessConfig,
) -> Result<(), SidecarError> {
    let argv: Vec<&str> = readiness.iter().map(String::as_str).collect();
    let start = Instant::now();
    loop {
        if matches!(exec_in_container(handle, &argv, None), Ok(o) if o.success) {
            return Ok(());
        }
        if start.elapsed() >= cfg.budget {
            return Err(SidecarError::NotReady {
                name: handle.name.clone(),
                budget: cfg.budget,
            });
        }
        tokio::time::sleep(cfg.interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeRunner {
        calls: Mutex<Vec<String>>,
        fail_on: Option<&'static str>,
        spawn_fail_on: Option<&'static str>,
    }

    impl FakeRunner {
        fn new(fail_on: Option<&'static str>, spawn_fail_on: Option<&'static str>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                fail_on,
                spawn_fail_on,
            })
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _program: &str, args: &[String]) -> std::io::Result<RunOutput> {
            let joined = args.join(" ");
            self.calls.lock().unwrap().push(joined.clone());
            if self.spawn_fail_on.is_some_and(|w| joined.starts_with(w)) {
                return Err(std::io::Error::other("spawn failed"));
            }
            let success = !self.fail_on.is_some_and(|w| joined.starts_with(w));
            Ok(RunOutput {
                success,
                stdout: String::new(),
                stderr: if success {
                    String::new()
                } else {
                    "boom".to_string()
                },
            })
        }
    }

    fn fast() -> ReadinessConfig {
        ReadinessConfig {
            budget: Duration::from_millis(5),
            interval: Duration::from_millis(1),
        }
    }

    #[test]
    fn database_name_is_sanitized_lowercased_and_prefixed() {
        assert_eq!(PostgresSidecar::database_name("Ab-1.c"), "task_ab_1_c");
    }

    #[test]
    fn database_name_is_truncated_to_postgres_limit() {
        let long = "x".repeat(100);
        let name = PostgresSidecar::database_name(&long);
        assert_eq!(name.len(), MAX_IDENTIFIER_LEN);
        assert!(name.starts_with("task_x"));
    }

    #[test]
    fn database_names_are_distinct_per_task_and_deterministic() {
        let a = PostgresSidecar::database_name("task-a");
        let b = PostgresSidecar::database_name("task-b");
        assert_ne!(a, b);
        assert_eq!(a, PostgresSidecar::database_name("task-a"));
    }

    #[test]
    fn for_task_generates_alphanumeric_passwords_that_differ() {
        let first = PostgresSidecar::for_task("t1");
        let second = PostgresSidecar::for_task("t1");
        assert_eq!(first.password.len(), PASSWORD_LEN);
        assert!(first.password.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(first.password, second.password);
        assert_eq!(first.database, second.database);
    }

    #[test]
    fn postgres_spec_exports_database_url_and_gates_on_pg_isready() {
        let pg = PostgresSidecar::with_password("t-1", "pw");
        let spec = pg.spec();
        assert_eq!(spec.name, "nanna-task-t-1-postgres");
        assert_eq!(spec.alias, POSTGRES_ALIAS);
        assert_eq!(spec.image, POSTGRES_IMAGE);
        assert_eq!(spec.port, POSTGRES_PORT);
        assert_eq!(
            spec.env,
            vec![
                ("POSTGRES_USER".to_string(), "postgres".to_string()),
                ("POSTGRES_PASSWORD".to_string(), "pw".to_string()),
                ("POSTGRES_DB".to_string(), "task_t_1".to_string()),
            ]
        );
        assert_eq!(
            spec.readiness,
            vec!["pg_isready", "-U", "postgres", "-d", "task_t_1"]
        );
        assert_eq!(
            spec.exports,
            vec![(
                "DATABASE_URL".to_string(),
                "postgres://postgres:pw@postgres:5432/task_t_1".to_string()
            )]
        );
    }

    #[test]
    fn run_args_attach_to_network_with_alias_env_and_image() {
        let spec = PostgresSidecar::with_password("t", "pw").spec();
        let args = spec.run_args("nanna-task-t-net");
        assert_eq!(
            args,
            vec![
                "run",
                "-d",
                "--rm",
                "--name",
                "nanna-task-t-postgres",
                "--network",
                "nanna-task-t-net",
                "--network-alias",
                "postgres",
                "-e",
                "POSTGRES_USER=postgres",
                "-e",
                "POSTGRES_PASSWORD=pw",
                "-e",
                "POSTGRES_DB=task_t",
                "docker.io/library/postgres:16",
            ]
        );
    }

    #[test]
    fn system_runner_reports_exit_status_and_spawn_errors() {
        let Ok(true_bin) = which::which("true") else {
            eprintln!("`true` not on PATH; skipping");
            return;
        };
        let ok = SystemRunner.run(&true_bin.to_string_lossy(), &[]).unwrap();
        assert!(ok.success);
        let err = SystemRunner
            .run("nanna-definitely-missing-binary", &[])
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn task_network_is_created_and_removed_on_drop() {
        let runner = FakeRunner::new(None, None);
        let net = TaskNetwork::create(ContainerRuntime::Stub, runner.clone(), "t1").unwrap();
        assert_eq!(net.name(), "nanna-task-t1-net");
        drop(net);
        assert_eq!(
            runner.calls(),
            vec![
                "network create nanna-task-t1-net",
                "network rm nanna-task-t1-net"
            ]
        );
    }

    #[test]
    fn task_network_debug_shows_name_and_runtime() {
        let runner = FakeRunner::new(None, None);
        let net = TaskNetwork::create(ContainerRuntime::Stub, runner, "dbg").unwrap();
        let rendered = format!("{net:?}");
        assert!(rendered.contains("nanna-task-dbg-net"), "{rendered}");
        assert!(rendered.contains("Stub"), "{rendered}");
    }

    #[test]
    fn task_network_create_failure_is_reported() {
        let runner = FakeRunner::new(Some("network create"), None);
        let err = TaskNetwork::create(ContainerRuntime::Stub, runner, "t1").unwrap_err();
        assert!(
            matches!(err, SidecarError::NetworkCreate { ref stderr, .. } if stderr == "boom"),
            "{err}"
        );
    }

    #[test]
    fn task_network_spawn_failure_is_reported() {
        let runner = FakeRunner::new(None, Some("network create"));
        let err = TaskNetwork::create(ContainerRuntime::Stub, runner, "t1").unwrap_err();
        assert!(matches!(err, SidecarError::Spawn { .. }), "{err}");
        assert!(err.to_string().contains("network create nanna-task-t1-net"));
    }

    #[test]
    fn task_network_drop_tolerates_removal_failure() {
        let runner = FakeRunner::new(Some("network rm"), None);
        let net = TaskNetwork::create(ContainerRuntime::Stub, runner.clone(), "t1").unwrap();
        drop(net);
        assert_eq!(runner.calls().len(), 2);
    }

    #[tokio::test]
    async fn sidecar_set_requires_a_runtime() {
        let runner = FakeRunner::new(None, None);
        let err = SidecarSet::start(ContainerRuntime::None, runner, "t", &[], fast())
            .await
            .unwrap_err();
        assert!(matches!(err, SidecarError::NoRuntime));
    }

    #[tokio::test]
    async fn sidecar_set_starts_specs_on_task_network_and_collects_exports() {
        let runner = FakeRunner::new(None, None);
        let specs = vec![PostgresSidecar::with_password("t2", "pw").spec()];
        let set = SidecarSet::start(ContainerRuntime::Stub, runner.clone(), "t2", &specs, fast())
            .await
            .unwrap();
        assert_eq!(set.network_name(), "nanna-task-t2-net");
        assert_eq!(set.container_args(), vec!["--network=nanna-task-t2-net"]);
        assert_eq!(set.exports(), specs[0].exports.as_slice());
        assert_eq!(set.sidecars().len(), 1);
        assert_eq!(set.sidecars()[0].handle.name, "nanna-task-t2-postgres");
        assert_eq!(set.sidecars()[0].spec, specs[0]);
        let calls = runner.calls();
        assert_eq!(calls[0], "network create nanna-task-t2-net");
        assert!(calls[1]
            .starts_with("run -d --rm --name nanna-task-t2-postgres --network nanna-task-t2-net"));
        drop(set);
        assert_eq!(
            runner.calls().last().unwrap(),
            "network rm nanna-task-t2-net"
        );
    }

    #[tokio::test]
    async fn sidecar_set_start_failure_tears_down_network() {
        let runner = FakeRunner::new(Some("run"), None);
        let specs = vec![PostgresSidecar::with_password("t3", "pw").spec()];
        let err = SidecarSet::start(ContainerRuntime::Stub, runner.clone(), "t3", &specs, fast())
            .await
            .unwrap_err();
        assert!(
            matches!(err, SidecarError::Start { ref name, .. } if name == "nanna-task-t3-postgres"),
            "{err}"
        );
        assert_eq!(
            runner.calls().last().unwrap(),
            "network rm nanna-task-t3-net"
        );
    }

    #[tokio::test]
    async fn sidecar_set_spawn_failure_is_reported() {
        let runner = FakeRunner::new(None, Some("run"));
        let specs = vec![PostgresSidecar::with_password("t4", "pw").spec()];
        let err = SidecarSet::start(ContainerRuntime::Stub, runner, "t4", &specs, fast())
            .await
            .unwrap_err();
        assert!(matches!(err, SidecarError::Spawn { .. }), "{err}");
    }

    #[tokio::test]
    async fn sidecar_set_network_failure_is_reported() {
        let runner = FakeRunner::new(Some("network create"), None);
        let err = SidecarSet::start(ContainerRuntime::Stub, runner, "t5", &[], fast())
            .await
            .unwrap_err();
        assert!(matches!(err, SidecarError::NetworkCreate { .. }), "{err}");
    }

    #[tokio::test]
    async fn wait_ready_times_out_when_readiness_never_succeeds() {
        let handle = ContainerHandle {
            name: "never-ready".to_string(),
            runtime: ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        };
        let err = wait_ready(&handle, &["pg_isready".to_string()], fast())
            .await
            .unwrap_err();
        assert!(
            matches!(err, SidecarError::NotReady { ref name, .. } if name == "never-ready"),
            "{err}"
        );
    }

    #[test]
    fn readiness_config_default_is_one_minute_polled_every_second() {
        let cfg = ReadinessConfig::default();
        assert_eq!(cfg.budget, Duration::from_secs(60));
        assert_eq!(cfg.interval, Duration::from_secs(1));
    }
}
