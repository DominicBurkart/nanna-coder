use crate::onboarding::DeterministicOnboarder;
use crate::onboarding::Onboarder;
use crate::task::{TaskId, TaskManager, TaskStatus};
use model::provider::ModelProvider;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub async fn handle_list_tasks(task_manager: &Arc<TaskManager>) -> Result<Value, String> {
    let tasks = task_manager.list().await;
    let summaries: Vec<Value> = tasks
        .into_iter()
        .map(|t| {
            let status_str = match &t.status {
                TaskStatus::Pending => "Pending",
                TaskStatus::Running { .. } => "Running",
                TaskStatus::Completed { .. } => "Completed",
                TaskStatus::Failed { .. } => "Failed",
            };
            serde_json::json!({
                "id": t.id.0,
                "status": status_str,
                "description": t.description,
                "created_at": t.created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(serde_json::json!(summaries))
}

pub async fn handle_cancel_task(
    params: &Value,
    task_manager: &Arc<TaskManager>,
) -> Result<Value, String> {
    let task_id_str = params
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field: task_id".to_string())?;

    let task_id = TaskId(task_id_str.to_string());
    task_manager.cancel(&task_id).await?;

    Ok(serde_json::json!({
        "task_id": task_id_str,
        "status": "Cancelled",
        "message": "Task has been cancelled"
    }))
}

pub async fn handle_assign_task(
    params: &Value,
    task_manager: &Arc<TaskManager>,
    provider: &Arc<dyn ModelProvider>,
    default_model: &str,
    default_max_iterations: usize,
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

    Ok(serde_json::json!({
        "task_id": task_id.0,
        "status": "Pending"
    }))
}

pub async fn handle_poll_task(
    params: &Value,
    task_manager: &Arc<TaskManager>,
) -> Result<Value, String> {
    let task_id_str = params
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field: task_id".to_string())?;

    let task_id = TaskId(task_id_str.to_string());
    let task = task_manager
        .poll(&task_id)
        .await
        .ok_or_else(|| format!("Task not found: {}", task_id_str))?;

    let (status_str, iterations, started_at) = match &task.status {
        TaskStatus::Pending => ("Pending", None, None),
        TaskStatus::Running {
            iterations,
            started_at,
        } => ("Running", Some(*iterations), Some(started_at.to_rfc3339())),
        TaskStatus::Completed { .. } => ("Completed", None, None),
        TaskStatus::Failed { .. } => ("Failed", None, None),
    };

    let mut response = serde_json::json!({
        "task_id": task_id_str,
        "status": status_str,
        "description": task.description,
    });

    if let Some(iters) = iterations {
        response["iterations"] = serde_json::json!(iters);
    }
    if let Some(started) = started_at {
        response["started_at"] = serde_json::json!(started);
    }

    Ok(response)
}

pub async fn handle_get_result(
    params: &Value,
    task_manager: &Arc<TaskManager>,
) -> Result<Value, String> {
    let task_id_str = params
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required field: task_id".to_string())?;

    let task_id = TaskId(task_id_str.to_string());
    let task = task_manager
        .poll(&task_id)
        .await
        .ok_or_else(|| format!("Task not found: {}", task_id_str))?;

    match task.status {
        TaskStatus::Completed {
            result,
            finished_at,
        } => Ok(serde_json::json!({
            "task_id": task_id_str,
            "status": "Completed",
            "finished_at": finished_at.to_rfc3339(),
            "result_summary": result.result_summary,
            "changes_patch": result.changes_patch,
            "files_modified": result.files_modified,
            "iterations": result.iterations,
            "model_used": result.model_used,
        })),
        TaskStatus::Failed {
            error,
            diagnostics,
            finished_at,
        } => Ok(serde_json::json!({
            "task_id": task_id_str,
            "status": "Failed",
            "finished_at": finished_at.to_rfc3339(),
            "error": error,
            "diagnostics": diagnostics.to_json(),
        })),
        TaskStatus::Pending => Err(format!("Task {} is still pending", task_id_str)),
        TaskStatus::Running { .. } => Err(format!("Task {} is still running", task_id_str)),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskManager;
    use async_trait::async_trait;
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

    #[tokio::test]
    async fn test_handle_assign_task_missing_description() {
        let manager = Arc::new(TaskManager::default());
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let params = serde_json::json!({"repo_path": "/tmp"});
        let result = handle_assign_task(&params, &manager, &provider, "qwen3:0.6b", 100).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("description"));
    }

    #[tokio::test]
    async fn test_handle_assign_task_missing_repo_path() {
        let manager = Arc::new(TaskManager::default());
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let params = serde_json::json!({"description": "Do something"});
        let result = handle_assign_task(&params, &manager, &provider, "qwen3:0.6b", 100).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("repo_path"));
    }

    #[tokio::test]
    async fn test_handle_assign_task_returns_task_id() {
        let manager = Arc::new(TaskManager::default());
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![stop_response("done")]);
        let params = serde_json::json!({
            "description": "Test task",
            "repo_path": "/tmp"
        });
        let result = handle_assign_task(&params, &manager, &provider, "qwen3:0.6b", 100).await;
        assert!(result.is_ok());
        let val = result.unwrap();
        assert!(val["task_id"].is_string());
        assert_eq!(val["status"], "Pending");
    }

    #[tokio::test]
    async fn test_handle_poll_task_invalid_id() {
        let manager = Arc::new(TaskManager::default());
        let params = serde_json::json!({"task_id": "nonexistent-id"});
        let result = handle_poll_task(&params, &manager).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handle_get_result_invalid_id() {
        let manager = Arc::new(TaskManager::default());
        let params = serde_json::json!({"task_id": "nonexistent-id"});
        let result = handle_get_result(&params, &manager).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handle_poll_task_missing_task_id() {
        let manager = Arc::new(TaskManager::default());
        let params = serde_json::json!({});
        let result = handle_poll_task(&params, &manager).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("task_id"));
    }

    #[tokio::test]
    async fn test_handle_list_tasks_empty() {
        let manager = Arc::new(TaskManager::default());
        let result = handle_list_tasks(&manager).await;
        assert!(result.is_ok());
        assert!(result.unwrap().as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_handle_list_tasks_after_submit() {
        let manager = Arc::new(TaskManager::default());
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let params = serde_json::json!({
            "description": "Test task",
            "repo_path": "/tmp"
        });
        handle_assign_task(&params, &manager, &provider, "qwen3:0.6b", 100)
            .await
            .unwrap();

        let result = handle_list_tasks(&manager).await.unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["description"], "Test task");
    }

    #[tokio::test]
    async fn test_handle_cancel_task_missing_task_id() {
        let manager = Arc::new(TaskManager::default());
        let params = serde_json::json!({});
        let result = handle_cancel_task(&params, &manager).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("task_id"));
    }

    #[tokio::test]
    async fn test_handle_cancel_task_nonexistent() {
        let manager = Arc::new(TaskManager::default());
        let params = serde_json::json!({"task_id": "nonexistent"});
        let result = handle_cancel_task(&params, &manager).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handle_onboard_repo_rejects_relative_path() {
        let params = serde_json::json!({"repo_path": "relative/path"});
        let result = handle_onboard_repo(&params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("absolute"));
    }

    // ── MCP JSON-contract invariants ────────────────────────────────────────
    //
    // The handlers in this module form the boundary between Nanna and any
    // external MCP orchestrator. The tests below pin three invariants that
    // orchestrators rely on (see `ARCHITECTURE.md` § API):
    //
    //   1. `handle_get_result` must distinguish `Pending` and `Running` from
    //      "task not found". Returning the wrong shape would break
    //      orchestrators that interleave `poll_task` and `get_result` per
    //      the documented Delegation Sequence.
    //   2. `handle_assign_task` must populate the persisted `Task` from
    //      caller-supplied params *and* default sensibly when optional
    //      params (`branch`, `model`, `max_iterations`) are omitted. The
    //      ARCHITECTURE.md description says a task is dispatched against
    //      "a designated repo" — orchestrators that omit `branch` expect
    //      "HEAD" without specifying it.
    //   3. `handle_onboard_repo` must reject malformed/missing input with
    //      a textual error that names the offending field, so that an
    //      orchestrator can produce an actionable error to its own user
    //      without inspecting JSON-RPC error codes alone.

    #[tokio::test]
    async fn test_handle_get_result_pending_returns_pending_error() {
        // Use TaskManager::new(0) so the submitted task can never start
        // (semaphore has zero permits) and stays in Pending.
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let assign_params = serde_json::json!({
            "description": "pending forever",
            "repo_path": "/tmp"
        });
        let assigned = handle_assign_task(&assign_params, &manager, &provider, "qwen3:0.6b", 100)
            .await
            .unwrap();
        let task_id = assigned["task_id"].as_str().unwrap().to_string();

        // Yield once but not long enough for the task loop to start
        // (it's blocked on the semaphore anyway).
        tokio::task::yield_now().await;

        let params = serde_json::json!({"task_id": task_id});
        let result = handle_get_result(&params, &manager).await;
        assert!(
            result.is_err(),
            "get_result on Pending task must be Err: got {:?}",
            result
        );
        let msg = result.unwrap_err();
        assert!(
            msg.contains("still pending"),
            "Pending error must say 'still pending', got: {}",
            msg
        );
        assert!(
            msg.contains(&task_id),
            "Pending error must include the task id, got: {}",
            msg
        );
    }

    #[tokio::test]
    async fn test_handle_assign_task_defaults_flow_through() {
        // Caller omits branch / model / max_iterations. The resulting Task
        // must use the supplied defaults verbatim. This test exercises the
        // `unwrap_or(default_model)` / `unwrap_or("HEAD")` /
        // `unwrap_or(default_max_iterations)` branches in handle_assign_task,
        // none of which are covered by the existing happy-path test
        // (which only asserts the response shape).
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let params = serde_json::json!({
            "description": "minimal",
            "repo_path": "/tmp/some-repo"
        });
        let assigned = handle_assign_task(&params, &manager, &provider, "default-model:1b", 42)
            .await
            .unwrap();
        let task_id = TaskId(assigned["task_id"].as_str().unwrap().to_string());

        // Inspect the persisted Task via poll(). Going through the public
        // API confirms the defaults reach storage, not just submit()'s
        // signature.
        let task = manager
            .poll(&task_id)
            .await
            .expect("submitted task must be retrievable");
        assert_eq!(task.branch, "HEAD", "branch must default to HEAD");
        assert_eq!(
            task.model, "default-model:1b",
            "model must default to handler's default_model"
        );
        // The list endpoint should also surface the description verbatim.
        let list = handle_list_tasks(&manager).await.unwrap();
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["description"], "minimal");
        assert_eq!(arr[0]["status"], "Pending");
    }

    #[tokio::test]
    async fn test_handle_assign_task_branch_override_flows_through() {
        // Explicit branch must override the "HEAD" default and reach the
        // persisted Task.
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let params = serde_json::json!({
            "description": "with branch",
            "repo_path": "/tmp/r",
            "branch": "feature/x"
        });
        let assigned = handle_assign_task(&params, &manager, &provider, "qwen3:0.6b", 1)
            .await
            .unwrap();
        let task_id = TaskId(assigned["task_id"].as_str().unwrap().to_string());
        let task = manager.poll(&task_id).await.unwrap();
        assert_eq!(task.branch, "feature/x");
    }

    #[tokio::test]
    async fn test_handle_poll_task_pending_status_string() {
        // The status string is part of the JSON contract. Pin its exact
        // spelling so a future rename of `TaskStatus::Pending` doesn't
        // silently break orchestrators reading `status` as a discriminator.
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let assigned = handle_assign_task(
            &serde_json::json!({"description": "p", "repo_path": "/tmp"}),
            &manager,
            &provider,
            "qwen3:0.6b",
            1,
        )
        .await
        .unwrap();
        let task_id = assigned["task_id"].as_str().unwrap().to_string();
        let resp = handle_poll_task(&serde_json::json!({"task_id": task_id}), &manager)
            .await
            .unwrap();
        assert_eq!(resp["status"], "Pending");
        // Pending must NOT carry an `iterations` or `started_at` field.
        assert!(
            resp.get("iterations").is_none(),
            "Pending response must omit iterations: {resp}"
        );
        assert!(
            resp.get("started_at").is_none(),
            "Pending response must omit started_at: {resp}"
        );
    }

    #[tokio::test]
    async fn test_handle_onboard_repo_missing_repo_path_field() {
        // No `repo_path` at all (different from the existing test, which
        // sends a relative path). The error must name the missing field
        // so orchestrators can route it back to the user.
        let params = serde_json::json!({});
        let result = handle_onboard_repo(&params).await;
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("repo_path"),
            "missing-field error must name 'repo_path', got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_handle_cancel_task_returns_cancelled_status_string() {
        // Pin the exact response shape: cancel() must return JSON with
        // `status: "Cancelled"` so orchestrators can treat the response
        // as terminal without re-polling.
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let assigned = handle_assign_task(
            &serde_json::json!({"description": "c", "repo_path": "/tmp"}),
            &manager,
            &provider,
            "qwen3:0.6b",
            1,
        )
        .await
        .unwrap();
        let task_id = assigned["task_id"].as_str().unwrap().to_string();
        // Yield so the spawned task loop reaches its first await point
        // (it will be blocked on the semaphore, which is fine — cancel()
        // works against Pending tasks too).
        tokio::task::yield_now().await;

        let resp = handle_cancel_task(&serde_json::json!({"task_id": &task_id}), &manager)
            .await
            .unwrap();
        assert_eq!(resp["status"], "Cancelled");
        assert_eq!(resp["task_id"], task_id);
        assert!(
            resp["message"].as_str().unwrap().contains("cancelled"),
            "cancel response must include human-readable confirmation: {resp}"
        );
    }
}
