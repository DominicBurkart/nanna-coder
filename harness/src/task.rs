use crate::agent::{AgentConfig, AgentContext, AgentError, AgentLoop};
use crate::entities::context::types::ToolCallRecord;
use crate::entities::InMemoryEntityStore;
use crate::workspace::TaskWorkspace;
use chrono::{DateTime, Utc};
use model::provider::ModelProvider;
use model::types::ChatMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

const MAX_DIFF_BYTES: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub result_summary: String,
    pub changes_patch: Option<String>,
    pub files_modified: Vec<String>,
    pub tool_calls_made: Vec<ToolCallRecord>,
    pub iterations: usize,
    pub model_used: String,
}

impl TaskResult {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "result_summary": self.result_summary,
            "changes_patch": self.changes_patch,
            "files_modified": self.files_modified,
            "tool_calls_made": self.tool_calls_made,
            "iterations": self.iterations,
            "model_used": self.model_used,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureDiagnostics {
    pub error_type: String,
    pub iterations_completed: usize,
    pub last_tool_call: Option<ToolCallRecord>,
    pub partial_changes: Option<String>,
}

impl FailureDiagnostics {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error_type": self.error_type,
            "iterations_completed": self.iterations_completed,
            "last_tool_call": self.last_tool_call,
            "partial_changes": self.partial_changes,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum TaskStatus {
    Pending,
    Running {
        started_at: DateTime<Utc>,
        iterations: usize,
    },
    Completed {
        finished_at: DateTime<Utc>,
        result: TaskResult,
    },
    Failed {
        finished_at: DateTime<Utc>,
        error: String,
        diagnostics: FailureDiagnostics,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub description: String,
    pub repo_path: PathBuf,
    pub branch: String,
    pub model: String,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
}

pub struct TaskManager {
    tasks: Arc<RwLock<HashMap<TaskId, Task>>>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn submit(
        &self,
        description: String,
        repo_path: PathBuf,
        branch: String,
        model: String,
        max_iterations: usize,
        provider: Arc<dyn ModelProvider>,
    ) -> TaskId {
        let task_id = TaskId::new();
        let task = Task {
            id: task_id.clone(),
            description: description.clone(),
            repo_path: repo_path.clone(),
            branch: branch.clone(),
            model: model.clone(),
            status: TaskStatus::Pending,
            created_at: Utc::now(),
        };
        {
            let mut tasks = self.tasks.write().await;
            tasks.insert(task_id.clone(), task);
        }

        let tasks_ref = Arc::clone(&self.tasks);
        let task_id_clone = task_id.clone();
        tokio::spawn(async move {
            {
                let mut tasks = tasks_ref.write().await;
                if let Some(task) = tasks.get_mut(&task_id_clone) {
                    task.status = TaskStatus::Running {
                        started_at: Utc::now(),
                        iterations: 0,
                    };
                }
            }

            let workspace_result = TaskWorkspace::create(&repo_path, &task_id_clone.0, &branch);
            match workspace_result {
                Err(e) => {
                    let mut tasks = tasks_ref.write().await;
                    if let Some(task) = tasks.get_mut(&task_id_clone) {
                        task.status = TaskStatus::Failed {
                            finished_at: Utc::now(),
                            error: e.to_string(),
                            diagnostics: FailureDiagnostics {
                                error_type: "WorkspaceCreationFailed".to_string(),
                                iterations_completed: 0,
                                last_tool_call: None,
                                partial_changes: None,
                            },
                        };
                    }
                }
                Ok(mut workspace) => {
                    let tool_registry = workspace.create_tool_registry();
                    let entity_store = InMemoryEntityStore::new();
                    let agent_config = AgentConfig {
                        max_iterations,
                        verbose: false,
                        system_prompt: "You are a helpful coding assistant. Use the available tools to accomplish tasks. When you have completed the task, respond with a summary.".to_string(),
                        model_name: model.clone(),
                    };
                    let context = AgentContext {
                        user_prompt: description.clone(),
                        conversation_history: vec![ChatMessage::user(&description)],
                        app_state_id: task_id_clone.0.clone(),
                    };

                    let mut agent =
                        AgentLoop::with_tools(agent_config, entity_store, provider, tool_registry);
                    let run_result = agent.run(context).await;

                    let changes_patch = workspace.extract_changes().ok().and_then(|patch| {
                        if patch.is_empty() {
                            None
                        } else if patch.len() > MAX_DIFF_BYTES {
                            Some(patch[..MAX_DIFF_BYTES].to_string())
                        } else {
                            Some(patch)
                        }
                    });

                    let _ = workspace.cleanup();

                    match run_result {
                        Ok(result) => {
                            let files_modified = parse_modified_files(changes_patch.as_deref());
                            let task_result = TaskResult {
                                result_summary: result.result_summary,
                                changes_patch,
                                files_modified,
                                tool_calls_made: result.tool_calls_made,
                                iterations: result.iterations,
                                model_used: model,
                            };
                            let mut tasks = tasks_ref.write().await;
                            if let Some(task) = tasks.get_mut(&task_id_clone) {
                                task.status = TaskStatus::Completed {
                                    finished_at: Utc::now(),
                                    result: task_result,
                                };
                            }
                        }
                        Err(e) => {
                            let partial_changes = changes_patch;
                            let (error_type, iterations_completed) = match &e {
                                AgentError::MaxIterationsExceeded { iterations } => {
                                    ("MaxIterationsExceeded".to_string(), *iterations)
                                }
                                AgentError::StateError(_) => ("StateError".to_string(), 0),
                                AgentError::TaskCheckFailed(_) => {
                                    ("TaskCheckFailed".to_string(), 0)
                                }
                            };
                            let diagnostics = FailureDiagnostics {
                                error_type,
                                iterations_completed,
                                last_tool_call: None,
                                partial_changes,
                            };
                            let mut tasks = tasks_ref.write().await;
                            if let Some(task) = tasks.get_mut(&task_id_clone) {
                                task.status = TaskStatus::Failed {
                                    finished_at: Utc::now(),
                                    error: e.to_string(),
                                    diagnostics,
                                };
                            }
                        }
                    }
                }
            }
        });

        task_id
    }

    pub async fn poll(&self, task_id: &TaskId) -> Option<Task> {
        let tasks = self.tasks.read().await;
        tasks.get(task_id).cloned()
    }

    pub async fn get_result(&self, task_id: &TaskId) -> Option<TaskResult> {
        let tasks = self.tasks.read().await;
        tasks.get(task_id).and_then(|t| {
            if let TaskStatus::Completed { result, .. } = &t.status {
                Some(result.clone())
            } else {
                None
            }
        })
    }

    pub async fn list(&self) -> Vec<Task> {
        let tasks = self.tasks.read().await;
        tasks.values().cloned().collect()
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_modified_files(diff: Option<&str>) -> Vec<String> {
    let Some(diff) = diff else {
        return vec![];
    };
    diff.lines()
        .filter(|line| line.starts_with("+++ b/"))
        .map(|line| line.trim_start_matches("+++ b/").to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentConfig, AgentContext, AgentLoop};
    use crate::entities::InMemoryEntityStore;
    use crate::tools::{EchoTool, ToolRegistry};
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

    #[test]
    fn test_task_id_uniqueness() {
        let id1 = TaskId::new();
        let id2 = TaskId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_task_result_to_json() {
        let result = TaskResult {
            result_summary: "Done".to_string(),
            changes_patch: Some("diff --git a/foo".to_string()),
            files_modified: vec!["foo.rs".to_string()],
            tool_calls_made: vec![],
            iterations: 3,
            model_used: "qwen3:0.6b".to_string(),
        };
        let json = result.to_json();
        assert_eq!(json["result_summary"], "Done");
        assert_eq!(json["iterations"], 3);
        assert!(json["changes_patch"].is_string());
    }

    #[test]
    fn test_failure_diagnostics_to_json() {
        let diag = FailureDiagnostics {
            error_type: "MaxIterationsExceeded".to_string(),
            iterations_completed: 100,
            last_tool_call: None,
            partial_changes: None,
        };
        let json = diag.to_json();
        assert_eq!(json["error_type"], "MaxIterationsExceeded");
        assert_eq!(json["iterations_completed"], 100);
    }

    #[test]
    fn test_parse_modified_files_from_diff() {
        let diff = "+++ b/src/main.rs\n+++ b/src/lib.rs\n--- a/src/main.rs\n";
        let files = parse_modified_files(Some(diff));
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn test_parse_modified_files_empty_diff() {
        let files = parse_modified_files(None);
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn test_task_manager_poll_returns_none_for_invalid_id() {
        let manager = TaskManager::new();
        let result = manager.poll(&TaskId("nonexistent".to_string())).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_task_manager_list_empty() {
        let manager = TaskManager::new();
        let tasks = manager.list().await;
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_agent_completes_with_mock_provider() {
        let provider: Arc<dyn ModelProvider> =
            MockProvider::new(vec![stop_response("Task complete!")]);
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);
        let context = AgentContext {
            user_prompt: "Test task".to_string(),
            conversation_history: vec![ChatMessage::user("Test task")],
            app_state_id: "test".to_string(),
        };
        let result = agent.run(context).await.unwrap();
        assert!(result.task_completed);
        assert_eq!(result.result_summary, "Task complete!");
    }

    #[tokio::test]
    async fn test_agent_fails_with_max_iterations() {
        let responses: Vec<ChatResponse> = (0..5).map(|_| stop_response("not done yet")).collect();
        let provider: Arc<dyn ModelProvider> = MockProvider::new(responses);
        let config = AgentConfig {
            max_iterations: 0,
            ..Default::default()
        };
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);
        let context = AgentContext {
            user_prompt: "Test task".to_string(),
            conversation_history: vec![ChatMessage::user("Test task")],
            app_state_id: "test".to_string(),
        };
        let result = agent.run(context).await;
        assert!(matches!(
            result,
            Err(AgentError::MaxIterationsExceeded { .. })
        ));
    }
}
