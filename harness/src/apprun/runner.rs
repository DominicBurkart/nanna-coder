//! Building, starting, probing, stopping and tailing the application inside
//! the dev container.
//!
//! Every container command goes through [`CommandRunner`] as a
//! `<runtime> exec` invocation (the same command `exec_in_container`
//! issues), so the whole flow is testable with a stub runner.

use super::{AppInstance, Limits, PortAllocator, PortError, RunningApps};
use crate::container::ContainerHandle;
use crate::onboarding::fullstack::FullStackRust;
use crate::onboarding::OnboardingError;
use crate::sidecar::{CommandRunner, RunOutput};
use crate::tools::{member_working_dir, trunk_build_args};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::warn;

/// Directory inside the dev container that holds application logs; outside
/// the mounted worktree so the log never shows up as a change.
pub const LOG_DIR: &str = "/tmp";
/// Environment variable telling the API where to listen.
pub const BIND_ADDR_VAR: &str = "BIND_ADDR";
/// Environment variable pointing at the built frontend bundle.
pub const FRONTEND_DIST_VAR: &str = "FRONTEND_DIST";
/// Environment variable controlling the API's log filter.
pub const RUST_LOG_VAR: &str = "RUST_LOG";
/// Log filter used when the sidecar environment does not set one.
pub const DEFAULT_RUST_LOG: &str = "info";
/// Interval between health probes.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Log lines attached to a start failure.
pub const LOG_TAIL_ON_FAILURE: usize = 20;
const TRUNK_DIST_DEFAULT: &str = "dist";
const OUTPUT_TAIL_CHARS: usize = 4000;

/// Errors from running the application.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("could not spawn `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{step} failed in {working_dir}\nstdout:\n{stdout}\nstderr:\n{stderr}")]
    StepFailed {
        step: String,
        working_dir: String,
        stdout: String,
        stderr: String,
    },
    #[error("wall-clock limit of {limit:?} exhausted during {step}")]
    WallClock { step: String, limit: Duration },
    #[error("cargo build produced no executable for package {package}")]
    NoExecutable { package: String },
    #[error("the start command printed no pid: {output}")]
    NoPid { output: String },
    #[error("app (pid {pid}) exited before {url} became healthy; last log lines:\n{log}")]
    Exited { pid: u32, url: String, log: String },
    #[error("{url} not healthy within {limit:?}; last log lines:\n{log}")]
    Unhealthy {
        url: String,
        limit: Duration,
        log: String,
    },
    #[error(transparent)]
    Port(#[from] PortError),
}

/// What the profile tells us about the application, as container paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSpec {
    /// Package name of the API binary crate.
    pub api_package: String,
    /// Workspace root inside the container.
    pub workspace_dir: String,
    /// Frontend member directory inside the container (`trunk build` runs here).
    pub frontend_dir: String,
    /// Directory trunk writes the bundle to, inside the container.
    pub frontend_dist: String,
    /// Health endpoint path polled after start.
    pub health_path: String,
}

impl AppSpec {
    /// Derive the spec from a detected profile. `workspace_root` is the
    /// host path of the worktree (to read the frontend's `Trunk.toml`) and
    /// `container_dir` the path it is mounted at inside the container.
    pub fn from_profile(
        profile: &FullStackRust,
        workspace_root: &Path,
        container_dir: &str,
    ) -> Result<Self, OnboardingError> {
        let frontend_dir = member_working_dir(container_dir, &profile.frontend.path);
        let dist = trunk_dist_dir(&workspace_root.join(&profile.frontend.path))?;
        Ok(Self {
            api_package: profile.api.name.clone(),
            workspace_dir: container_dir.to_string(),
            frontend_dist: format!("{frontend_dir}/{dist}"),
            frontend_dir,
            health_path: profile.health_path().to_string(),
        })
    }
}

fn trunk_dist_dir(frontend_dir: &Path) -> Result<String, OnboardingError> {
    let path = frontend_dir.join("Trunk.toml");
    if !path.is_file() {
        return Ok(TRUNK_DIST_DEFAULT.to_string());
    }
    let content = std::fs::read_to_string(&path)?;
    let doc: toml::Value = content
        .parse()
        .map_err(|e| OnboardingError::ParseError(format!("invalid {}: {e}", path.display())))?;
    let dist = doc
        .get("build")
        .and_then(|b| b.get("dist"))
        .and_then(toml::Value::as_str)
        .unwrap_or(TRUNK_DIST_DEFAULT);
    Ok(dist.to_string())
}

/// Log file of the application of `task_id`, inside the container.
pub fn app_log_path(task_id: &str) -> String {
    format!("{LOG_DIR}/nanna-app-{task_id}.log")
}

