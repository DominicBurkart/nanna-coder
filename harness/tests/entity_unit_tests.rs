//! Unit tests for [`InMemoryEntityStore`].
//!
//! These tests exercise the store/retrieve/delete/query contract of
//! [`EntityStore`] using the concrete [`ContextEntity`] and [`GitRepository`]
//! types so that no production logic is stubbed out.  They are intentionally
//! small and fast — no containers, no network, no filesystem access.

use harness::entities::{
    EntityError, EntityQuery, EntityRelationship, EntityStore, EntityType,
    InMemoryEntityStore, RelationshipType,
};
use harness::entities::context::types::ContextEntity;
use harness::entities::git::types::GitRepository;

// ---------------------------------------------------------------------------
// Helper: build a minimal ContextEntity
// ---------------------------------------------------------------------------

fn make_context_entity(description: &str) -> ContextEntity {
    ContextEntity::new(
        description.to_string(),
        vec![],
        vec![],
        String::new(),
        "test-model".to_string(),
    )
}

fn make_git_entity(remote: &str) -> GitRepository {
    GitRepository::new(remote.to_string(), "main".to_string())
}

// ---------------------------------------------------------------------------
// store / exists
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_and_exists_by_id() {
    let mut store = InMemoryEntityStore::new();
    let entity = make_context_entity("store and exists test");
    let id = store.store(Box::new(entity)).await.unwrap();

    assert!(store.exists(&id).await, "stored entity should be found by id");
    assert!(
        !store.exists("nonexistent-id").await,
        "unknown id must not exist"
    );
}

