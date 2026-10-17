use super::{CapabilitySpec, ScopedCapabilities};
use crate::auditor::{
    Allowed, AuditContext, AuditLog, Gate, RuleAuditor, SpawnRequest, TaskSummary,
};
use crate::effects::EffectClass;
use crate::identity::{AgentIdentity, IdentityCatalog};
use crate::tools::{Tool, ToolRegistry, ToolResult};
use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, SchemaType, ToolDefinition};
use serde_json::Value;

struct StubTool {
    name: String,
    description: String,
    effect: EffectClass,
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
        Ok(Value::Null)
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
    let dir = dir.to_path_buf();
    let name = name.to_string();
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
                parent_task: TaskSummary::new("t1", "Do work", "github.com/example/repo"),
                identity: name.clone(),
                subtask: "Add a regression test.".to_string(),
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
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join(format!("{}.toml", identity.name()));
    std::fs::write(file, identity.to_toml_string().unwrap()).unwrap();
    allowed_from_dir(dir.path(), identity.name())
}

pub(crate) fn scoped(identity: &AgentIdentity, specs: &[CapabilitySpec]) -> ScopedCapabilities {
    let mut registry = ToolRegistry::new();
    for spec in specs {
        registry.register(Box::new(StubTool {
            name: spec.name.clone(),
            description: spec.description.clone(),
            effect: spec.effect,
        }));
    }
    ScopedCapabilities::scope(registry, identity)
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
        };
        assert_eq!(tool.execute(Value::Null).await.unwrap(), Value::Null);
    }
}
