use super::adapter::{
    AdapterError, AdapterOp, FallbackPolicy, FallbackSupport, Slot, Swapped, TargetAdapter,
};
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
/// `{image}`, `{slot}`, `{percent}`, `{retries}` and `{statuses}`
/// placeholders.
///
/// `deploy_inactive` must print the new slot's name; `current_image` must
/// print the live image reference; `swap` must print the new active slot
/// and the retired candidate, in that order; `set_fallback` must print
/// `native` or `best-effort`. The two fallback templates are optional: a
/// provider without an edge retry leaves them unset and every rollout on
/// it records its fallback as [`FallbackSupport::BestEffort`].
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
    /// Template for [`TargetAdapter::mirror`].
    pub mirror: String,
    /// Template for [`TargetAdapter::swap`].
    pub swap: String,
    /// Template for [`TargetAdapter::set_fallback`], when the provider has one.
    pub set_fallback: Option<String>,
    /// Template for [`TargetAdapter::clear_fallback`], when the provider has one.
    pub clear_fallback: Option<String>,
}

/// Environment variables holding each required template, in [`AdapterOp`] order.
pub const SERVERLESS_ENV: [(&str, AdapterOp); 7] = [
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
    ("NANNA_SERVERLESS_MIRROR_CMD", AdapterOp::Mirror),
    ("NANNA_SERVERLESS_SWAP_CMD", AdapterOp::Swap),
];

/// Environment variables holding the optional fallback templates.
pub const SERVERLESS_FALLBACK_ENV: [(&str, AdapterOp); 2] = [
    ("NANNA_SERVERLESS_SET_FALLBACK_CMD", AdapterOp::SetFallback),
    (
        "NANNA_SERVERLESS_CLEAR_FALLBACK_CMD",
        AdapterOp::ClearFallback,
    ),
];

impl ServerlessConfig {
    /// Read every template from the process environment.
    pub fn from_env() -> Result<Self, AdapterError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Build from `lookup`, which resolves each [`SERVERLESS_ENV`] and
    /// [`SERVERLESS_FALLBACK_ENV`] name. A missing or blank required
    /// template is an error naming the operation; a missing or blank
    /// fallback template means the provider has no edge retry.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, AdapterError> {
        let mut templates = Vec::with_capacity(SERVERLESS_ENV.len());
        for (name, op) in SERVERLESS_ENV {
            let Some(template) = lookup(name).filter(|t| !t.trim().is_empty()) else {
                let reason = format!("{name} is not set");
                return Err(AdapterError { op, reason });
            };
            templates.push(template);
        }
        let [(set_fallback_name, _), (clear_fallback_name, _)] = SERVERLESS_FALLBACK_ENV;
        let optional = |name: &str| lookup(name).filter(|t| !t.trim().is_empty());
        let mut templates = templates.into_iter();
        Ok(Self {
            deploy_inactive: templates.next().expect("seven templates"),
            set_traffic: templates.next().expect("seven templates"),
            current_image: templates.next().expect("seven templates"),
            rollback_to: templates.next().expect("seven templates"),
            retire: templates.next().expect("seven templates"),
            mirror: templates.next().expect("seven templates"),
            swap: templates.next().expect("seven templates"),
            set_fallback: optional(set_fallback_name),
            clear_fallback: optional(clear_fallback_name),
        })
    }
}

