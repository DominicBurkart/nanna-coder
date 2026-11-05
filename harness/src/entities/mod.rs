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
use std::path::Path;
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

    /// Lint/static-analysis result entity
    Lint,

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

    /// List all entity IDs for a given entity kind.
    ///
    /// The default implementation runs a full `query` constrained to the
    /// requested kind and projects the IDs out of the results. Backends that
    /// can answer this more cheaply (e.g. indexed SQL) should override it.
    ///
    /// This hook is introduced to prepare for the persistent-store work in
    /// issue #193 (Phase A). It has no behavior change for the in-memory
    /// store.
    async fn list_by_kind(&self, kind: EntityType) -> EntityResult<Vec<EntityId>> {
        let query = EntityQuery {
            entity_types: vec![kind],
            ..EntityQuery::default()
        };
        let results = self.query(&query).await?;
        Ok(results.into_iter().map(|r| r.entity_id).collect())
    }

    /// Invalidate stale entries against the given workspace root.
    ///
    /// Persistent backends override this to drop entries whose backing
    /// on-disk state has changed (file mtime advanced, HEAD commit moved,
    /// etc.). The in-memory store has no persistence to invalidate, so the
    /// default returns `Ok(0)`.
    ///
    /// Returns the number of entries that were invalidated/removed. Added
    /// as part of issue #193 Phase A so that callers can be generified
    /// against the trait ahead of the persistent-store implementation.
    async fn invalidate_stale(&mut self, _workspace_root: &Path) -> EntityResult<usize> {
        Ok(0)
    }
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

#[cfg(kani)]
mod kani_proofs {
    use super::*;
    use std::collections::HashMap;

    /// The InMemoryEntityStore stores entities in a HashMap keyed by ID.
    /// Verify the core invariant: after `store()`, the entity exists;
    /// after `delete()`, it does not.
    ///
    /// We model this with a plain HashMap<u8, u8> to avoid async.
    #[kani::proof]
    fn store_then_exists() {
        let mut map: HashMap<u8, u8> = HashMap::new();
        let key: u8 = kani::any();
        let val: u8 = kani::any();

        // Before store
        assert!(!map.contains_key(&key));

        map.insert(key, val);

        // After store
        assert!(map.contains_key(&key));
        assert_eq!(map.len(), 1);
    }

    /// store() rejects duplicates (mirrors InMemoryEntityStore::store,
    /// which returns `Err(AlreadyExists)` on the second insert).
    ///
    /// We model this with the raw `HashMap` API so Kani can verify it
    /// without async runtime. `HashMap::insert` returns `None` for a new
    /// key and `Some(previous_value)` for a duplicate — the latter is the
    /// signal that a `contains_key` guard in `store()` would fire on.
    #[kani::proof]
    fn store_rejects_duplicate() {
        let mut map: HashMap<u8, u8> = HashMap::new();
        let key: u8 = kani::any();
        let v1: u8 = kani::any();
        let v2: u8 = kani::any();

        // First insert must succeed (no previous value).
        assert!(map.insert(key, v1).is_none());

        // Mirror the `contains_key` guard used by `InMemoryEntityStore::store`.
        // If we follow the real code path, we must reject the duplicate *before*
        // mutating the map, leaving the original value untouched.
        if map.contains_key(&key) {
            // Duplicate detected — do NOT insert.
            assert_eq!(map.get(&key), Some(&v1));
            assert_eq!(map.len(), 1);
        } else {
            // Unreachable given the assertion above, but included so the
            // proof fails loudly if the first insert didn't actually land.
            assert!(false, "first insert did not persist");
        }

        // As a second independent witness: bypassing the guard and letting
        // `HashMap::insert` overwrite returns `Some(old_value)` — i.e., the
        // duplicate is observable from the return value, which is exactly
        // what a guard would branch on.
        let overwritten = map.insert(key, v2);
        assert_eq!(overwritten, Some(v1));
    }

