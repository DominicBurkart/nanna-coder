//! Tests for InMemoryEntityStore CRUD, query, and relationship operations.
//!
//! The entity store is the backbone of Nanna's entity management system.
//! These tests verify the full lifecycle: store, exists, update, delete,
//! query filtering, and relationship integrity.

use harness::entities::context::types::ContextEntity;
use harness::entities::git::types::GitRepository;
use harness::entities::{
    Entity, EntityError, EntityQuery, EntityRelationship, EntityStore, EntityType,
    InMemoryEntityStore, RelationshipType, TimeRange,
};

// ---------------------------------------------------------------------------
// CRUD basics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_and_exists() {
    let mut store = InMemoryEntityStore::new();
    let repo = Box::new(GitRepository::new("url".into(), "main".into()));
    let id = repo.id().to_string();

    assert!(!store.exists(&id).await);
    let returned_id = store.store(repo).await.unwrap();
    assert_eq!(returned_id, id);
    assert!(store.exists(&id).await);
}

#[tokio::test]
async fn store_duplicate_returns_error() {
    let mut store = InMemoryEntityStore::new();
    let repo = GitRepository::new("url".into(), "main".into());
    let _id = repo.id().to_string();

    // Clone-via-serde so we can store twice with the same id
    let json = repo.to_json().unwrap();
    let repo2: GitRepository = serde_json::from_str(&json).unwrap();

    store.store(Box::new(repo)).await.unwrap();
    let err = store.store(Box::new(repo2)).await.unwrap_err();
    assert!(matches!(err, EntityError::AlreadyExists(_)));
}

#[tokio::test]
async fn update_existing_entity() {
    let mut store = InMemoryEntityStore::new();
    let mut repo = GitRepository::new("url".into(), "main".into());
    let id = repo.id().to_string();
    store.store(Box::new(repo.clone())).await.unwrap();

    // Mutate and update
    repo.default_branch = "develop".into();
    store.update(Box::new(repo)).await.unwrap();
    assert!(store.exists(&id).await);
}

#[tokio::test]
async fn update_nonexistent_entity_fails() {
    let mut store = InMemoryEntityStore::new();
    let repo = GitRepository::new("url".into(), "main".into());
    let err = store.update(Box::new(repo)).await.unwrap_err();
    assert!(matches!(err, EntityError::NotFound(_)));
}

#[tokio::test]
async fn delete_existing_entity() {
    let mut store = InMemoryEntityStore::new();
    let repo = GitRepository::new("url".into(), "main".into());
    let id = repo.id().to_string();
    store.store(Box::new(repo)).await.unwrap();

    store.delete(&id).await.unwrap();
    assert!(!store.exists(&id).await);
}

#[tokio::test]
async fn delete_nonexistent_entity_fails() {
    let mut store = InMemoryEntityStore::new();
    let err = store.delete("ghost").await.unwrap_err();
    assert!(matches!(err, EntityError::NotFound(_)));
}

// ---------------------------------------------------------------------------
// Query filtering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_all_returns_everything() {
    let mut store = InMemoryEntityStore::new();
    store
        .store(Box::new(GitRepository::new("u".into(), "m".into())))
        .await
        .unwrap();
    store
        .store(Box::new(ContextEntity::default()))
        .await
        .unwrap();

    let results = store.query(&EntityQuery::default()).await.unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn query_filters_by_entity_type() {
    let mut store = InMemoryEntityStore::new();
    store
        .store(Box::new(GitRepository::new("u".into(), "m".into())))
        .await
        .unwrap();
    store
        .store(Box::new(ContextEntity::default()))
        .await
        .unwrap();

    let query = EntityQuery {
        entity_types: vec![EntityType::Git],
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entity_type, EntityType::Git);
}

