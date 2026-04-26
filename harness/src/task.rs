use crate::agent::{AgentConfig, AgentContext, AgentError, AgentLoop};
use crate::entities::context::types::ToolCallRecord;
use crate::entities::InMemoryEntityStore;
use crate::workspace::TaskWorkspace;
use chrono::{DateTime, Utc};
use model::provider::ModelProvider;
use model::types::ChatMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, Semaphore};
use uuid::Uuid;

const MAX_DIFF_BYTES: usize = 1_000_000;
pub const DEFAULT_MAX_CONCURRENT_TASKS: usize = 8;

// DEFAULT_SYSTEM_PROMPT is defined in crate::agent and re-exported via
// crate::agent::DEFAULT_SYSTEM_PROMPT; no local copy needed.

/// Build the system prompt for a task run, appending any repo-level guidance
/// discovered under the task's workspace path (closes #231).
///
/// Precedence: `AGENTS.md` over `CLAUDE.md` (see
/// [`crate::agent::agents_md::load`]). Missing files produce no injection;
/// read errors are logged and swallowed so a broken guidance file never blocks
/// a task from starting.
fn build_task_system_prompt(workspace_path: &std::path::Path) -> String {
    match crate::agent::agents_md::load(workspace_path) {
        Ok(Some(doc)) => {
            tracing::info!(
                path = %doc.path.display(),
                source = doc.source.filename(),
                truncated = doc.truncated,
                "Loaded repo-level agent guidance into task system prompt"
            );
            format!(
                "{}\n\n{}",
                crate::agent::DEFAULT_SYSTEM_PROMPT,
                crate::agent::agents_md::format_system_prompt_fragment(&doc)
            )
        }
        Ok(None) => crate::agent::DEFAULT_SYSTEM_PROMPT.to_string(),
        Err(e) => {
            tracing::error!(
                error = %e,
                "Failed to read AGENTS.md / CLAUDE.md for task; continuing without repo guidance"
            );
            crate::agent::DEFAULT_SYSTEM_PROMPT.to_string()
        }
    }
}