    /// delete() removes an entity and preserves others.
    #[kani::proof]
    fn delete_preserves_others() {
        let mut map: HashMap<u8, u8> = HashMap::new();
        let k1: u8 = kani::any();
        let k2: u8 = kani::any();
        kani::assume(k1 != k2);
        let v1: u8 = kani::any();
        let v2: u8 = kani::any();

        map.insert(k1, v1);
        map.insert(k2, v2);
        assert_eq!(map.len(), 2);

        map.remove(&k1);
        assert_eq!(map.len(), 1);
        assert!(!map.contains_key(&k1));
        assert!(map.contains_key(&k2));
        assert_eq!(map[&k2], v2);
    }

    /// Query with a limit never returns more results than requested.
    #[kani::proof]
    #[kani::unwind(6)]
    fn query_limit_respected() {
        let total: usize = kani::any();
        kani::assume(total <= 5);
        let limit: usize = kani::any();
        kani::assume(limit <= 5);

        // Simulate: we have `total` results and truncate to `limit`
        let mut results: Vec<u8> = Vec::new();
        for i in 0..total {
            results.push(i as u8);
        }
        results.truncate(limit);
        assert!(results.len() <= limit);
    }

    // Note: a prior `metadata_version_starts_at_one` harness was removed
    // because it was a tautology (`let version: u64 = 1; assert!(version > 0)`)
    // that never actually touched `EntityMetadata::new`. `EntityMetadata::new`
    // uses chrono/uuid which are impractical to model under Kani today; the
    // `version == 1` invariant is instead covered by the unit test
    // `test_entity_metadata_creation` below.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_metadata_creation() {
        let metadata = EntityMetadata::new(EntityType::Git);
        assert_eq!(metadata.entity_type, EntityType::Git);
        assert_eq!(metadata.version, 1);
        assert!(metadata.tags.is_empty());
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

    #[tokio::test]
    async fn test_in_memory_store_basic_operations() {
        // Note: Full tests will be added when concrete entity types are implemented
        let store = InMemoryEntityStore::new();
        assert_eq!(store.entities.len(), 0);
    }

    #[tokio::test]
    async fn test_list_by_kind_returns_matching_ids() {
        use crate::entities::context::types::ContextEntity;
        use crate::entities::git::types::GitRepository;

        let mut store = InMemoryEntityStore::new();

        let git_entity =
            Box::new(GitRepository::new(String::new(), "main".to_string())) as Box<dyn Entity>;
        let git_id = store.store(git_entity).await.unwrap();

        let context_entity = Box::new(ContextEntity::new(
            "task".to_string(),
            vec![],
            vec![],
            String::new(),
            "model".to_string(),
        )) as Box<dyn Entity>;
        let context_id = store.store(context_entity).await.unwrap();

        let git_ids = store.list_by_kind(EntityType::Git).await.unwrap();
        assert_eq!(git_ids, vec![git_id.clone()]);

        let context_ids = store.list_by_kind(EntityType::Context).await.unwrap();
        assert_eq!(context_ids, vec![context_id.clone()]);

        let ast_ids = store.list_by_kind(EntityType::Ast).await.unwrap();
        assert!(ast_ids.is_empty());
    }

    #[tokio::test]
    async fn test_invalidate_stale_default_is_noop() {
        use crate::entities::git::types::GitRepository;

        let mut store = InMemoryEntityStore::new();
        let git_entity =
            Box::new(GitRepository::new(String::new(), "main".to_string())) as Box<dyn Entity>;
        store.store(git_entity).await.unwrap();

        // The in-memory store uses the default impl, which is a no-op.
        let invalidated = store
            .invalidate_stale(std::path::Path::new("/nonexistent"))
            .await
            .unwrap();
        assert_eq!(invalidated, 0);

        // Entity should still be present.
        let remaining = store.query(&EntityQuery::default()).await.unwrap();
        assert_eq!(remaining.len(), 1);
    }

