use crate::auditor::SpawnGate;
use crate::changes;
use crate::mcp::handlers;
use crate::task::{Task, TaskId, TaskManager, TaskStatus};
use serde_json::{json, Value};

pub const MAX_CLOSES_CHARS: usize = 200;
pub const COMPACT_LIMIT_BYTES: usize = 8 * 1024;
const SUMMARY_CHARS: usize = 3000;
const FILES_BUDGET_BYTES: usize = 1500;
const MESSAGE_CHARS: usize = 1000;
const PR_TITLE_CHARS: usize = 72;

pub fn parse_closes(params: &Value) -> Result<Option<String>, String> {
    match params.get("closes") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if (1..=MAX_CLOSES_CHARS).contains(&value.chars().count())
                && value.chars().all(|c| !c.is_control()) =>
        {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(format!(
            "closes must be a string of 1 to {MAX_CLOSES_CHARS} printable characters, such as `#462` or `owner/repo#462`"
        )),
    }
}

pub fn extend_tool_list(tools: &mut Vec<Value>) {
    for tool in tools.iter_mut() {
        match tool["name"].as_str() {
            Some("assign_task") => {
                tool["inputSchema"]["properties"]["closes"] = json!({
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_CLOSES_CHARS,
                    "description": "Optional issue or pull request this change closes, for example `#462` or `DominicBurkart/marigold#462`. It only shapes the suggested_pr_body that get_task_result returns (`Closes <ref>`)."
                });
            }
            Some("get_task_result") => {
                tool["description"] = json!("Return the outcome of a task created by assign_task. Never blocks: while the task is still working it returns its status instead, so call it again later. By default the answer is a compact summary (status, identity, repo, result_summary, files_modified, suggested_branch, suggested_pr_title, suggested_pr_body); call get_task_changes for the file contents. When it is completed, `next` tells you how to open the pull request with your own GitHub tools. Pass detail=\"full\" for the complete payload (patches, tool calls, audits), which can be very large. Takes the `task_id` returned by assign_task.");
                tool["inputSchema"]["properties"]["detail"] = json!({
                    "type": "string",
                    "enum": ["compact", "full"],
                    "description": "compact (default) or full"
                });
            }
            Some("get_task_status") => {
                tool["description"] = json!("Return the status of a task created by assign_task (working, completed, failed or cancelled) without waiting, with its identity and repo. A failed task also reports reason_code, message, retryable and a short fix hint. Takes the `task_id` returned by assign_task.");
            }
            _ => {}
        }
    }
    tools.push(changes_tool_definition());
}

fn changes_tool_definition() -> Value {
    json!({
        "name": "get_task_changes",
        "description": "Return the files a finished task changed, with their final contents, so they can be written into a branch or pull request: per file {path, status (added, modified, deleted, renamed), binary, content (UTF-8) or content_base64, bytes, truncated}, plus base_ref and next_offset for paging. A failed task that left partial changes also answers. Without `path`, `offset` is a file index and `limit` a file count (default 50, max 200), and about 200 KB of content is returned per call. With `path`, `offset` and `limit` are byte offsets into that one file (limit max 200000); use it to fetch a file reported with truncated=true.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "The task_id returned by assign_task" },
                "path": { "type": "string", "description": "Return only this changed file, paged by bytes" },
                "offset": { "type": "integer", "minimum": 0, "description": "File index, or byte offset when path is given" },
                "limit": { "type": "integer", "minimum": 1, "description": "File count, or byte count when path is given" }
            },
            "required": ["task_id"]
        }
    })
}

pub fn classify_failure(error_type: &str, message: &str) -> (String, bool, String) {
    let lower = message.to_lowercase();
    let spawn_refusal = |lower: &str| -> (String, bool, String) {
        let known = [
            (
                "effect_above_ceiling",
                "Lower requested_effect or use an identity whose effect ceiling covers the task.",
            ),
            (
                "unknown_identity",
                "Call list_identities and assign the task to a registered identity.",
            ),
            (
                "scope_creep",
                "Narrow the task description to the identity's scope or pick a wider identity.",
            ),
            (
                "loop_mismatch",
                "Use an identity whose dev_loop matches the requested loop.",
            ),
            (
                "prompt_injection",
                "The auditor read the task text as orders aimed at the agent. Describe the change in plain terms: the file, the function and the expected behaviour. Avoid commands about how the agent should work (must, numbered steps, output formats). Then submit again with a new client_ref.",
            ),
        ];
        for (code, fix) in known {
            if lower.contains(code) {
                return (code.to_string(), false, fix.to_string());
            }
        }
        if lower.contains("scope.repos") {
            return (
                "repo_outside_scope".to_string(),
                false,
                "Use an identity whose scope.repos includes this repository.".to_string(),
            );
        }
        (
            "spawn_refused".to_string(),
            false,
            "Read the message for the auditor's reason, adjust the request and submit it again with a new client_ref.".to_string(),
        )
    };
    match error_type {
        "SpawnAuditFailed" | "RestoreAuditFailed" => {
            let transient = error_type == "RestoreAuditFailed"
                || lower.contains("unavailable")
                || lower.contains("timed out")
                || lower.contains("timeout")
                || lower.contains("model call failed");
            (
                error_type.to_string(),
                transient,
                if transient {
                    "The audit model was unavailable or timed out, or the server restarted. Submit the task again; use a new client_ref because the old one returns this failed task.".to_string()
                } else {
                    "The audit could not be recorded; ask the operator to check the audit log."
                        .to_string()
                },
            )
        }
        "SpawnRefused" | "RestoreRefused" => spawn_refusal(&lower),
        "RestoreNoIdentity" | "IdentityUnavailable" => (
            "unknown_identity".to_string(),
            false,
            "Call list_identities and assign the task to a registered identity.".to_string(),
        ),
        "ScopeError" => (
            "scope_error".to_string(),
            false,
            "The identity's scope could not be applied; check the identity card.".to_string(),
        ),
        "MaxIterationsExceeded" => (
            error_type.to_string(),
            false,
            "Raise max_iterations or split the task into smaller ones.".to_string(),
        ),
        "NoProgress" => (
            "no_progress".to_string(),
            true,
            "The agent changed no files. The message says which limit ended the task, how many edits failed and the last tool error; address that error before retrying, and call get_task_result with detail=full to see every call.".to_string(),
        ),
        "ProtectedPathViolation" => (
            error_type.to_string(),
            false,
            "The task touched a protected path; remove that change from the task.".to_string(),
        ),
        "StateError"
            if lower.contains("llm perform failed")
                && (lower.contains("request timeout")
                    || lower.contains("service unavailable")
                    || lower.contains("connection refused")) =>
        {
            (
                "model_unavailable".to_string(),
                true,
                "The local model did not answer in time, usually because the machine is busy. Submit the task again with a new client_ref; a smaller, more specific task finishes faster.".to_string(),
            )
        }
        "TaskCheckFailed" | "StateError" => (
            error_type.to_string(),
            false,
            "Inspect get_task_result with detail=full for the agent's last state.".to_string(),
        ),
        other => (
            other.to_string(),
            false,
            "Read the message for the cause.".to_string(),
        ),
    }
}