/// Environment for the API process: bind address, frontend bundle path, a
/// default log filter, then `extra` (the sidecar exports). A `RUST_LOG` in
/// `extra` replaces the default.
pub fn app_env(spec: &AppSpec, port: u16, extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = vec![
        (BIND_ADDR_VAR.to_string(), format!("0.0.0.0:{port}")),
        (FRONTEND_DIST_VAR.to_string(), spec.frontend_dist.clone()),
    ];
    if !extra.iter().any(|(k, _)| k == RUST_LOG_VAR) {
        env.push((RUST_LOG_VAR.to_string(), DEFAULT_RUST_LOG.to_string()));
    }
    env.extend(extra.iter().cloned());
    env
}

/// Single-quote `s` for `sh`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `sh -c` script that starts `executable` detached with `env`, both
/// output streams appended to `log_path`, and prints its pid.
pub fn start_script(executable: &str, env: &[(String, String)], log_path: &str) -> String {
    let assignments: Vec<String> = env
        .iter()
        .map(|(k, v)| format!("{k}={}", shell_quote(v)))
        .collect();
    format!(
        "{} nohup {} >{} 2>&1 & echo $!",
        assignments.join(" "),
        shell_quote(executable),
        shell_quote(log_path)
    )
}

/// `cargo build` for the API package with JSON messages on stdout (to learn
/// the executable path) and human-readable diagnostics on stderr.
pub fn api_build_argv(package: &str) -> Vec<String> {
    vec![
        "cargo".to_string(),
        "build".to_string(),
        "--package".to_string(),
        package.to_string(),
        "--message-format=json-render-diagnostics".to_string(),
    ]
}

/// The executable of the last `compiler-artifact` message that has one.
pub fn executable_from_build_output(stdout: &str) -> Option<String> {
    let mut executable = None;
    for line in stdout.lines() {
        if let Some(path) = executable_in_message(line) {
            executable = Some(path);
        }
    }
    executable
}

fn executable_in_message(line: &str) -> Option<String> {
    let msg: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(msg.get("executable")?.as_str()?.to_string())
}

/// `curl` probe of the health endpoint from inside the container.
/// Per-probe curl timeout, in seconds.
///
/// A connection that accepts a socket but never answers would otherwise let
/// a single health probe hang past [`wait_healthy`]'s deadline check, which
/// only runs *between* probes: `app_start`'s hard wall-clock limit could be
/// defeated by one stalled request. A healthy app answers in well under
/// this window; if it doesn't, the probe should fail fast and let the
/// deadline loop decide whether to keep waiting or give up.
pub const HEALTH_PROBE_TIMEOUT_SECS: u64 = 5;

pub fn health_probe_argv(port: u16, health_path: &str) -> Vec<String> {
    vec![
        "curl".to_string(),
        "-sf".to_string(),
        "-m".to_string(),
        HEALTH_PROBE_TIMEOUT_SECS.to_string(),
        "-o".to_string(),
        "/dev/null".to_string(),
        format!("http://127.0.0.1:{port}{health_path}"),
    ]
}

/// Prefix `argv` with coreutils `timeout` so the step cannot outlive its budget.
pub fn with_timeout(secs: u64, argv: &[String]) -> Vec<String> {
    let mut args = vec!["timeout".to_string(), secs.to_string()];
    args.extend_from_slice(argv);
    args
}

/// `<runtime> exec [-w dir] <container> argv...`, the argument vector
/// `exec_in_container` issues.
pub fn exec_args(
    handle: &ContainerHandle,
    argv: &[String],
    working_dir: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    if let Some(dir) = working_dir {
        args.push("-w".to_string());
        args.push(dir.to_string());
    }
    args.push(handle.name.clone());
    args.extend_from_slice(argv);
    args
}

/// The pid printed by [`start_script`].
pub fn parse_pid(stdout: &str) -> Option<u32> {
    stdout.trim().parse().ok()
}

