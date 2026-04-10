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
    use crate::entities::git::GitRepository;

    fn make_git_repo(remote: &str) -> GitRepository {
        GitRepository::new(remote.to_string(), "main".to_string())
    }

    #[test]
    fn test_entity_metadata_creation() {
        let metadata = EntityMetadata::new(EntityType::Git);
        assert_eq!(metadata.entity_type, EntityType::Git);
        assert_eq!(metadata.version, 1);
        assert!(metadata.tags.is_empty());
        // ID should be a non-empty UUID-like string
        assert!(!metadata.id.is_empty());
        // created_at and updated_at should be equal on creation
        assert_eq!(metadata.created_at, metadata.updated_at);
    }

    #[test]
    fn test_entity_metadata_ids_are_unique() {
        let m1 = EntityMetadata::new(EntityType::Git);
        let m2 = EntityMetadata::new(EntityType::Ast);
        assert_ne!(m1.id, m2.id, "Each entity should get a unique ID");
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

    // ===== InMemoryEntityStore: store / exists / update / delete =====

    #[tokio::test]
    async fn test_store_and_exists() {
        let mut store = InMemoryEntityStore::new();
        let repo = make_git_repo("https://github.com/a/b");
        let id = repo.id().to_string();

        assert!(!store.exists(&id).await);
        let returned_id = store.store(Box::new(repo)).await.unwrap();
        assert_eq!(returned_id, id);
        assert!(store.exists(&id).await);
    }

    #[tokio::test]
    async fn test_store_duplicate_returns_error() {
        let mut store = InMemoryEntityStore::new();
        let repo = make_git_repo("https://github.com/a/b");
        let id = repo.id().to_string();

        store.store(Box::new(repo.clone())).await.unwrap();

        // Storing the same entity again should fail
        let result = store.store(Box::new(repo)).await;
        assert!(matches!(result, Err(EntityError::AlreadyExists(_))));
        assert!(result.unwrap_err().to_string().contains(&id));
    }

    #[tokio::test]
    async fn test_delete_existing_entity() {
        let mut store = InMemoryEntityStore::new();
        let repo = make_git_repo("https://github.com/a/b");
        let id = repo.id().to_string();

        store.store(Box::new(repo)).await.unwrap();
        assert!(store.exists(&id).await);

        store.delete(&id).await.unwrap();
        assert!(!store.exists(&id).await);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_entity_returns_error() {
        let mut store = InMemoryEntityStore::new();
        let result = store.delete("nonexistent-id").await;
        assert!(matches!(result, Err(EntityError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_update_existing_entity() {
        let mut store = InMemoryEntityStore::new();
        let repo = make_git_repo("https://github.com/a/b");
        let id = repo.id().to_string();

        store.store(Box::new(repo.clone())).await.unwrap();

        // Update with a different entity sharing the same ID
        let result = store.update(Box::new(repo)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_update_nonexistent_entity_returns_error() {
        let mut store = InMemoryEntityStore::new();
        let repo = make_git_repo("https://github.com/a/b");

        let result = store.update(Box::new(repo)).await;
        assert!(matches!(result, Err(EntityError::NotFound(_))));
    }

    // ===== InMemoryEntityStore: query =====

    #[tokio::test]
    async fn test_query_all_returns_all_entities() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(make_git_repo("https://github.com/a/b")))
            .await
            .unwrap();
        store
            .store(Box::new(make_git_repo("https://github.com/c/d")))
            .await
            .unwrap();

        let results = store.query(&EntityQuery::default()).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_query_by_entity_type_filters_correctly() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(make_git_repo("https://github.com/a/b")))
            .await
            .unwrap();

        // Query for Git entities — should return the one we stored
        let git_query = EntityQuery {
            entity_types: vec![EntityType::Git],
            ..Default::default()
        };
        let git_results = store.query(&git_query).await.unwrap();
        assert_eq!(git_results.len(), 1);
        assert_eq!(git_results[0].entity_type, EntityType::Git);

        // Query for Ast entities — should return none
        let ast_query = EntityQuery {
            entity_types: vec![EntityType::Ast],
            ..Default::default()
        };
        let ast_results = store.query(&ast_query).await.unwrap();
        assert_eq!(ast_results.len(), 0);
    }

    #[tokio::test]
    async fn test_query_limit_is_respected() {
        let mut store = InMemoryEntityStore::new();
        for i in 0..5 {
            store
                .store(Box::new(make_git_repo(&format!("https://github.com/{}/r", i))))
                .await
                .unwrap();
        }

        let limited = EntityQuery {
            limit: Some(3),
            ..Default::default()
        };
        let results = store.query(&limited).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn test_query_text_search_filters_by_content() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(make_git_repo("https://github.com/foo/bar")))
            .await
            .unwrap();
        store
            .store(Box::new(make_git_repo("https://github.com/other/repo")))
            .await
            .unwrap();

        let text_query = EntityQuery {
            text_query: Some("foo".to_string()),
            ..Default::default()
        };
        let results = store.query(&text_query).await.unwrap();
        // Only the "foo/bar" repo should match
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].relevance, 0.8);
    }

    #[tokio::test]
    async fn test_query_empty_store_returns_empty() {
        let store = InMemoryEntityStore::new();
        let results = store.query(&EntityQuery::default()).await.unwrap();
        assert!(results.is_empty());
    }

    // ===== InMemoryEntityStore: relationships =====

    #[tokio::test]
    async fn test_create_and_retrieve_relationship() {
        let mut store = InMemoryEntityStore::new();
        let repo_a = make_git_repo("https://github.com/a/r");
        let repo_b = make_git_repo("https://github.com/b/r");
        let id_a = repo_a.id().to_string();
        let id_b = repo_b.id().to_string();

        store.store(Box::new(repo_a)).await.unwrap();
        store.store(Box::new(repo_b)).await.unwrap();

        let rel = EntityRelationship {
            from: id_a.clone(),
            to: id_b.clone(),
            relationship_type: RelationshipType::References,
            metadata: HashMap::new(),
        };
        store.create_relationship(rel).await.unwrap();

        let rels_a = store.get_relationships(&id_a).await.unwrap();
        assert_eq!(rels_a.len(), 1);
        assert_eq!(rels_a[0].relationship_type, RelationshipType::References);

        // B is also a participant, so it should also appear
        let rels_b = store.get_relationships(&id_b).await.unwrap();
        assert_eq!(rels_b.len(), 1);
    }

    #[tokio::test]
    async fn test_create_relationship_with_missing_entity_fails() {
        let mut store = InMemoryEntityStore::new();
        let repo = make_git_repo("https://github.com/a/r");
        let id = repo.id().to_string();
        store.store(Box::new(repo)).await.unwrap();

        let rel = EntityRelationship {
            from: id,
            to: "nonexistent".to_string(),
            relationship_type: RelationshipType::Calls,
            metadata: HashMap::new(),
        };
        let result = store.create_relationship(rel).await;
        assert!(matches!(result, Err(EntityError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_delete_relationship() {
        let mut store = InMemoryEntityStore::new();
        let repo_a = make_git_repo("https://github.com/a/r");
        let repo_b = make_git_repo("https://github.com/b/r");
        let id_a = repo_a.id().to_string();
        let id_b = repo_b.id().to_string();

        store.store(Box::new(repo_a)).await.unwrap();
        store.store(Box::new(repo_b)).await.unwrap();

        let rel = EntityRelationship {
            from: id_a.clone(),
            to: id_b.clone(),
            relationship_type: RelationshipType::Contains,
            metadata: HashMap::new(),
        };
        store.create_relationship(rel).await.unwrap();

        // Verify it exists
        assert_eq!(store.get_relationships(&id_a).await.unwrap().len(), 1);

        // Delete it
        store
            .delete_relationship(&id_a, &id_b, RelationshipType::Contains)
            .await
            .unwrap();

        // Should be gone
        assert_eq!(store.get_relationships(&id_a).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_no_relationships_for_unknown_entity() {
        let store = InMemoryEntityStore::new();
        let rels = store.get_relationships("unknown").await.unwrap();
        assert!(rels.is_empty());
    }
}
