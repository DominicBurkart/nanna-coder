use super::adapter::{AdapterError, AdapterOp, Slot, TargetAdapter};
use async_trait::async_trait;
use std::process::Command;
use std::sync::Arc;

/// What a command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Whether the exit status was zero.
    pub success: bool,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Runs one command; injected so the adapter is unit-testable.
pub trait CommandRunner: Send + Sync {
    /// Run `program` with `args` and capture its output.
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput, String>;
}

/// Runs commands as child processes.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput, String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| format!("{program}: {e}"))?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Command templates for each adapter operation, whitespace-separated with
/// `{image}`, `{slot}` and `{percent}` placeholders.
///
/// `deploy_inactive` must print the new slot's name; `current_image` must
/// print the live image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessConfig {
    /// Template for [`TargetAdapter::deploy_inactive`].
    pub deploy_inactive: String,
    /// Template for [`TargetAdapter::set_traffic`].
    pub set_traffic: String,
    /// Template for [`TargetAdapter::current_image`].
    pub current_image: String,
    /// Template for [`TargetAdapter::rollback_to`].
    pub rollback_to: String,
    /// Template for [`TargetAdapter::retire`].
    pub retire: String,
}

/// Environment variables holding each template, in [`AdapterOp`] order.
pub const SERVERLESS_ENV: [(&str, AdapterOp); 5] = [
    (
        "NANNA_SERVERLESS_DEPLOY_INACTIVE_CMD",
        AdapterOp::DeployInactive,
    ),
    ("NANNA_SERVERLESS_SET_TRAFFIC_CMD", AdapterOp::SetTraffic),
    (
        "NANNA_SERVERLESS_CURRENT_IMAGE_CMD",
        AdapterOp::CurrentImage,
    ),
    ("NANNA_SERVERLESS_ROLLBACK_TO_CMD", AdapterOp::RollbackTo),
    ("NANNA_SERVERLESS_RETIRE_CMD", AdapterOp::Retire),
];

impl ServerlessConfig {
    /// Read every template from the process environment.
    pub fn from_env() -> Result<Self, AdapterError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Build from `lookup`, which resolves each [`SERVERLESS_ENV`] name.
    /// A missing or blank template is an error naming the operation.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, AdapterError> {
        let mut templates = Vec::with_capacity(SERVERLESS_ENV.len());
        for (name, op) in SERVERLESS_ENV {
            match lookup(name).filter(|t| !t.trim().is_empty()) {
                Some(template) => templates.push(template),
                None => {
                    return Err(AdapterError {
                        op,
                        reason: format!("{name} is not set"),
                    })
                }
            }
        }
        let mut templates = templates.into_iter();
        Ok(Self {
            deploy_inactive: templates.next().expect("five templates"),
            set_traffic: templates.next().expect("five templates"),
            current_image: templates.next().expect("five templates"),
            rollback_to: templates.next().expect("five templates"),
            retire: templates.next().expect("five templates"),
        })
    }
}

