//! Entity management system
//!
//! This module implements the entity management system which forms the core domain
//! complexity of Nanna. Entities represent all development artifacts and their relationships:
//!
//! - Version control state (git)
//! - Code structure (AST)
//! - Test results and analysis
//! - Environment and deployment configuration
//! - Project context and conversation history
//! - Telemetry and observability (future)
//!
//! See ARCHITECTURE.md for the complete entity management architecture.

pub mod ast;
pub mod context;
pub mod env;
pub mod git;
pub mod telemetry;
pub mod test;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use uuid::Uuid;

/// Unique identifier for entities
pub type EntityId = String;

/// Errors that can occur in the entity system
#[derive(Error, Debug)]
pub enum EntityError {
    #[error("Entity not found: {0}")]
    NotFound(EntityId),

    #[error("Entity already exists: {0}")]
    AlreadyExists(EntityId),

    #[error("Invalid entity type: {0}")]
    InvalidType(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Query error: {0}")]
    QueryError(String),

    #[error("Modification error: {0}")]
    ModificationError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

pub type EntityResult<T> = Result<T, EntityError>;

/// Entity metadata common to all entity types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityMetadata {
    /// Unique identifier
    pub id: EntityId,

    /// Entity type
    pub entity_type: EntityType,

    /// Creation timestamp
    pub created_at: chrono::DateTime<chrono::Utc>,

    /// Last modification timestamp
    pub updated_at: chrono::DateTime<chrono::Utc>,

    /// Version (for optimistic locking)
    pub version: u64,

    /// Tags for categorization
    pub tags: Vec<String>,
}

impl EntityMetadata {
    /// Create new metadata for an entity
    pub fn new(entity_type: EntityType) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            entity_type,
            created_at: now,
            updated_at: now,
            version: 1,
            tags: Vec::new(),
        }
    }
}

/// Types of entities in the system
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityType {
    /// Version control entity (git)
    Git,

    /// AST/Filesystem entity
    Ast,

    /// Test/Analysis entity
    Test,

    /// Environment/Deployment entity
    Env,

    /// Project context entity
    Context,

    /// Telemetry entity (future)
    Telemetry,
}

/// Core entity trait implemented by all entity types
///
/// # Design Decisions
///
/// ## Why Not Clone?
///
/// The `Entity` trait intentionally does **not** require `Clone` for several reasons:
///
/// 1. **Large Data Structures**: Some entities (especially AST and telemetry) may contain
///    large amounts of data that would be expensive to clone.
///
/// 2. **Reference Semantics**: Entities are meant to be stored and referenced, not copied.
///    The entity store manages ownership, and consumers should work with references or IDs.
///
/// 3. **Relationship Integrity**: Cloning entities could lead to duplicate IDs or broken
///    relationships in the entity graph.
///
/// ## Alternative Patterns
///
/// Instead of cloning, use these patterns:
///
/// - **References**: Store `&Entity` or `EntityId` and query when needed
/// - **Serialization**: Use `to_json()` for persistence or transfer
/// - **Selective Copying**: Copy only the metadata or specific fields needed
///
/// ## Future Considerations
///
/// If entity retrieval by value becomes necessary:
///
/// - Add `fn to_owned(&self) -> Box<dyn Entity>` for explicit cloning
/// - Use `Arc<dyn Entity>` for cheap reference counting
/// - Implement `Clone` on specific entity types that need it
///
/// For now, the `EntityStore::exists()` method provides existence checking without
/// requiring entity retrieval.
#[async_trait]
pub trait Entity: Send + Sync {
    /// Get entity metadata
    fn metadata(&self) -> &EntityMetadata;

    /// Get mutable metadata
    fn metadata_mut(&mut self) -> &mut EntityMetadata;

    /// Serialize entity to JSON
    ///
    /// This is the primary way to persist or transmit entities. For large entities,
    /// consider implementing streaming serialization in the concrete type.
    fn to_json(&self) -> EntityResult<String>;

