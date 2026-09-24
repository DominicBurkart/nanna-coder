//! Agent tools over [`AppContext`]: `app_start`, `app_stop`, `app_logs`.

use super::{AppContext, AppError};
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Name of the tool that builds and starts the application.
pub const APP_START_TOOL: &str = "app_start";
/// Name of the tool that stops the application.
pub const APP_STOP_TOOL: &str = "app_stop";
/// Name of the tool that tails the application log.
pub const APP_LOGS_TOOL: &str = "app_logs";
/// Lines `app_logs` returns when `tail` is omitted.
pub const DEFAULT_LOG_TAIL: usize = 50;

fn tool_error(e: AppError) -> ToolError {
    ToolError::ExecutionFailed {
        message: e.to_string(),
    }
}

fn definition(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        function: FunctionDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters: JsonSchema {
                schema_type: SchemaType::Object,
                properties: None,
                required: None,
            },
        },
    }
}

/// `app_start`: build the frontend with trunk, build the API, start it on a
/// per-task port inside the dev container and wait for its health endpoint.
/// Takes no arguments. Returns `{ task_id, base_url, api_url, frontend_url,
/// pid, log_path, port }`; a second call returns the running instance.
pub struct AppStartTool {
    ctx: AppContext,
}

impl AppStartTool {
    pub fn new(ctx: AppContext) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for AppStartTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            APP_START_TOOL,
            "Build the frontend and the API, start the API inside the dev container on a per-task port and wait for its health endpoint. Idempotent: returns the running instance. Result: { task_id, base_url, api_url, frontend_url, pid, log_path, port }.",
        )
    }

    async fn execute(&self, _args: Value) -> ToolResult<Value> {
        let instance = self.ctx.start().await.map_err(tool_error)?;
        Ok(instance.to_json())
    }

    fn name(&self) -> &str {
        APP_START_TOOL
    }
}

/// `app_stop`: kill the task's application inside the dev container and
/// release its port. Takes no arguments. Returns `{ task_id, stopped }`
/// plus `{ pid, port, killed }` when something was running; `killed` is
/// `false` when the process had already exited. Idempotent.
pub struct AppStopTool {
    ctx: AppContext,
}

impl AppStopTool {
    pub fn new(ctx: AppContext) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for AppStopTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            APP_STOP_TOOL,
            "Stop the application started by app_start and release its port. Idempotent. Result: { task_id, stopped, pid?, port?, killed? }.",
        )
    }

    async fn execute(&self, _args: Value) -> ToolResult<Value> {
        let task_id = &self.ctx.task_id;
        match self.ctx.stop().map_err(tool_error)? {
            Some(stopped) => Ok(json!({
                "task_id": task_id,
                "stopped": true,
                "pid": stopped.instance.pid,
                "port": stopped.instance.port,
                "killed": stopped.killed,
            })),
            None => Ok(json!({ "task_id": task_id, "stopped": false })),
        }
    }

    fn name(&self) -> &str {
        APP_STOP_TOOL
    }
}

/// `app_logs`: the last `tail` lines (default [`DEFAULT_LOG_TAIL`]) of the
/// application log. Argument `{ "tail": n }` as a number or numeric string,
/// at least 1. Returns `{ task_id, log_path, tail, lines }`.
pub struct AppLogsTool {
    ctx: AppContext,
}

impl AppLogsTool {
    pub fn new(ctx: AppContext) -> Self {
        Self { ctx }
    }
}

/// The `tail` argument: a positive integer, given as a number or a string.
pub fn parse_tail(args: &Value) -> ToolResult<usize> {
    let raw = args.get("tail");
    let tail = match raw {
        None | Some(Value::Null) => Some(DEFAULT_LOG_TAIL),
        Some(Value::Number(n)) => n.as_u64().map(|n| n as usize),
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(_) => None,
    };
    match tail {
        Some(n) if n >= 1 => Ok(n),
        _ => Err(ToolError::InvalidArguments {
            message: format!("'tail' must be a positive integer, got {raw:?}"),
        }),
    }
}