fn truncate_chars(text: &str, limit: usize) -> (String, bool) {
    match text.char_indices().nth(limit) {
        Some((cut, _)) => (text[..cut].to_string(), true),
        None => (text.to_string(), false),
    }
}

fn effect_ceiling(gate: &SpawnGate, task: &Task) -> Value {
    task.identity_hint
        .as_deref()
        .and_then(|name| gate.context().catalog().get(name))
        .map_or(Value::Null, |identity| json!(identity.scope.max_effect))
}

fn repo_name(task: &Task) -> String {
    crate::scope::origin_slug(&task.repo_path)
        .unwrap_or_else(|| task.repo_path.display().to_string())
}

pub fn suggested_pr_title(description: &str) -> String {
    let first = description.lines().next().unwrap_or("").trim();
    truncate_chars(first, PR_TITLE_CHARS)
        .0
        .trim_end()
        .to_string()
}

pub fn suggested_pr_body(summary: &str, closes: Option<&str>) -> String {
    match closes {
        Some(reference) => format!("{summary}\n\nCloses {reference}"),
        None => summary.to_string(),
    }
}

fn identity_fields(gate: &SpawnGate, task: &Task) -> Value {
    json!({
        "task_id": task.id.0,
        "identity": task.identity_hint,
        "repo": repo_name(task),
        "effect_ceiling": effect_ceiling(gate, task),
    })
}

fn merge(base: &mut Value, extra: Value) {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
}

fn tools_unavailable_of(task: &Task) -> &[crate::exec_capabilities::ToolsUnavailable] {
    match &task.status {
        TaskStatus::Completed { result, .. } => &result.tools_unavailable,
        TaskStatus::Failed { diagnostics, .. } => &diagnostics.tools_unavailable,
        _ => &[],
    }
}

fn merge_tools_unavailable(value: &mut Value, task: &Task) {
    let entries = tools_unavailable_of(task);
    if !entries.is_empty() {
        value["tools_unavailable"] = json!(entries);
    }
}

pub fn status_view(gate: &SpawnGate, task: &Task) -> Value {
    let mut view = handlers::task_to_wire(task);
    merge(&mut view, identity_fields(gate, task));
    merge_tools_unavailable(&mut view, task);
    if let TaskStatus::Failed {
        error, diagnostics, ..
    } = &task.status
    {
        let (reason_code, retryable, fix) = classify_failure(&diagnostics.error_type, error);
        merge(
            &mut view,
            json!({
                "reason_code": reason_code,
                "message": truncate_chars(error, MESSAGE_CHARS).0,
                "retryable": retryable,
                "fix": fix,
            }),
        );
    }
    view
}

fn compact_files(files: &[String]) -> (Vec<String>, usize) {
    let mut used = 0;
    let mut kept = Vec::new();
    for file in files {
        used += file.len() + 4;
        if used > FILES_BUDGET_BYTES {
            break;
        }
        kept.push(file.clone());
    }
    (kept, files.len())
}

fn text_result(value: &Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string(value).unwrap_or_default() }],
        "isError": is_error,
    })
}

fn bounded(mut build: impl FnMut(usize) -> Value) -> String {
    let mut limit = SUMMARY_CHARS;
    loop {
        let text = serde_json::to_string(&build(limit)).unwrap_or_default();
        if text.len() <= COMPACT_LIMIT_BYTES || limit <= 16 {
            return text;
        }
        limit /= 2;
    }
}

pub struct ResultInputs<'a> {
    pub gate: &'a SpawnGate,
    pub task: &'a Task,
    pub closes: Option<String>,
    pub capture: Option<changes::ChangeCapture>,
}