#[tokio::test]
async fn query_filters_by_tags() {
    let mut store = InMemoryEntityStore::new();

    let mut repo = GitRepository::new("u".into(), "m".into());
    repo.metadata.tags = vec!["important".into()];
    store.store(Box::new(repo)).await.unwrap();

    let mut ctx = ContextEntity::default();
    ctx.metadata.tags = vec!["archive".into()];
    store.store(Box::new(ctx)).await.unwrap();

    let query = EntityQuery {
        tags: vec!["important".into()],
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn query_text_search_matches_json_content() {
    let mut store = InMemoryEntityStore::new();
    store
        .store(Box::new(GitRepository::new(
            "https://github.com/foo/bar".into(),
            "main".into(),
        )))
        .await
        .unwrap();

    let query = EntityQuery {
        text_query: Some("foo/bar".into()),
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert_eq!(results.len(), 1);

    // Non-matching text returns nothing
    let query = EntityQuery {
        text_query: Some("zzz_nonexistent_zzz".into()),
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn query_respects_limit() {
    let mut store = InMemoryEntityStore::new();
    for _ in 0..5 {
        store
            .store(Box::new(GitRepository::new("u".into(), "m".into())))
            .await
            .unwrap();
    }

    let query = EntityQuery {
        limit: Some(2),
        ..Default::default()
    };
    let results = store.query(&query).await.unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn query_filters_by_time_range() {
    let mut store = InMemoryEntityStore::new();
    let repo = GitRepository::new("u".into(), "m".into());
    let created = repo.metadata.created_at;
    store.store(Box::new(repo)).await.unwrap();

    // Range that includes the entity
    let query = EntityQuery {
        time_range: Some(TimeRange {
            start: created - chrono::Duration::seconds(1),
            end: created + chrono::Duration::seconds(1),
        }),
        ..Default::default()
    };
    assert_eq!(store.query(&query).await.unwrap().len(), 1);

    // Range that excludes it (far in the past)
    let query = EntityQuery {
        time_range: Some(TimeRange {
            start: created - chrono::Duration::days(10),
            end: created - chrono::Duration::days(5),
        }),
        ..Default::default()
    };
    assert!(store.query(&query).await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Relationships
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_and_get_relationship() {
    let mut store = InMemoryEntityStore::new();
    let repo = GitRepository::new("u".into(), "m".into());
    let ctx = ContextEntity::default();
    let repo_id = repo.id().to_string();
    let ctx_id = ctx.id().to_string();

    store.store(Box::new(repo)).await.unwrap();
    store.store(Box::new(ctx)).await.unwrap();

    let rel = EntityRelationship {
        from: repo_id.clone(),
        to: ctx_id.clone(),
        relationship_type: RelationshipType::References,
        metadata: Default::default(),
    };
    store.create_relationship(rel).await.unwrap();

    let rels = store.get_relationships(&repo_id).await.unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].to, ctx_id);

    // Also reachable from the other side
    let rels = store.get_relationships(&ctx_id).await.unwrap();
    assert_eq!(rels.len(), 1);
}

#[tokio::test]
async fn create_relationship_with_missing_entity_fails() {
    let mut store = InMemoryEntityStore::new();
    let repo = GitRepository::new("u".into(), "m".into());
    let repo_id = repo.id().to_string();
    store.store(Box::new(repo)).await.unwrap();

    let rel = EntityRelationship {
        from: repo_id,
        to: "nonexistent".into(),
        relationship_type: RelationshipType::Contains,
        metadata: Default::default(),
    };
    let err = store.create_relationship(rel).await.unwrap_err();
    assert!(matches!(err, EntityError::NotFound(_)));
}

#[tokio::test]
async fn delete_relationship() {
    let mut store = InMemoryEntityStore::new();
    let repo = GitRepository::new("u".into(), "m".into());
    let ctx = ContextEntity::default();
    let repo_id = repo.id().to_string();
    let ctx_id = ctx.id().to_string();
    store.store(Box::new(repo)).await.unwrap();
    store.store(Box::new(ctx)).await.unwrap();

    store
        .create_relationship(EntityRelationship {
            from: repo_id.clone(),
            to: ctx_id.clone(),
            relationship_type: RelationshipType::Modifies,
            metadata: Default::default(),
        })
        .await
        .unwrap();

    store
        .delete_relationship(&repo_id, &ctx_id, RelationshipType::Modifies)
        .await
        .unwrap();

    let rels = store.get_relationships(&repo_id).await.unwrap();
    assert!(rels.is_empty());
}