    /// Generic helper that exercises the `EntityStore` trait contract.
    ///
    /// Added as part of issue #193 Phase A so that future backends (e.g. the
    /// planned `PersistentEntityStore`) can be validated against the same
    /// sequence of operations as `InMemoryEntityStore`.
    async fn exercise_store<S: EntityStore + ?Sized>(store: &mut S) {
        use crate::entities::git::types::GitRepository;

        // Store two entities.
        let a = Box::new(GitRepository::new(String::new(), "main".to_string())) as Box<dyn Entity>;
        let a_id = store.store(a).await.unwrap();

        let b = Box::new(GitRepository::new(String::new(), "dev".to_string())) as Box<dyn Entity>;
        let b_id = store.store(b).await.unwrap();

        assert!(store.exists(&a_id).await);
        assert!(store.exists(&b_id).await);

        // list_by_kind should enumerate both Git entities.
        let git_ids = store.list_by_kind(EntityType::Git).await.unwrap();
        assert_eq!(git_ids.len(), 2);

        // Delete one and verify.
        store.delete(&a_id).await.unwrap();
        assert!(!store.exists(&a_id).await);

        let remaining = store.list_by_kind(EntityType::Git).await.unwrap();
        assert_eq!(remaining, vec![b_id.clone()]);

        // invalidate_stale should be callable through the trait.
        let _ = store
            .invalidate_stale(std::path::Path::new("/tmp"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_exercise_store_with_in_memory() {
        let mut store = InMemoryEntityStore::new();
        exercise_store(&mut store).await;
    }

    #[test]
    fn test_entity_metadata_new() {
        let before = chrono::Utc::now();
        let mut metadata = EntityMetadata::new(EntityType::Ast);
        let after = chrono::Utc::now();

        assert_eq!(metadata.version, 1);
        assert_eq!(metadata.entity_type, EntityType::Ast);
        assert!(metadata.created_at >= before && metadata.created_at <= after);
        assert!(metadata.tags.is_empty());
        // ID must be a valid UUID v4 ("random") — rejects v1, v3, v5, and nil UUIDs.
        let parsed = uuid::Uuid::parse_str(&metadata.id).expect("should be valid UUID");
        assert_eq!(parsed.get_version(), Some(uuid::Version::Random)); // v4
        metadata.updated_at = chrono::Utc::now();
        assert!(
            metadata.updated_at >= metadata.created_at,
            "updated_at must not precede created_at after mutation"
        );
    }

    #[test]
    fn test_entity_metadata_json_roundtrip() {
        let original = EntityMetadata::new(EntityType::Context);
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: EntityMetadata = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(original.id, restored.id);
        assert_eq!(original.entity_type, restored.entity_type);
        assert_eq!(original.version, restored.version);
        assert_eq!(original.created_at, restored.created_at);
        assert_eq!(original.updated_at, restored.updated_at);
        assert_eq!(original.tags, restored.tags);
    }

    /// Returns one instance of every `EntityType` variant.
    ///
    /// The closure `_exhaustive_check` performs a compiler-enforced exhaustive
    /// `match` so that adding a new variant to `EntityType` without updating
    /// this helper produces a **compile error** rather than a silent test gap.
    fn all_entity_type_variants() -> Vec<EntityType> {
        // Compiler error here if a variant is missing — no runtime surprise.
        let _exhaustive_check = |v: &EntityType| match v {
            EntityType::Git
            | EntityType::Ast
            | EntityType::Test
            | EntityType::Lint
            | EntityType::Env
            | EntityType::Context
            | EntityType::Telemetry => {}
        };
        vec![
            EntityType::Git,
            EntityType::Ast,
            EntityType::Test,
            EntityType::Lint,
            EntityType::Env,
            EntityType::Context,
            EntityType::Telemetry,
        ]
    }

    #[test]
    fn test_entity_type_debug_variants() {
        // EntityType does not implement Display, so verify Debug output
        // covers all variants and each produces a distinct string.
        let variants = all_entity_type_variants();

        let debug_strings: Vec<String> = variants.iter().map(|v| format!("{:?}", v)).collect();
        // All debug strings should be unique
        let unique: std::collections::HashSet<&String> = debug_strings.iter().collect();
        assert_eq!(unique.len(), variants.len());

        // Spot-check a few
        assert_eq!(format!("{:?}", EntityType::Git), "Git");
        assert_eq!(format!("{:?}", EntityType::Telemetry), "Telemetry");
    }
}
