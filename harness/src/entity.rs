//! Entity management and graph-based RAG system for the agent.
//!
//! This module defines the core entity types that the agent operates on,
//! and provides a graph-based system for entity relationships and querying.
//! The entity graph uses `petgraph` for efficient graph operations and
//! enables RAG (Retrieval-Augmented Generation) capabilities.

use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during entity operations
#[derive(Error, Debug)]
pub enum EntityError {
    /// Entity not found in the graph
    #[error("Entity not found: {id}")]
    NotFound { id: String },

    /// Invalid entity operation
    #[error("Invalid operation: {message}")]
    InvalidOperation { message: String },

    /// Query execution failed
    #[error("Query failed: {reason}")]
    QueryFailed { reason: String },

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type EntityResult<T> = Result<T, EntityError>;

/// Types of entities that the agent can work with
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityType {
    /// Source code file
    File,
    /// Container image
    Container,
    /// Compiled binary
    Binary,
    /// Configuration file
    Config,
    /// Test file
    Test,
    /// Documentation
    Documentation,
    /// Build artifact
    Artifact,
}

/// Core entity representation in the agent's knowledge graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Unique identifier for the entity
    pub id: String,
    /// Type of the entity
    pub entity_type: EntityType,
    /// Human-readable name
    pub name: String,
    /// Path to the entity (if applicable)
    pub path: Option<PathBuf>,
    /// Metadata associated with the entity
    pub metadata: EntityMetadata,
}

impl Entity {
    /// Create a new entity
    pub fn new(id: String, entity_type: EntityType, name: String) -> Self {
        Self {
            id,
            entity_type,
            name,
            path: None,
            metadata: EntityMetadata::default(),
        }
    }

    /// Set the path for this entity
    pub fn with_path(mut self, path: PathBuf) -> Self {
        self.path = Some(path);
        self
    }

    /// Set metadata for this entity
    pub fn with_metadata(mut self, metadata: EntityMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Rich metadata for entities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityMetadata {
    /// Creation timestamp
    pub created_at: Option<String>,
    /// Last modified timestamp
    pub modified_at: Option<String>,
    /// Size in bytes
    pub size: Option<u64>,
    /// Tags for categorization
    pub tags: Vec<String>,
    /// Key-value properties
    pub properties: HashMap<String, String>,
    /// Content hash for deduplication
    pub content_hash: Option<String>,
}

impl EntityMetadata {
    /// Create new metadata with tags
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Add a property
    pub fn add_property(&mut self, key: String, value: String) {
        self.properties.insert(key, value);
    }
}

/// Relationship types between entities
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityRelation {
    /// Entity A depends on Entity B
    DependsOn,
    /// Entity A contains Entity B
    Contains,
    /// Entity A is derived from Entity B
    DerivedFrom,
    /// Entity A modifies Entity B
    Modifies,
    /// Entity A references Entity B
    References,
    /// Entity A compiles to Entity B
    CompilesTo,
    /// Entity A is promoted to Entity B
    PromotesTo,
}

/// Graph-based entity storage with RAG capabilities
pub struct EntityGraph {
    /// The underlying directed graph
    graph: DiGraph<Entity, EntityRelation>,
    /// Index mapping entity IDs to graph nodes
    index: HashMap<String, NodeIndex>,
}

impl EntityGraph {
    /// Create a new empty entity graph
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            index: HashMap::new(),
        }
    }

    /// Add an entity to the graph
    pub fn add_entity(&mut self, entity: Entity) -> NodeIndex {
        let id = entity.id.clone();
        let node_idx = self.graph.add_node(entity);
        self.index.insert(id, node_idx);
        node_idx
    }

    /// Add a relationship between two entities
    pub fn add_relation(
        &mut self,
        from_id: &str,
        to_id: &str,
        relation: EntityRelation,
    ) -> EntityResult<()> {
        let from_idx = self
            .index
            .get(from_id)
            .ok_or_else(|| EntityError::NotFound {
                id: from_id.to_string(),
            })?;
        let to_idx = self.index.get(to_id).ok_or_else(|| EntityError::NotFound {
            id: to_id.to_string(),
        })?;

        self.graph.add_edge(*from_idx, *to_idx, relation);
        Ok(())
    }

    /// Get an entity by ID
    pub fn get_entity(&self, id: &str) -> EntityResult<&Entity> {
        let node_idx = self
            .index
            .get(id)
            .ok_or_else(|| EntityError::NotFound { id: id.to_string() })?;
        Ok(&self.graph[*node_idx])
    }