    /// Get entity type
    fn entity_type(&self) -> EntityType {
        self.metadata().entity_type.clone()
    }

    /// Get entity ID
    fn id(&self) -> &str {
        &self.metadata().id
    }
}

/// Relationship between entities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRelationship {
    /// Source entity ID
    pub from: EntityId,

    /// Target entity ID
    pub to: EntityId,

    /// Type of relationship
    pub relationship_type: RelationshipType,

    /// Optional metadata about the relationship
    pub metadata: HashMap<String, String>,
}

/// Types of relationships between entities
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationshipType {
    /// Git commit contains file changes
    Contains,

    /// Code calls another function/method
    Calls,

    /// Module imports another module
    Imports,

    /// Type implements trait/interface
    Implements,

    /// Entity references another entity
    References,

    /// Commit modifies entity
    Modifies,

    /// Test validates entity
    Validates,

    /// Custom relationship
    Custom(String),
}

/// Query interface for entity retrieval
#[derive(Debug, Clone, Default)]
pub struct EntityQuery {
    /// Entity types to query
    pub entity_types: Vec<EntityType>,

    /// Free text search query
    pub text_query: Option<String>,

    /// Filter by tags
    pub tags: Vec<String>,

    /// Filter by time range
    pub time_range: Option<TimeRange>,

    /// Maximum results to return
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct TimeRange {
    pub start: chrono::DateTime<chrono::Utc>,
    pub end: chrono::DateTime<chrono::Utc>,
}

/// Query result
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// Entity ID
    pub entity_id: EntityId,

    /// Entity type
    pub entity_type: EntityType,

    /// Relevance score (0.0 to 1.0)
    pub relevance: f64,

    /// Matching snippet
    pub snippet: Option<String>,
}

/// Entity storage abstraction
///
/// # Design Decisions
///
/// ## Why No `get()` Method?
///
/// This trait intentionally **does not** include a `get(id) -> Box<dyn Entity>` method
/// because `Box<dyn Entity>` cannot implement `Clone`, which would be required for
/// returning owned entities.
///
/// ### Alternative Approaches
///
/// 1. **Query by ID**: Use `query()` with an ID filter to get `QueryResult` metadata
/// 2. **Check Existence**: Use `exists()` to verify an entity is present
/// 3. **Type-Specific Stores**: Implement separate stores for each concrete entity type
///    that can return typed entities (e.g., `GitEntityStore::get() -> GitRepository`)
/// 4. **Future Enhancement**: Add a visitor pattern or callback-based access method
///    that allows operating on entities without transferring ownership
///
/// ## Query-Centric Design
///
/// The interface is designed around **querying** rather than direct retrieval:
///
/// - `query()` returns lightweight `QueryResult` with metadata and relevance
/// - Consumers work with IDs and metadata rather than full entities
/// - Reduces memory overhead for large entity graphs
/// - Aligns with RAG (Retrieval-Augmented Generation) patterns
///
/// ## Concrete Store Implementations
///
/// Specific storage backends (database, file system, etc.) can provide type-safe
/// retrieval methods for their concrete entity types while implementing this trait
/// for the generic operations.
#[async_trait]
pub trait EntityStore: Send + Sync {
    /// Store an entity
    async fn store(&mut self, entity: Box<dyn Entity>) -> EntityResult<EntityId>;

    /// Check if entity exists
    ///
    /// This is the primary way to verify entity presence without requiring
    /// entity retrieval or cloning.
    async fn exists(&self, id: &str) -> bool;

    /// Update an existing entity
    async fn update(&mut self, entity: Box<dyn Entity>) -> EntityResult<()>;

    /// Delete an entity
    async fn delete(&mut self, id: &str) -> EntityResult<()>;

    /// Query entities
    ///
    /// Returns lightweight query results with metadata. Use this instead of
    /// `get()` for working with entities. For full entity data, implement
    /// type-specific stores or use serialization.
    async fn query(&self, query: &EntityQuery) -> EntityResult<Vec<QueryResult>>;

