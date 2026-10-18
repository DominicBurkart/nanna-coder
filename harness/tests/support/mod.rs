use harness::action_auditor::{
    ActionAuditError, ActionAuditLog, ActionAuditor, ActionContext, ActionGate, ActionReview,
    ActionVerdict,
};
use harness::tools::{ActionSubject, ToolRegistry};
use std::sync::Arc;

struct AllowAuditor;

#[async_trait::async_trait]
impl ActionAuditor for AllowAuditor {
    fn name(&self) -> &str {
        "allow"
    }

    async fn review_action(
        &self,
        _review: &ActionReview,
        _context: &ActionContext<'_>,
    ) -> Result<ActionVerdict, ActionAuditError> {
        Ok(ActionVerdict::Allow)
    }
}

pub fn allow_gated(registry: ToolRegistry) -> ToolRegistry {
    let gate = Arc::new(ActionGate::new(
        Arc::new(AllowAuditor),
        ActionAuditLog::in_memory(),
    ));
    let subject = ActionSubject {
        task_id: harness::task::TaskId("integration".to_string()),
        max_effect: harness::effects::EffectClass::Workspace,
        window: None,
        repo: "example/repo".to_string(),
        branch: None,
        pr: None,
        environment: None,
        paths: vec![],
        description: String::new(),
    };
    registry.with_action_gate(gate, subject)
}
