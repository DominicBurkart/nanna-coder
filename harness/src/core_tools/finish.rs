use crate::effects::EffectClass;
use crate::tool_contract::{ErrorCode, FinishClaim, FinishSlot, ToolOutcome, TypedTool};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FinishArgs {
    pub summary: String,
    #[serde(default)]
    pub files_changed: Vec<String>,
}

pub struct FinishTool {
    slot: FinishSlot,
}

impl FinishTool {
    pub fn new(slot: FinishSlot) -> Self {
        Self { slot }
    }
}

#[async_trait]
impl TypedTool for FinishTool {
    type Args = FinishArgs;

    fn name(&self) -> &str {
        "finish"
    }

    fn description(&self) -> &str {
        "Declare the task complete with a short summary of what changed and which files were touched. Call this once, after your edits build and pass."
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }

    async fn run(&self, args: FinishArgs) -> ToolOutcome {
        if args.summary.trim().is_empty() {
            return ToolOutcome::failure(ErrorCode::InvalidArgs, "summary is empty")
                .with_hint("describe what you changed in one or two sentences");
        }
        let claim = FinishClaim {
            summary: args.summary,
            files_changed: args.files_changed,
        };
        let count = claim.files_changed.len();
        match self.slot.lock() {
            Ok(mut slot) => *slot = Some(claim),
            Err(poisoned) => *poisoned.into_inner() = Some(claim),
        }
        ToolOutcome::success(json!({"recorded": true, "files_changed": count}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_contract::{FinishSlot, Typed};
    use crate::tools::Tool;
    use serde_json::json;

    #[tokio::test]
    async fn finish_records_the_claim_in_the_slot() {
        let slot = FinishSlot::default();
        let tool = Typed(FinishTool::new(slot.clone()));

        let result = tool
            .execute(json!({"summary": "done", "files_changed": ["a.rs"]}))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        let claim = slot.lock().unwrap().clone().unwrap();
        assert_eq!(claim.summary, "done");
        assert_eq!(claim.files_changed, ["a.rs"]);
    }

    #[tokio::test]
    async fn finish_defaults_files_changed_to_empty() {
        let slot = FinishSlot::default();
        let tool = Typed(FinishTool::new(slot.clone()));

        tool.execute(json!({"summary": "done"})).await.unwrap();

        assert!(slot
            .lock()
            .unwrap()
            .clone()
            .unwrap()
            .files_changed
            .is_empty());
    }

    #[tokio::test]
    async fn finish_rejects_a_blank_summary_and_leaves_the_slot_empty() {
        let slot = FinishSlot::default();
        let tool = Typed(FinishTool::new(slot.clone()));

        let result = tool.execute(json!({"summary": "   "})).await.unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
        assert!(slot.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn finish_rejects_unknown_arguments_by_name() {
        let slot = FinishSlot::default();
        let tool = Typed(FinishTool::new(slot));

        let result = tool
            .execute(json!({"summary": "x", "status": "ok"}))
            .await
            .unwrap();

        assert_eq!(result["ok"], false);
        assert!(result["message"].as_str().unwrap().contains("status"));
    }
}