/// Target adapter for `container-registry+serverless` that shells out to a
/// configured CLI, so no provider SDK enters the core crate.
///
/// ```
/// use harness::rollout::{CommandOutput, CommandRunner, ServerlessAdapter, ServerlessConfig, TargetAdapter};
/// use std::sync::{Arc, Mutex};
///
/// struct Echo(Mutex<Vec<Vec<String>>>);
/// impl CommandRunner for Echo {
///     fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput, String> {
///         let mut line = vec![program.to_string()];
///         line.extend(args.iter().cloned());
///         self.0.lock().unwrap().push(line);
///         Ok(CommandOutput { success: true, stdout: "green\n".into(), stderr: String::new() })
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let config = ServerlessConfig {
///     deploy_inactive: "cloudctl deploy --image {image} --inactive".into(),
///     set_traffic: "cloudctl traffic {slot} {percent}".into(),
///     current_image: "cloudctl current-image".into(),
///     rollback_to: "cloudctl rollback {image}".into(),
///     retire: "cloudctl retire {slot}".into(),
/// };
/// let runner = Arc::new(Echo(Mutex::new(Vec::new())));
/// let adapter = ServerlessAdapter::new(config, runner.clone());
/// let slot = adapter.deploy_inactive("registry.example.invalid/ns/app:v2").await.unwrap();
/// assert_eq!(slot.name(), "green");
/// adapter.set_traffic(&slot, 10).await.unwrap();
/// assert_eq!(runner.0.lock().unwrap()[1], ["cloudctl", "traffic", "green", "10"]);
/// # });
/// ```
pub struct ServerlessAdapter {
    config: ServerlessConfig,
    runner: Arc<dyn CommandRunner>,
}

impl ServerlessAdapter {
    /// An adapter running `config`'s templates through `runner`.
    pub fn new(config: ServerlessConfig, runner: Arc<dyn CommandRunner>) -> Self {
        Self { config, runner }
    }

    /// Substitute the placeholders and split the template into arguments.
    pub fn render(template: &str, image: &str, slot: &str, percent: u8) -> Vec<String> {
        template
            .split_whitespace()
            .map(|token| {
                token
                    .replace("{image}", image)
                    .replace("{slot}", slot)
                    .replace("{percent}", &percent.to_string())
            })
            .collect()
    }

    fn invoke(
        &self,
        op: AdapterOp,
        template: &str,
        image: &str,
        slot: &str,
        percent: u8,
    ) -> Result<String, AdapterError> {
        let argv = Self::render(template, image, slot, percent);
        let Some((program, args)) = argv.split_first() else {
            return Err(AdapterError {
                op,
                reason: "command template is empty".into(),
            });
        };
        let output = self
            .runner
            .run(program, args)
            .map_err(|reason| AdapterError { op, reason })?;
        if !output.success {
            return Err(AdapterError {
                op,
                reason: format!("{program} exited with failure: {}", output.stderr.trim()),
            });
        }
        Ok(output.stdout.trim().to_string())
    }

    fn invoke_for_output(
        &self,
        op: AdapterOp,
        template: &str,
        image: &str,
    ) -> Result<String, AdapterError> {
        let out = self.invoke(op, template, image, "", 0)?;
        if out.is_empty() {
            return Err(AdapterError {
                op,
                reason: "command printed nothing".into(),
            });
        }
        Ok(out)
    }
}

impl std::fmt::Debug for ServerlessAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerlessAdapter")
            .field("config", &self.config)
            .finish()
    }
}

#[async_trait]
impl TargetAdapter for ServerlessAdapter {
    async fn deploy_inactive(&self, image: &str) -> Result<Slot, AdapterError> {
        self.invoke_for_output(
            AdapterOp::DeployInactive,
            &self.config.deploy_inactive,
            image,
        )
        .map(Slot::new)
    }

    async fn set_traffic(&self, slot: &Slot, percent: u8) -> Result<(), AdapterError> {
        self.invoke(
            AdapterOp::SetTraffic,
            &self.config.set_traffic,
            "",
            slot.name(),
            percent,
        )
        .map(drop)
    }

    async fn current_image(&self) -> Result<String, AdapterError> {
        self.invoke_for_output(AdapterOp::CurrentImage, &self.config.current_image, "")
    }

    async fn rollback_to(&self, image: &str) -> Result<(), AdapterError> {
        self.invoke(
            AdapterOp::RollbackTo,
            &self.config.rollback_to,
            image,
            "",
            100,
        )
        .map(drop)
    }

