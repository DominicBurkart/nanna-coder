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
}