    /// Get relationships for an entity
    async fn get_relationships(&self, id: &str) -> EntityResult<Vec<EntityRelationship>>;

    /// Create a relationship between entities
    async fn create_relationship(&mut self, relationship: EntityRelationship) -> EntityResult<()>;

    /// Delete a relationship
    async fn delete_relationship(
        &mut self,
        from: &str,
        to: &str,
        relationship_type: RelationshipType,
    ) -> EntityResult<()>;
}

/// In-memory entity store implementation (for testing and development)
pub struct InMemoryEntityStore {
    entities: HashMap<EntityId, Box<dyn Entity>>,
    relationships: Vec<EntityRelationship>,
}

impl InMemoryEntityStore {
    /// Create a new in-memory entity store
    pub fn new() -> Self {
        Self {
            entities: HashMap::new(),
            relationships: Vec::new(),
        }
    }
}

impl Default for InMemoryEntityStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EntityStore for InMemoryEntityStore {
    async fn store(&mut self, entity: Box<dyn Entity>) -> EntityResult<EntityId> {
        let id = entity.id().to_string();
        if self.entities.contains_key(&id) {
            return Err(EntityError::AlreadyExists(id));
        }
        self.entities.insert(id.clone(), entity);
        Ok(id)
    }

    async fn exists(&self, id: &str) -> bool {
        self.entities.contains_key(id)
    }

    async fn update(&mut self, entity: Box<dyn Entity>) -> EntityResult<()> {
        let id = entity.id().to_string();
        if !self.entities.contains_key(&id) {
            return Err(EntityError::NotFound(id));
        }
        self.entities.insert(id, entity);
        Ok(())
    }

    async fn delete(&mut self, id: &str) -> EntityResult<()> {
        self.entities
            .remove(id)
            .ok_or_else(|| EntityError::NotFound(id.to_string()))?;
        Ok(())
    }

    async fn query(&self, query: &EntityQuery) -> EntityResult<Vec<QueryResult>> {
        let mut results = Vec::new();

        for (id, entity) in &self.entities {
            // Filter by entity type
            if !query.entity_types.is_empty() && !query.entity_types.contains(&entity.entity_type())
            {
                continue;
            }

            // Filter by tags
            if !query.tags.is_empty() {
                let entity_tags = &entity.metadata().tags;
                if !query.tags.iter().any(|t| entity_tags.contains(t)) {
                    continue;
                }
            }

            // Filter by time range
            if let Some(ref time_range) = query.time_range {
                let created_at = entity.metadata().created_at;
                if created_at < time_range.start || created_at > time_range.end {
                    continue;
                }
            }

            // Text search (basic implementation)
            let relevance = if let Some(ref text_query) = query.text_query {
                // Simple substring match for now
                // Real implementation would use proper search indexing
                if let Ok(json) = entity.to_json() {
                    if json.to_lowercase().contains(&text_query.to_lowercase()) {
                        0.8 // High relevance if found
                    } else {
                        continue; // Skip if not found
                    }
                } else {
                    continue;
                }
            } else {
                1.0 // No text query, full relevance
            };

            results.push(QueryResult {
                entity_id: id.clone(),
                entity_type: entity.entity_type(),
                relevance,
                snippet: None,
            });
        }

        // Sort by relevance
        results.sort_by(|a, b| b.relevance.partial_cmp(&a.relevance).unwrap());

        // Apply limit
        if let Some(limit) = query.limit {
            results.truncate(limit);
        }

        Ok(results)
    }

    async fn get_relationships(&self, id: &str) -> EntityResult<Vec<EntityRelationship>> {
        Ok(self
            .relationships
            .iter()
            .filter(|r| r.from == id || r.to == id)
            .cloned()
            .collect())
    }

