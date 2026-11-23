use super::binding::BoundWorkspace;
use super::{image_is_valid, Endpoint, ResolvedAgent};
use crate::action_auditor::ActionAuditLogEntry;
use crate::agent::{AgentError, AgentRunResult, AgentState};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

pub const PI_IMAGE_ENV: &str = "NANNA_PI_IMAGE";
pub const PI_BROKER_DIR_ENV: &str = "NANNA_PI_BROKER_DIR";

const DIGEST_PREFIX: &str = "@sha256:";
const DIGEST_HEX_LEN: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PiRouteError {
    #[error("pi image `{image}` is not a digest-pinned reference (name@sha256:<64 hex>)")]
    Image { image: String },
    #[error("pi broker directory `{path}` is not a normalised absolute path")]
    Socket { path: String },
    #[error("pi route is enabled but {variable} is not set")]
    Missing { variable: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PiLimits {
    pub memory_mb: u32,
    pub pids: u32,
}

impl Default for PiLimits {
    fn default() -> Self {
        Self {
            memory_mb: 2048,
            pids: 512,
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub struct PiLaunch<'a> {
    task_id: &'a str,
    container_name: &'a str,
    agent: &'a ResolvedAgent,
    binding: BoundWorkspace,
    route: &'a PiRoute,
    progress: Arc<AtomicUsize>,
    deadline: Duration,
}

#[cfg_attr(not(test), allow(dead_code))]
impl<'a> PiLaunch<'a> {
    pub(crate) fn new(
        task_id: &'a str,
        container_name: &'a str,
        agent: &'a ResolvedAgent,
        binding: BoundWorkspace,
        route: &'a PiRoute,
        progress: Arc<AtomicUsize>,
        deadline: Duration,
    ) -> Self {
        Self {
            task_id,
            container_name,
            agent,
            binding,
            route,
            progress,
            deadline,
        }
    }

    pub(crate) fn task_id(&self) -> &str {
        self.task_id
    }

    pub(crate) fn container_name(&self) -> &str {
        self.container_name
    }

    pub(crate) fn agent(&self) -> &ResolvedAgent {
        self.agent
    }

    pub(crate) fn route(&self) -> &PiRoute {
        self.route
    }

    pub(crate) fn progress(&self) -> &Arc<AtomicUsize> {
        &self.progress
    }

    pub(crate) fn deadline(&self) -> Duration {
        self.deadline
    }

    pub(crate) fn into_binding(self) -> BoundWorkspace {
        self.binding
    }
}

pub struct PiRunOutcome {
    pub result: Result<AgentRunResult, AgentError>,
    pub action_audit: Vec<ActionAuditLogEntry>,
}

#[async_trait::async_trait]
pub trait PiExecutor: Send + Sync + std::fmt::Debug {
    async fn run(&self, launch: PiLaunch<'_>) -> PiRunOutcome;
}

#[derive(Debug)]
pub struct UnwiredPiExecutor;

#[async_trait::async_trait]
impl PiExecutor for UnwiredPiExecutor {
    async fn run(&self, launch: PiLaunch<'_>) -> PiRunOutcome {
        PiRunOutcome {
            result: Err(AgentError::StateError {
                message: format!(
                    "the pi executor is not wired into this build; task {} was not run",
                    launch.task_id()
                ),
                iterations_completed: 0,
                tool_calls_made: vec![],
                conversation_snapshot: vec![],
                last_agent_state: AgentState::Error("pi executor not wired".to_string()),
            }),
            action_audit: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PiRoute {
    image: String,
    broker_dir: PathBuf,
    endpoint: Endpoint,
    limits: PiLimits,
    executor: Arc<dyn PiExecutor>,
}

fn digest_pinned(image: &str) -> bool {
    let Some((name, digest)) = image.rsplit_once(DIGEST_PREFIX) else {
        return false;
    };
    image_is_valid(image)
        && !name.is_empty()
        && digest.len() == DIGEST_HEX_LEN
        && digest
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
}

fn default_endpoint() -> Endpoint {
    Endpoint::new("model-gateway", 11434).expect("the default endpoint is valid")
}

fn socket_is_valid(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let charset = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    let Some(rest) = text.strip_prefix('/') else {
        return false;
    };
    charset
        && !rest.is_empty()
        && rest
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && super::broker_socket_is_valid(&format!("{text}/probe.sock"))
}

impl PiRoute {
    #[doc = r#"Build a route from host configuration. The image must be pinned by digest
and the broker directory must be a normalised absolute path.

```
use harness::harness_adapter::{PiLimits, PiRoute, UnwiredPiExecutor};
use std::sync::Arc;

let digest = "a".repeat(64);
let route = PiRoute::new(
    format!("nanna-pi@sha256:{digest}"),
    "/run/user/1000/nanna/broker",
    PiLimits::default(),
    Arc::new(UnwiredPiExecutor),
)
.unwrap();
assert_eq!(route.limits(), PiLimits::default());
assert!(PiRoute::new("nanna-pi:latest", "/run/nanna/broker", PiLimits::default(), Arc::new(UnwiredPiExecutor)).is_err());
```"#]
    pub fn new(
        image: impl Into<String>,
        broker_dir: impl Into<PathBuf>,
        limits: PiLimits,
        executor: Arc<dyn PiExecutor>,
    ) -> Result<Self, PiRouteError> {
        Self::checked(image.into(), broker_dir.into(), limits, executor)
    }

    fn checked(
        image: String,
        broker_dir: PathBuf,
        limits: PiLimits,
        executor: Arc<dyn PiExecutor>,
    ) -> Result<Self, PiRouteError> {
        if !digest_pinned(&image) {
            return Err(PiRouteError::Image { image });
        }
        if !socket_is_valid(&broker_dir) {
            return Err(PiRouteError::Socket {
                path: broker_dir.display().to_string(),
            });
        }
        let endpoint = default_endpoint();
        Ok(Self {
            image,
            broker_dir,
            endpoint,
            limits,
            executor,
        })
    }

    pub fn from_lookup(
        lookup: &dyn Fn(&str) -> Option<OsString>,
        executor: Arc<dyn PiExecutor>,
    ) -> Result<Self, PiRouteError> {
        let image = lookup(PI_IMAGE_ENV).ok_or(PiRouteError::Missing {
            variable: PI_IMAGE_ENV,
        })?;
        let socket = lookup(PI_BROKER_DIR_ENV).ok_or(PiRouteError::Missing {
            variable: PI_BROKER_DIR_ENV,
        })?;
        let image = image.into_string().map_err(|raw| PiRouteError::Image {
            image: raw.to_string_lossy().into_owned(),
        })?;
        Self::new(image, socket, PiLimits::default(), executor)
    }

    pub fn image(&self) -> &str {
        &self.image
    }

    pub fn broker_dir(&self) -> &Path {
        &self.broker_dir
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn with_endpoint(mut self, endpoint: Endpoint) -> Self {
        self.endpoint = endpoint;
        self
    }

    pub(crate) fn broker_socket(&self, task_id: &str) -> PathBuf {
        PathBuf::from(format!("{}/{task_id}.sock", self.broker_dir.display()))
    }

    pub fn limits(&self) -> PiLimits {
        self.limits
    }

    pub fn executor(&self) -> &Arc<dyn PiExecutor> {
        &self.executor
    }
}

pub(crate) fn pi_container_name(task_id: &str) -> String {
    format!("nanna-pi-{task_id}")
}

pub(crate) struct PiContainerGuard {
    name: String,
}

impl PiContainerGuard {
    pub(crate) fn arm(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

pub(super) const REMOVE_LIMIT: Duration = Duration::from_secs(10);

pub(super) fn off_runtime<F: FnOnce()>(work: F) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

pub(super) fn bounded_status(command: &str, args: &[String], limit: Duration) {
    let Ok(mut child) = std::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return;
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if started.elapsed() < limit => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

impl Drop for PiContainerGuard {
    fn drop(&mut self) {
        off_runtime(|| {
            let runtime = crate::container::detect_runtime();
            if runtime.is_available() {
                let args = ["rm", "-f", self.name.as_str()].map(String::from);
                bounded_status(runtime.command(), &args, REMOVE_LIMIT);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::test_support::FakePodman;

    fn digest() -> String {
        "a".repeat(64)
    }

    fn image() -> String {
        format!("nanna-pi@sha256:{}", digest())
    }

    fn wired(image: &str, socket: &str) -> Result<PiRoute, PiRouteError> {
        PiRoute::new(
            image,
            socket,
            PiLimits::default(),
            Arc::new(UnwiredPiExecutor),
        )
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[test]
    fn a_digest_pinned_image_and_a_normalised_socket_build_a_route() {
        let route = wired(&image(), "/run/user/1000/nanna/broker").unwrap();
        assert_eq!(route.image(), image());
        assert_eq!(route.broker_dir(), Path::new("/run/user/1000/nanna/broker"));
        assert_eq!(route.limits(), PiLimits::default());
        assert!(format!("{route:?}").contains("UnwiredPiExecutor"));
    }

    #[test]
    fn the_model_endpoint_defaults_and_can_be_replaced() {
        let route = wired(&image(), "/run/nanna/broker").unwrap();
        assert_eq!(
            route.endpoint(),
            &Endpoint::new("model-gateway", 11434).unwrap()
        );
        let other = Endpoint::new("gw.internal", 8080).unwrap();
        let route = route.with_endpoint(other.clone());
        assert_eq!(route.endpoint(), &other);
    }

    #[test]
    fn an_image_that_is_not_pinned_by_digest_is_refused() {
        let socket = "/run/nanna/broker";
        for bad in [
            "nanna-pi:latest".to_string(),
            "nanna-pi".to_string(),
            format!("@sha256:{}", digest()),
            format!("nanna-pi@sha256:{}", "a".repeat(63)),
            format!("nanna-pi@sha256:{}", "a".repeat(65)),
            format!("nanna-pi@sha256:{}", "A".repeat(64)),
            format!("nanna-pi@sha256:{}", "g".repeat(64)),
            format!("-nanna-pi@sha256:{}", digest()),
            format!("nanna pi@sha256:{}", digest()),
            format!("nanna-pi@sha512:{}", digest()),
        ] {
            assert_eq!(
                wired(&bad, socket).unwrap_err(),
                PiRouteError::Image { image: bad.clone() },
                "{bad}"
            );
        }
    }

    #[test]
    fn a_socket_that_is_not_a_normalised_absolute_path_is_refused() {
        for bad in [
            "gateway.sock",
            "/",
            "/run/../etc/gw.sock",
            "/run/./gw.sock",
            "/run/gw sock",
            "/run/gw;sock",
        ] {
            assert_eq!(
                wired(&image(), bad).unwrap_err(),
                PiRouteError::Socket {
                    path: bad.to_string()
                },
                "{bad}"
            );
        }
    }

    #[test]
    fn host_environment_configures_the_route() {
        let lookup = env(&[
            (
                PI_IMAGE_ENV,
                "nanna-pi@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            (PI_BROKER_DIR_ENV, "/run/user/1000/nanna/broker"),
        ]);
        let route = PiRoute::from_lookup(&lookup, Arc::new(UnwiredPiExecutor)).unwrap();
        assert_eq!(route.image(), image());
        assert_eq!(
            PiRoute::from_lookup(&env(&[]), Arc::new(UnwiredPiExecutor)).unwrap_err(),
            PiRouteError::Missing {
                variable: PI_IMAGE_ENV
            }
        );
        assert_eq!(
            PiRoute::from_lookup(
                &env(&[(
                    PI_IMAGE_ENV,
                    "x@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                )]),
                Arc::new(UnwiredPiExecutor)
            )
            .unwrap_err(),
            PiRouteError::Missing {
                variable: PI_BROKER_DIR_ENV
            }
        );
        assert!(matches!(
            PiRoute::from_lookup(
                &env(&[
                    (PI_IMAGE_ENV, "latest"),
                    (PI_BROKER_DIR_ENV, "/run/nanna/broker")
                ]),
                Arc::new(UnwiredPiExecutor)
            ),
            Err(PiRouteError::Image { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_image_is_refused() {
        use std::os::unix::ffi::OsStringExt;
        let lookup = |key: &str| match key {
            PI_IMAGE_ENV => Some(OsString::from_vec(vec![0x6e, 0xff])),
            _ => Some(OsString::from("/run/nanna/broker")),
        };
        assert!(matches!(
            PiRoute::from_lookup(&lookup, Arc::new(UnwiredPiExecutor)),
            Err(PiRouteError::Image { .. })
        ));
    }

    #[test]
    fn the_container_name_is_deterministic_per_task() {
        assert_eq!(pi_container_name("t-1"), "nanna-pi-t-1");
        let guard = PiContainerGuard::arm("nanna-pi-t-1");
        assert_eq!(guard.name(), "nanna-pi-t-1");
        std::mem::forget(guard);
    }

    const SLOW_RM: &str = "case \"$1\" in rm) sleep 1;; esac";

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_slow_removal_does_not_stall_a_multi_thread_runtime() {
        let fake = FakePodman::install_scripted_async(SLOW_RM).await;
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ticker = {
            let ticks = Arc::clone(&ticks);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let before = ticks.load(std::sync::atomic::Ordering::SeqCst);
        drop(PiContainerGuard::arm("nanna-pi-t-1"));
        let after = ticks.load(std::sync::atomic::Ordering::SeqCst);
        ticker.abort();
        assert!(fake.calls().contains(&"rm -f nanna-pi-t-1".to_string()));
        assert!(after >= before + 10, "ticker advanced {before} -> {after}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_slow_removal_completes_inline_on_a_current_thread_runtime() {
        let fake = FakePodman::install_scripted_async(SLOW_RM).await;
        let started = std::time::Instant::now();
        drop(PiContainerGuard::arm("nanna-pi-t-1"));
        assert!(started.elapsed() >= Duration::from_millis(900));
        assert!(fake.calls().contains(&"rm -f nanna-pi-t-1".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn a_removal_that_hangs_is_killed_at_the_limit() {
        let started = std::time::Instant::now();
        bounded_status(
            "sh",
            &["-c".to_string(), "sleep 30".to_string()],
            Duration::from_millis(200),
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn a_removal_that_finishes_returns_before_the_limit() {
        let started = std::time::Instant::now();
        bounded_status(
            "sh",
            &["-c".to_string(), "exit 0".to_string()],
            Duration::from_secs(20),
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_removal_command_returns_at_once() {
        bounded_status(
            "/nonexistent/nanna-pi-test-command",
            &[],
            Duration::from_secs(20),
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_guard_force_removes_the_named_container() {
        let fake = FakePodman::install_async(None).await;
        let guard = PiContainerGuard::arm("nanna-pi-t-1");
        assert!(!fake.calls().contains(&"rm -f nanna-pi-t-1".to_string()));
        drop(guard);
        assert!(fake.calls().contains(&"rm -f nanna-pi-t-1".to_string()));
    }

    #[tokio::test]
    async fn the_unwired_executor_refuses_every_launch() {
        let route = wired(&image(), "/run/nanna/broker").unwrap();
        let card = "[identity]\nname = \"reader\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\nsystem_prompt = { inline = \"p\" }\n\n[scope]\nrepos = []\npaths = [\"**\"]\nmax_effect = \"none\"\ntools = []\n\n[limits]\nmax_iterations = 1\nmax_wall_clock_secs = 1\nmax_concurrent = 1\n";
        let identity =
            crate::identity::AgentIdentity::from_toml_str(card, "agents/reader.toml").unwrap();
        let allowed = crate::harness_adapter::testing::allowed_for(&identity);
        let capabilities = crate::harness_adapter::testing::scoped(&identity, &[]);
        let agent = ResolvedAgent::resolve(
            &allowed,
            &capabilities,
            route.endpoint().clone(),
            route.image(),
            route.broker_socket("t-1"),
        )
        .unwrap();
        let bound = BoundWorkspace::for_test(
            crate::tools::ToolRegistry::new(),
            &identity,
            allowed.repo(),
            &agent,
        );
        let outcome = UnwiredPiExecutor
            .run(PiLaunch::new(
                "t-1",
                "nanna-pi-t-1",
                &agent,
                bound,
                &route,
                Arc::new(AtomicUsize::new(0)),
                Duration::from_secs(1),
            ))
            .await;
        assert!(outcome.action_audit.is_empty());
        match outcome.result {
            Err(AgentError::StateError {
                message,
                iterations_completed,
                tool_calls_made,
                conversation_snapshot,
                last_agent_state,
            }) => {
                assert_eq!(
                    message,
                    "the pi executor is not wired into this build; task t-1 was not run"
                );
                assert_eq!(iterations_completed, 0);
                assert!(tool_calls_made.is_empty());
                assert!(conversation_snapshot.is_empty());
                assert_eq!(
                    last_agent_state,
                    AgentState::Error("pi executor not wired".to_string())
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
