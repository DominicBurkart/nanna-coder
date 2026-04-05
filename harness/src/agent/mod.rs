//! Agent architecture implementation
//!
//! This module implements the main agent control loop following ARCHITECTURE.md:
//!
//! 1. Application State 1 → **Entity Enrichment**
//! 2. Entity Enrichment → **Plan Entity Modification** ← User Prompt
//! 3. Plan Entity Modification → **Perform Entity Modification**
//! 4. Perform Entity Modification → **Update Entities**
//! 5. Update Entities → **Task Complete?**
//! 6. If Yes → Application State 2 (completed)
//! 7. If No → **Entity Modification Decision**
//! 8. Decision → **Query Entities (RAG)** → back to Decision
//! 9. Decision → **Plan Entity Modification** (loop)

pub mod eval;
pub mod eval_case;
pub mod prompts;

use crate::entities::context::types::{ContextEntity, ToolCallRecord};
use crate::entities::{EntityStore, InMemoryEntityStore};
use crate::tools::ToolRegistry;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thiserror::Error;

use model::provider::ModelProvider;
use model::types::{ChatMessage, ChatRequest, ChatResponse, FinishReason, MessageRole};

const MAX_LLM_RESPONSE_LENGTH: usize = 2000;
const DEFAULT_PLANNING_RAG_LIMIT: usize = 10;
const DEFAULT_QUERY_RAG_LIMIT: usize = 5;
const PLANNING_TEMPERATURE: f32 = 0.7;
const COMPLETION_TEMPERATURE: f32 = 0.2;
const DECISION_TEMPERATURE: f32 = 0.3;
const DEFAULT_MODEL: &str = "qwen2.5:0.5b";
const MAX_TOOL_ITERATIONS: usize = 10;
