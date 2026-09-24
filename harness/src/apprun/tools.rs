//! Agent tools over [`AppContext`]: `app_start`, `app_stop`, `app_logs`.

use super::{AppContext, AppError};
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, SchemaType, ToolDefinition};
use serde_json::Value;

/// Name of the tool that builds and starts the application.
pub const APP_START_TOOL: &str = "app_start";

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

/// Register the app tools for one task on `registry`.
pub fn register_app_tools(registry: &mut ToolRegistry, ctx: AppContext) {
    registry.register(Box::new(AppStartTool::new(ctx)));
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
