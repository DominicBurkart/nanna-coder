use harness::entities::context::types::ToolCallRecord;
use harness::task::{FailureDiagnostics, TaskId, TaskResult, TaskStatus};
use chrono::Utc;

// ──────────────────────────────────────────────
// TaskId
// ──────────────────────────────────────────────

#[test]
fn test_task_id_new_generates_uuid() {
    let id = TaskId::new();
    // A UUID v4 in string form is 36 characters (8-4-4-4-12 + dashes)
    assert_eq!(id.0.len(), 36);
    // Each call produces a different ID
    let id2 = TaskId::new();
    assert_ne!(id, id2);
}

#[test]
fn test_task_id_default() {
    let id = TaskId::default();
    assert_eq!(id.0.len(), 36);
}

#[test]
fn test_task_id_display() {
    let id = TaskId::new();
    let displayed = format!("{id}");
    assert_eq!(displayed, id.0);
}

#[test]
fn test_task_id_debug() {
    let id = TaskId::new();
    let debug = format!("{id:?}");
    assert!(!debug.is_empty());
}

#[test]
fn test_task_id_eq_and_hash() {
    use std::collections::HashSet;
    let id = TaskId::new();
    let id_clone = id.clone();
    assert_eq!(id, id_clone);

    let mut set = HashSet::new();
    set.insert(id.clone());
    assert!(set.contains(&id));
}

// ──────────────────────────────────────────────
// TaskResult::to_json
// ──────────────────────────────────────────────

#[test]
fn test_task_result_to_json_has_expected_keys() {
    let result = TaskResult {
        result_summary: "All done".to_string(),
        changes_patch: Some("diff --git a/x.rs b/x.rs\n".to_string()),
        format_patch: Some("From abc\n".to_string()),
        files_modified: vec!["x.rs".to_string(), "y.rs".to_string()],
        tool_calls_made: vec![ToolCallRecord {
            tool_name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "x.rs"}),
            call_id: "c1".to_string(),
            result: "content".to_string(),
        }],
        iterations: 5,
        model_used: "qwen3:0.6b".to_string(),
    };

    let json = result.to_json();

    assert_eq!(json["result_summary"], "All done");
    assert_eq!(json["iterations"], 5);
    assert_eq!(json["model_used"], "qwen3:0.6b");
    assert!(json["changes_patch"].is_string());
    assert!(json["format_patch"].is_string());
    assert_eq!(json["files_modified"].as_array().unwrap().len(), 2);
    assert_eq!(json["tool_calls_made"].as_array().unwrap().len(), 1);
}

#[test]
fn test_task_result_to_json_none_patches() {
    let result = TaskResult {
        result_summary: "Nothing changed".to_string(),
        changes_patch: None,
        format_patch: None,
        files_modified: vec![],
        tool_calls_made: vec![],
        iterations: 1,
        model_used: "llama3:8b".to_string(),
    };

    let json = result.to_json();
    assert!(json["changes_patch"].is_null());
    assert!(json["format_patch"].is_null());
    assert_eq!(json["files_modified"].as_array().unwrap().len(), 0);
}

// ──────────────────────────────────────────────
// FailureDiagnostics::to_json
// ──────────────────────────────────────────────

#[test]
fn test_failure_diagnostics_to_json_minimal() {
    let diag = FailureDiagnostics {
        error_type: "Timeout".to_string(),
        iterations_completed: 42,
        last_tool_call: None,
        partial_changes: None,
        tool_call_history: vec![],
        last_agent_state: None,
        conversation_snapshot: None,
    };

    let json = diag.to_json();
    assert_eq!(json["error_type"], "Timeout");
    assert_eq!(json["iterations_completed"], 42);
    assert!(json["last_tool_call"].is_null());
    assert!(json["partial_changes"].is_null());
    assert_eq!(json["tool_call_history"].as_array().unwrap().len(), 0);
    assert!(json["last_agent_state"].is_null());
    assert!(json["conversation_snapshot"].is_null());
}

#[test]
fn test_failure_diagnostics_to_json_with_tool_calls() {
    let record = ToolCallRecord {
        tool_name: "write_file".to_string(),
        arguments: serde_json::json!({"path": "out.txt", "content": "hello"}),
        call_id: "c42".to_string(),
        result: "ok".to_string(),
    };

    let diag = FailureDiagnostics {
        error_type: "AgentError".to_string(),
        iterations_completed: 10,
        last_tool_call: Some(record.clone()),
        partial_changes: Some("partial diff".to_string()),
        tool_call_history: vec![record],
        last_agent_state: Some("Performing".to_string()),
        conversation_snapshot: None,
    };

    let json = diag.to_json();
    assert_eq!(json["error_type"], "AgentError");
    assert!(json["last_tool_call"].is_object());
    assert_eq!(json["last_tool_call"]["tool_name"], "write_file");
    assert_eq!(json["tool_call_history"].as_array().unwrap().len(), 1);
    assert_eq!(json["last_agent_state"], "Performing");
    assert_eq!(json["partial_changes"], "partial diff");
}

// ──────────────────────────────────────────────
// TaskStatus – serde serialization / deserialization
// ──────────────────────────────────────────────

#[test]
fn test_task_status_pending_serde() {
    let status = TaskStatus::Pending;
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["status"], "Pending");

    let roundtrip: TaskStatus = serde_json::from_value(json).unwrap();
    assert!(matches!(roundtrip, TaskStatus::Pending));
}

#[test]
fn test_task_status_running_serde() {
    let now = Utc::now();
    let status = TaskStatus::Running {
        started_at: now,
        iterations: 7,
    };
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["status"], "Running");
    assert_eq!(json["iterations"], 7);

    let roundtrip: TaskStatus = serde_json::from_value(json).unwrap();
    assert!(matches!(roundtrip, TaskStatus::Running { iterations: 7, .. }));
}

#[test]
fn test_task_status_completed_serde() {
    let result = TaskResult {
        result_summary: "ok".to_string(),
        changes_patch: None,
        format_patch: None,
        files_modified: vec![],
        tool_calls_made: vec![],
        iterations: 2,
        model_used: "m".to_string(),
    };
    let status = TaskStatus::Completed {
        finished_at: Utc::now(),
        result,
    };
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["status"], "Completed");
    assert_eq!(json["result"]["result_summary"], "ok");
}

#[test]
fn test_task_status_failed_serde() {
    let diag = FailureDiagnostics {
        error_type: "Err".to_string(),
        iterations_completed: 0,
        last_tool_call: None,
        partial_changes: None,
        tool_call_history: vec![],
        last_agent_state: None,
        conversation_snapshot: None,
    };
    let status = TaskStatus::Failed {
        finished_at: Utc::now(),
        error: "something failed".to_string(),
        diagnostics: diag,
    };
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["status"], "Failed");
    assert_eq!(json["error"], "something failed");
}