#[async_trait]
impl Tool for AppLogsTool {
    fn definition(&self) -> ToolDefinition {
        let mut def = definition(
            APP_LOGS_TOOL,
            "Return the last lines of the application log written by app_start. Result: { task_id, log_path, tail, lines }.",
        );
        let mut props = HashMap::new();
        props.insert(
            "tail".to_string(),
            PropertySchema {
                schema_type: SchemaType::Integer,
                description: Some(format!(
                    "Number of trailing lines to return (default {DEFAULT_LOG_TAIL})"
                )),
                items: None,
            },
        );
        def.function.parameters.properties = Some(props);
        def
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let tail = parse_tail(&args)?;
        let lines = self.ctx.logs(tail).map_err(tool_error)?;
        Ok(json!({
            "task_id": self.ctx.task_id,
            "log_path": super::app_log_path(&self.ctx.task_id),
            "tail": tail,
            "lines": lines,
        }))
    }

    fn name(&self) -> &str {
        APP_LOGS_TOOL
    }
}

/// Register the app tools for one task on `registry`.
pub fn register_app_tools(registry: &mut ToolRegistry, ctx: AppContext) {
    registry.register(Box::new(AppStartTool::new(ctx.clone())));
    registry.register(Box::new(AppStopTool::new(ctx.clone())));
    registry.register(Box::new(AppLogsTool::new(ctx)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprun::{AppSpec, Limits, PortAllocator, RunningApps};
    use crate::container::{ContainerHandle, ContainerRuntime};
    use crate::sidecar::{CommandRunner, RunOutput};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    const BUILD_JSON: &str = r#"{"reason":"compiler-artifact","executable":"/t/debug/api"}"#;

    struct Scripted {
        healthy: bool,
    }

    impl CommandRunner for Scripted {
        fn run(&self, _: &str, args: &[String]) -> std::io::Result<RunOutput> {
            let last = args.last().map(String::as_str).unwrap_or("");
            let (success, stdout) = if args.iter().any(|a| a == "cargo") {
                (true, BUILD_JSON)
            } else if last.contains("nohup") {
                (true, "31\n")
            } else if args.iter().any(|a| a == "curl") {
                (self.healthy, "")
            } else if args.iter().any(|a| a == "tail") {
                (true, "a\nb\nc\n")
            } else {
                (true, "")
            };
            Ok(RunOutput {
                success,
                stdout: stdout.to_string(),
                stderr: "boom".to_string(),
            })
        }
    }

    fn context(healthy: bool) -> (AppContext, TempDir) {
        let dir = TempDir::new().unwrap();
        let ctx = AppContext {
            task_id: "tool-task".to_string(),
            handle: Arc::new(ContainerHandle {
                name: "c".to_string(),
                runtime: ContainerRuntime::Podman,
                port: None,
                needs_cleanup: false,
            }),
            runner: Arc::new(Scripted { healthy }),
            apps: Arc::new(RunningApps::new()),
            ports: Arc::new(PortAllocator::new(42000..=42000, dir.path())),
            spec: AppSpec {
                api_package: "api".to_string(),
                workspace_dir: "/workspace".to_string(),
                frontend_dir: "/workspace/ui".to_string(),
                frontend_dist: "/workspace/ui/dist".to_string(),
                health_path: "/health".to_string(),
            },
            env: vec![],
            limits: Limits {
                max_wall_clock_secs: 1,
            },
            poll_interval: Duration::from_millis(1),
        };
        (ctx, dir)
    }

    #[tokio::test]
    async fn app_start_returns_the_instance_shape() {
        let (ctx, _dir) = context(true);
        let mut registry = ToolRegistry::new();
        register_app_tools(&mut registry, ctx.clone());
        let tool = registry.get_tool(APP_START_TOOL).unwrap();
        assert_eq!(tool.name(), "app_start");
        let def = tool.definition();
        assert_eq!(def.function.name, "app_start");
        assert!(def.function.description.contains("base_url"));
        let result = registry.execute(APP_START_TOOL, Value::Null).await.unwrap();
        assert_eq!(result["task_id"], "tool-task");
        assert_eq!(result["base_url"], "http://127.0.0.1:42000");
        assert_eq!(result["api_url"], "http://127.0.0.1:42000");
        assert_eq!(result["frontend_url"], "http://127.0.0.1:42000");
        assert_eq!(result["pid"], 31);
        assert_eq!(result["log_path"], "/tmp/nanna-app-tool-task.log");
        assert_eq!(result["port"], 42000);
        let again = registry.execute(APP_START_TOOL, Value::Null).await.unwrap();
        assert_eq!(again, result);
        assert_eq!(ctx.apps.len(), 1);
    }

    #[tokio::test]
    async fn app_stop_after_app_start_then_idempotent() {
        let (ctx, _dir) = context(true);
        let mut registry = ToolRegistry::new();
        register_app_tools(&mut registry, ctx.clone());
        assert!(registry.get_tool(APP_STOP_TOOL).is_some());
        assert!(registry.get_tool(APP_LOGS_TOOL).is_some());
        let stopped = registry.execute(APP_STOP_TOOL, Value::Null).await.unwrap();
        assert_eq!(stopped, json!({ "task_id": "tool-task", "stopped": false }));
        registry.execute(APP_START_TOOL, Value::Null).await.unwrap();
        let stopped = registry.execute(APP_STOP_TOOL, Value::Null).await.unwrap();
        assert_eq!(stopped["stopped"], true);
        assert_eq!(stopped["pid"], 31);
        assert_eq!(stopped["port"], 42000);
        assert_eq!(stopped["killed"], true);
        assert!(ctx.apps.is_empty());
        assert!(ctx.ports.held().is_empty());
        let again = registry.execute(APP_STOP_TOOL, Value::Null).await.unwrap();
        assert_eq!(again["stopped"], false);
    }

    #[tokio::test]
    async fn app_stop_spawn_failure_is_an_execution_error() {
        struct Broken;
        impl CommandRunner for Broken {
            fn run(&self, _: &str, _: &[String]) -> std::io::Result<RunOutput> {
                Err(std::io::Error::other("no runtime"))
            }
        }
        let (mut ctx, _dir) = context(true);
        let lease = ctx.ports.allocate("tool-task").unwrap();
        ctx.apps.insert(
            crate::apprun::AppInstance::local("tool-task", lease.port(), 5, "/l"),
            lease,
        );
        ctx.runner = Arc::new(Broken);
        let err = AppStopTool::new(ctx.clone())
            .execute(Value::Null)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { ref message } if message.contains("no runtime")),
            "{err}"
        );
        assert_eq!(ctx.apps.len(), 1, "a failed stop keeps the entry");
    }

    #[tokio::test]
    async fn app_logs_returns_lines_and_validates_tail() {
        let (ctx, _dir) = context(true);
        let tool = AppLogsTool::new(ctx);
        assert_eq!(tool.name(), "app_logs");
        let def = tool.definition();
        assert!(def
            .function
            .parameters
            .properties
            .unwrap()
            .contains_key("tail"));
        let result = tool.execute(json!({ "tail": 2 })).await.unwrap();
        assert_eq!(result["task_id"], "tool-task");
        assert_eq!(result["log_path"], "/tmp/nanna-app-tool-task.log");
        assert_eq!(result["tail"], 2);
        assert_eq!(result["lines"], json!(["a", "b", "c"]));
        assert_eq!(
            tool.execute(json!({ "tail": "7" })).await.unwrap()["tail"],
            7
        );
        assert_eq!(
            tool.execute(Value::Null).await.unwrap()["tail"],
            DEFAULT_LOG_TAIL
        );
        assert_eq!(
            tool.execute(json!({ "tail": null })).await.unwrap()["tail"],
            DEFAULT_LOG_TAIL
        );
        for bad in [
            json!({ "tail": 0 }),
            json!({ "tail": -1 }),
            json!({ "tail": "x" }),
            json!({ "tail": true }),
        ] {
            let err = tool.execute(bad).await.unwrap_err();
            assert!(matches!(err, ToolError::InvalidArguments { .. }), "{err}");
        }
    }

    #[tokio::test]
    async fn app_logs_failure_is_an_execution_error() {
        struct NoLog;
        impl CommandRunner for NoLog {
            fn run(&self, _: &str, _: &[String]) -> std::io::Result<RunOutput> {
                Ok(RunOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: "No such file".to_string(),
                })
            }
        }
        let (mut ctx, _dir) = context(true);
        ctx.runner = Arc::new(NoLog);
        let err = AppLogsTool::new(ctx)
            .execute(Value::Null)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { ref message } if message.contains("No such file")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn app_start_failure_is_an_execution_error() {
        let (ctx, _dir) = context(false);
        let err = AppStartTool::new(ctx)
            .execute(Value::Null)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { ref message } if message.contains("not healthy")),
            "{err}"
        );
    }
}
