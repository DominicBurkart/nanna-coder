use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};

use super::config::VerifyConfig;
use super::outcome::{finished, skipped, CheckOutcome, CheckStatus, ExecError};
use super::runner::CheckRunner;
use super::status::VERIFIER_TOOL;
use crate::effects::EffectClass;
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};

#[doc = r#"The one tool a verifier's registry holds. A call names a declared check
and runs that check's declared command, never a command taken from the
arguments, through the runner; the result is the check's [`CheckOutcome`]
as JSON. It is `workspace` class, and the registry reviews every call to it
(see [`ToolRegistry::execute`]), so each check leaves an audit record.

```
use harness::effects::EffectClass;
use harness::tools::Tool;
use harness::verify::{CheckRunner, CheckTool, RawRun, VerifyConfig};
use std::sync::Arc;
use std::time::Duration;

struct Pass;
impl CheckRunner for Pass {
    fn run(&self, _: &str, _: Duration) -> Result<RawRun, String> {
        Ok(RawRun { exit_code: Some(0), timed_out: false, stdout: String::new(), stderr: String::new() })
    }
}

let config = VerifyConfig::parse("[[check]]\nname = \"a\"\ncommand = \"true\"\n").unwrap();
let tool = CheckTool::new(Arc::new(Pass), Arc::new(config));
assert_eq!(tool.name(), "run_checks");
assert_eq!(tool.effect_class(), EffectClass::Workspace);
```"#]
pub struct CheckTool {
    runner: Arc<dyn CheckRunner>,
    config: Arc<VerifyConfig>,
}

impl CheckTool {
    pub fn new(runner: Arc<dyn CheckRunner>, config: Arc<VerifyConfig>) -> Self {
        Self { runner, config }
    }
}

#[async_trait]
impl Tool for CheckTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: VERIFIER_TOOL.to_string(),
                description: "Run one check declared in the repository's .nanna/verify.toml inside the dev container".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some(
                        [(
                            "name".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("The declared check to run".to_string()),
                                items: None,
                            },
                        )]
                        .into_iter()
                        .collect(),
                    ),
                    required: Some(vec!["name".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
        let check = self
            .config
            .checks()
            .iter()
            .find(|check| check.name == name)
            .cloned()
            .ok_or_else(|| ToolError::InvalidArguments {
                message: format!("no declared check is named `{name}`"),
            })?;
        let runner = Arc::clone(&self.runner);
        let command = check.command.clone();
        let timeout = Duration::from_secs(check.timeout_secs);
        let started = Instant::now();
        let raw = tokio::task::spawn_blocking(move || runner.run(&command, timeout))
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("the verification worker failed: {e}"),
            })?
            .map_err(|message| ToolError::ExecutionFailed { message })?;
        Ok(serde_json::to_value(finished(&check, raw, started))?)
    }

    fn name(&self) -> &str {
        VERIFIER_TOOL
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Workspace
    }
}

#[doc = r#"Runs every declared check through `registry`, one `run_checks` call
each, so scope, the production hold and the action gate apply to every
check and each one is recorded in [`ToolRegistry::action_reviews`]. A
failing check does not stop the rest; a check that times out stops the run
and the remaining checks are reported `skipped` without a call. A refused
call, a launcher error or an unreadable result aborts with an [`ExecError`]
that keeps the outcomes already collected. There is no ungated way to run
the checks outside tests:

```compile_fail,E0432
use harness::verify::run_checks;
```"#]
pub async fn run_gated(
    config: &VerifyConfig,
    registry: &ToolRegistry,
) -> Result<Vec<CheckOutcome>, ExecError> {
    let mut outcomes: Vec<CheckOutcome> = Vec::new();
    let mut stopped = false;
    for check in config.checks() {
        if stopped {
            outcomes.push(skipped(check));
            continue;
        }
        let args = json!({ "name": check.name, "command": check.command });
        let failed = |message: String, denied: bool| ExecError {
            check: check.name.clone(),
            message,
            completed: outcomes.clone(),
            denied,
        };
        let value = match registry.execute(VERIFIER_TOOL, args).await {
            Ok(value) => value,
            Err(ToolError::ExecutionFailed { message }) => return Err(failed(message, false)),
            Err(refused) => return Err(failed(refused.to_string(), true)),
        };
        let outcome: CheckOutcome = serde_json::from_value(value)
            .map_err(|e| failed(format!("unreadable check result: {e}"), false))?;
        stopped = outcome.status == CheckStatus::TimedOut;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::test_support::{gate, raw, subject, timed_out, Recording, ScriptedRunner};

    struct Unreadable;

    #[async_trait]
    impl Tool for Unreadable {
        fn definition(&self) -> ToolDefinition {
            CheckTool::new(Arc::new(Recording::default()), config(&["a"])).definition()
        }

        async fn execute(&self, _args: Value) -> ToolResult<Value> {
            Ok(json!(7))
        }

        fn name(&self) -> &str {
            VERIFIER_TOOL
        }

        fn effect_class(&self) -> EffectClass {
            EffectClass::Workspace
        }
    }

    fn config(names: &[&str]) -> Arc<VerifyConfig> {
        let text: String = names
            .iter()
            .map(|n| format!("[[check]]\nname = \"{n}\"\ncommand = \"run {n}\"\n"))
            .collect();
        Arc::new(VerifyConfig::parse(&text).unwrap())
    }

    fn gated(
        runner: Arc<dyn CheckRunner>,
        config: &Arc<VerifyConfig>,
        allow: bool,
    ) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(CheckTool::new(runner, Arc::clone(config))));
        registry.with_action_gate(gate(allow), subject())
    }

    #[tokio::test]
    async fn each_executed_check_is_one_reviewed_call_and_skipped_checks_are_not() {
        let config = config(&["a", "b", "c", "d"]);
        let runner = Arc::new(ScriptedRunner::new(vec![raw(Some(0), "", ""), timed_out()]));
        let registry = gated(runner, &config, true);
        let outcomes = run_gated(&config, &registry).await.unwrap();
        let statuses: Vec<_> = outcomes.iter().map(|o| o.status).collect();
        assert_eq!(
            statuses,
            [
                CheckStatus::Passed,
                CheckStatus::TimedOut,
                CheckStatus::Skipped,
                CheckStatus::Skipped
            ]
        );
        let reviews = registry.action_reviews();
        assert_eq!(reviews.len(), 2);
        assert!(reviews.iter().all(|r| r.review.tool == VERIFIER_TOOL));
        assert_eq!(reviews[0].review.args["name"], "a");
        assert_eq!(reviews[1].review.args["command"], "run b");
        assert!(reviews.iter().all(|r| r.verdict.is_allow()));
    }

    #[tokio::test]
    async fn a_blocked_check_never_reaches_the_runner_and_aborts_the_run() {
        let config = config(&["a", "b"]);
        let runner = Arc::new(Recording::default());
        let registry = gated(runner.clone(), &config, false);
        let error = run_gated(&config, &registry).await.unwrap_err();
        assert!(error.denied);
        assert_eq!(error.check, "a");
        assert!(error.completed.is_empty());
        assert!(runner.0.lock().unwrap().is_empty());
        assert_eq!(registry.action_reviews().len(), 1);
    }

    #[tokio::test]
    async fn a_registry_without_a_gate_refuses_every_check() {
        let config = config(&["a"]);
        let runner = Arc::new(Recording::default());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(CheckTool::new(
            runner.clone(),
            Arc::clone(&config),
        )));
        let error = run_gated(&config, &registry).await.unwrap_err();
        assert!(error.denied, "{error}");
        assert!(runner.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_check_outside_the_identity_scope_is_a_denial() {
        let config = config(&["a"]);
        let registry = ToolRegistry::new().scoped_for(&crate::identity::example());
        let error = run_gated(&config, &registry).await.unwrap_err();
        assert!(error.denied);
        assert_eq!(registry.denial_count(), 1);
    }

    #[tokio::test]
    async fn the_declared_command_runs_whatever_the_arguments_say() {
        let config = config(&["a"]);
        let runner = Arc::new(Recording::default());
        let tool = CheckTool::new(runner.clone(), Arc::clone(&config));
        let value = tool
            .execute(json!({ "name": "a", "command": "rm -rf /" }))
            .await
            .unwrap();
        assert_eq!(*runner.0.lock().unwrap(), ["run a"]);
        assert_eq!(value["status"], "passed");
    }

    #[tokio::test]
    async fn an_undeclared_or_missing_name_is_an_invalid_call() {
        let config = config(&["a"]);
        let runner = Arc::new(Recording::default());
        let tool = CheckTool::new(runner.clone(), config);
        for args in [json!({ "name": "zzz" }), json!({}), json!({ "name": 4 })] {
            let error = tool.execute(args).await.unwrap_err();
            assert!(
                matches!(error, ToolError::InvalidArguments { .. }),
                "{error}"
            );
        }
        assert!(runner.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_launcher_error_and_a_panicking_runner_are_execution_failures() {
        let config = config(&["a"]);
        let failing = ScriptedRunner::new(vec![Err("no runtime".to_string())]);
        let tool = CheckTool::new(Arc::new(failing), Arc::clone(&config));
        let error = tool.execute(json!({ "name": "a" })).await.unwrap_err();
        assert!(
            matches!(&error, ToolError::ExecutionFailed { message } if message == "no runtime")
        );
        let panicking = CheckTool::new(Arc::new(ScriptedRunner::new(vec![])), config);
        let error = panicking.execute(json!({ "name": "a" })).await.unwrap_err();
        assert!(
            matches!(&error, ToolError::ExecutionFailed { message } if message.contains("worker failed")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn run_gated_keeps_completed_outcomes_when_a_later_check_cannot_launch() {
        let config = config(&["a", "b"]);
        let runner = ScriptedRunner::new(vec![raw(Some(0), "", ""), Err("gone".to_string())]);
        let registry = gated(Arc::new(runner), &config, true);
        let error = run_gated(&config, &registry).await.unwrap_err();
        assert!(!error.denied);
        assert_eq!(error.check, "b");
        assert_eq!(error.message, "gone");
        assert_eq!(error.completed.len(), 1);
    }

    #[tokio::test]
    async fn an_unreadable_result_is_an_exec_error_not_a_pass() {
        let config = config(&["a"]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(Unreadable));
        let registry = registry.with_action_gate(gate(true), subject());
        let error = run_gated(&config, &registry).await.unwrap_err();
        assert!(!error.denied);
        assert!(
            error.message.starts_with("unreadable check result"),
            "{error}"
        );
    }

    #[test]
    fn the_tool_is_workspace_class_and_named_for_the_card_tool() {
        let tool = CheckTool::new(Arc::new(Recording::default()), config(&["a"]));
        assert_eq!(tool.effect_class(), EffectClass::Workspace);
        assert_eq!(tool.name(), "run_checks");
        assert_eq!(tool.name(), tool.definition().function.name);
    }

    #[test]
    fn the_definition_names_the_tool_and_requires_a_check_name() {
        let tool = CheckTool::new(Arc::new(Recording::default()), config(&["a"]));
        let definition = tool.definition();
        assert_eq!(definition.function.name, VERIFIER_TOOL);
        assert_eq!(
            definition.function.parameters.required,
            Some(vec!["name".to_string()])
        );
    }
}