    async fn create_relationship(&mut self, relationship: EntityRelationship) -> EntityResult<()> {
        // Verify both entities exist
        if !self.entities.contains_key(&relationship.from) {
            return Err(EntityError::NotFound(relationship.from));
        }
        if !self.entities.contains_key(&relationship.to) {
            return Err(EntityError::NotFound(relationship.to));
        }

        self.relationships.push(relationship);
        Ok(())
    }

    async fn delete_relationship(
        &mut self,
        from: &str,
        to: &str,
        relationship_type: RelationshipType,
    ) -> EntityResult<()> {
        self.relationships.retain(|r| {
            !(r.from == from && r.to == to && r.relationship_type == relationship_type)
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::ast::types::{FileEntity, FileType};
    use crate::entities::git::types::GitRepository;

    #[test]
    fn test_entity_metadata_creation() {
        let metadata = EntityMetadata::new(EntityType::Git);
        assert_eq!(metadata.entity_type, EntityType::Git);
        assert_eq!(metadata.version, 1);
        assert!(metadata.tags.is_empty());
    }

    #[test]
    fn test_entity_metadata_has_unique_ids() {
        let a = EntityMetadata::new(EntityType::Git);
        let b = EntityMetadata::new(EntityType::Git);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn test_entity_metadata_timestamps_are_equal_at_creation() {
        let m = EntityMetadata::new(EntityType::Ast);
        assert_eq!(m.created_at, m.updated_at);
    }

    #[test]
    fn test_relationship_types() {
        let rel = EntityRelationship {
            from: "entity1".to_string(),
            to: "entity2".to_string(),
            relationship_type: RelationshipType::Calls,
            metadata: HashMap::new(),
        };
        assert_eq!(rel.relationship_type, RelationshipType::Calls);
    }

    #[test]
    fn test_custom_relationship_type() {
        let custom = RelationshipType::Custom("depends-on".to_string());
        assert_eq!(custom, RelationshipType::Custom("depends-on".to_string()));
        assert_ne!(custom, RelationshipType::Custom("other".to_string()));
    }

    /// Helper: create a GitRepository entity with optional tags.
    fn make_git_entity(tags: Vec<String>) -> Box<dyn Entity> {
        let mut entity = GitRepository::new("/tmp/repo".to_string(), "HEAD".to_string());
        entity.metadata.tags = tags;
        Box::new(entity)
    }

    fn make_file_entity() -> Box<dyn Entity> {
        let entity = FileEntity {
            metadata: EntityMetadata::new(EntityType::Ast),
            path: std::path::PathBuf::from("/tmp/file.rs"),
            relative_path: "file.rs".to_string(),
            file_type: FileType::Rust,
            size_bytes: 100,
            content_preview: "fn main() {}".to_string(),
            line_count: 1,
        };
        Box::new(entity)
    }

    // ── EntityStore: store / exists / delete lifecycle ──

    #[tokio::test]
    async fn test_store_and_exists() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_git_entity(vec![]);
        let id = entity.id().to_string();

        let stored_id = store.store(entity).await.unwrap();
        assert_eq!(stored_id, id);
        assert!(store.exists(&id).await);
    }

    #[tokio::test]
    async fn test_store_rejects_duplicate_id() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_git_entity(vec![]);
        let id = entity.id().to_string();
        store.store(entity).await.unwrap();

        // Build another entity and forcibly assign the same ID.
        let mut dup = GitRepository::new("/dup".to_string(), "HEAD".to_string());
        dup.metadata.id = id.clone();
        let result = store.store(Box::new(dup)).await;

        assert!(matches!(result, Err(EntityError::AlreadyExists(ref eid)) if eid == &id));
    }

    #[tokio::test]
    async fn test_delete_removes_entity() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_git_entity(vec![]);
        let id = store.store(entity).await.unwrap();

        store.delete(&id).await.unwrap();
        assert!(!store.exists(&id).await);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_returns_not_found() {
        let mut store = InMemoryEntityStore::new();
        let result = store.delete("does-not-exist").await;
        assert!(matches!(result, Err(EntityError::NotFound(_))));
    }

    // ── EntityStore: update ──

    #[tokio::test]
    async fn test_update_existing_entity() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_git_entity(vec![]);
        let id = entity.id().to_string();
        store.store(entity).await.unwrap();

        // Build a replacement with the same ID.
        let mut replacement = GitRepository::new("/updated".to_string(), "main".to_string());
        replacement.metadata.id = id.clone();
        store.update(Box::new(replacement)).await.unwrap();

        assert!(store.exists(&id).await);
    }

    #[tokio::test]
    async fn test_update_nonexistent_returns_not_found() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_git_entity(vec![]);
        // Don't store it first — update should fail.
        let result = store.update(entity).await;
        assert!(matches!(result, Err(EntityError::NotFound(_))));
    }