fn tail_chars(s: &str, n: usize) -> &str {
    let start = s
        .char_indices()
        .rev()
        .nth(n.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    &s[start..]
}

fn run(
    runner: &dyn CommandRunner,
    handle: &ContainerHandle,
    argv: &[String],
    working_dir: Option<&str>,
) -> Result<RunOutput, AppError> {
    let args = exec_args(handle, argv, working_dir);
    let program = handle.runtime.command();
    runner
        .run(program, &args)
        .map_err(|source| AppError::Spawn {
            command: format!("{program} {}", args.join(" ")),
            source,
        })
}

fn run_step(
    runner: &dyn CommandRunner,
    handle: &ContainerHandle,
    step: &str,
    argv: &[String],
    working_dir: Option<&str>,
) -> Result<RunOutput, AppError> {
    let output = run(runner, handle, argv, working_dir)?;
    if output.success {
        return Ok(output);
    }
    Err(AppError::StepFailed {
        step: step.to_string(),
        working_dir: working_dir.unwrap_or("/").to_string(),
        stdout: tail_chars(&output.stdout, OUTPUT_TAIL_CHARS).to_string(),
        stderr: tail_chars(&output.stderr, OUTPUT_TAIL_CHARS).to_string(),
    })
}

/// Last `tail` lines of `log_path` inside the container.
pub fn tail_log(
    runner: &dyn CommandRunner,
    handle: &ContainerHandle,
    log_path: &str,
    tail: usize,
) -> Result<Vec<String>, AppError> {
    let argv = vec![
        "tail".to_string(),
        "-n".to_string(),
        tail.to_string(),
        log_path.to_string(),
    ];
    let output = run_step(runner, handle, "tail", &argv, None)?;
    Ok(output.stdout.lines().map(str::to_string).collect())
}

fn log_excerpt(runner: &dyn CommandRunner, handle: &ContainerHandle, log_path: &str) -> String {
    match tail_log(runner, handle, log_path, LOG_TAIL_ON_FAILURE) {
        Ok(lines) => lines.join("\n"),
        Err(e) => format!("(log unavailable: {e})"),
    }
}

fn kill(runner: &dyn CommandRunner, handle: &ContainerHandle, pid: u32) -> Result<bool, AppError> {
    let argv = vec!["sh".to_string(), "-c".to_string(), format!("kill {pid}")];
    Ok(run(runner, handle, &argv, None)?.success)
}

fn kill_quietly(runner: &dyn CommandRunner, handle: &ContainerHandle, pid: u32) {
    if !matches!(kill(runner, handle, pid), Ok(true)) {
        warn!("could not kill app pid {pid} in {}", handle.name);
    }
}

/// An instance removed by [`stop_app`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoppedApp {
    pub instance: AppInstance,
    /// Whether `kill` succeeded; `false` when the process was already gone.
    pub killed: bool,
}

/// Stop the application of `task_id` if one is registered: kill its pid in
/// the container and forget it (releasing its port). `Ok(None)` when
/// nothing was running, so calling it twice is harmless.
pub fn stop_app(
    runner: &dyn CommandRunner,
    handle: &ContainerHandle,
    apps: &RunningApps,
    task_id: &str,
) -> Result<Option<StoppedApp>, AppError> {
    let Some(instance) = apps.get(task_id) else {
        return Ok(None);
    };
    let killed = kill(runner, handle, instance.pid)?;
    apps.remove(task_id);
    Ok(Some(StoppedApp { instance, killed }))
}

/// Everything the app tools of one task share.
#[derive(Clone)]
pub struct AppContext {
    pub task_id: String,
    pub handle: Arc<ContainerHandle>,
    pub runner: Arc<dyn CommandRunner>,
    pub apps: Arc<RunningApps>,
    pub ports: Arc<PortAllocator>,
    pub spec: AppSpec,
    /// Environment exported by the sidecars (for example `DATABASE_URL`).
    pub env: Vec<(String, String)>,
    pub limits: Limits,
    pub poll_interval: Duration,
}

impl AppContext {
    /// Build the frontend and the API, start the API on a fresh port and
    /// wait for its health endpoint. Returns the running instance when one
    /// is already registered for the task.
    pub async fn start(&self) -> Result<AppInstance, AppError> {
        if let Some(running) = self.apps.get(&self.task_id) {
            return Ok(running);
        }
        let deadline = Instant::now() + self.limits.max_wall_clock();
        let budget = self.remaining("trunk build", deadline)?;
        self.step(
            "trunk build",
            deadline,
            &with_timeout(budget, &trunk_build_args(false)),
            &self.spec.frontend_dir,
        )?;
        let budget = self.remaining("cargo build", deadline)?;
        let build = self.step(
            "cargo build",
            deadline,
            &with_timeout(budget, &api_build_argv(&self.spec.api_package)),
            &self.spec.workspace_dir,
        )?;
        let executable =
            executable_from_build_output(&build.stdout).ok_or_else(|| AppError::NoExecutable {
                package: self.spec.api_package.clone(),
            })?;
        let lease = self.ports.allocate(&self.task_id)?;
        let log_path = app_log_path(&self.task_id);
        let script = start_script(
            &executable,
            &app_env(&self.spec, lease.port(), &self.env),
            &log_path,
        );
        let argv = vec!["sh".to_string(), "-c".to_string(), script];
        let started = self.step("start", deadline, &argv, &self.spec.workspace_dir)?;
        let pid = parse_pid(&started.stdout).ok_or_else(|| AppError::NoPid {
            output: started.stdout.clone(),
        })?;
        let instance = AppInstance::local(&self.task_id, lease.port(), pid, &log_path);
        self.wait_healthy(&instance, deadline).await?;
        self.apps.insert(instance.clone(), lease);
        Ok(instance)
    }