/// Target adapter for `container-registry+serverless` that shells out to a
/// configured CLI, so no provider SDK enters the core crate.
///
/// ```
/// use harness::rollout::{CommandOutput, CommandRunner, FallbackPolicy, FallbackSupport, ServerlessAdapter, ServerlessConfig, TargetAdapter};
/// use std::sync::{Arc, Mutex};
///
/// struct Echo(Mutex<Vec<Vec<String>>>);
/// impl CommandRunner for Echo {
///     fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput, String> {
///         let mut line = vec![program.to_string()];
///         line.extend(args.iter().cloned());
///         self.0.lock().unwrap().push(line);
///         let stdout = if args.first().map(String::as_str) == Some("swap") { "green blue\n" } else { "green\n" };
///         Ok(CommandOutput { success: true, stdout: stdout.into(), stderr: String::new() })
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
///     mirror: "cloudctl mirror {slot} {percent}".into(),
///     swap: "cloudctl swap".into(),
///     set_fallback: None,
///     clear_fallback: None,
/// };
/// let runner = Arc::new(Echo(Mutex::new(Vec::new())));
/// let adapter = ServerlessAdapter::new(config, runner.clone());
/// let slot = adapter.deploy_inactive("registry.example.invalid/ns/app:v2").await.unwrap();
/// assert_eq!(slot.name(), "green");
/// adapter.mirror(&slot, 5).await.unwrap();
/// assert_eq!(runner.0.lock().unwrap()[1], ["cloudctl", "mirror", "green", "5"]);
/// let swapped = adapter.swap().await.unwrap();
/// assert_eq!((swapped.active.name(), swapped.retired_candidate.name()), ("green", "blue"));
/// let support = adapter.set_fallback(&slot, &FallbackPolicy::default()).await.unwrap();
/// assert_eq!(support, FallbackSupport::BestEffort, "no fallback template: best effort");
/// assert_eq!(runner.0.lock().unwrap().len(), 3);
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

    /// Substitute each `{name}` in `substitutions` and split the template
    /// into arguments.
    pub fn render(template: &str, substitutions: &[(&str, &str)]) -> Vec<String> {
        template
            .split_whitespace()
            .map(|token| {
                substitutions
                    .iter()
                    .fold(token.to_string(), |t, (name, value)| {
                        t.replace(&format!("{{{name}}}"), value)
                    })
            })
            .collect()
    }

    fn invoke(
        &self,
        op: AdapterOp,
        template: &str,
        substitutions: &[(&str, &str)],
    ) -> Result<String, AdapterError> {
        let argv = Self::render(template, substitutions);
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
        substitutions: &[(&str, &str)],
    ) -> Result<String, AdapterError> {
        let out = self.invoke(op, template, substitutions)?;
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
        let subs = [("image", image)];
        self.invoke_for_output(
            AdapterOp::DeployInactive,
            &self.config.deploy_inactive,
            &subs,
        )
        .map(Slot::new)
    }

    async fn set_traffic(&self, slot: &Slot, percent: u8) -> Result<(), AdapterError> {
        let percent = percent.to_string();
        let subs = [("slot", slot.name()), ("percent", percent.as_str())];
        self.invoke(AdapterOp::SetTraffic, &self.config.set_traffic, &subs)
            .map(drop)
    }

    async fn current_image(&self) -> Result<String, AdapterError> {
        self.invoke_for_output(AdapterOp::CurrentImage, &self.config.current_image, &[])
    }

    async fn rollback_to(&self, image: &str) -> Result<(), AdapterError> {
        let subs = [("image", image), ("percent", "100")];
        self.invoke(AdapterOp::RollbackTo, &self.config.rollback_to, &subs)
            .map(drop)
    }

    async fn retire(&self, slot: &Slot) -> Result<(), AdapterError> {
        let subs = [("slot", slot.name())];
        self.invoke(AdapterOp::Retire, &self.config.retire, &subs)
            .map(drop)
    }

    async fn mirror(&self, slot: &Slot, percent: u8) -> Result<(), AdapterError> {
        let percent = percent.to_string();
        let subs = [("slot", slot.name()), ("percent", percent.as_str())];
        self.invoke(AdapterOp::Mirror, &self.config.mirror, &subs)
            .map(drop)
    }

    async fn swap(&self) -> Result<Swapped, AdapterError> {
        let out = self.invoke_for_output(AdapterOp::Swap, &self.config.swap, &[])?;
        let mut names = out.split_whitespace();
        match (names.next(), names.next(), names.next()) {
            (Some(active), Some(retired), None) => Ok(Swapped {
                active: Slot::new(active),
                retired_candidate: Slot::new(retired),
            }),
            _ => Err(AdapterError {
                op: AdapterOp::Swap,
                reason: format!("command must print `<active> <retired>`, got `{out}`"),
            }),
        }
    }

    async fn set_fallback(
        &self,
        slot: &Slot,
        policy: &FallbackPolicy,
    ) -> Result<FallbackSupport, AdapterError> {
        let Some(template) = &self.config.set_fallback else {
            return Ok(FallbackSupport::BestEffort);
        };
        let retries = policy.retries.to_string();
        let statuses = format!("{}-{}", policy.status_min, policy.status_max);
        let subs = [
            ("slot", slot.name()),
            ("retries", retries.as_str()),
            ("statuses", statuses.as_str()),
        ];
        let out = self.invoke_for_output(AdapterOp::SetFallback, template, &subs)?;
        match out.as_str() {
            "native" => Ok(FallbackSupport::Native),
            "best-effort" => Ok(FallbackSupport::BestEffort),
            other => Err(AdapterError {
                op: AdapterOp::SetFallback,
                reason: format!("command must print `native` or `best-effort`, got `{other}`"),
            }),
        }
    }

    async fn clear_fallback(&self, slot: &Slot) -> Result<(), AdapterError> {
        let Some(template) = &self.config.clear_fallback else {
            return Ok(());
        };
        let subs = [("slot", slot.name())];
        self.invoke(AdapterOp::ClearFallback, template, &subs)
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

        fn print(&self, stdout: &str) {
            self.set(Ok(CommandOutput {
                success: true,
                stdout: stdout.into(),
                stderr: String::new(),
            }));
        }

        fn last(&self) -> Vec<String> {
            self.calls.lock().unwrap().last().cloned().unwrap()
        }

        fn count(&self) -> usize {
            self.calls.lock().unwrap().len()
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
            (
                "NANNA_SERVERLESS_MIRROR_CMD",
                "cloudctl mirror {slot} {percent}".to_string(),
            ),
            ("NANNA_SERVERLESS_SWAP_CMD", "cloudctl swap".to_string()),
            (
                "NANNA_SERVERLESS_SET_FALLBACK_CMD",
                "cloudctl fallback {slot} --retries {retries} --on {statuses}".to_string(),
            ),
            (
                "NANNA_SERVERLESS_CLEAR_FALLBACK_CMD",
                "cloudctl fallback {slot} --off".to_string(),
            ),
        ])
    }

    fn config() -> ServerlessConfig {
        ServerlessConfig::from_lookup(|name| env().get(name).cloned()).unwrap()
    }

    #[test]
    fn config_requires_every_template_but_fallback() {
        assert_eq!(config().retire, "cloudctl retire {slot}");
        assert_eq!(config().swap, "cloudctl swap");
        assert_eq!(
            config().set_fallback.as_deref(),
            Some("cloudctl fallback {slot} --retries {retries} --on {statuses}")
        );
        let mut partial = env();
        partial.insert("NANNA_SERVERLESS_ROLLBACK_TO_CMD", "  ".into());
        let err = ServerlessConfig::from_lookup(|name| partial.get(name).cloned()).unwrap_err();
        assert_eq!(err.op, AdapterOp::RollbackTo);
        assert_eq!(
            err.to_string(),
            "target adapter rollback_to failed: NANNA_SERVERLESS_ROLLBACK_TO_CMD is not set"
        );
        let mut no_mirror = env();
        no_mirror.remove("NANNA_SERVERLESS_MIRROR_CMD");
        let err = ServerlessConfig::from_lookup(|name| no_mirror.get(name).cloned()).unwrap_err();
        assert_eq!(err.op, AdapterOp::Mirror);
        let mut no_fallback = env();
        no_fallback.remove("NANNA_SERVERLESS_SET_FALLBACK_CMD");
        no_fallback.insert("NANNA_SERVERLESS_CLEAR_FALLBACK_CMD", " ".into());
        let config = ServerlessConfig::from_lookup(|name| no_fallback.get(name).cloned()).unwrap();
        assert_eq!(config.set_fallback, None);
        assert_eq!(config.clear_fallback, None);
        assert!(ServerlessConfig::from_env().is_err());
    }

    #[test]
    fn render_substitutes_every_placeholder() {
        assert_eq!(
            ServerlessAdapter::render(
                " a  {image}/{slot}:{percent} ",
                &[("image", "img"), ("slot", "s"), ("percent", "42")]
            ),
            ["a", "img/s:42"]
        );
        assert_eq!(
            ServerlessAdapter::render("{slot} {retries}", &[("slot", "s")]),
            ["s", "{retries}"]
        );
        assert!(ServerlessAdapter::render("", &[("image", "i")]).is_empty());
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
        adapter.split(&slot, 30).await.unwrap();
        assert_eq!(runner.last(), ["cloudctl", "traffic", "green", "30"]);
        assert_eq!(adapter.current_image().await.unwrap(), "green");
        assert_eq!(runner.last(), ["cloudctl", "current"]);
        adapter.rollback_to("app:v1").await.unwrap();
        assert_eq!(runner.last(), ["cloudctl", "rollback", "app:v1"]);
        adapter.retire(&slot).await.unwrap();
        assert_eq!(runner.last(), ["cloudctl", "retire", "green"]);
        adapter.mirror(&slot, 7).await.unwrap();
        assert_eq!(runner.last(), ["cloudctl", "mirror", "green", "7"]);
        adapter.clear_fallback(&slot).await.unwrap();
        assert_eq!(runner.last(), ["cloudctl", "fallback", "green", "--off"]);
    }

    #[tokio::test]
    async fn swap_parses_the_active_and_retired_slots() {
        let runner = Scripted::ok("green blue\n");
        let adapter = ServerlessAdapter::new(config(), runner.clone());
        let swapped = adapter.swap().await.unwrap();
        assert_eq!(runner.last(), ["cloudctl", "swap"]);
        assert_eq!(swapped.active, Slot::new("green"));
        assert_eq!(swapped.retired_candidate, Slot::new("blue"));
        runner.print("green\n");
        let err = adapter.swap().await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter swap failed: command must print `<active> <retired>`, got `green`"
        );
        runner.print("a b c\n");
        assert!(adapter.swap().await.is_err());
        runner.print("");
        assert_eq!(
            adapter.swap().await.unwrap_err().to_string(),
            "target adapter swap failed: command printed nothing"
        );
    }

    #[tokio::test]
    async fn fallback_reports_support_and_is_best_effort_without_a_template() {
        let runner = Scripted::ok("native\n");
        let adapter = ServerlessAdapter::new(config(), runner.clone());
        let slot = Slot::new("green");
        let policy = FallbackPolicy::default();
        assert_eq!(
            adapter.set_fallback(&slot, &policy).await.unwrap(),
            FallbackSupport::Native
        );
        assert_eq!(
            runner.last(),
            [
                "cloudctl",
                "fallback",
                "green",
                "--retries",
                "1",
                "--on",
                "500-599"
            ]
        );
        runner.print("best-effort");
        assert_eq!(
            adapter.set_fallback(&slot, &policy).await.unwrap(),
            FallbackSupport::BestEffort
        );
        runner.print("maybe");
        assert_eq!(
            adapter.set_fallback(&slot, &policy).await.unwrap_err().to_string(),
            "target adapter set_fallback failed: command must print `native` or `best-effort`, got `maybe`"
        );
        runner.print("");
        assert_eq!(
            adapter.set_fallback(&slot, &policy).await.unwrap_err().op,
            AdapterOp::SetFallback
        );
        let mut without = config();
        without.set_fallback = None;
        without.clear_fallback = None;
        let adapter = ServerlessAdapter::new(without, runner.clone());
        let before = runner.count();
        assert_eq!(
            adapter.set_fallback(&slot, &policy).await.unwrap(),
            FallbackSupport::BestEffort
        );
        adapter.clear_fallback(&slot).await.unwrap();
        assert_eq!(runner.count(), before, "no template, no command");
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
        assert_eq!(
            adapter.mirror(&Slot::new("g"), 1).await.unwrap_err().op,
            AdapterOp::Mirror
        );
        assert_eq!(
            adapter
                .clear_fallback(&Slot::new("g"))
                .await
                .unwrap_err()
                .op,
            AdapterOp::ClearFallback
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
