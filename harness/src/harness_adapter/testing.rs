use super::{CapabilitySpec, ScopedCapabilities};
use crate::auditor::{
    Allowed, AuditContext, AuditLog, Gate, RuleAuditor, SpawnRequest, TaskSummary,
};
use crate::effects::EffectClass;
use crate::identity::{AgentIdentity, IdentityCatalog};
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, SchemaType, ToolDefinition};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub(crate) type Behavior = Arc<dyn Fn() -> ToolResult<Value> + Send + Sync>;

struct StubTool {
    name: String,
    description: String,
    effect: EffectClass,
    behavior: Behavior,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for StubTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: None,
                    required: None,
                },
            },
        }
    }

    async fn execute(&self, _args: Value) -> ToolResult<Value> {
        tokio::task::yield_now().await;
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.behavior)()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn effect_class(&self) -> EffectClass {
        self.effect
    }
}

const AUDITOR_CARD: &str = "[identity]\nname = \"auditor\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\nsystem_prompt = { inline = \"p\" }\n\n[scope]\nrepos = []\npaths = [\"**\"]\nmax_effect = \"none\"\ntools = []\n\n[limits]\nmax_iterations = 1\nmax_wall_clock_secs = 1\nmax_concurrent = 1\n";

pub(crate) fn allowed_from_dir(dir: &std::path::Path, name: &str) -> Allowed {
    allowed_from_dir_for_repo(dir, name, "github.com/example/repo")
}

pub(crate) fn allowed_from_dir_for_repo(dir: &std::path::Path, name: &str, repo: &str) -> Allowed {
    allowed_from_dir_for(dir, name, repo, "Add a regression test.")
}

pub(crate) fn allowed_from_dir_for(
    dir: &std::path::Path,
    name: &str,
    repo: &str,
    subtask: &str,
) -> Allowed {
    let dir = dir.to_path_buf();
    let name = name.to_string();
    let repo = repo.to_string();
    let subtask = subtask.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            std::fs::write(dir.join("auditor.toml"), AUDITOR_CARD).unwrap();
            let catalog = IdentityCatalog::load(&dir).unwrap();
            let identity = catalog.get(&name).unwrap().clone();
            let context =
                AuditContext::new(catalog.clone(), catalog.get("auditor").unwrap().clone())
                    .unwrap();
            let request = SpawnRequest {
                parent_task: TaskSummary::new("t1", "Do work", &repo),
                identity: name.clone(),
                subtask,
                dev_loop: identity.identity.dev_loop,
                requested_effect: EffectClass::None,
            };
            let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
            gate.check(request, &context).await.unwrap()
        })
    })
    .join()
    .unwrap()
}

pub(crate) fn allowed_for(identity: &AgentIdentity) -> Allowed {
    allowed_for_repo(identity, "github.com/example/repo")
}

pub(crate) fn allowed_for_repo(identity: &AgentIdentity, repo: &str) -> Allowed {
    allowed_for_subtask(identity, repo, "Add a regression test.")
}

pub(crate) fn allowed_for_subtask(identity: &AgentIdentity, repo: &str, subtask: &str) -> Allowed {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join(format!("{}.toml", identity.name()));
    std::fs::write(file, identity.to_toml_string().unwrap()).unwrap();
    allowed_from_dir_for(dir.path(), identity.name(), repo, subtask)
}

pub(crate) struct FixedVerdict(pub(crate) crate::action_auditor::ActionVerdict);

#[async_trait]
impl crate::action_auditor::ActionAuditor for FixedVerdict {
    fn name(&self) -> &str {
        "fixed-verdict"
    }

    async fn review_action(
        &self,
        _review: &crate::action_auditor::ActionReview,
        _context: &crate::action_auditor::ActionContext<'_>,
    ) -> Result<crate::action_auditor::ActionVerdict, crate::action_auditor::ActionAuditError> {
        Ok(self.0.clone())
    }
}

pub(crate) fn scoped(identity: &AgentIdentity, specs: &[CapabilitySpec]) -> ScopedCapabilities {
    let mut registry = ToolRegistry::new();
    for spec in specs {
        registry.register(Box::new(StubTool {
            name: spec.name.clone(),
            description: spec.description.clone(),
            effect: spec.effect,
            behavior: ok_with(Value::Null),
            calls: Arc::new(AtomicUsize::new(0)),
        }));
    }
    ScopedCapabilities::scope(registry, identity)
}

pub(crate) type Counters = Vec<(String, Arc<AtomicUsize>)>;

