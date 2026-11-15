use super::{image_is_valid, ScopedCapabilities};
use crate::agent::{AgentError, AgentRunResult, AgentState};
use crate::auditor::Allowed;
use crate::tools::ToolRegistry;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

pub const PI_IMAGE_ENV: &str = "NANNA_PI_IMAGE";
pub const PI_GATEWAY_SOCKET_ENV: &str = "NANNA_PI_GATEWAY_SOCKET";

const DIGEST_PREFIX: &str = "@sha256:";
const DIGEST_HEX_LEN: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PiRouteError {
    #[error("pi image `{image}` is not a digest-pinned reference (name@sha256:<64 hex>)")]
    Image { image: String },
    #[error("pi gateway socket `{path}` is not a normalised absolute path")]
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

pub struct PiLaunch<'a> {
    pub task_id: &'a str,
    pub container_name: &'a str,
    pub repo: &'a str,
    pub subtask: &'a str,
    pub allowed: &'a Allowed,
    pub capabilities: &'a ScopedCapabilities,
    pub registry: &'a ToolRegistry,
    pub route: &'a PiRoute,
    pub progress: Arc<AtomicUsize>,
    pub deadline: Duration,
}

#[async_trait::async_trait]
pub trait PiExecutor: Send + Sync + std::fmt::Debug {
    async fn run(&self, launch: PiLaunch<'_>) -> Result<AgentRunResult, AgentError>;
}

#[derive(Debug)]
pub struct UnwiredPiExecutor;

#[async_trait::async_trait]
impl PiExecutor for UnwiredPiExecutor {
    async fn run(&self, launch: PiLaunch<'_>) -> Result<AgentRunResult, AgentError> {
        Err(AgentError::StateError {
            message: format!(
                "the pi executor is not wired into this build; task {} was not run",
                launch.task_id
            ),
            iterations_completed: 0,
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            last_agent_state: AgentState::Error("pi executor not wired".to_string()),
        })
    }
}

#[derive(Debug, Clone)]
pub struct PiRoute {
    image: String,
    gateway_socket: PathBuf,
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
}

impl PiRoute {
    #[doc = r#"Build a route from host configuration. The image must be pinned by digest
and the gateway socket must be a normalised absolute path.

```
use harness::harness_adapter::{PiLimits, PiRoute, UnwiredPiExecutor};
use std::sync::Arc;

let digest = "a".repeat(64);
let route = PiRoute::new(
    format!("nanna-pi@sha256:{digest}"),
    "/run/user/1000/nanna/gateway.sock",
    PiLimits::default(),
    Arc::new(UnwiredPiExecutor),
)
.unwrap();
assert_eq!(route.limits(), PiLimits::default());
assert!(PiRoute::new("nanna-pi:latest", "/run/gw.sock", PiLimits::default(), Arc::new(UnwiredPiExecutor)).is_err());
```"#]
    pub fn new(
        image: impl Into<String>,
        gateway_socket: impl Into<PathBuf>,
        limits: PiLimits,
        executor: Arc<dyn PiExecutor>,
    ) -> Result<Self, PiRouteError> {
        let image = image.into();
        let gateway_socket = gateway_socket.into();
        if !digest_pinned(&image) {
            return Err(PiRouteError::Image { image });
        }
        if !socket_is_valid(&gateway_socket) {
            return Err(PiRouteError::Socket {
                path: gateway_socket.display().to_string(),
            });
        }
        Ok(Self {
            image,
            gateway_socket,
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
        let socket = lookup(PI_GATEWAY_SOCKET_ENV).ok_or(PiRouteError::Missing {
            variable: PI_GATEWAY_SOCKET_ENV,
        })?;
        let image = image.into_string().map_err(|raw| PiRouteError::Image {
            image: raw.to_string_lossy().into_owned(),
        })?;
        Self::new(image, socket, PiLimits::default(), executor)
    }

    pub fn image(&self) -> &str {
        &self.image
    }

    pub fn gateway_socket(&self) -> &Path {
        &self.gateway_socket
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

impl Drop for PiContainerGuard {
    fn drop(&mut self) {
        let runtime = crate::container::detect_runtime();
        if !runtime.is_available() {
            return;
        }
        let _ = std::process::Command::new(runtime.command())
            .args(["rm", "-f", &self.name])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let route = wired(&image(), "/run/user/1000/nanna/gateway.sock").unwrap();
        assert_eq!(route.image(), image());
        assert_eq!(
            route.gateway_socket(),
            Path::new("/run/user/1000/nanna/gateway.sock")
        );
        assert_eq!(route.limits(), PiLimits::default());
        assert!(format!("{route:?}").contains("UnwiredPiExecutor"));
    }

    #[test]
    fn an_image_that_is_not_pinned_by_digest_is_refused() {
        let socket = "/run/gw.sock";
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
            (PI_GATEWAY_SOCKET_ENV, "/run/user/1000/nanna/gateway.sock"),
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
                variable: PI_GATEWAY_SOCKET_ENV
            }
        );
        assert!(matches!(
            PiRoute::from_lookup(
                &env(&[
                    (PI_IMAGE_ENV, "latest"),
                    (PI_GATEWAY_SOCKET_ENV, "/run/gw.sock")
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
            _ => Some(OsString::from("/run/gw.sock")),
        };
        assert!(matches!(
            PiRoute::from_lookup(&lookup, Arc::new(UnwiredPiExecutor)),
            Err(PiRouteError::Image { .. })
        ));
    }

    #[test]
    fn the_container_name_is_deterministic_per_task() {
        assert_eq!(pi_container_name("t-1"), "nanna-pi-t-1");
        assert_eq!(PiContainerGuard::arm("nanna-pi-t-1").name(), "nanna-pi-t-1");
    }

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
        let route = wired(&image(), "/run/gw.sock").unwrap();
        let card = "[identity]\nname = \"reader\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\nsystem_prompt = { inline = \"p\" }\n\n[scope]\nrepos = []\npaths = [\"**\"]\nmax_effect = \"none\"\ntools = []\n\n[limits]\nmax_iterations = 1\nmax_wall_clock_secs = 1\nmax_concurrent = 1\n";
        let identity =
            crate::identity::AgentIdentity::from_toml_str(card, "agents/reader.toml").unwrap();
        let allowed = crate::harness_adapter::testing::allowed_for(&identity);
        let capabilities = crate::harness_adapter::testing::scoped(&identity, &[]);
        let registry = ToolRegistry::new();
        let outcome = UnwiredPiExecutor
            .run(PiLaunch {
                task_id: "t-1",
                container_name: "nanna-pi-t-1",
                repo: allowed.repo(),
                subtask: &allowed.request().subtask,
                allowed: &allowed,
                capabilities: &capabilities,
                registry: &registry,
                route: &route,
                progress: Arc::new(AtomicUsize::new(0)),
                deadline: Duration::from_secs(1),
            })
            .await;
        match outcome {
            Err(AgentError::StateError {
                message,
                iterations_completed,
                ..
            }) => {
                assert!(message.contains("t-1"), "{message}");
                assert_eq!(iterations_completed, 0);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
