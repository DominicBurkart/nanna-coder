use async_trait::async_trait;
use harness::auditor::{AuditContext, AuditLog, RuleAuditor, SpawnGate};
use harness::identity::IdentityCatalog;
use harness::mcp::handlers::handle_assign_task;
use harness::scope::DenialReason;
use harness::task::{TaskId, TaskManager, TaskResult, TaskStatus};
use model::provider::{ModelError, ModelResult};
use model::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, MessageRole,
    ModelInfo, ToolCall,
};
use model::ModelProvider;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

const CATALOG: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/one_track/catalog"
);
const OVERRIDES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/one_track/repo/.nanna/agents"
);

struct Script(Mutex<Vec<ChatResponse>>);

#[async_trait]
impl ModelProvider for Script {
    async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
        let mut responses = self.0.lock().unwrap();
        if responses.is_empty() {
            return Err(ModelError::Unknown {
                message: "script exhausted".to_string(),
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
        "script"
    }
}

fn message(
    content: Option<&str>,
    calls: Option<Vec<ToolCall>>,
    stop: FinishReason,
) -> ChatResponse {
    ChatResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: MessageRole::Assistant,
                content: content.map(str::to_string),
                tool_calls: calls,
                tool_call_id: None,
            },
            finish_reason: Some(stop),
        }],
        usage: None,
    }
}

fn call(tool: &str, arguments: Value) -> ChatResponse {
    let call = ToolCall {
        id: "call_0".to_string(),
        function: FunctionCall {
            name: tool.to_string(),
            arguments,
        },
    };
    message(None, Some(vec![call]), FinishReason::ToolCalls)
}

fn provider(calls: Vec<ChatResponse>) -> Arc<dyn ModelProvider> {
    let mut script = vec![message(Some("Plan: do it"), None, FinishReason::Stop)];
    script.extend(calls);
    script.push(message(Some("performed"), None, FinishReason::Stop));
    script.push(message(Some("COMPLETE - done"), None, FinishReason::Stop));
    Arc::new(Script(Mutex::new(script)))
}

fn gate() -> SpawnGate {
    let catalog = IdentityCatalog::load(CATALOG).unwrap();
    let auditor = catalog.get("auditor").unwrap().clone();
    let context = AuditContext::new(catalog, auditor).unwrap();
    SpawnGate::new(Box::new(RuleAuditor::new()), AuditLog::in_memory(), context)
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
}

fn one_track_repo(with_overrides: bool) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t.com"],
        vec!["config", "user.name", "T"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["commit", "-q", "--allow-empty", "-m", "init"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/1-Track/one_track.git",
        ],
    ] {
        git(repo.path(), &args);
    }
    if with_overrides {
        let dir = repo.path().join(".nanna/agents");
        std::fs::create_dir_all(&dir).unwrap();
        for entry in std::fs::read_dir(OVERRIDES).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
        }
    }
    repo
}

async fn run(
    repo: &Path,
    identity: &str,
    calls: Vec<ChatResponse>,
) -> (TaskResult, Arc<TaskManager>, TaskId) {
    let manager = Arc::new(TaskManager::default());
    let model: Arc<dyn ModelProvider> = provider(calls);
    let params = json!({
        "description": "Change the api.",
        "repo_path": repo.to_str().unwrap(),
        "identity": identity,
    });
    let wire = handle_assign_task(&params, &manager, &model, &gate(), 20, None)
        .await
        .unwrap();
    let id = TaskId(wire["taskId"].as_str().unwrap().to_string());
    let status = manager.wait_terminal(&id).await.unwrap();
    assert!(matches!(status, TaskStatus::Completed { .. }), "{status:?}");
    let result = manager.get_result(&id).await.unwrap();
    (result, manager, id)
}

fn write(path: &str) -> ChatResponse {
    call("write_file", json!({"path": path, "content": "x"}))
}

fn read(path: &str) -> ChatResponse {
    call("read_file", json!({"path": path}))
}

#[tokio::test]
async fn a_scoped_task_for_one_track_is_denied_writes_outside_the_narrowed_paths() {
    let repo = one_track_repo(true);
    let (result, _, _) = run(
        repo.path(),
        "one-track-api",
        vec![
            write("api/src/new.rs"),
            write("ui/src/app.rs"),
            write("shared/src/lib.rs"),
            write("Cargo.toml"),
            write(".nanna/agents/one-track-api.toml"),
        ],
    )
    .await;
    assert_eq!(result.files_modified, ["api/src/new.rs"]);
    let denied: Vec<_> = result
        .denials
        .iter()
        .map(|denial| denial.tool.as_str())
        .collect();
    assert_eq!(denied, ["write_file"; 4]);
    assert!(result
        .denials
        .iter()
        .all(|denial| denial.identity == "one-track-api"));
}

#[tokio::test]
async fn the_repo_override_narrows_reads_the_global_card_grants() {
    let narrowed = one_track_repo(true);
    let plain = one_track_repo(false);
    let calls = || vec![read("ui/src/app.rs")];

    let (without, _, _) = run(plain.path(), "one-track-api", calls()).await;
    assert!(
        without.denials.is_empty(),
        "the global card reads ui/: {:?}",
        without.denials
    );

    let (with, _, _) = run(narrowed.path(), "one-track-api", calls()).await;
    assert_eq!(with.denials.len(), 1, "{:?}", with.denials);
    assert!(matches!(
        with.denials[0].reason,
        DenialReason::PathOutsideScope { .. }
    ));
}

#[tokio::test]
async fn the_one_track_reviewer_cannot_write_at_all() {
    let repo = one_track_repo(true);
    let (result, _, _) = run(
        repo.path(),
        "one-track-reviewer",
        vec![write("api/src/new.rs"), read("api/Cargo.toml")],
    )
    .await;
    assert!(result.files_modified.is_empty());
    assert_eq!(result.denials.len(), 1);
    assert!(matches!(
        result.denials[0].reason,
        DenialReason::ToolNotInScope
    ));
}

#[tokio::test]
async fn a_widened_one_track_override_fails_the_assignment_closed() {
    let repo = one_track_repo(true);
    let path = repo.path().join(".nanna/agents/one-track-api.toml");
    let widened = std::fs::read_to_string(&path)
        .unwrap()
        .replace(r#"paths = ["api/**"]"#, r#"paths = ["api/**", "ui/**"]"#);
    std::fs::write(&path, widened).unwrap();
    let manager = Arc::new(TaskManager::default());
    let model: Arc<dyn ModelProvider> = provider(vec![]);
    let params = json!({
        "description": "Change the api.",
        "repo_path": repo.path().to_str().unwrap(),
        "identity": "one-track-api",
    });
    let err = handle_assign_task(&params, &manager, &model, &gate(), 20, None)
        .await
        .unwrap_err();
    assert!(err.contains("widens"), "{err}");
    assert!(manager.list().await.is_empty());
}

#[tokio::test]
async fn the_scripted_provider_reports_its_surface_and_runs_dry() {
    let model = provider(vec![]);
    assert_eq!(model.provider_name(), "script");
    assert!(model.list_models().await.unwrap().is_empty());
    model.health_check().await.unwrap();
    let request = ChatRequest::new("m", vec![]);
    for _ in 0..3 {
        model.chat(request.clone()).await.unwrap();
    }
    model.chat(request).await.unwrap_err();
}
