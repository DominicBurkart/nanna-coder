use super::handlers::{text_tool_result, wire_status};
use super::resolve::{self, Refusal};
use crate::auditor::SpawnGate;
use crate::identity::{AgentIdentity, AUDITOR_IDENTITY};
use crate::task::{Task, TaskManager};
use serde_json::{json, Value};
use std::path::Path;

pub const DESCRIPTION_LIMIT: usize = 200;

const STATUSES: [&str; 4] = ["working", "completed", "failed", "cancelled"];

pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_identities",
            "description": "List the agent identities assign_task can run a task under. Call this before assign_task to learn each identity's name, development loop, model, effect ceiling (`max_effect`), allowed repositories (`scope.repos`), writable paths, tools and limits. Identities are capped by their effect ceiling: a request above it is refused, so pick an identity whose `max_effect` covers the task. System prompts are never disclosed.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "describe_identity",
            "description": "Describe one agent identity: loop, model, effect ceiling, scope (repos, paths, tools) and limits. Pass `repo_path` to see the card as it applies to that repository, after any narrowing from the repository's own `.nanna/agents` overrides. An unknown name lists the known identities.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Identity name, as returned by list_identities" },
                    "repo_path": { "type": "string", "description": "Optional absolute path on the server to a repository whose per-repo overrides should be applied" }
                },
                "required": ["name"]
            }
        },
        {
            "name": "list_tasks",
            "description": "List the tasks this server knows about, optionally filtered by the identity they run under and by status (working, completed, failed, cancelled). Returns id, status, created_at, updated_at, identity_hint and a description truncated to 200 characters. Use get_task_status or get_task_result with an id for details.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "identity": { "type": "string", "description": "Only tasks running under this identity" },
                    "status": { "type": "string", "enum": STATUSES, "description": "Only tasks in this status" }
                }
            }
        }
    ])
}

pub fn is_introspection_tool(name: &str) -> bool {
    matches!(name, "list_identities" | "describe_identity" | "list_tasks")
}

fn card(identity: &AgentIdentity) -> Value {
    json!({
        "name": identity.name(),
        "role": role(identity.name()),
        "assignable": resolve::is_assignable(identity.name()),
        "description": identity.identity.description,
        "loop": identity.identity.dev_loop.to_string(),
        "model": identity.identity.model,
        "max_effect": identity.scope.max_effect.to_string(),
        "scope": {
            "repos": identity.scope.repos,
            "paths": identity.scope.paths,
            "read_paths": identity.scope.read_paths,
            "tools": identity.scope.tools.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
        },
        "limits": identity.limits,
    })
}

fn role(name: &str) -> &'static str {
    if name == AUDITOR_IDENTITY {
        "auditor"
    } else {
        "agent"
    }
}

pub fn list_identities(gate: &SpawnGate) -> Value {
    let cards: Vec<Value> = gate.context().catalog().iter().map(card).collect();
    text_tool_result(&json!({ "identities": cards }))
}

pub fn annotate_assign_task(tools: &mut Value, gate: &SpawnGate) {
    let catalog = gate.context().catalog();
    let assignable = resolve::assignable_identities(catalog);
    let listing: Vec<String> = assignable
        .iter()
        .map(|card| {
            format!(
                "{} (max_effect {}; repos: {})",
                card.name(),
                card.scope.max_effect,
                if card.scope.repos.is_empty() {
                    "none".to_string()
                } else {
                    card.scope.repos.join(", ")
                }
            )
        })
        .collect();
    let description = format!(
        "Name of an assignable agent identity. The task runs under that identity's scope (tools, effect ceiling, paths, repos). Optional when exactly one identity is assignable. Assignable identities: {}.",
        if listing.is_empty() { "none".to_string() } else { listing.join("; ") }
    );
    let names: Vec<&str> = assignable.iter().map(|card| card.name()).collect();
    let Some(tool) = tools
        .as_array_mut()
        .and_then(|list| list.iter_mut().find(|t| t["name"] == "assign_task"))
    else {
        return;
    };
    let property = &mut tool["inputSchema"]["properties"]["identity"];
    property["description"] = Value::String(description);
    if !names.is_empty() {
        property["enum"] = json!(names);
    }
}

pub fn describe_identity(gate: &SpawnGate, params: &Value) -> Result<Value, Refusal> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| "Missing required argument: name".to_string())?;
    let global = gate.context().catalog();
    let unknown = || {
        let known = resolve::assignable_identities(global)
            .iter()
            .map(|i| i.name())
            .collect::<Vec<_>>()
            .join(", ");
        Refusal::new(
            "unknown_identity",
            format!("Unknown identity `{name}`; known identities: {known}"),
            "Pass one of the names in assignable_identities.",
        )
        .with("assignable_identities", resolve::identity_summaries(global))
    };
    if global.get(name).is_none() {
        return Err(unknown());
    }
    let layered;
    let catalog = match params.get("repo_path").and_then(Value::as_str) {
        Some(repo) => {
            layered = gate
                .context()
                .with_repo_overrides(Path::new(repo))
                .map_err(|e| format!("repo-local identity overrides rejected: {e}"))?;
            layered.catalog()
        }
        None => global,
    };
    let identity = catalog.get(name).ok_or_else(unknown)?;
    Ok(text_tool_result(&card(identity)))
}

fn truncate(text: &str) -> String {
    text.chars().take(DESCRIPTION_LIMIT).collect()
}

fn task_row(task: &Task) -> Value {
    json!({
        "id": task.id.0,
        "status": wire_status(&task.status).0,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.last_updated_at.to_rfc3339(),
        "identity": task.identity_hint,
        "identity_hint": task.identity_hint,
        "description": truncate(&task.description),
    })
}

pub async fn list_tasks(manager: &TaskManager, params: &Value) -> Result<Value, Refusal> {
    let status = match params.get("status").and_then(Value::as_str) {
        Some(s) if STATUSES.contains(&s) => Some(s),
        Some(other) => {
            return Err(Refusal::new(
                "invalid_arguments",
                format!(
                    "Unknown status `{other}`; expected one of: {}",
                    STATUSES.join(", ")
                ),
                "Pass one of valid_statuses or omit `status`.",
            )
            .with("valid_statuses", json!(STATUSES)))
        }
        None => None,
    };
    let mut tasks = match params.get("identity").and_then(Value::as_str) {
        Some(identity) => manager.list_for_identity(identity).await,
        None => manager.list().await,
    };
    tasks.sort_by_key(|t| t.created_at);
    let rows: Vec<Value> = tasks
        .iter()
        .filter(|t| status.is_none_or(|s| wire_status(&t.status).0 == s))
        .map(task_row)
        .collect();
    Ok(text_tool_result(&json!({ "tasks": rows })))
}
