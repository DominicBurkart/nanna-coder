//! Business-logic helpers behind the MCP Tasks surface.
//!
//! The wire dispatch (method routing, `CreateTaskResult` / `tasks/*` framing)
//! lives in [`super`]; this module holds the argument parsing, the delegated
//! work (`assign_task`, `onboard_repo`), and the mappers that turn an internal
//! [`Task`] into its MCP Tasks wire representation.

use crate::onboarding::DeterministicOnboarder;
use crate::onboarding::Onboarder;
use crate::task::{Task, TaskManager, TaskStatus};
use model::provider::ModelProvider;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Suggested client polling interval (milliseconds) advertised in task
/// responses via `pollInterval`.
pub const POLL_INTERVAL_MS: u64 = 2000;

/// Maximum task lifetime (milliseconds) the server will honor; requested
/// `ttl` values are clamped to this. 24 hours.
pub const MAX_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// Parse `assign_task` arguments, submit the task, record its TTL, and return
/// the `CreateTaskResult` task object.
///
/// `ttl_ms` is the client-requested lifetime (from the request's `task`
/// field), clamped to [`MAX_TTL_MS`] here.
pub async fn handle_assign_task(
    params: &Value,
    task_manager: &Arc<TaskManager>,
    provider: &Arc<dyn ModelProvider>,
    default_model: &str,
    default_max_iterations: usize,
    ttl_ms: Option<u64>,
) -> Result<Value, String> {
    let description = params
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field: description".to_string())?
        .to_string();

    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field: repo_path".to_string())?;
    let repo_path = PathBuf::from(repo_path);

    let branch = params
        .get("branch")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD")
        .to_string();

    let model = params
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(default_model)
        .to_string();

    let max_iterations = params
        .get("max_iterations")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(default_max_iterations);

    let ttl = ttl_ms.map(|t| t.min(MAX_TTL_MS));
    let task_id = task_manager
        .submit(
            description,
            repo_path,
            branch,
            model,
            max_iterations,
            Arc::clone(provider),
        )
        .await;

    task_manager.set_ttl(&task_id, ttl).await;

    // The task is freshly created and always in the initial `working` state,
    // so the CreateTaskResult body is built directly rather than re-reading
    // the task (which would introduce an unreachable not-found branch).
    Ok(initial_task_wire(&task_id.0, ttl))
}

/// Build the MCP Tasks `Task` object returned inside a `CreateTaskResult` for a
/// just-submitted task (always initial `working` state).
pub fn initial_task_wire(task_id: &str, ttl_ms: Option<u64>) -> Value {
    let now = chrono::Utc::now().to_rfc3339();
    serde_json::json!({
        "taskId": task_id,
        "status": "working",
        "statusMessage": "Task is queued.",
        "createdAt": now,
        "lastUpdatedAt": now,
        "ttl": ttl_ms,
        "pollInterval": POLL_INTERVAL_MS,
    })
}

/// Run the synchronous `onboard_repo` tool and return its (non-task) result
/// payload. This tool does not support task augmentation.
pub async fn handle_onboard_repo(params: &Value) -> Result<Value, String> {
    let repo_path = params
        .get("repo_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field: repo_path".to_string())?;

    let source = Path::new(repo_path);
    if !source.is_absolute() {
        return Err("repo_path must be an absolute path".to_string());
    }
    let onboarder = DeterministicOnboarder;
    let result = onboarder.onboard(source).await.map_err(|e| e.to_string())?;

    let tools: Vec<Value> = result
        .profile
        .tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "command": t.command,
                "description": t.description,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "project_name": result.profile.project_name,
        "flake_path": result.flake_path.to_string_lossy(),
        "nix_packages": result.profile.nix_packages,
        "tools": tools,
    }))
}

/// Map an internal [`TaskStatus`] to the MCP Tasks wire status string and an
/// optional human-readable `statusMessage`.
fn wire_status(status: &TaskStatus) -> (&'static str, Option<String>) {
    match status {
        TaskStatus::Pending => ("working", Some("Task is queued.".to_string())),
        TaskStatus::Running { iterations, .. } => (
            "working",
            Some(format!("Working ({} iterations completed).", iterations)),
        ),
        TaskStatus::Completed { .. } => ("completed", None),
        TaskStatus::Failed { error, .. } => ("failed", Some(error.clone())),
        TaskStatus::Cancelled { .. } => (
            "cancelled",
            Some("The task was cancelled by request.".to_string()),
        ),
    }
}

