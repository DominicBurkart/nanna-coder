//! Unit tests for `InMemoryEntityStore`
//!
//! The only prior coverage was a single "new store is empty" assertion.
//! These tests document and lock down the full CRUD contract plus
//! relationship management so regressions surface immediately without
//! requiring a live model or container.

use harness::entities::{
    Entity, EntityError, EntityMetadata, EntityQuery, EntityRelationship, EntityResult,
    EntityStore, EntityType, InMemoryEntityStore, RelationshipType,
};

// ---------------------------------------------------------------------------
// Minimal concrete entity for testing — no business logic, just plumbing.
// ---------------------------------------------------------------------------

struct SimpleEntity {
    meta: EntityMetadata,
    payload: String,
}

impl SimpleEntity {
    fn new(entity_type: EntityType, payload: impl Into<String>) -> Self {
        Self {
            meta: EntityMetadata::new(entity_type),
            payload: payload.into(),
        }
    }
}

impl Entity for SimpleEntity {
    fn metadata(&self) -> &EntityMetadata {
        &self.meta
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.meta
    }

    fn to_json(&self) -> EntityResult<String> {
        Ok(format!(
            r#"{{"id":"{}","payload":"{}"}}"#,
            self.meta.id, self.payload
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn git_entity(payload: &str) -> Box<SimpleEntity> {
    Box::new(SimpleEntity::new(EntityType::Git, payload))
}

fn ast_entity(payload: &str) -> Box<SimpleEntity> {
    Box::new(SimpleEntity::new(EntityType::Ast, payload))
}

// ---------------------------------------------------------------------------
// store / exists
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_returns_id_and_entity_is_then_found() {
    let mut store = InMemoryEntityStore::new();
    let entity = git_entity("repo_url");
    let id = store.store(entity).await.unwrap();
    assert!(store.exists(&id).await, "stored entity should be found by id");
}

#[tokio::test]
async fn store_duplicate_id_returns_already_exists_error() {
    let mut store = InMemoryEntityStore::new();
    let entity = git_entity("first");
    let id = store.store(entity).await.unwrap();

    // Build a second entity and manually set the same id so we can test the
    // duplicate guard — the store keys on Entity::id().
    let mut dup = SimpleEntity::new(EntityType::Git, "second");
    dup.meta.id = id.clone();

    let err = store.store(Box::new(dup)).await.unwrap_err();
    assert!(
        matches!(err, EntityError::AlreadyExists(_)),
        "storing a duplicate id must return AlreadyExists, got {:?}",
        err
    );
}

#[tokio::test]
async fn exists_returns_false_for_unknown_id() {
    let store = InMemoryEntityStore::new();
    assert!(!store.exists("no-such-id").await);
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_replaces_entity_and_exists_still_true() {
    let mut store = InMemoryEntityStore::new();
    let id = store.store(git_entity("v1")).await.unwrap();

    let mut updated = SimpleEntity::new(EntityType::Git, "v2");
    updated.meta.id = id.clone();
    store.update(Box::new(updated)).await.unwrap();

    // entity should still exist
    assert!(store.exists(&id).await);
}

#[tokio::test]
async fn update_missing_entity_returns_not_found() {
    let mut store = InMemoryEntityStore::new();
    let err = store.update(git_entity("ghost")).await.unwrap_err();
    assert!(
        matches!(err, EntityError::NotFound(_)),
        "update of unknown id must return NotFound"
    );
}

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_removes_entity_and_exists_returns_false() {
    let mut store = InMemoryEntityStore::new();
    let id = store.store(git_entity("to-delete")).await.unwrap();
    store.delete(&id).await.unwrap();
    assert!(!store.exists(&id).await);
}

#[tokio::test]
async fn delete_missing_entity_returns_not_found() {
    let mut store = InMemoryEntityStore::new();
    let err = store.delete("ghost").await.unwrap_err();
    assert!(matches!(err, EntityError::NotFound(_)));
}

// ---------------------------------------------------------------------------
// query — entity-type filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_by_entity_type_returns_only_matching_type() {
    let mut store = InMemoryEntityStore::new();
    store.store(git_entity("git-1")).await.unwrap();
    store.store(git_entity("git-2")).await.unwrap();
    store.store(ast_entity("ast-1")).await.unwrap();

    let query = EntityQuery {
        entity_types: vec![EntityType::Git],
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();

    assert_eq!(results.len(), 2, "expected exactly 2 Git entities");
    assert!(
        results.iter().all(|r| r.entity_type == EntityType::Git),
        "all results must be Git type"
    );
}

#[tokio::test]
async fn query_with_no_type_filter_returns_all_entities() {
    let mut store = InMemoryEntityStore::new();
    store.store(git_entity("g")).await.unwrap();
    store.store(ast_entity("a")).await.unwrap();

    let results = store.query(&EntityQuery::default()).await.unwrap();
    assert_eq!(results.len(), 2);
}

// ---------------------------------------------------------------------------
// query — text search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_text_search_finds_matching_entity() {
    let mut store = InMemoryEntityStore::new();
    store.store(git_entity("needle_in_payload")).await.unwrap();
    store.store(git_entity("unrelated")).await.unwrap();

    let query = EntityQuery {
        text_query: Some("needle_in_payload".to_string()),
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert_eq!(results.len(), 1, "text search should find exactly one match");
}

#[tokio::test]
async fn query_text_search_is_case_insensitive() {
    let mut store = InMemoryEntityStore::new();
    store.store(git_entity("UniqueToken")).await.unwrap();

    let query = EntityQuery {
        text_query: Some("uniquetoken".to_string()),
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn query_text_search_with_no_match_returns_empty() {
    let mut store = InMemoryEntityStore::new();
    store.store(git_entity("something")).await.unwrap();

    let query = EntityQuery {
        text_query: Some("totally_absent".to_string()),
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert!(results.is_empty());
}

// ---------------------------------------------------------------------------
// query — limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_limit_caps_result_count() {
    let mut store = InMemoryEntityStore::new();
    for i in 0..5 {
        store.store(git_entity(&format!("item-{}", i))).await.unwrap();
    }

    let query = EntityQuery {
        limit: Some(3),
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert_eq!(results.len(), 3, "limit should cap results at 3");
}

// ---------------------------------------------------------------------------
// query — tag filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_tag_filter_returns_only_tagged_entities() {
    let mut store = InMemoryEntityStore::new();

    let mut tagged = SimpleEntity::new(EntityType::Git, "tagged");
    tagged.meta.tags.push("important".to_string());
    store.store(Box::new(tagged)).await.unwrap();

    store.store(git_entity("untagged")).await.unwrap();

    let query = EntityQuery {
        tags: vec!["important".to_string()],
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert_eq!(results.len(), 1);
}

// ---------------------------------------------------------------------------
// relationships — happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_and_retrieve_relationship_between_two_entities() {
    let mut store = InMemoryEntityStore::new();
    let from_id = store.store(git_entity("from")).await.unwrap();
    let to_id = store.store(ast_entity("to")).await.unwrap();

    let rel = EntityRelationship {
        from: from_id.clone(),
        to: to_id.clone(),
        relationship_type: RelationshipType::Modifies,
        metadata: Default::default(),
    };
    store.create_relationship(rel).await.unwrap();

    let rels = store.get_relationships(&from_id).await.unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].relationship_type, RelationshipType::Modifies);
    assert_eq!(rels[0].to, to_id);
}

#[tokio::test]
async fn get_relationships_visible_from_both_ends() {
    let mut store = InMemoryEntityStore::new();
    let a = store.store(git_entity("a")).await.unwrap();
    let b = store.store(git_entity("b")).await.unwrap();

    store
        .create_relationship(EntityRelationship {
            from: a.clone(),
            to: b.clone(),
            relationship_type: RelationshipType::References,
            metadata: Default::default(),
        })
        .await
        .unwrap();

    // Relationship must be visible when querying either participant.
    assert_eq!(store.get_relationships(&a).await.unwrap().len(), 1);
    assert_eq!(store.get_relationships(&b).await.unwrap().len(), 1);
}

#[tokio::test]
async fn delete_relationship_removes_it() {
    let mut store = InMemoryEntityStore::new();
    let a = store.store(git_entity("a")).await.unwrap();
    let b = store.store(git_entity("b")).await.unwrap();

    store
        .create_relationship(EntityRelationship {
            from: a.clone(),
            to: b.clone(),
            relationship_type: RelationshipType::Calls,
            metadata: Default::default(),
        })
        .await
        .unwrap();

    store
        .delete_relationship(&a, &b, RelationshipType::Calls)
        .await
        .unwrap();

    let rels = store.get_relationships(&a).await.unwrap();
    assert!(rels.is_empty(), "relationship should have been removed");
}

// ---------------------------------------------------------------------------
// relationships — sad paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_relationship_with_missing_from_entity_is_rejected() {
    let mut store = InMemoryEntityStore::new();
    let to_id = store.store(git_entity("to")).await.unwrap();

    let err = store
        .create_relationship(EntityRelationship {
            from: "ghost".to_string(),
            to: to_id,
            relationship_type: RelationshipType::Contains,
            metadata: Default::default(),
        })
        .await
        .unwrap_err();

    assert!(matches!(err, EntityError::NotFound(_)));
}

#[tokio::test]
async fn create_relationship_with_missing_to_entity_is_rejected() {
    let mut store = InMemoryEntityStore::new();
    let from_id = store.store(git_entity("from")).await.unwrap();

    let err = store
        .create_relationship(EntityRelationship {
            from: from_id,
            to: "ghost".to_string(),
            relationship_type: RelationshipType::Contains,
            metadata: Default::default(),
        })
        .await
        .unwrap_err();

    assert!(matches!(err, EntityError::NotFound(_)));
}

#[tokio::test]
async fn get_relationships_for_unknown_id_returns_empty_vec() {
    let store = InMemoryEntityStore::new();
    // The current implementation returns Ok(vec![]) for unknown IDs, which is
    // a valid design choice (no entity → no relationships).
    let rels = store.get_relationships("unknown").await.unwrap();
    assert!(rels.is_empty());
}
