use super::{CapabilitySpec, ScopedCapabilities};
use crate::effects::EffectClass;
use crate::identity::AgentIdentity;
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

pub(crate) fn scoped(identity: &AgentIdentity, specs: &[CapabilitySpec]) -> ScopedCapabilities {
    let mut registry = ToolRegistry::new();
    for spec in specs {
        registry.register(Box::new(StubTool {
            name: spec.name.clone(),
            description: spec.description.clone(),
            effect: spec.effect,
        }));
    }
    ScopedCapabilities::from_registry(&registry.scoped_for(identity)).unwrap()
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
