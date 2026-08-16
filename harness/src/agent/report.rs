//! Versioned DTO for serialising agent run results across the binary/eval
//! boundary.
//!
//! The eval pipeline subprocess-calls the `nanna agent --output-json <path>`
//! binary; that binary writes [`AgentRunReport`] to disk and the eval reads
//! it back. Keeping a separate DTO from the in-process [`AgentRunResult`]
//! lets internal types (`ChatMessage`, `ToolCallRecord`, `AgentState`) churn
//! without breaking the on-disk contract.
//!
//! Bump [`SCHEMA_VERSION`] on any breaking shape change.

use crate::agent::AgentRunResult;
use model::types::Usage;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunReport {
    pub schema_version: u32,
    pub task_completed: bool,
    pub iterations: usize,
    pub final_state: String,
    pub result_summary: String,
    pub token_usage: Option<TokenUsageDto>,
    pub tool_calls: Vec<ToolCallSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageDto {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallSummary {
    pub tool_name: String,
    pub call_id: String,
    pub arguments: serde_json::Value,
    pub result: String,
}

impl From<&Usage> for TokenUsageDto {
    fn from(u: &Usage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }
    }
}

impl From<&AgentRunResult> for AgentRunReport {
    fn from(r: &AgentRunResult) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            task_completed: r.task_completed,
            iterations: r.iterations,
            final_state: format!("{:?}", r.final_state),
            result_summary: r.result_summary.clone(),
            token_usage: r.token_usage.as_ref().map(TokenUsageDto::from),
            tool_calls: r
                .tool_calls_made
                .iter()
                .map(|tc| ToolCallSummary {
                    tool_name: tc.tool_name.clone(),
                    call_id: tc.call_id.clone(),
                    arguments: tc.arguments.clone(),
                    result: tc.result.clone(),
                })
                .collect(),
        }
    }
}

impl AgentRunReport {
    pub fn to_json_pretty(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn write_to_path(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .expect("AgentRunReport: standard-type DTO cannot fail to serialize");
        std::fs::write(path, json)
    }

    pub fn read_from_path(path: &Path) -> io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentState;
    use crate::entities::context::types::ToolCallRecord;
    use model::types::ChatMessage;

    fn make_result() -> AgentRunResult {
        AgentRunResult {
            final_state: AgentState::Completed,
            iterations: 3,
            task_completed: true,
            result_summary: "done".to_string(),
            tool_calls_made: vec![ToolCallRecord {
                tool_name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "main.rs"}),
                call_id: "call-1".to_string(),
                result: "fn main(){}".to_string(),
            }],
            conversation_snapshot: vec![ChatMessage::user("hi")],
            token_usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        }
    }

    #[test]
    fn report_from_result_preserves_scalars() {
        let report = AgentRunReport::from(&make_result());
        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert!(report.task_completed);
        assert_eq!(report.iterations, 3);
        assert_eq!(report.final_state, "Completed");
        assert_eq!(report.result_summary, "done");
    }

    #[test]
    fn report_token_usage_round_trips() {
        let report = AgentRunReport::from(&make_result());
        let usage = report.token_usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn report_tool_calls_carry_through() {
        let report = AgentRunReport::from(&make_result());
        assert_eq!(report.tool_calls.len(), 1);
        let tc = &report.tool_calls[0];
        assert_eq!(tc.tool_name, "read_file");
        assert_eq!(tc.call_id, "call-1");
        assert_eq!(tc.result, "fn main(){}");
    }

    #[test]
    fn report_round_trips_via_json() {
        let report = AgentRunReport::from(&make_result());
        let json = report.to_json_pretty().unwrap();
        let parsed: AgentRunReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.iterations, 3);
        assert_eq!(parsed.tool_calls.len(), 1);
    }

    #[test]
    fn report_round_trips_via_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let report = AgentRunReport::from(&make_result());
        report.write_to_path(&path).unwrap();
        let loaded = AgentRunReport::read_from_path(&path).unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.task_completed, report.task_completed);
        assert_eq!(loaded.tool_calls.len(), 1);
    }

    #[test]
    fn report_handles_no_token_usage() {
        let mut result = make_result();
        result.token_usage = None;
        let report = AgentRunReport::from(&result);
        assert!(report.token_usage.is_none());
    }
}