pub fn compact_result(inputs: &ResultInputs<'_>) -> Value {
    let task = inputs.task;
    let closes = inputs.closes.as_deref();
    let patch_original = inputs.capture.as_ref().and_then(|c| c.patch_original_bytes);
    let changes_available = inputs.capture.as_ref().is_some_and(|c| !c.files.is_empty());
    let build_common = |value: &mut Value| {
        merge(value, identity_fields(inputs.gate, task));
        merge_tools_unavailable(value, task);
        merge(
            value,
            json!({
                "closes": closes,
                "changes_available": changes_available,
                "patch_truncated": patch_original.is_some(),
                "patch_original_bytes": patch_original,
                "suggested_branch": format!("nanna/{}", task.id.0),
            }),
        );
    };
    match &task.status {
        TaskStatus::Completed { result, .. } => {
            let (files, total) = compact_files(&result.files_modified);
            let no_changes = result.files_modified.is_empty();
            let text = bounded(|limit| {
                let (summary, summary_truncated) = truncate_chars(&result.result_summary, limit);
                let mut value = json!({
                    "status": "completed",
                    "result_summary": summary,
                    "result_summary_truncated": summary_truncated,
                    "files_modified": files,
                    "files_modified_total": total,
                    "iterations": result.iterations,
                    "max_effect_class": result.max_effect_class(),
                    "suggested_pr_title": suggested_pr_title(&task.description),
                    "suggested_pr_body": suggested_pr_body(&summary, closes),
                    "no_changes": no_changes,
                    "next": if no_changes {
                        "The task finished without changing any files, so it did not implement anything and there is no pull request to open. Read result_summary to see what it did, then either assign a more specific task (name the file, the function and the exact change) or make the change yourself.".to_string()
                    } else {
                        format!(
                            "To open the pull request, use your own GitHub tools: 1. call get_task_changes for the files; 2. create branch nanna/{} from the repository's default branch; 3. write each file on it (remove the deleted ones); 4. open a draft pull request using suggested_pr_title and suggested_pr_body; 5. give the user the PR link. Nanna cannot push or open pull requests. If you have no GitHub tool connected, show the user the files, the title and the body, and say a GitHub connection is needed.",
                            task.id.0
                        )
                    },
                });
                build_common(&mut value);
                value
            });
            json!({
                "content": [{ "type": "text", "text": text }],
                "isError": false,
            })
        }
        TaskStatus::Failed {
            error, diagnostics, ..
        } => {
            let (reason_code, retryable, fix) = classify_failure(&diagnostics.error_type, error);
            let text = bounded(|limit| {
                let mut value = json!({
                    "status": "failed",
                    "reason_code": reason_code,
                    "message": truncate_chars(error, limit.min(MESSAGE_CHARS)).0,
                    "retryable": retryable,
                    "fix": fix,
                    "error_type": diagnostics.error_type,
                    "iterations": diagnostics.iterations_completed,
                    "denial_count": diagnostics.denials.len(),
                    "next": "Call get_task_result with detail=full for the diagnostics.",
                });
                build_common(&mut value);
                value
            });
            json!({
                "content": [{ "type": "text", "text": text }],
                "isError": true,
            })
        }
        TaskStatus::Cancelled {
            iterations_completed,
            ..
        } => {
            let mut value = json!({
                "status": "cancelled",
                "iterations": iterations_completed,
            });
            build_common(&mut value);
            text_result(&value, true)
        }
        _ => text_result(&status_view(inputs.gate, task), false),
    }
}

pub fn full_result(task: &Task, capture: Option<&changes::ChangeCapture>) -> Value {
    let extra = json!({
        "patch_truncated": capture.is_some_and(|c| c.patch_original_bytes.is_some()),
        "patch_original_bytes": capture.and_then(|c| c.patch_original_bytes),
    });
    handlers::task_result_with_extra(&task.id.0, &task.status, &extra)
}

pub async fn handle_get_task_result(
    manager: &TaskManager,
    gate: &SpawnGate,
    params: &Value,
) -> Result<Value, String> {
    let task = find_task(manager, params).await?;
    let detail = match params.get("detail") {
        None | Some(Value::Null) => "compact",
        Some(Value::String(value)) if value == "compact" || value == "full" => value.as_str(),
        Some(_) => return Err("detail must be \"compact\" or \"full\"".to_string()),
    };
    if !task.status.is_terminal() {
        return Ok(text_result(&status_view(gate, &task), false));
    }
    let capture = manager.captured_changes(&task.id).await;
    if detail == "full" {
        return Ok(full_result(&task, capture.as_ref()));
    }
    let closes = manager.closes(&task.id);
    Ok(compact_result(&ResultInputs {
        gate,
        task: &task,
        closes,
        capture,
    }))
}

pub async fn handle_get_task_status(
    manager: &TaskManager,
    gate: &SpawnGate,
    params: &Value,
) -> Result<Value, String> {
    let task = find_task(manager, params).await?;
    Ok(handlers::text_tool_result(&status_view(gate, &task)))
}

async fn find_task(manager: &TaskManager, params: &Value) -> Result<Task, String> {
    let raw = params
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing required argument: task_id".to_string())?;
    let task_id = TaskId(raw.to_string());
    manager
        .poll(&task_id)
        .await
        .ok_or_else(|| format!("Task not found: {task_id}"))
}

fn optional_index(params: &Value, key: &str) -> Result<Option<usize>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or_else(|| format!("{key} must be a non-negative integer")),
    }
}