    // ── EntityStore: query filtering ──

    #[tokio::test]
    async fn test_query_filter_by_entity_type() {
        let mut store = InMemoryEntityStore::new();
        store.store(make_git_entity(vec![])).await.unwrap();
        store.store(make_file_entity()).await.unwrap();

        let git_only = EntityQuery {
            entity_types: vec![EntityType::Git],
            ..Default::default()
        };
        let results = store.query(&git_only).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_type, EntityType::Git);

        let ast_only = EntityQuery {
            entity_types: vec![EntityType::Ast],
            ..Default::default()
        };
        let results = store.query(&ast_only).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_type, EntityType::Ast);
    }

    #[tokio::test]
    async fn test_query_empty_type_filter_returns_all() {
        let mut store = InMemoryEntityStore::new();
        store.store(make_git_entity(vec![])).await.unwrap();
        store.store(make_file_entity()).await.unwrap();

        let all = EntityQuery::default();
        let results = store.query(&all).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_query_filter_by_tags() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(make_git_entity(vec!["important".to_string()]))
            .await
            .unwrap();
        store.store(make_git_entity(vec![])).await.unwrap();

        let tagged = EntityQuery {
            tags: vec!["important".to_string()],
            ..Default::default()
        };
        let results = store.query(&tagged).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_query_filter_by_time_range() {
        let mut store = InMemoryEntityStore::new();
        store.store(make_git_entity(vec![])).await.unwrap();

        // A time range in the distant past should match nothing.
        let ancient = EntityQuery {
            time_range: Some(TimeRange {
                start: chrono::DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                end: chrono::DateTime::parse_from_rfc3339("2000-01-02T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            }),
            ..Default::default()
        };
        let results = store.query(&ancient).await.unwrap();
        assert!(results.is_empty());

        // A range including now should match.
        let now_range = EntityQuery {
            time_range: Some(TimeRange {
                start: chrono::Utc::now() - chrono::Duration::hours(1),
                end: chrono::Utc::now() + chrono::Duration::hours(1),
            }),
            ..Default::default()
        };
        let results = store.query(&now_range).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_query_text_search_case_insensitive() {
        let mut store = InMemoryEntityStore::new();
        // GitRepository serialises the repo_path field.
        store.store(make_git_entity(vec![])).await.unwrap();

        let search = EntityQuery {
            text_query: Some("/tmp/repo".to_string()),
            ..Default::default()
        };
        let results = store.query(&search).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!((results[0].relevance - 0.8).abs() < f64::EPSILON);

        let no_match = EntityQuery {
            text_query: Some("zzz_nonexistent_zzz".to_string()),
            ..Default::default()
        };
        let results = store.query(&no_match).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_query_limit() {
        let mut store = InMemoryEntityStore::new();
        for _ in 0..5 {
            store.store(make_git_entity(vec![])).await.unwrap();
        }

        let limited = EntityQuery {
            limit: Some(2),
            ..Default::default()
        };
        let results = store.query(&limited).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_query_no_text_gives_full_relevance() {
        let mut store = InMemoryEntityStore::new();
        store.store(make_git_entity(vec![])).await.unwrap();

        let all = EntityQuery::default();
        let results = store.query(&all).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!((results[0].relevance - 1.0).abs() < f64::EPSILON);
    }

    // ── EntityStore: relationships ──

    #[tokio::test]
    async fn test_create_and_get_relationship() {
        let mut store = InMemoryEntityStore::new();
        let id_a = store.store(make_git_entity(vec![])).await.unwrap();
        let id_b = store.store(make_file_entity()).await.unwrap();

        let rel = EntityRelationship {
            from: id_a.clone(),
            to: id_b.clone(),
            relationship_type: RelationshipType::Contains,
            metadata: HashMap::new(),
        };
        store.create_relationship(rel).await.unwrap();

        // Queried from either side.
        let from_a = store.get_relationships(&id_a).await.unwrap();
        assert_eq!(from_a.len(), 1);
        assert_eq!(from_a[0].to, id_b);

        let from_b = store.get_relationships(&id_b).await.unwrap();
        assert_eq!(from_b.len(), 1);
        assert_eq!(from_b[0].from, id_a);
    }

    #[tokio::test]
    async fn test_create_relationship_rejects_missing_entity() {
        let mut store = InMemoryEntityStore::new();
        let id_a = store.store(make_git_entity(vec![])).await.unwrap();

        let rel = EntityRelationship {
            from: id_a,
            to: "nonexistent".to_string(),
            relationship_type: RelationshipType::References,
            metadata: HashMap::new(),
        };
        let result = store.create_relationship(rel).await;
        assert!(matches!(result, Err(EntityError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_delete_relationship() {
        let mut store = InMemoryEntityStore::new();
        let id_a = store.store(make_git_entity(vec![])).await.unwrap();
        let id_b = store.store(make_file_entity()).await.unwrap();

        let rel = EntityRelationship {
            from: id_a.clone(),
            to: id_b.clone(),
            relationship_type: RelationshipType::Modifies,
            metadata: HashMap::new(),
        };
        store.create_relationship(rel).await.unwrap();
        assert_eq!(store.get_relationships(&id_a).await.unwrap().len(), 1);

        store
            .delete_relationship(&id_a, &id_b, RelationshipType::Modifies)
            .await
            .unwrap();
        assert!(store.get_relationships(&id_a).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_delete_relationship_only_removes_matching_type() {
        let mut store = InMemoryEntityStore::new();
        let id_a = store.store(make_git_entity(vec![])).await.unwrap();
        let id_b = store.store(make_file_entity()).await.unwrap();

        for rtype in [RelationshipType::Contains, RelationshipType::Modifies] {
            store
                .create_relationship(EntityRelationship {
                    from: id_a.clone(),
                    to: id_b.clone(),
                    relationship_type: rtype,
                    metadata: HashMap::new(),
                })
                .await
                .unwrap();
        }
        assert_eq!(store.get_relationships(&id_a).await.unwrap().len(), 2);

        store
            .delete_relationship(&id_a, &id_b, RelationshipType::Contains)
            .await
            .unwrap();
        let remaining = store.get_relationships(&id_a).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].relationship_type, RelationshipType::Modifies);
    }

    // ── Entity trait default methods ──

    #[test]
    fn test_entity_trait_id_and_type() {
        let entity = GitRepository::new("/r".to_string(), "main".to_string());
        assert_eq!(entity.entity_type(), EntityType::Git);
        assert!(!entity.id().is_empty());
    }

    #[test]
    fn test_entity_to_json_roundtrip() {
        let entity = GitRepository::new("/tmp/repo".to_string(), "main".to_string());
        let json = entity.to_json().unwrap();
        assert!(json.contains("/tmp/repo"));
        assert!(json.contains("main"));
    }

    // ── InMemoryEntityStore default trait ──

    #[tokio::test]
    async fn test_in_memory_store_default() {
        let store = InMemoryEntityStore::default();
        assert_eq!(store.entities.len(), 0);
        assert!(store.relationships.is_empty());
    }
}