    async fn retire(&self, slot: &Slot) -> Result<(), AdapterError> {
        self.invoke(AdapterOp::Retire, &self.config.retire, "", slot.name(), 0)
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct Scripted {
        calls: Mutex<Vec<Vec<String>>>,
        reply: Mutex<Result<CommandOutput, String>>,
    }

    impl Scripted {
        fn ok(stdout: &str) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                reply: Mutex::new(Ok(CommandOutput {
                    success: true,
                    stdout: stdout.into(),
                    stderr: String::new(),
                })),
            })
        }

        fn set(&self, reply: Result<CommandOutput, String>) {
            *self.reply.lock().unwrap() = reply;
        }

        fn last(&self) -> Vec<String> {
            self.calls.lock().unwrap().last().cloned().unwrap()
        }
    }

    impl CommandRunner for Scripted {
        fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput, String> {
            let mut line = vec![program.to_string()];
            line.extend(args.iter().cloned());
            self.calls.lock().unwrap().push(line);
            self.reply.lock().unwrap().clone()
        }
    }

    fn env() -> HashMap<&'static str, String> {
        HashMap::from([
            (
                "NANNA_SERVERLESS_DEPLOY_INACTIVE_CMD",
                "cloudctl deploy --image {image}".to_string(),
            ),
            (
                "NANNA_SERVERLESS_SET_TRAFFIC_CMD",
                "cloudctl traffic {slot} {percent}".to_string(),
            ),
            (
                "NANNA_SERVERLESS_CURRENT_IMAGE_CMD",
                "cloudctl current".to_string(),
            ),
            (
                "NANNA_SERVERLESS_ROLLBACK_TO_CMD",
                "cloudctl rollback {image}".to_string(),
            ),
            (
                "NANNA_SERVERLESS_RETIRE_CMD",
                "cloudctl retire {slot}".to_string(),
            ),
        ])
    }

    fn config() -> ServerlessConfig {
        ServerlessConfig::from_lookup(|name| env().get(name).cloned()).unwrap()
    }

    #[test]
    fn config_requires_every_template() {
        assert_eq!(config().retire, "cloudctl retire {slot}");
        let mut partial = env();
        partial.insert("NANNA_SERVERLESS_ROLLBACK_TO_CMD", "  ".into());
        let err = ServerlessConfig::from_lookup(|name| partial.get(name).cloned()).unwrap_err();
        assert_eq!(err.op, AdapterOp::RollbackTo);
        assert_eq!(
            err.to_string(),
            "target adapter rollback_to failed: NANNA_SERVERLESS_ROLLBACK_TO_CMD is not set"
        );
        assert!(ServerlessConfig::from_env().is_err());
    }

    #[test]
    fn render_substitutes_every_placeholder() {
        assert_eq!(
            ServerlessAdapter::render(" a  {image}/{slot}:{percent} ", "img", "s", 42),
            ["a", "img/s:42"]
        );
        assert!(ServerlessAdapter::render("", "i", "s", 0).is_empty());
    }

    #[tokio::test]
    async fn every_operation_runs_its_template() {
        let runner = Scripted::ok("green\n");
        let adapter = ServerlessAdapter::new(config(), runner.clone());
        assert!(format!("{adapter:?}").contains("cloudctl"));
        let slot = adapter.deploy_inactive("app:v2").await.unwrap();
        assert_eq!(slot, Slot::new("green"));
        assert_eq!(runner.last(), ["cloudctl", "deploy", "--image", "app:v2"]);
        adapter.set_traffic(&slot, 25).await.unwrap();
        assert_eq!(runner.last(), ["cloudctl", "traffic", "green", "25"]);
        assert_eq!(adapter.current_image().await.unwrap(), "green");
        assert_eq!(runner.last(), ["cloudctl", "current"]);
        adapter.rollback_to("app:v1").await.unwrap();
        assert_eq!(runner.last(), ["cloudctl", "rollback", "app:v1"]);
        adapter.retire(&slot).await.unwrap();
        assert_eq!(runner.last(), ["cloudctl", "retire", "green"]);
    }

    #[tokio::test]
    async fn failures_name_the_operation() {
        let runner = Scripted::ok("");
        let adapter = ServerlessAdapter::new(config(), runner.clone());
        let err = adapter.deploy_inactive("app:v2").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter deploy_inactive failed: command printed nothing"
        );
        assert!(adapter.current_image().await.is_err());
        assert!(adapter.retire(&Slot::new("g")).await.is_ok());
        runner.set(Ok(CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: "denied\n".into(),
        }));
        let err = adapter.set_traffic(&Slot::new("g"), 1).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter set_traffic failed: cloudctl exited with failure: denied"
        );
        runner.set(Err("spawn failed".into()));
        let err = adapter.rollback_to("app:v1").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter rollback_to failed: spawn failed"
        );
        let mut empty = config();
        empty.retire = " ".into();
        let adapter = ServerlessAdapter::new(empty, runner);
        let err = adapter.retire(&Slot::new("g")).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter retire failed: command template is empty"
        );
    }

    #[test]
    fn process_runner_captures_output_status_and_spawn_errors() {
        let ok = ProcessRunner.run("echo", &["hello".into()]).unwrap();
        assert_eq!(
            ok,
            CommandOutput {
                success: true,
                stdout: "hello\n".into(),
                stderr: String::new()
            }
        );
        let failed = ProcessRunner
            .run("sh", &["-c".into(), "echo oops >&2; exit 3".into()])
            .unwrap();
        assert!(!failed.success);
        assert_eq!(failed.stderr, "oops\n");
        let err = ProcessRunner
            .run("/nonexistent/nanna-no-such-binary", &[])
            .unwrap_err();
        assert!(err.starts_with("/nonexistent/nanna-no-such-binary: "));
    }
}