    /// Stop the task's application; see [`stop_app`].
    pub fn stop(&self) -> Result<Option<StoppedApp>, AppError> {
        stop_app(
            self.runner.as_ref(),
            &self.handle,
            &self.apps,
            &self.task_id,
        )
    }

    /// Last `tail` lines of the task's application log.
    pub fn logs(&self, tail: usize) -> Result<Vec<String>, AppError> {
        tail_log(
            self.runner.as_ref(),
            &self.handle,
            &app_log_path(&self.task_id),
            tail,
        )
    }

    fn remaining(&self, step: &str, deadline: Instant) -> Result<u64, AppError> {
        let left = deadline.saturating_duration_since(Instant::now());
        let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
        if secs == 0 {
            return Err(AppError::WallClock {
                step: step.to_string(),
                limit: self.limits.max_wall_clock(),
            });
        }
        Ok(secs)
    }

    fn step(
        &self,
        step: &str,
        deadline: Instant,
        argv: &[String],
        working_dir: &str,
    ) -> Result<RunOutput, AppError> {
        let result = run_step(
            self.runner.as_ref(),
            &self.handle,
            step,
            argv,
            Some(working_dir),
        );
        if result.is_err() && Instant::now() >= deadline {
            return Err(AppError::WallClock {
                step: step.to_string(),
                limit: self.limits.max_wall_clock(),
            });
        }
        result
    }

    fn probe(&self, instance: &AppInstance) -> Result<bool, AppError> {
        Ok(run(
            self.runner.as_ref(),
            &self.handle,
            &health_probe_argv(instance.port, &self.spec.health_path),
            None,
        )?
        .success)
    }

    fn alive(&self, pid: u32) -> Result<bool, AppError> {
        let argv = vec!["sh".to_string(), "-c".to_string(), format!("kill -0 {pid}")];
        Ok(run(self.runner.as_ref(), &self.handle, &argv, None)?.success)
    }