pub async fn handle_get_task_changes(
    manager: &TaskManager,
    params: &Value,
) -> Result<Value, String> {
    let task = find_task(manager, params).await?;
    let path = match params.get("path") {
        None | Some(Value::Null) => None,
        Some(Value::String(path)) => Some(path.as_str()),
        Some(_) => return Err("path must be a string".to_string()),
    };
    let offset = optional_index(params, "offset")?.unwrap_or(0);
    let limit = optional_index(params, "limit")?;
    if !task.status.is_terminal() {
        return Ok(text_result(
            &json!({
                "status": "working",
                "message": "The task has not finished; call get_task_status until it is completed or failed, then call get_task_changes.",
            }),
            true,
        ));
    }
    let Some(capture) = manager.captured_changes(&task.id).await else {
        if manager.changes_not_retained(&task.id) {
            return Ok(text_result(
                &json!({
                    "status": "changes_not_retained",
                    "message": "This task changed files, but the contents were not retained (the stored copy was missing, unreadable, oversize or corrupt). The result itself is intact; get_task_result with detail=full still carries the patch when one was recorded.",
                }),
                true,
            ));
        }
        return Ok(text_result(
            &json!({
                "status": "no_changes",
                "message": "This task recorded no file changes.",
            }),
            true,
        ));
    };
    let body = changes::page(&capture, path, offset, limit)?;
    Ok(text_result(&body, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_accepts_short_printable_strings_only() {
        assert_eq!(parse_closes(&json!({})).unwrap(), None);
        assert_eq!(parse_closes(&json!({"closes": null})).unwrap(), None);
        assert_eq!(
            parse_closes(&json!({"closes": "#462"})).unwrap().as_deref(),
            Some("#462")
        );
        for bad in [
            json!(""),
            json!(7),
            json!("a\nb"),
            json!("x".repeat(MAX_CLOSES_CHARS + 1)),
        ] {
            assert!(parse_closes(&json!({ "closes": bad }))
                .unwrap_err()
                .contains("closes"));
        }
    }

    #[test]
    fn pr_title_is_the_first_line_trimmed_to_72_characters() {
        assert_eq!(suggested_pr_title("  Fix it  \nmore"), "Fix it");
        let long = "\u{00e9}".repeat(100);
        assert_eq!(suggested_pr_title(&long).chars().count(), 72);
        assert_eq!(suggested_pr_title(""), "");
    }

    #[test]
    fn pr_body_appends_closes() {
        assert_eq!(suggested_pr_body("done", None), "done");
        assert_eq!(
            suggested_pr_body("done", Some("#462")),
            "done\n\nCloses #462"
        );
    }

    #[test]
    fn failures_classify_into_reason_codes() {
        let code = |t: &str, m: &str| classify_failure(t, m);
        assert_eq!(
            code(
                "SpawnRefused",
                "spawn refused: block: effect_above_ceiling: x"
            )
            .0,
            "effect_above_ceiling"
        );
        assert_eq!(
            code(
                "SpawnRefused",
                "spawn refused: escalate: unknown_identity: x"
            )
            .0,
            "unknown_identity"
        );
        assert_eq!(
            code(
                "SpawnRefused",
                "identity `a` may not run against `r`: not in scope.repos"
            )
            .0,
            "repo_outside_scope"
        );
        let (c, retryable, fix) = code(
            "SpawnAuditFailed",
            "spawn refused: audit failed: auditor model unavailable: timeout",
        );
        assert_eq!((c.as_str(), retryable), ("SpawnAuditFailed", true));
        assert!(!fix.is_empty());
        assert!(!code("SpawnAuditFailed", "audit log write failed: disk").1);
        assert!(code("RestoreAuditFailed", "x").1);
        assert!(!code("MaxIterationsExceeded", "x").1);
        let (c, retryable, fix) = code("NoProgress", "Agent made no progress");
        assert_eq!((c.as_str(), retryable), ("no_progress", true));
        assert!(fix.contains("last tool error"), "{fix}");
        assert!(!fix.contains("name the file"), "{fix}");
        assert_eq!(code("SomethingNew", "x").0, "SomethingNew");
    }

    use crate::changes::{CapturedFile, ChangeCapture, ChangeStatus};
    use crate::entities::context::types::ToolCallRecord;
    use crate::mcp::jsonrpc::JsonRpcRequest;
    use crate::mcp::NannaMcpServer;
    use crate::task::{FailureDiagnostics, TaskResult};
    use async_trait::async_trait;
    use model::provider::{ModelProvider, ModelResult};
    use model::types::{ChatRequest, ChatResponse, ModelInfo};
    use std::sync::Arc;

    struct NoopProvider;

    #[async_trait]
    impl ModelProvider for NoopProvider {
        async fn chat(&self, _: ChatRequest) -> ModelResult<ChatResponse> {
            unimplemented!()
        }
        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }
        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }
        fn provider_name(&self) -> &'static str {
            "noop"
        }
    }

    fn server() -> NannaMcpServer {
        NannaMcpServer::new(
            Arc::new(TaskManager::new(0)),
            Arc::new(NoopProvider),
            crate::auditor::test_support::fixture_gate(),
            100,
        )
    }

    fn call(name: &str, arguments: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "tools/call".to_string(),
            params: Some(json!({ "name": name, "arguments": arguments })),
        }
    }

    fn body(result: &Value) -> Value {
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    async fn assign(server: &NannaMcpServer, description: &str, extra: Value) -> TaskId {
        let mut arguments = json!({
            "description": description,
            "repo_path": crate::auditor::test_support::shared_repo().to_str().unwrap(),
            "identity": "rust-implementer",
        });
        for (key, value) in extra.as_object().unwrap() {
            arguments[key] = value.clone();
        }
        let resp = server.handle_request(call("assign_task", arguments)).await;
        assert!(resp.error.is_none(), "{:?}", resp.error);
        TaskId(
            body(&resp.result.unwrap())["task_id"]
                .as_str()
                .unwrap()
                .to_string(),
        )
    }

    fn big_result() -> TaskResult {
        let record = |n: usize| ToolCallRecord {
            tool_name: format!("tool{n}"),
            arguments: json!({"blob": "x".repeat(500)}),
            call_id: format!("c{n}"),
            result: "ok".to_string(),
            effect: None,
        };
        TaskResult {
            changes: None,
            tools_unavailable: vec![],
            result_summary: format!("{}\u{0001}", "summary \u{0001}\"".repeat(20_000)),
            changes_patch: Some("diff --git a/a b/a\n".repeat(5000)),
            format_patch: Some("From abc\n".repeat(5000)),
            files_modified: (0..3000)
                .map(|i| format!("src/module_{i}/file.rs"))
                .collect(),
            tool_calls_made: (0..50).map(record).collect(),
            denials: vec![],
            action_audit: vec![],
            iterations: 7,
            model_used: "m".to_string(),
            blast_radius: None,
            qa_summary: Default::default(),
            budget: Default::default(),
        }
    }

    fn completed(result: TaskResult) -> TaskStatus {
        TaskStatus::Completed {
            finished_at: chrono::Utc::now(),
            result,
        }
    }

    fn file(path: &str, body: &str) -> CapturedFile {
        CapturedFile {
            path: path.to_string(),
            old_path: None,
            status: ChangeStatus::Added,
            binary: false,
            content: Some(body.as_bytes().to_vec()),
            size: body.len(),
            capture_truncated: false,
        }
    }

    async fn finished(
        server: &NannaMcpServer,
        status: TaskStatus,
        capture: Option<ChangeCapture>,
    ) -> TaskId {
        let id = assign(
            server,
            "Add the thing\nwith detail",
            json!({"closes": "#462"}),
        )
        .await;
        let status = match status {
            TaskStatus::Completed {
                finished_at,
                mut result,
            } => {
                result.changes = capture;
                TaskStatus::Completed {
                    finished_at,
                    result,
                }
            }
            other => other,
        };
        server.task_manager.finish_for_test(&id, status).await;
        id
    }

    async fn fetch(server: &NannaMcpServer, tool: &str, arguments: Value) -> Value {
        let resp = server.handle_request(call(tool, arguments)).await;
        assert!(resp.error.is_none(), "{:?}", resp.error);
        resp.result.unwrap()
    }

    #[tokio::test]
    async fn the_compact_view_omits_bulky_fields_and_stays_under_8_kb() {
        let server = server();
        let id = finished(&server, completed(big_result()), None).await;
        let result = fetch(&server, "get_task_result", json!({"task_id": id.0})).await;
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.len() < COMPACT_LIMIT_BYTES, "{} bytes", text.len());
        assert_eq!(result["isError"], false);
        let view = body(&result);
        for omitted in [
            "format_patch",
            "changes_patch",
            "action_audit",
            "tool_calls_made",
            "conversation_snapshot",
        ] {
            assert!(view.get(omitted).is_none(), "{omitted}");
        }
        assert_eq!(view["status"], "completed");
        assert_eq!(view["task_id"], id.0);
        assert_eq!(view["identity"], "rust-implementer");
        assert!(view["repo"].is_string());
        assert!(view["effect_ceiling"].is_string());
        assert_eq!(view["iterations"], 7);
        assert_eq!(view["suggested_branch"], format!("nanna/{}", id.0));
        assert_eq!(view["suggested_pr_title"], "Add the thing");
        assert_eq!(view["files_modified_total"], 3000);
        assert!(view["files_modified"].as_array().unwrap().len() < 3000);
        assert_eq!(view["result_summary_truncated"], true);
    }

    #[tokio::test]
    async fn a_completed_result_tells_the_client_how_to_open_the_pull_request() {
        let server = server();
        let id = finished(&server, completed(big_result()), None).await;
        let view = body(&fetch(&server, "get_task_result", json!({"task_id": id.0})).await);
        let next = view["next"].as_str().unwrap();
        assert!(next.contains("get_task_changes"), "{next}");
        assert!(next.contains(&format!("nanna/{}", id.0)), "{next}");
        assert!(next.contains("draft pull request"), "{next}");
        assert!(next.contains("suggested_pr_title"), "{next}");
        assert!(
            next.contains("Nanna cannot push or open pull requests"),
            "{next}"
        );
        assert!(next.contains("no GitHub tool"), "{next}");
        assert!(next.len() < 900, "{}", next.len());
    }

    #[tokio::test]
    async fn a_completed_result_without_file_changes_is_not_presented_as_an_implementation() {
        let server = server();
        let mut result = big_result();
        result.files_modified = vec![];
        result.changes_patch = None;
        result.format_patch = None;
        let id = finished(&server, completed(result), None).await;
        let view = body(&fetch(&server, "get_task_result", json!({"task_id": id.0})).await);
        assert_eq!(view["status"], "completed");
        assert_eq!(view["no_changes"], true);
        let next = view["next"].as_str().unwrap();
        assert!(next.contains("without changing any files"), "{next}");
        assert!(next.contains("no pull request to open"), "{next}");
        assert!(!next.contains("create branch"), "{next}");
        let with_changes = finished(&server, completed(big_result()), None).await;
        let view = body(
            &fetch(
                &server,
                "get_task_result",
                json!({"task_id": with_changes.0}),
            )
            .await,
        );
        assert_eq!(view["no_changes"], false);
    }

    #[test]
    fn a_model_timeout_is_retryable_and_says_the_machine_is_busy() {
        for message in [
            "Agent state error: LLM perform failed after 3 attempts: Service unavailable: Request timeout",
            "Agent state error: LLM perform failed after 3 attempts: Service unavailable: connection refused",
        ] {
            let (code, retryable, fix) = classify_failure("StateError", message);
            assert_eq!(code, "model_unavailable", "{message}");
            assert!(retryable, "{message}");
            assert!(fix.contains("busy"), "{fix}");
            assert!(fix.contains("new client_ref"), "{fix}");
        }
        let (code, retryable, _) =
            classify_failure("StateError", "Agent state error: unexpected finish reason");
        assert_eq!(code, "StateError");
        assert!(!retryable);
    }

    #[test]
    fn a_prompt_injection_refusal_asks_for_a_plain_description_not_less_detail() {
        let (code, retryable, fix) = classify_failure(
            "SpawnRefused",
            "spawn refused: block: prompt_injection: The subtask text contains extensive, directive instructions",
        );
        assert_eq!(code, "prompt_injection");
        assert!(!retryable);
        assert!(fix.contains("plain terms"), "{fix}");
        assert!(fix.contains("file"), "{fix}");
        assert!(fix.contains("new client_ref"), "{fix}");
        assert!(!fix.contains("Remove instructions"), "{fix}");
    }

    #[tokio::test]
    async fn a_failed_result_does_not_offer_pull_request_steps() {
        let server = server();
        let id = assign(&server, "d", json!({})).await;
        let diagnostics = FailureDiagnostics {
            changes: None,
            tools_unavailable: vec![],
            error_type: "SpawnRefused".to_string(),
            iterations_completed: 0,
            last_tool_call: None,
            partial_changes: None,
            tool_call_history: vec![],
            last_agent_state: None,
            conversation_snapshot: None,
            denials: vec![],
            action_audit: vec![],
        };
        let refused = TaskStatus::Failed {
            finished_at: chrono::Utc::now(),
            error: "spawn refused: block: unknown".to_string(),
            diagnostics,
        };
        server.task_manager.finish_for_test(&id, refused).await;
        let view = body(&fetch(&server, "get_task_result", json!({"task_id": id.0})).await);
        let text = view.to_string();
        assert!(!text.contains("draft pull request"), "{text}");
    }

    #[tokio::test]
    async fn closes_flows_into_the_suggested_pr_body() {
        let server = server();
        let mut result = big_result();
        result.result_summary = "Added the thing.".to_string();
        let id = finished(&server, completed(result), None).await;
        let view = body(&fetch(&server, "get_task_result", json!({"task_id": id.0})).await);
        assert_eq!(view["closes"], "#462");
        assert_eq!(view["suggested_pr_body"], "Added the thing.\n\nCloses #462");
        let plain = assign(&server, "no ref", json!({})).await;
        server
            .task_manager
            .finish_for_test(&plain, completed(big_result()))
            .await;
        let view = body(&fetch(&server, "get_task_result", json!({"task_id": plain.0})).await);
        assert_eq!(view["closes"], Value::Null);
        assert!(!view["suggested_pr_body"]
            .as_str()
            .unwrap()
            .contains("Closes"));
    }

    #[tokio::test]
    async fn an_invalid_closes_is_invalid_params() {
        let server = server();
        let resp = server
            .handle_request(call(
                "assign_task",
                json!({
                    "description": "d",
                    "repo_path": crate::auditor::test_support::shared_repo().to_str().unwrap(),
                    "identity": "rust-implementer",
                    "closes": "a\nb",
                }),
            ))
            .await;
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("closes"));
    }

    #[tokio::test]
    async fn detail_full_returns_the_whole_payload_with_truncation_markers() {
        let server = server();
        let capture = ChangeCapture {
            base_ref: None,
            files: vec![],
            patch_original_bytes: Some(2_500_000),
        };
        let id = finished(&server, completed(big_result()), Some(capture)).await;
        let result = fetch(
            &server,
            "get_task_result",
            json!({"task_id": id.0, "detail": "full"}),
        )
        .await;
        let full = body(&result);
        assert!(full["format_patch"].is_string());
        assert!(full["changes_patch"].is_string());
        assert!(full["tool_calls_made"].is_array());
        assert!(full["action_audit"].is_array());
        assert_eq!(full["patch_truncated"], true);
        assert_eq!(full["patch_original_bytes"], 2_500_000);
        let compact = body(&fetch(&server, "get_task_result", json!({"task_id": id.0})).await);
        assert_eq!(compact["patch_truncated"], true);
        assert_eq!(compact["patch_original_bytes"], 2_500_000);
    }

    #[tokio::test]
    async fn an_unknown_detail_is_invalid_params() {
        let server = server();
        let id = finished(&server, completed(big_result()), None).await;
        let resp = server
            .handle_request(call(
                "get_task_result",
                json!({"task_id": id.0, "detail": "everything"}),
            ))
            .await;
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn tasks_result_still_returns_the_full_payload() {
        let server = server();
        let id = finished(&server, completed(big_result()), None).await;
        let status = server.task_manager.poll(&id).await.unwrap().status;
        let result = handlers::task_result_to_call_tool_result(&id.0, &status);
        let full: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(full["format_patch"].is_string());
        assert!(full.get("patch_truncated").is_none());
        assert!(result["_meta"]["io.modelcontextprotocol/related-task"].is_object());
    }

    fn unavailable() -> Vec<crate::exec_capabilities::ToolsUnavailable> {
        vec![crate::exec_capabilities::ToolsUnavailable {
            capability: "cargo".to_string(),
            tools: vec!["cargo_check".to_string()],
            reason: "CARGO_NET_OFFLINE=true is not set for the harness".to_string(),
        }]
    }

    #[tokio::test]
    async fn the_compact_result_and_status_show_tools_unavailable_only_when_present() {
        let server = server();
        let mut with = big_result();
        with.tools_unavailable = unavailable();
        let flagged = finished(&server, completed(with), None).await;
        let plain = finished(&server, completed(big_result()), None).await;
        let expected = json!([{
            "capability": "cargo",
            "tools": ["cargo_check"],
            "reason": "CARGO_NET_OFFLINE=true is not set for the harness",
        }]);
        for tool in ["get_task_result", "get_task_status"] {
            let view = body(&fetch(&server, tool, json!({"task_id": flagged.0})).await);
            assert_eq!(view["tools_unavailable"], expected, "{tool}");
            let view = body(&fetch(&server, tool, json!({"task_id": plain.0})).await);
            assert!(view.get("tools_unavailable").is_none(), "{tool}");
        }
    }

    #[tokio::test]
    async fn a_failed_task_shows_tools_unavailable_in_result_and_status() {
        let server = server();
        let id = assign(&server, "d", json!({})).await;
        let diagnostics = FailureDiagnostics {
            changes: None,
            tools_unavailable: unavailable(),
            error_type: "MaxIterationsExceeded".to_string(),
            iterations_completed: 3,
            last_tool_call: None,
            partial_changes: None,
            tool_call_history: vec![],
            last_agent_state: None,
            conversation_snapshot: None,
            denials: vec![],
            action_audit: vec![],
        };
        let status = TaskStatus::Failed {
            finished_at: chrono::Utc::now(),
            error: "ran out of iterations".to_string(),
            diagnostics,
        };
        server.task_manager.finish_for_test(&id, status).await;
        for tool in ["get_task_result", "get_task_status"] {
            let view = body(&fetch(&server, tool, json!({"task_id": id.0})).await);
            assert_eq!(
                view["tools_unavailable"][0]["tools"][0], "cargo_check",
                "{tool}"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_task_reports_reason_code_retryable_and_fix() {
        let server = server();
        let id = assign(&server, "d", json!({})).await;
        let diagnostics = FailureDiagnostics {
            changes: None,
            tools_unavailable: vec![],
            error_type: "SpawnRefused".to_string(),
            iterations_completed: 0,
            last_tool_call: None,
            partial_changes: None,
            tool_call_history: vec![],
            last_agent_state: None,
            conversation_snapshot: None,
            denials: vec![],
            action_audit: vec![],
        };
        let refused = TaskStatus::Failed {
            finished_at: chrono::Utc::now(),
            error: "spawn refused: block: effect_above_ceiling: needs `repository` (inferred from \"push the branch\" in the task text); `rust-implementer` is capped at `workspace`".to_string(),
            diagnostics,
        };
        server.task_manager.finish_for_test(&id, refused).await;
        let status = body(&fetch(&server, "get_task_status", json!({"task_id": id.0})).await);
        assert_eq!(status["status"], "failed");
        assert_eq!(status["identity"], "rust-implementer");
        assert!(status["repo"].is_string());
        assert!(status["reason_code"]
            .as_str()
            .is_some_and(|c| !c.is_empty()));
        assert_eq!(status["retryable"], false);
        assert!(status["fix"].as_str().is_some_and(|f| !f.is_empty()));
        assert!(status["message"]
            .as_str()
            .unwrap()
            .contains("spawn refused"));
        let result = fetch(&server, "get_task_result", json!({"task_id": id.0})).await;
        assert_eq!(result["isError"], true);
        let view = body(&result);
        assert_eq!(view["reason_code"], status["reason_code"]);
        assert_eq!(view["retryable"], false);
        assert!(view["fix"].is_string());
    }

    #[tokio::test]
    async fn a_task_that_made_no_progress_reports_no_progress_retryable_with_a_fix() {
        let server = server();
        let id = assign(&server, "d", json!({})).await;
        let diagnostics = FailureDiagnostics {
            changes: None,
            tools_unavailable: vec![],
            error_type: "NoProgress".to_string(),
            iterations_completed: 4,
            last_tool_call: None,
            partial_changes: None,
            tool_call_history: vec![],
            last_agent_state: None,
            conversation_snapshot: None,
            denials: vec![],
            action_audit: vec![],
        };
        let status = TaskStatus::Failed {
            finished_at: chrono::Utc::now(),
            error: "Agent made no progress: it stopped 2 time(s) after being prompted without changing any files. Last message: I will add a test".to_string(),
            diagnostics,
        };
        server.task_manager.finish_for_test(&id, status).await;
        let view = body(&fetch(&server, "get_task_status", json!({"task_id": id.0})).await);
        assert_eq!(view["reason_code"], "no_progress");
        assert_eq!(view["retryable"], true);
        let fix = view["fix"].as_str().unwrap();
        assert!(fix.contains("last tool error"), "{fix}");
        assert!(!fix.contains("name the file"), "{fix}");
        let result = body(&fetch(&server, "get_task_result", json!({"task_id": id.0})).await);
        assert_eq!(result["reason_code"], "no_progress");
        assert_eq!(result["retryable"], true);
    }

    #[tokio::test]
    async fn a_transient_audit_failure_is_retryable() {
        let server = server();
        let id = assign(&server, "d", json!({})).await;
        let diagnostics = FailureDiagnostics {
            changes: None,
            tools_unavailable: vec![],
            error_type: "SpawnAuditFailed".to_string(),
            iterations_completed: 0,
            last_tool_call: None,
            partial_changes: None,
            tool_call_history: vec![],
            last_agent_state: None,
            conversation_snapshot: None,
            denials: vec![],
            action_audit: vec![],
        };
        let status = TaskStatus::Failed {
            finished_at: chrono::Utc::now(),
            error: "spawn refused: audit failed: auditor model unavailable: timed out".to_string(),
            diagnostics,
        };
        server.task_manager.finish_for_test(&id, status).await;
        let view = body(&fetch(&server, "get_task_result", json!({"task_id": id.0})).await);
        assert_eq!(view["reason_code"], "SpawnAuditFailed");
        assert_eq!(view["retryable"], true);
        let status = body(&fetch(&server, "get_task_status", json!({"task_id": id.0})).await);
        assert_eq!(status["retryable"], true);
    }

    #[tokio::test]
    async fn get_task_changes_pages_files_and_reports_every_change_kind() {
        let server = server();
        let mut binary = file("img.bin", "");
        binary.binary = true;
        binary.content = Some(vec![0, 1, 2, 255]);
        binary.size = 4;
        let mut deleted = file("gone.rs", "");
        deleted.status = ChangeStatus::Deleted;
        deleted.content = None;
        deleted.size = 0;
        let mut renamed = file("to.rs", "fn a() {}\n");
        renamed.status = ChangeStatus::Renamed;
        renamed.old_path = Some("from.rs".to_string());
        let capture = ChangeCapture {
            base_ref: Some("abc".to_string()),
            files: vec![file("a.rs", "hello"), binary, deleted, renamed],
            patch_original_bytes: None,
        };
        let id = finished(&server, completed(big_result()), Some(capture)).await;
        let first = body(
            &fetch(
                &server,
                "get_task_changes",
                json!({"task_id": id.0, "limit": 2}),
            )
            .await,
        );
        assert_eq!(first["base_ref"], "abc");
        assert_eq!(first["next_offset"], 2);
        assert_eq!(first["files"][0]["content"], "hello");
        assert_eq!(first["files"][1]["content_base64"], "AAEC/w==");
        let second = body(
            &fetch(
                &server,
                "get_task_changes",
                json!({"task_id": id.0, "offset": 2}),
            )
            .await,
        );
        assert_eq!(second["next_offset"], Value::Null);
        assert_eq!(second["files"][0]["status"], "deleted");
        assert_eq!(second["files"][1]["status"], "renamed");
        let one = body(
            &fetch(
                &server,
                "get_task_changes",
                json!({"task_id": id.0, "path": "a.rs", "offset": 1, "limit": 2}),
            )
            .await,
        );
        assert_eq!(one["files"][0]["content"], "el");
        assert_eq!(one["next_offset"], 3);
    }

    #[tokio::test]
    async fn get_task_changes_answers_unknown_working_and_empty_tasks_clearly() {
        let server = server();
        let resp = server
            .handle_request(call("get_task_changes", json!({"task_id": "nope"})))
            .await;
        let error = resp.error.unwrap();
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("not found"));
        let working = assign(&server, "d", json!({})).await;
        let result = fetch(&server, "get_task_changes", json!({"task_id": working.0})).await;
        assert_eq!(result["isError"], true);
        assert_eq!(body(&result)["status"], "working");
        let empty = finished(&server, completed(big_result()), None).await;
        let result = fetch(&server, "get_task_changes", json!({"task_id": empty.0})).await;
        assert_eq!(result["isError"], true);
        assert_eq!(body(&result)["status"], "no_changes");
        let with_capture = finished(
            &server,
            completed(big_result()),
            Some(ChangeCapture {
                base_ref: None,
                files: vec![file("a", "x")],
                patch_original_bytes: None,
            }),
        )
        .await;
        let resp = server
            .handle_request(call(
                "get_task_changes",
                json!({"task_id": with_capture.0, "path": "missing"}),
            ))
            .await;
        assert_eq!(resp.error.unwrap().code, -32602);
        let resp = server
            .handle_request(call(
                "get_task_changes",
                json!({"task_id": with_capture.0, "offset": "x"}),
            ))
            .await;
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    async fn server_over(dir: &std::path::Path) -> NannaMcpServer {
        let manager = Arc::new(TaskManager::new(0));
        let store = Arc::new(
            crate::result_store::ResultStore::open(dir, std::time::Duration::from_secs(86400))
                .unwrap(),
        );
        manager.attach_result_store(store).await;
        NannaMcpServer::new(
            manager,
            Arc::new(NoopProvider),
            crate::auditor::test_support::fixture_gate(),
            100,
        )
    }

    fn mixed_capture() -> ChangeCapture {
        let mut binary = file("img.bin", "");
        binary.binary = true;
        binary.content = Some(vec![0, 1, 2, 255]);
        binary.size = 4;
        let mut deleted = file("gone.rs", "");
        deleted.status = ChangeStatus::Deleted;
        deleted.content = None;
        deleted.size = 0;
        let mut renamed = file("to.rs", "fn a() {}\n");
        renamed.status = ChangeStatus::Renamed;
        renamed.old_path = Some("from.rs".to_string());
        let mut truncated = file("big.txt", "caf\u{e9} caf\u{e9} ");
        truncated.size = 9_000_000;
        truncated.capture_truncated = true;
        ChangeCapture {
            base_ref: Some("abc".to_string()),
            files: vec![file("a.rs", "hello"), binary, deleted, renamed, truncated],
            patch_original_bytes: Some(70_000),
        }
    }

    async fn every_page(server: &NannaMcpServer, id: &TaskId) -> Vec<Value> {
        let mut pages = Vec::new();
        for args in [
            json!({"task_id": id.0, "limit": 2}),
            json!({"task_id": id.0, "offset": 2, "limit": 2}),
            json!({"task_id": id.0, "offset": 4}),
            json!({"task_id": id.0, "path": "big.txt", "offset": 3, "limit": 5}),
            json!({"task_id": id.0, "path": "img.bin"}),
        ] {
            pages.push(body(&fetch(server, "get_task_changes", args).await));
        }
        pages
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_task_changes_and_closes_survive_a_restart_with_identical_paging() {
        let dir = tempfile::tempdir().unwrap();
        let first = server_over(dir.path()).await;
        let id = finished(&first, completed(big_result()), Some(mixed_capture())).await;
        let before = every_page(&first, &id).await;
        let result_before = body(&fetch(&first, "get_task_result", json!({"task_id": id.0})).await);
        drop(first);

        let second = server_over(dir.path()).await;
        assert_eq!(every_page(&second, &id).await, before);
        assert_eq!(before[0]["files"][1]["content_base64"], "AAEC/w==");
        assert_eq!(before[1]["files"][1]["status"], "renamed");
        assert_eq!(before[3]["files"][0]["truncated"], true);
        let result_after = body(&fetch(&second, "get_task_result", json!({"task_id": id.0})).await);
        assert_eq!(result_after["closes"], "#462");
        assert_eq!(result_after, result_before);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_damaged_sidecar_after_restart_answers_not_retained_and_keeps_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let first = server_over(dir.path()).await;
        let id = finished(&first, completed(big_result()), Some(mixed_capture())).await;
        drop(first);
        let sidecar = dir
            .path()
            .join(crate::result_store::RESULTS_DIR)
            .join(format!("{}.changes.json", id.0));
        std::fs::write(&sidecar, b"{broken").unwrap();

        let second = server_over(dir.path()).await;
        let changes = fetch(&second, "get_task_changes", json!({"task_id": id.0})).await;
        assert_eq!(changes["isError"], true);
        let view = body(&changes);
        assert_eq!(view["status"], "changes_not_retained");
        assert!(view["message"].as_str().unwrap().contains("not retained"));
        let result = fetch(&second, "get_task_result", json!({"task_id": id.0})).await;
        assert_eq!(result["isError"], false);
        assert_eq!(body(&result)["status"], "completed");
    }

    #[test]
    fn extending_the_tool_list_adds_the_closes_detail_and_changes_schemas() {
        let mut tools = vec![
            json!({"name": "assign_task", "inputSchema": {"properties": {}}}),
            json!({"name": "get_task_result", "description": "", "inputSchema": {"properties": {}}}),
            json!({"name": "get_task_status", "description": ""}),
        ];
        extend_tool_list(&mut tools);
        assert_eq!(
            tools[0]["inputSchema"]["properties"]["closes"]["type"],
            "string"
        );
        assert_eq!(
            tools[1]["inputSchema"]["properties"]["detail"]["enum"],
            json!(["compact", "full"])
        );
        let changes = tools.last().unwrap();
        assert_eq!(changes["name"], "get_task_changes");
        assert_eq!(changes["inputSchema"]["required"], json!(["task_id"]));
    }
}