    /// Get a mutable reference to an entity
    pub fn get_entity_mut(&mut self, id: &str) -> EntityResult<&mut Entity> {
        let node_idx = self
            .index
            .get(id)
            .ok_or_else(|| EntityError::NotFound { id: id.to_string() })?;
        Ok(&mut self.graph[*node_idx])
    }

    /// Query entities by type
    ///
    /// # Implementation Note
    /// The actual query implementation is not yet defined and will use
    /// graph traversal algorithms for RAG-based retrieval.
    pub fn query_by_type(&self, _entity_type: EntityType) -> EntityResult<Vec<&Entity>> {
        unimplemented!("Entity querying by type requires further problem definition")
    }

    /// Query entities by relationship
    ///
    /// # Implementation Note
    /// This will use graph algorithms to find entities with specific relationships.
    pub fn query_by_relation(
        &self,
        _from_id: &str,
        _relation: EntityRelation,
    ) -> EntityResult<Vec<&Entity>> {
        unimplemented!("Entity querying by relation requires further problem definition")
    }

    /// Perform a RAG-based semantic query
    ///
    /// # Implementation Note
    /// This will integrate with vector embeddings and semantic search
    /// once the RAG system is fully defined.
    pub fn semantic_query(&self, _query: &str) -> EntityResult<Vec<&Entity>> {
        unimplemented!("Semantic entity querying requires further problem definition")
    }

    /// Get the total number of entities in the graph
    pub fn entity_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Get the total number of relationships in the graph
    pub fn relation_count(&self) -> usize {
        self.graph.edge_count()
    }
}

impl Default for EntityGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_creation() {
        let entity = Entity::new(
            "test-1".to_string(),
            EntityType::File,
            "main.rs".to_string(),
        );
        assert_eq!(entity.id, "test-1");
        assert_eq!(entity.entity_type, EntityType::File);
        assert_eq!(entity.name, "main.rs");
    }

    #[test]
    fn test_entity_with_path() {
        let entity = Entity::new(
            "test-1".to_string(),
            EntityType::File,
            "main.rs".to_string(),
        )
        .with_path(PathBuf::from("/src/main.rs"));

        assert_eq!(entity.path, Some(PathBuf::from("/src/main.rs")));
    }

    #[test]
    fn test_entity_graph_add() {
        let mut graph = EntityGraph::new();
        let entity = Entity::new(
            "test-1".to_string(),
            EntityType::File,
            "main.rs".to_string(),
        );

        graph.add_entity(entity);
        assert_eq!(graph.entity_count(), 1);
    }

    #[test]
    fn test_entity_graph_get() {
        let mut graph = EntityGraph::new();
        let entity = Entity::new(
            "test-1".to_string(),
            EntityType::File,
            "main.rs".to_string(),
        );

        graph.add_entity(entity);
        let retrieved = graph.get_entity("test-1").unwrap();
        assert_eq!(retrieved.id, "test-1");
    }

    #[test]
    fn test_entity_relation() {
        let mut graph = EntityGraph::new();

        let entity1 = Entity::new(
            "test-1".to_string(),
            EntityType::File,
            "main.rs".to_string(),
        );
        let entity2 = Entity::new("test-2".to_string(), EntityType::Binary, "main".to_string());

        graph.add_entity(entity1);
        graph.add_entity(entity2);
        graph
            .add_relation("test-1", "test-2", EntityRelation::CompilesTo)
            .unwrap();

        assert_eq!(graph.relation_count(), 1);
    }

    #[test]
    fn test_entity_not_found() {
        let graph = EntityGraph::new();
        let result = graph.get_entity("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_metadata_builder() {
        let metadata = EntityMetadata::default().with_tags(vec!["source".to_string()]);
        assert_eq!(metadata.tags.len(), 1);
        assert_eq!(metadata.tags[0], "source");
    }

    #[test]
    fn test_query_by_type_unimplemented() {
        let graph = EntityGraph::new();
        let result = std::panic::catch_unwind(|| graph.query_by_type(EntityType::File));
        assert!(result.is_err());
    }

    #[test]
    fn test_query_by_relation_unimplemented() {
        let graph = EntityGraph::new();
        let result = std::panic::catch_unwind(|| {
            graph.query_by_relation("test-1", EntityRelation::DependsOn)
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_semantic_query_unimplemented() {
        let graph = EntityGraph::new();
        let result = std::panic::catch_unwind(|| graph.semantic_query("test query"));
        assert!(result.is_err());
    }
}