    async fn wait_healthy(
        &self,
        instance: &AppInstance,
        deadline: Instant,
    ) -> Result<(), AppError> {
        let url = format!("{}{}", instance.base_url, self.spec.health_path);
        let mut healthy = self.probe(instance)?;
        while !healthy {
            if !self.alive(instance.pid)? {
                let log = log_excerpt(self.runner.as_ref(), &self.handle, &instance.log_path);
                return Err(AppError::Exited {
                    pid: instance.pid,
                    url,
                    log,
                });
            }
            if Instant::now() >= deadline {
                kill_quietly(self.runner.as_ref(), &self.handle, instance.pid);
                let log = log_excerpt(self.runner.as_ref(), &self.handle, &instance.log_path);
                return Err(AppError::Unhealthy {
                    url,
                    limit: self.limits.max_wall_clock(),
                    log,
                });
            }
            tokio::time::sleep(self.poll_interval).await;
            healthy = self.probe(instance)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprun::PortAllocator;
    use crate::container::ContainerRuntime;
    use crate::onboarding::fullstack::{FullStackRust, MemberCrate};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tempfile::TempDir;

    const BUILD_JSON: &str = concat!(
        r#"{"reason":"compiler-artifact","target":{"name":"shared"},"executable":null}"#,
        "\n",
        r#"{"reason":"compiler-artifact","target":{"name":"api"},"executable":"/cache/target/debug/api"}"#,
        "\n",
        r#"{"reason":"build-finished","success":true}"#,
        "\n"
    );

    type Responder = Box<dyn Fn(&[String], usize) -> std::io::Result<RunOutput> + Send + Sync>;

    struct StubRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
        probes: AtomicUsize,
        respond: Responder,
    }

    impl StubRunner {
        fn new(respond: Responder) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                probes: AtomicUsize::new(0),
                respond,
            })
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(_, a)| a.clone())
                .collect()
        }

        fn programs(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(p, _)| p.clone())
                .collect()
        }
    }

    impl CommandRunner for StubRunner {
        fn run(&self, program: &str, args: &[String]) -> std::io::Result<RunOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((program.to_string(), args.to_vec()));
            let probes = if args.contains(&"curl".to_string()) {
                self.probes.fetch_add(1, Ordering::SeqCst) + 1
            } else {
                0
            };
            (self.respond)(args, probes)
        }
    }

    fn ok(stdout: &str) -> std::io::Result<RunOutput> {
        Ok(RunOutput {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    }

    fn failed(stderr: &str) -> std::io::Result<RunOutput> {
        Ok(RunOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
        })
    }

    fn has(args: &[String], word: &str) -> bool {
        args.iter().any(|a| a == word)
    }

    fn script(args: &[String]) -> &str {
        args.last().map(String::as_str).unwrap_or("")
    }

    fn healthy_after(n: usize) -> Responder {
        Box::new(move |args, probes| {
            if has(args, "trunk") || script(args).starts_with("kill") {
                ok("")
            } else if has(args, "cargo") {
                ok(BUILD_JSON)
            } else if script(args).contains("nohup") {
                ok("4242\n")
            } else if has(args, "curl") {
                if probes >= n {
                    ok("")
                } else {
                    failed("")
                }
            } else if has(args, "tail") {
                ok("line one\nline two\n")
            } else {
                failed("unexpected")
            }
        })
    }

    fn spec() -> AppSpec {
        AppSpec {
            api_package: "api".to_string(),
            workspace_dir: "/workspace".to_string(),
            frontend_dir: "/workspace/ui".to_string(),
            frontend_dist: "/workspace/ui/dist".to_string(),
            health_path: "/health/v1".to_string(),
        }
    }

    fn context(runner: Arc<StubRunner>, secs: u64) -> (AppContext, TempDir) {
        let dir = TempDir::new().unwrap();
        let ctx = AppContext {
            task_id: "t".to_string(),
            handle: Arc::new(ContainerHandle {
                name: "nanna-task-t".to_string(),
                runtime: ContainerRuntime::Podman,
                port: None,
                needs_cleanup: false,
            }),
            runner,
            apps: Arc::new(RunningApps::new()),
            ports: Arc::new(PortAllocator::new(41000..=41001, dir.path())),
            spec: spec(),
            env: vec![(
                "DATABASE_URL".to_string(),
                "postgres://u:p@postgres:5432/db".to_string(),
            )],
            limits: Limits {
                max_wall_clock_secs: secs,
            },
            poll_interval: Duration::from_millis(1),
        };
        (ctx, dir)
    }

    #[test]
    fn spec_from_profile_uses_trunk_dist_setting() {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("web")).unwrap();
        std::fs::write(
            root.path().join("web/Trunk.toml"),
            "[build]\ndist = \"out\"\n",
        )
        .unwrap();
        let profile = FullStackRust {
            members: vec![],
            api: MemberCrate {
                name: "backend".to_string(),
                path: "srv".into(),
            },
            frontend: MemberCrate {
                name: "web".to_string(),
                path: "web".into(),
            },
            shared: vec![],
            database: None,
            health_paths: vec!["/healthz".to_string()],
            proxy_backends: vec![],
        };
        let spec = AppSpec::from_profile(&profile, root.path(), "/workspace").unwrap();
        assert_eq!(spec.api_package, "backend");
        assert_eq!(spec.workspace_dir, "/workspace");
        assert_eq!(spec.frontend_dir, "/workspace/web");
        assert_eq!(spec.frontend_dist, "/workspace/web/out");
        assert_eq!(spec.health_path, "/healthz");
        std::fs::write(root.path().join("web/Trunk.toml"), "[build]\n").unwrap();
        assert_eq!(
            AppSpec::from_profile(&profile, root.path(), "/workspace")
                .unwrap()
                .frontend_dist,
            "/workspace/web/dist"
        );
        std::fs::remove_file(root.path().join("web/Trunk.toml")).unwrap();
        assert_eq!(
            AppSpec::from_profile(&profile, root.path(), "/workspace")
                .unwrap()
                .frontend_dist,
            "/workspace/web/dist"
        );
        std::fs::write(root.path().join("web/Trunk.toml"), "[build\n").unwrap();
        assert!(AppSpec::from_profile(&profile, root.path(), "/workspace").is_err());
    }

    #[test]
    fn spec_from_fixture_profile() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/fullstack");
        let profile = FullStackRust::detect(&root).unwrap().unwrap();
        let spec = AppSpec::from_profile(&profile, &root, "/workspace").unwrap();
        assert_eq!(spec.api_package, "api");
        assert_eq!(spec.frontend_dir, "/workspace/ui");
        assert_eq!(spec.frontend_dist, "/workspace/ui/dist");
        assert_eq!(spec.health_path, "/health/v1");
    }

    #[test]
    fn argument_construction() {
        assert_eq!(app_log_path("task-9"), "/tmp/nanna-app-task-9.log");
        assert_eq!(
            api_build_argv("api"),
            [
                "cargo",
                "build",
                "--package",
                "api",
                "--message-format=json-render-diagnostics"
            ]
        );
        assert_eq!(
            health_probe_argv(18000, "/health/v1"),
            [
                "curl",
                "-sf",
                "-m",
                &HEALTH_PROBE_TIMEOUT_SECS.to_string(),
                "-o",
                "/dev/null",
                "http://127.0.0.1:18000/health/v1"
            ]
        );
        assert_eq!(
            with_timeout(7, &["trunk".to_string(), "build".to_string()]),
            ["timeout", "7", "trunk", "build"]
        );
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(parse_pid(" 4242 \n"), Some(4242));
        assert_eq!(parse_pid("nope"), None);
        assert_eq!(parse_pid(""), None);
        let handle = ContainerHandle {
            name: "c".to_string(),
            runtime: ContainerRuntime::Podman,
            port: None,
            needs_cleanup: false,
        };
        assert_eq!(
            exec_args(&handle, &["ls".to_string()], Some("/w")),
            ["exec", "-w", "/w", "c", "ls"]
        );
        assert_eq!(
            exec_args(&handle, &["ls".to_string()], None),
            ["exec", "c", "ls"]
        );
    }

    #[test]
    fn env_and_start_script() {
        let extra = vec![("DATABASE_URL".to_string(), "postgres://x".to_string())];
        let env = app_env(&spec(), 18000, &extra);
        assert_eq!(
            env,
            vec![
                ("BIND_ADDR".to_string(), "0.0.0.0:18000".to_string()),
                (
                    "FRONTEND_DIST".to_string(),
                    "/workspace/ui/dist".to_string()
                ),
                ("RUST_LOG".to_string(), "info".to_string()),
                ("DATABASE_URL".to_string(), "postgres://x".to_string()),
            ]
        );
        let with_log = vec![("RUST_LOG".to_string(), "debug".to_string())];
        let env = app_env(&spec(), 1, &with_log);
        assert_eq!(env.iter().filter(|(k, _)| k == "RUST_LOG").count(), 1);
        assert!(env.contains(&("RUST_LOG".to_string(), "debug".to_string())));
        let script = start_script("/cache/target/debug/api", &extra, "/tmp/app.log");
        assert_eq!(script, "DATABASE_URL='postgres://x' nohup '/cache/target/debug/api' >'/tmp/app.log' 2>&1 & echo $!");
    }

    #[test]
    fn executable_is_taken_from_the_last_artifact_with_one() {
        assert_eq!(
            executable_from_build_output(BUILD_JSON),
            Some("/cache/target/debug/api".to_string())
        );
        assert_eq!(
            executable_from_build_output("not json\n{\"reason\":\"x\"}\n"),
            None
        );
        assert_eq!(executable_from_build_output(""), None);
    }

    #[test]
    fn tail_chars_keeps_the_end() {
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("ab", 3), "ab");
    }

    #[tokio::test]
    async fn start_builds_starts_probes_and_registers() {
        let runner = StubRunner::new(healthy_after(3));
        let (ctx, _dir) = context(Arc::clone(&runner), 120);
        let instance = ctx.start().await.unwrap();
        assert_eq!(
            instance,
            AppInstance::local("t", 41000, 4242, "/tmp/nanna-app-t.log")
        );
        assert_eq!(ctx.apps.get("t"), Some(instance.clone()));
        assert_eq!(ctx.ports.held(), vec![41000]);
        let calls = runner.calls();
        assert!(runner.programs().iter().all(|p| p == "podman"));
        assert_eq!(
            calls[0],
            [
                "exec",
                "-w",
                "/workspace/ui",
                "nanna-task-t",
                "timeout",
                "120",
                "trunk",
                "build"
            ]
        );
        assert_eq!(
            &calls[1][..5],
            ["exec", "-w", "/workspace", "nanna-task-t", "timeout"]
        );
        assert_eq!(
            &calls[1][6..],
            [
                "cargo",
                "build",
                "--package",
                "api",
                "--message-format=json-render-diagnostics"
            ]
        );
        assert_eq!(
            &calls[2][..6],
            ["exec", "-w", "/workspace", "nanna-task-t", "sh", "-c"]
        );
        assert_eq!(
            calls[2][6],
            "BIND_ADDR='0.0.0.0:41000' FRONTEND_DIST='/workspace/ui/dist' RUST_LOG='info' DATABASE_URL='postgres://u:p@postgres:5432/db' nohup '/cache/target/debug/api' >'/tmp/nanna-app-t.log' 2>&1 & echo $!"
        );
        let probes = calls
            .iter()
            .filter(|c| c.contains(&"curl".to_string()))
            .count();
        assert_eq!(probes, 3);
        let liveness = calls
            .iter()
            .filter(|c| c.last().is_some_and(|s| s == "kill -0 4242"))
            .count();
        assert_eq!(liveness, 2, "liveness is checked between failed probes");
        assert!(calls.iter().any(|c| c[..]
            == [
                "exec",
                "nanna-task-t",
                "curl",
                "-sf",
                "-m",
                &HEALTH_PROBE_TIMEOUT_SECS.to_string(),
                "-o",
                "/dev/null",
                "http://127.0.0.1:41000/health/v1"
            ]));
    }

    #[tokio::test]
    async fn second_start_returns_the_running_instance_without_commands() {
        let runner = StubRunner::new(healthy_after(1));
        let (ctx, _dir) = context(Arc::clone(&runner), 120);
        let first = ctx.start().await.unwrap();
        let before = runner.calls().len();
        let again = ctx.start().await.unwrap();
        assert_eq!(first, again);
        assert_eq!(runner.calls().len(), before);
        assert_eq!(ctx.apps.len(), 1);
    }

    #[tokio::test]
    async fn stop_kills_the_pid_and_releases_the_port() {
        let runner = StubRunner::new(healthy_after(1));
        let (ctx, _dir) = context(Arc::clone(&runner), 120);
        let instance = ctx.start().await.unwrap();
        let stopped = ctx.stop().unwrap().unwrap();
        assert_eq!(stopped.instance, instance);
        assert!(stopped.killed);
        assert!(runner
            .calls()
            .iter()
            .any(|c| c[..] == ["exec", "nanna-task-t", "sh", "-c", "kill 4242"]));
        assert!(ctx.apps.is_empty());
        assert!(ctx.ports.held().is_empty());
        assert!(ctx.stop().unwrap().is_none(), "stop is idempotent");
    }

    #[tokio::test]
    async fn stop_reports_a_process_that_was_already_gone() {
        let runner = StubRunner::new(Box::new(|args, _| {
            if script(args).starts_with("kill 4242") {
                failed("No such process")
            } else {
                healthy_after(1)(args, 1)
            }
        }));
        let (ctx, _dir) = context(Arc::clone(&runner), 120);
        ctx.start().await.unwrap();
        let stopped = ctx.stop().unwrap().unwrap();
        assert!(!stopped.killed);
        assert!(ctx.apps.is_empty());
    }

    #[tokio::test]
    async fn logs_tail_the_log_file() {
        let runner = StubRunner::new(healthy_after(1));
        let (ctx, _dir) = context(Arc::clone(&runner), 120);
        let lines = ctx.logs(2).unwrap();
        assert_eq!(lines, vec!["line one".to_string(), "line two".to_string()]);
        assert!(runner.calls().iter().any(|c| c[..]
            == [
                "exec",
                "nanna-task-t",
                "tail",
                "-n",
                "2",
                "/tmp/nanna-app-t.log"
            ]));
    }

    #[tokio::test]
    async fn logs_failure_is_reported() {
        let runner = StubRunner::new(Box::new(|args, _| {
            if has(args, "tail") {
                failed("No such file")
            } else {
                ok("")
            }
        }));
        let (ctx, _dir) = context(runner, 120);
        let err = ctx.logs(5).unwrap_err();
        assert!(
            matches!(err, AppError::StepFailed { ref step, .. } if step == "tail"),
            "{err}"
        );
        assert!(err.to_string().contains("No such file"));
    }

    #[tokio::test]
    async fn failed_frontend_build_is_a_step_error() {
        let runner = StubRunner::new(Box::new(|args, _| {
            if has(args, "trunk") {
                failed("wasm-bindgen mismatch")
            } else {
                ok("")
            }
        }));
        let (ctx, _dir) = context(runner, 120);
        let err = ctx.start().await.unwrap_err();
        assert!(
            matches!(err, AppError::StepFailed { ref step, .. } if step == "trunk build"),
            "{err}"
        );
        assert!(err.to_string().contains("wasm-bindgen mismatch"));
        assert!(ctx.apps.is_empty());
        assert!(ctx.ports.held().is_empty());
    }

    #[tokio::test]
    async fn build_without_executable_is_an_error() {
        let runner = StubRunner::new(Box::new(|_, _| ok("{}\n")));
        let (ctx, _dir) = context(runner, 120);
        let err = ctx.start().await.unwrap_err();
        assert!(
            matches!(err, AppError::NoExecutable { ref package } if package == "api"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn start_without_pid_is_an_error() {
        let runner = StubRunner::new(Box::new(|args, _| {
            if has(args, "cargo") {
                ok(BUILD_JSON)
            } else {
                ok("sh: nohup: not found\n")
            }
        }));
        let (ctx, _dir) = context(runner, 120);
        let err = ctx.start().await.unwrap_err();
        assert!(matches!(err, AppError::NoPid { .. }), "{err}");
        assert!(err.to_string().contains("nohup: not found"));
        assert!(ctx.ports.held().is_empty());
    }

    #[tokio::test]
    async fn spawn_failure_is_reported_with_the_command() {
        let runner = StubRunner::new(Box::new(|_, _| {
            Err(std::io::Error::other("podman missing"))
        }));
        let (ctx, _dir) = context(runner, 120);
        let err = ctx.start().await.unwrap_err();
        assert!(matches!(err, AppError::Spawn { .. }), "{err}");
        assert!(err
            .to_string()
            .contains("podman exec -w /workspace/ui nanna-task-t timeout 120 trunk build"));
        assert!(err.to_string().contains("podman missing"));
    }

    #[tokio::test]
    async fn exhausted_wall_clock_before_the_first_step() {
        let runner = StubRunner::new(healthy_after(1));
        let (ctx, _dir) = context(Arc::clone(&runner), 0);
        let err = ctx.start().await.unwrap_err();
        assert!(
            matches!(err, AppError::WallClock { ref step, .. } if step == "trunk build"),
            "{err}"
        );
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn step_failing_after_the_deadline_is_a_wall_clock_error() {
        let runner = StubRunner::new(Box::new(|_, _| {
            std::thread::sleep(Duration::from_millis(1100));
            failed("")
        }));
        let (ctx, _dir) = context(runner, 1);
        let err = ctx.start().await.unwrap_err();
        assert!(
            matches!(err, AppError::WallClock { ref step, .. } if step == "trunk build"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn process_exiting_before_health_is_reported_with_logs() {
        let runner = StubRunner::new(Box::new(|args, _| {
            if has(args, "cargo") {
                ok(BUILD_JSON)
            } else if script(args).contains("nohup") {
                ok("77\n")
            } else if has(args, "curl") || script(args).starts_with("kill -0") {
                failed("")
            } else if has(args, "tail") {
                ok("panicked at main.rs\n")
            } else {
                ok("")
            }
        }));
        let (ctx, _dir) = context(runner, 120);
        let err = ctx.start().await.unwrap_err();
        assert!(matches!(err, AppError::Exited { pid: 77, .. }), "{err}");
        assert!(err.to_string().contains("panicked at main.rs"));
        assert!(ctx.apps.is_empty());
        assert!(ctx.ports.held().is_empty());
    }

    #[tokio::test]
    async fn never_healthy_is_killed_and_reported() {
        let runner = StubRunner::new(Box::new(|args, _| {
            if has(args, "cargo") {
                ok(BUILD_JSON)
            } else if script(args).contains("nohup") {
                ok("78\n")
            } else if has(args, "curl") || has(args, "tail") {
                failed("tail: cannot open")
            } else {
                ok("")
            }
        }));
        let (ctx, _dir) = context(Arc::clone(&runner), 1);
        let err = ctx.start().await.unwrap_err();
        assert!(matches!(err, AppError::Unhealthy { .. }), "{err}");
        assert!(
            err.to_string().contains("tail: cannot open"),
            "log failure is surfaced: {err}"
        );
        assert!(runner
            .calls()
            .iter()
            .any(|c| c.last().is_some_and(|s| s == "kill 78")));
        assert!(ctx.ports.held().is_empty());
    }

    #[tokio::test]
    async fn kill_failure_after_unhealthy_is_tolerated() {
        let runner = StubRunner::new(Box::new(|args, _| {
            if has(args, "cargo") {
                ok(BUILD_JSON)
            } else if script(args).contains("nohup") {
                ok("79\n")
            } else if has(args, "curl") {
                failed("")
            } else if script(args) == "kill 79" {
                Err(std::io::Error::other("gone"))
            } else {
                ok("")
            }
        }));
        let (ctx, _dir) = context(runner, 1);
        assert!(matches!(
            ctx.start().await.unwrap_err(),
            AppError::Unhealthy { .. }
        ));
    }

    #[tokio::test]
    async fn port_exhaustion_is_reported() {
        let runner = StubRunner::new(healthy_after(1));
        let (ctx, _dir) = context(runner, 120);
        let _a = ctx.ports.allocate("x").unwrap();
        let _b = ctx.ports.allocate("y").unwrap();
        let err = ctx.start().await.unwrap_err();
        assert!(matches!(err, AppError::Port(_)), "{err}");
    }
}