pub(crate) fn base_registry(tools: Vec<(&str, EffectClass, Behavior)>) -> (ToolRegistry, Counters) {
    let mut registry = ToolRegistry::new();
    let mut counters = Vec::new();
    for (name, effect, behavior) in tools {
        let calls = Arc::new(AtomicUsize::new(0));
        counters.push((name.to_string(), Arc::clone(&calls)));
        registry.register(Box::new(StubTool {
            name: name.to_string(),
            description: format!("{name} tool"),
            effect,
            behavior,
            calls,
        }));
    }
    (registry, counters)
}

pub(crate) fn ok_with(value: Value) -> Behavior {
    Arc::new(move || Ok(value.clone()))
}

pub(crate) fn fail_with(make: fn() -> ToolError) -> Behavior {
    Arc::new(move || Err(make()))
}

pub(crate) fn calls(counters: &Counters, name: &str) -> usize {
    counters
        .iter()
        .find(|(n, _)| n == name)
        .unwrap()
        .1
        .load(Ordering::SeqCst)
}

pub(crate) fn git(dir: &std::path::Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(dir).args(args);
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    for var in [
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
    ] {
        command.env_remove(var);
    }
    let out = command.output().unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

pub(crate) fn repo_with_origin(url: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    git(dir.path(), &["init"]);
    git(dir.path(), &["config", "user.email", "t@t"]);
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(dir.path().join("api")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("README.md"), "# r").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "init"]);
    git(dir.path(), &["remote", "add", "origin", url]);
    dir
}

pub(crate) fn unique_task(prefix: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    format!("{prefix}-{}-{n}", std::process::id())
}

pub(crate) fn real_workspace(task: &str) -> (crate::workspace::TaskWorkspace, tempfile::TempDir) {
    let repo = repo_with_origin("https://github.com/example/repo.git");
    let workspace = crate::workspace::TaskWorkspace::create(repo.path(), task, "HEAD").unwrap();
    (workspace, repo)
}

pub(crate) struct Layered {
    pub(crate) layers: crate::task::ModelFacingLayers,
    pub(crate) log: Arc<crate::escalation::EscalationLog>,
}

#[cfg(test)]
pub(crate) fn layers_for(
    identity: &AgentIdentity,
    task: &str,
    repo: &str,
    verdict: crate::action_auditor::ActionVerdict,
) -> Layered {
    let log = Arc::new(crate::escalation::EscalationLog::in_memory());
    let layers = crate::task::ModelFacingLayers {
        escalation_log: Arc::clone(&log),
        action_gate: Arc::new(crate::action_auditor::ActionGate::new(
            Arc::new(FixedVerdict(verdict)),
            crate::action_auditor::ActionAuditLog::in_memory(),
        )),
        action_escalator: Arc::new(crate::escalation::Escalator::new(
            Arc::clone(&log),
            Arc::new(crate::escalation::FanoutSink(Vec::new())),
            Arc::new(crate::leases::SystemClock),
            crate::escalation::default_window(),
        )),
        subject: crate::tools::ActionSubject {
            task_id: crate::task::TaskId(task.to_string()),
            max_effect: identity.scope.max_effect,
            window: None,
            repo: repo.to_string(),
            branch: None,
            pr: None,
            environment: None,
            paths: identity.scope.paths.clone(),
            description: "d".to_string(),
        },
        hub: None,
    };
    Layered { layers, log }
}

pub(crate) fn gated_registry_with_stub(
    workspace: &crate::workspace::TaskWorkspace,
    identity: &AgentIdentity,
    layers: crate::task::ModelFacingLayers,
    stub: &str,
) -> (ToolRegistry, Arc<AtomicUsize>) {
    let mut registry = crate::task::model_facing_registry(workspace, Some(identity), layers)
        .expect("the workspace registry builds");
    let calls = Arc::new(AtomicUsize::new(0));
    registry.register(Box::new(StubTool {
        name: stub.to_string(),
        description: format!("{stub} tool"),
        effect: EffectClass::Workspace,
        behavior: ok_with(serde_json::json!("ran")),
        calls: Arc::clone(&calls),
    }));
    (registry, calls)
}

pub(crate) fn layers_allowing(identity: &AgentIdentity, task: &str) -> Layered {
    layers_for(
        identity,
        task,
        "example/repo",
        crate::action_auditor::ActionVerdict::Allow,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_tool_executes_through_a_registry() {
        let tool = StubTool {
            name: "x".into(),
            description: "d".into(),
            effect: EffectClass::None,
            behavior: ok_with(Value::Null),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        assert_eq!(tool.execute(Value::Null).await.unwrap(), Value::Null);
    }

    #[test]
    fn the_fixed_verdict_auditor_is_named() {
        let auditor = FixedVerdict(crate::action_auditor::ActionVerdict::Allow);
        assert_eq!(
            crate::action_auditor::ActionAuditor::name(&auditor),
            "fixed-verdict"
        );
    }
}
