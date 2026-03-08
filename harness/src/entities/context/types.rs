//! Context entity types
//!
//! Defines context entity type for storing agent run history, conversation,
//! and tool call records. Implementation tracked in issue #26.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use model::types::ChatMessage;
use serde::{Deserialize, Serialize};

/// Record of a single tool call made during an agent run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub call_id: String,
    pub result: String,
}

/// Project context entity — persists the history of a completed agent run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    pub task_description: String,
    pub conversation: Vec<ChatMessage>,
    pub tool_calls_made: Vec<ToolCallRecord>,
    pub result_summary: String,
    pub model_used: String,
}

#[async_trait]
impl Entity for ContextEntity {
    fn metadata(&self) -> &EntityMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.metadata
    }

    fn to_json(&self) -> EntityResult<String> {
        serde_json::to_string(self)
            .map_err(|e| crate::entities::EntityError::SerializationError(e.to_string()))
    }
}

impl ContextEntity {
    pub fn new(
        task_description: String,
        conversation: Vec<ChatMessage>,
        tool_calls_made: Vec<ToolCallRecord>,
        result_summary: String,
        model_used: String,
    ) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Context),
            task_description,
            conversation,
            tool_calls_made,
            result_summary,
            model_used,
        }
    }
}

impl Default for ContextEntity {
    fn default() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Context),
            task_description: String::new(),
            conversation: Vec::new(),
            tool_calls_made: Vec::new(),
            result_summary: String::new(),
            model_used: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::{ChatMessage, MessageRole};

    #[test]
    fn test_context_entity_full_round_trip() {
        let tool_call = ToolCallRecord {
            tool_name: "echo".to_string(),
            arguments: serde_json::json!({"message": "hello"}),
            call_id: "call_0".to_string(),
            result: "echoed: hello".to_string(),
        };

        let entity = ContextEntity::new(
            "Test task".to_string(),
            vec![
                ChatMessage::user("hello"),
                ChatMessage::assistant("hi there"),
            ],
            vec![tool_call],
            "Task completed".to_string(),
            "qwen2.5:0.5b".to_string(),
        );

        let json = entity.to_json().unwrap();
        let deserialized: ContextEntity = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.task_description, "Test task");
        assert_eq!(deserialized.conversation.len(), 2);
        assert_eq!(deserialized.tool_calls_made.len(), 1);
        assert_eq!(deserialized.result_summary, "Task completed");
        assert_eq!(deserialized.model_used, "qwen2.5:0.5b");
        assert_eq!(deserialized.tool_calls_made[0].tool_name, "echo");
    }

    #[test]
    fn test_context_entity_metadata_type() {
        let entity = ContextEntity::default();
        assert_eq!(entity.metadata().entity_type, EntityType::Context);
    }

    #[test]
    fn test_tool_call_record_serialization() {
        let record = ToolCallRecord {
            tool_name: "calculate".to_string(),
            arguments: serde_json::json!({"operation": "add", "a": 1, "b": 2}),
            call_id: "call_1".to_string(),
            result: "3".to_string(),
        };

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: ToolCallRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.tool_name, "calculate");
        assert_eq!(deserialized.call_id, "call_1");
        assert_eq!(deserialized.result, "3");
        assert_eq!(deserialized.arguments["operation"], "add");
    }

    #[test]
    fn test_context_entity_empty_fields() {
        let entity = ContextEntity::new(
            String::new(),
            Vec::new(),
            Vec::new(),
            String::new(),
            String::new(),
        );

        assert!(entity.conversation.is_empty());
        assert!(entity.tool_calls_made.is_empty());
        assert!(entity.to_json().is_ok());
    }

    #[test]
    fn test_context_entity_default_is_backward_compat() {
        let entity = ContextEntity::default();
        assert_eq!(entity.entity_type(), EntityType::Context);
        assert!(entity.to_json().is_ok());
    }

    #[test]
    fn test_conversation_roles_preserved() {
        let messages = vec![
            ChatMessage::system("Be helpful"),
            ChatMessage::user("Do X"),
            ChatMessage::assistant("Done"),
        ];

        let entity = ContextEntity::new(
            "task".to_string(),
            messages,
            vec![],
            "done".to_string(),
            "model".to_string(),
        );

        let json = entity.to_json().unwrap();
        let back: ContextEntity = serde_json::from_str(&json).unwrap();

        assert_eq!(back.conversation[0].role, MessageRole::System);
        assert_eq!(back.conversation[1].role, MessageRole::User);
        assert_eq!(back.conversation[2].role, MessageRole::Assistant);
    }
}