/// Build the MCP Tasks `Task` wire object for `tasks/get`, `tasks/list`,
/// `tasks/cancel`, and the `CreateTaskResult` body.
pub fn task_to_wire(task: &Task) -> Value {
    let (status, status_message) = wire_status(&task.status);
    let mut obj = serde_json::json!({
        "taskId": task.id.0,
        "status": status,
        "createdAt": task.created_at.to_rfc3339(),
        "lastUpdatedAt": task.last_updated_at.to_rfc3339(),
        "ttl": task.ttl_ms,
        "pollInterval": POLL_INTERVAL_MS,
    });
    if let Some(msg) = status_message {
        obj["statusMessage"] = serde_json::json!(msg);
    }
    obj
}

/// Build the underlying `CallToolResult` returned by `tasks/result` for a
/// terminal task. Includes the required `io.modelcontextprotocol/related-task`
/// metadata. For non-terminal input the caller is expected to have awaited a
/// terminal state first; a defensive `isError` result is returned regardless.
pub fn task_result_to_call_tool_result(task_id: &str, status: &TaskStatus) -> Value {
    let related = serde_json::json!({
        "io.modelcontextprotocol/related-task": { "taskId": task_id }
    });
    match status {
        TaskStatus::Completed { result, .. } => serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result.to_json()).unwrap_or_default()
            }],
            "isError": false,
            "_meta": related,
        }),
        TaskStatus::Failed {
            error, diagnostics, ..
        } => serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!(
                    "{}\n\n{}",
                    error,
                    serde_json::to_string_pretty(&diagnostics.to_json()).unwrap_or_default()
                )
            }],
            "isError": true,
            "_meta": related,
        }),
        TaskStatus::Cancelled {
            iterations_completed,
            ..
        } => serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("Task was cancelled after {} iterations.", iterations_completed)
            }],
            "isError": true,
            "_meta": related,
        }),
        TaskStatus::Pending | TaskStatus::Running { .. } => serde_json::json!({
            "content": [{ "type": "text", "text": "Task has not reached a terminal state." }],
            "isError": true,
            "_meta": related,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{FailureDiagnostics, TaskId, TaskManager, TaskResult};
    use async_trait::async_trait;
    use chrono::Utc;
    use model::provider::{ModelError, ModelResult};
    use model::types::{
        ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, MessageRole, ModelInfo,
    };
    use std::sync::Mutex;

    struct MockProvider {
        responses: Mutex<Vec<ChatResponse>>,
    }

    impl MockProvider {
        fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses),
            })
        }
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(ModelError::Unknown {
                    message: "No more responses".to_string(),
                });
            }
            Ok(responses.remove(0))
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "mock"
        }
    }

    fn stop_response(content: &str) -> ChatResponse {
        ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: MessageRole::Assistant,
                    content: Some(content.to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: None,
        }
    }

    fn task_with_status(status: TaskStatus) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId("wire-test".to_string()),
            description: "d".to_string(),
            repo_path: PathBuf::from("/tmp"),
            branch: "HEAD".to_string(),
            model: "mock".to_string(),
            status,
            created_at: now,
            last_updated_at: now,
            ttl_ms: Some(60000),
        }
    }

    fn sample_result() -> TaskResult {
        TaskResult {
            result_summary: "did the thing".to_string(),
            changes_patch: Some("diff".to_string()),
            format_patch: None,
            files_modified: vec!["a.rs".to_string()],
            tool_calls_made: vec![],
            iterations: 3,
            model_used: "mock".to_string(),
        }
    }

    fn sample_diagnostics() -> FailureDiagnostics {
        FailureDiagnostics {
            error_type: "MaxIterationsExceeded".to_string(),
            iterations_completed: 5,
            last_tool_call: None,
            partial_changes: None,
            tool_call_history: vec![],
            last_agent_state: None,
            conversation_snapshot: None,
        }
    }

    #[tokio::test]
    async fn test_handle_assign_task_missing_description() {
        let manager = Arc::new(TaskManager::default());
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let params = serde_json::json!({"repo_path": "/tmp"});
        let result =
            handle_assign_task(&params, &manager, &provider, "qwen3:0.6b", 100, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("description"));
    }

    #[tokio::test]
    async fn test_handle_assign_task_missing_repo_path() {
        let manager = Arc::new(TaskManager::default());
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let params = serde_json::json!({"description": "Do something"});
        let result =
            handle_assign_task(&params, &manager, &provider, "qwen3:0.6b", 100, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("repo_path"));
    }

    #[tokio::test]
    async fn test_handle_assign_task_returns_task_id_and_sets_ttl() {
        let manager = Arc::new(TaskManager::default());
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![stop_response("done")]);
        let params = serde_json::json!({
            "description": "Test task",
            "repo_path": "/tmp"
        });
        let wire = handle_assign_task(&params, &manager, &provider, "qwen3:0.6b", 100, Some(5000))
            .await
            .unwrap();
        assert_eq!(wire["status"], "working");
        assert_eq!(wire["ttl"], 5000);
        // The TTL is also persisted so tasks/get echoes it.
        let tid = TaskId(wire["taskId"].as_str().unwrap().to_string());
        assert_eq!(manager.poll(&tid).await.unwrap().ttl_ms, Some(5000));
    }

    #[tokio::test]
    async fn test_handle_assign_task_clamps_ttl() {
        let manager = Arc::new(TaskManager::default());
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![stop_response("done")]);
        let params = serde_json::json!({"description": "t", "repo_path": "/tmp"});
        let wire = handle_assign_task(
            &params,
            &manager,
            &provider,
            "qwen3:0.6b",
            100,
            Some(u64::MAX),
        )
        .await
        .unwrap();
        assert_eq!(wire["ttl"], MAX_TTL_MS);
    }

    #[tokio::test]
    async fn test_handle_onboard_repo_success() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let params = serde_json::json!({ "repo_path": dir.path().to_str().unwrap() });
        let result = handle_onboard_repo(&params).await.unwrap();
        assert_eq!(result["project_name"], "demo");
        assert!(result["flake_path"]
            .as_str()
            .unwrap()
            .ends_with("flake.nix"));
        assert!(result["nix_packages"].is_array());
        assert!(result["tools"].is_array());
    }

    #[tokio::test]
    async fn test_handle_onboard_repo_rejects_relative_path() {
        let params = serde_json::json!({"repo_path": "relative/path"});
        let result = handle_onboard_repo(&params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("absolute"));
    }

    #[tokio::test]
    async fn test_handle_onboard_repo_missing_path() {
        let params = serde_json::json!({});
        let result = handle_onboard_repo(&params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("repo_path"));
    }

    #[test]
    fn test_task_to_wire_working_for_pending_and_running() {
        let w = task_to_wire(&task_with_status(TaskStatus::Pending));
        assert_eq!(w["status"], "working");
        assert_eq!(w["taskId"], "wire-test");
        assert_eq!(w["ttl"], 60000);
        assert_eq!(w["pollInterval"], POLL_INTERVAL_MS);
        assert!(w["createdAt"].is_string());
        assert!(w["lastUpdatedAt"].is_string());
        assert!(w["statusMessage"].is_string());

        let running = task_to_wire(&task_with_status(TaskStatus::Running {
            started_at: Utc::now(),
            iterations: 2,
        }));
        assert_eq!(running["status"], "working");
        assert!(running["statusMessage"]
            .as_str()
            .unwrap()
            .contains("2 iterations"));
    }

    #[test]
    fn test_task_to_wire_terminal_statuses() {
        let completed = task_to_wire(&task_with_status(TaskStatus::Completed {
            finished_at: Utc::now(),
            result: sample_result(),
        }));
        assert_eq!(completed["status"], "completed");
        assert!(completed.get("statusMessage").is_none());

        let failed = task_to_wire(&task_with_status(TaskStatus::Failed {
            finished_at: Utc::now(),
            error: "boom".to_string(),
            diagnostics: sample_diagnostics(),
        }));
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["statusMessage"], "boom");

        let cancelled = task_to_wire(&task_with_status(TaskStatus::Cancelled {
            finished_at: Utc::now(),
            iterations_completed: 1,
        }));
        assert_eq!(cancelled["status"], "cancelled");
    }

    #[test]
    fn test_task_result_completed_is_not_error() {
        let r = task_result_to_call_tool_result(
            "wire-test",
            &TaskStatus::Completed {
                finished_at: Utc::now(),
                result: sample_result(),
            },
        );
        assert_eq!(r["isError"], false);
        assert_eq!(
            r["_meta"]["io.modelcontextprotocol/related-task"]["taskId"],
            "wire-test"
        );
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("did the thing"));
    }

    #[test]
    fn test_task_result_failed_is_error() {
        let r = task_result_to_call_tool_result(
            "wire-test",
            &TaskStatus::Failed {
                finished_at: Utc::now(),
                error: "boom".to_string(),
                diagnostics: sample_diagnostics(),
            },
        );
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("boom"));
    }

    #[test]
    fn test_task_result_cancelled_is_error() {
        let r = task_result_to_call_tool_result(
            "wire-test",
            &TaskStatus::Cancelled {
                finished_at: Utc::now(),
                iterations_completed: 4,
            },
        );
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cancelled after 4"));
    }

    #[test]
    fn test_task_result_non_terminal_is_defensive_error() {
        let r = task_result_to_call_tool_result("wire-test", &TaskStatus::Pending);
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("terminal"));
    }
}