#[tokio::test]
async fn store_duplicate_id_returns_already_exists_error() {
    let mut store = InMemoryEntityStore::new();
    let entity = make_context_entity("duplicate test");

    // Store once — succeeds.
    let id = store.store(Box::new(entity.clone())).await.unwrap();

    // Store the same entity (same UUID) a second time — must fail.
    let entity2 = entity; // same UUID embedded in metadata
    let result = store.store(Box::new(entity2)).await;
    assert!(
        matches!(result, Err(EntityError::AlreadyExists(ref eid)) if eid == &id),
        "expected AlreadyExists error, got: {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// multiple entities / query
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_multiple_entities_all_retrievable_via_query() {
    let mut store = InMemoryEntityStore::new();

    let ids: Vec<String> = futures::future::join_all(
        (0..3).map(|i| {
            let entity = make_context_entity(&format!("entity {}", i));
            store.store(Box::new(entity))
        }),
    )
    .await
    .into_iter()
    .map(|r| r.unwrap())
    .collect();

    // Querying with no filters should return all three.
    let query = EntityQuery::default();
    let results = store.query(&query).await.unwrap();

    assert_eq!(
        results.len(),
        3,
        "all stored entities should appear in an unfiltered query"
    );

    let result_ids: Vec<_> = results.iter().map(|r| r.entity_id.clone()).collect();
    for id in &ids {
        assert!(result_ids.contains(id), "id {} missing from query results", id);
    }
}

#[tokio::test]
async fn query_by_entity_type_filters_correctly() {
    let mut store = InMemoryEntityStore::new();

    // Store one Context entity and one Git entity.
    let ctx_id = store
        .store(Box::new(make_context_entity("context entity")))
        .await
        .unwrap();
    let git_id = store
        .store(Box::new(make_git_entity("https://github.com/user/repo")))
        .await
        .unwrap();

    // Query for Context only.
    let ctx_query = EntityQuery {
        entity_types: vec![EntityType::Context],
        ..Default::default()
    };
    let ctx_results = store.query(&ctx_query).await.unwrap();
    assert_eq!(ctx_results.len(), 1);
    assert_eq!(ctx_results[0].entity_id, ctx_id);
    assert_eq!(ctx_results[0].entity_type, EntityType::Context);

    // Query for Git only.
    let git_query = EntityQuery {
        entity_types: vec![EntityType::Git],
        ..Default::default()
    };
    let git_results = store.query(&git_query).await.unwrap();
    assert_eq!(git_results.len(), 1);
    assert_eq!(git_results[0].entity_id, git_id);
    assert_eq!(git_results[0].entity_type, EntityType::Git);
}

#[tokio::test]
async fn query_limit_is_respected() {
    let mut store = InMemoryEntityStore::new();

    for i in 0..5 {
        store
            .store(Box::new(make_context_entity(&format!("entity {}", i))))
            .await
            .unwrap();
    }

    let query = EntityQuery {
        limit: Some(2),
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert_eq!(results.len(), 2, "limit should cap the number of results");
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_existing_entity_succeeds() {
    let mut store = InMemoryEntityStore::new();
    let entity = make_context_entity("original description");
    let id = store.store(Box::new(entity)).await.unwrap();

    // Build a replacement entity with the same UUID.
    let mut replacement = make_context_entity("updated description");
    // Reuse the assigned UUID so update() recognises it.
    replacement.metadata.id = id.clone();

    store.update(Box::new(replacement)).await.unwrap();
    assert!(store.exists(&id).await);
}

#[tokio::test]
async fn update_nonexistent_entity_returns_not_found() {
    let mut store = InMemoryEntityStore::new();
    let entity = make_context_entity("ghost entity");
    // Don't store it first — update should fail.
    let result = store.update(Box::new(entity)).await;
    assert!(
        matches!(result, Err(EntityError::NotFound(_))),
        "expected NotFound, got: {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_removes_entity() {
    let mut store = InMemoryEntityStore::new();
    let entity = make_context_entity("to be deleted");
    let id = store.store(Box::new(entity)).await.unwrap();

    store.delete(&id).await.unwrap();
    assert!(!store.exists(&id).await, "entity should no longer exist after deletion");
}

#[tokio::test]
async fn delete_nonexistent_entity_returns_not_found() {
    let mut store = InMemoryEntityStore::new();
    let result = store.delete("totally-unknown-id").await;
    assert!(
        matches!(result, Err(EntityError::NotFound(_))),
        "expected NotFound, got: {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// relationships
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_and_retrieve_relationship() {
    let mut store = InMemoryEntityStore::new();
    let id_a = store
        .store(Box::new(make_context_entity("entity A")))
        .await
        .unwrap();
    let id_b = store
        .store(Box::new(make_context_entity("entity B")))
        .await
        .unwrap();

    let rel = EntityRelationship {
        from: id_a.clone(),
        to: id_b.clone(),
        relationship_type: RelationshipType::References,
        metadata: std::collections::HashMap::new(),
    };
    store.create_relationship(rel).await.unwrap();

    let rels_a = store.get_relationships(&id_a).await.unwrap();
    assert_eq!(rels_a.len(), 1);
    assert_eq!(rels_a[0].relationship_type, RelationshipType::References);
    assert_eq!(rels_a[0].from, id_a);
    assert_eq!(rels_a[0].to, id_b);

    // The relationship should be visible from both ends.
    let rels_b = store.get_relationships(&id_b).await.unwrap();
    assert_eq!(rels_b.len(), 1);
}

#[tokio::test]
async fn create_relationship_requires_both_entities_to_exist() {
    let mut store = InMemoryEntityStore::new();
    let id_a = store
        .store(Box::new(make_context_entity("entity A")))
        .await
        .unwrap();

    // "entity B" was never stored.
    let rel = EntityRelationship {
        from: id_a.clone(),
        to: "does-not-exist".to_string(),
        relationship_type: RelationshipType::References,
        metadata: std::collections::HashMap::new(),
    };
    let result = store.create_relationship(rel).await;
    assert!(
        matches!(result, Err(EntityError::NotFound(_))),
        "expected NotFound for missing target, got: {:?}",
        result
    );
}

#[tokio::test]
async fn delete_relationship_removes_it() {
    let mut store = InMemoryEntityStore::new();
    let id_a = store
        .store(Box::new(make_context_entity("entity A")))
        .await
        .unwrap();
    let id_b = store
        .store(Box::new(make_context_entity("entity B")))
        .await
        .unwrap();

    let rel = EntityRelationship {
        from: id_a.clone(),
        to: id_b.clone(),
        relationship_type: RelationshipType::Contains,
        metadata: std::collections::HashMap::new(),
    };
    store.create_relationship(rel).await.unwrap();

    // Verify it exists.
    assert_eq!(store.get_relationships(&id_a).await.unwrap().len(), 1);

    // Now delete it.
    store
        .delete_relationship(&id_a, &id_b, RelationshipType::Contains)
        .await
        .unwrap();

    // Must be gone.
    assert_eq!(
        store.get_relationships(&id_a).await.unwrap().len(),
        0,
        "relationship should be removed after delete_relationship"
    );
}
