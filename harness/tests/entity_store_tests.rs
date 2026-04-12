//! Unit tests for `InMemoryEntityStore`.
//!
//! The store is the backbone of the agent loop — every entity the agent
//! reads or writes flows through it.  The existing in-file test only checked
//! that a fresh store is empty; all the filtering, guard-rail, and
//! relationship logic was untested.  These tests cover every non-trivial
//! branch in the `EntityStore` implementation.

use chrono::{Duration, Utc};
use harness::entities::{
    context::types::ContextEntity,
    git::types::{GitBranch, GitRepository},
    EntityQuery, EntityRelationship, EntityStore, EntityType, InMemoryEntityStore,
    RelationshipType, TimeRange,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn git_repo() -> GitRepository {
    GitRepository::new(
        "git@github.com:example/repo.git".to_string(),
        "main".to_string(),
    )
}

fn git_branch(name: &str) -> GitBranch {
    GitBranch::new_local(name.to_string(), "abc123def456".to_string())
}

fn context_entity(task: &str) -> ContextEntity {
    ContextEntity::new(
        task.to_string(),
        vec![model::types::ChatMessage::user(task)],
        vec![],
        "done".to_string(),
        "qwen2.5:0.5b".to_string(),
    )
}

// ---------------------------------------------------------------------------
// Basic CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_returns_the_assigned_id() {
    let mut store = InMemoryEntityStore::new();
    let repo = git_repo();
    let expected_id = repo.metadata.id.clone();

    let returned_id = store.store(Box::new(repo)).await.unwrap();
    assert_eq!(returned_id, expected_id);
}

#[tokio::test]
async fn exists_is_true_after_store_and_false_before() {
    let mut store = InMemoryEntityStore::new();
    let repo = git_repo();
    let id = repo.metadata.id.clone();

    assert!(!store.exists(&id).await, "should not exist before storing");
    store.store(Box::new(repo)).await.unwrap();
    assert!(store.exists(&id).await, "should exist after storing");
}

#[tokio::test]
async fn storing_duplicate_id_returns_already_exists_error() {
    let mut store = InMemoryEntityStore::new();
    let repo = git_repo();
    let id = repo.metadata.id.clone();

    store.store(Box::new(repo.clone())).await.unwrap();
    let err = store.store(Box::new(repo)).await.unwrap_err();

    let err_string = err.to_string();
    assert!(
        err_string.contains(&id) || err_string.to_lowercase().contains("already exists"),
        "error should mention the duplicate id: {err_string}"
    );
}

#[tokio::test]
async fn update_replaces_the_entity() {
    let mut store = InMemoryEntityStore::new();
    let mut repo = git_repo();
    let id = repo.metadata.id.clone();
    store.store(Box::new(repo.clone())).await.unwrap();

    // Mutate and update
    repo.default_branch = "develop".to_string();
    store.update(Box::new(repo)).await.unwrap();

    // Confirm update was recorded (query and check JSON)
    let results = store
        .query(&EntityQuery {
            entity_types: vec![EntityType::Git],
            text_query: Some("develop".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        results.iter().any(|r| r.entity_id == id),
        "updated entity should be findable by its new content"
    );
}

#[tokio::test]
async fn update_nonexistent_entity_returns_not_found_error() {
    let mut store = InMemoryEntityStore::new();
    let repo = git_repo();

    let err = store.update(Box::new(repo)).await.unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("not found"),
        "error should say not found: {err}"
    );
}

#[tokio::test]
async fn delete_removes_entity_and_exists_returns_false() {
    let mut store = InMemoryEntityStore::new();
    let repo = git_repo();
    let id = repo.metadata.id.clone();

    store.store(Box::new(repo)).await.unwrap();
    store.delete(&id).await.unwrap();

    assert!(!store.exists(&id).await, "should not exist after deletion");
}

#[tokio::test]
async fn delete_nonexistent_entity_returns_not_found_error() {
    let mut store = InMemoryEntityStore::new();

    let err = store.delete("ghost-id-that-never-existed").await.unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("not found"),
        "error should say not found: {err}"
    );
}

#[tokio::test]
async fn delete_is_idempotent_only_for_first_call() {
    let mut store = InMemoryEntityStore::new();
    let repo = git_repo();
    let id = repo.metadata.id.clone();
    store.store(Box::new(repo)).await.unwrap();

    store.delete(&id).await.unwrap(); // first delete: ok
    let err = store.delete(&id).await.unwrap_err(); // second delete: error
    assert!(err.to_string().to_lowercase().contains("not found"));
}

// ---------------------------------------------------------------------------
// Query – entity-type filtering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_returns_all_when_no_type_filter() {
    let mut store = InMemoryEntityStore::new();
    store.store(Box::new(git_repo())).await.unwrap();
    store.store(Box::new(context_entity("task A"))).await.unwrap();

    let results = store.query(&EntityQuery::default()).await.unwrap();
    assert_eq!(results.len(), 2, "unfiltered query should return all entities");
}

#[tokio::test]
async fn query_filters_by_entity_type() {
    let mut store = InMemoryEntityStore::new();
    store.store(Box::new(git_repo())).await.unwrap();
    store.store(Box::new(git_branch("feat/foo"))).await.unwrap();
    store.store(Box::new(context_entity("task A"))).await.unwrap();

    let git_results = store
        .query(&EntityQuery {
            entity_types: vec![EntityType::Git],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(git_results.len(), 2, "should return only Git-type entities");

    let ctx_results = store
        .query(&EntityQuery {
            entity_types: vec![EntityType::Context],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ctx_results.len(), 1, "should return only Context entities");
}

#[tokio::test]
async fn query_with_type_filter_and_zero_matches_returns_empty_vec() {
    let mut store = InMemoryEntityStore::new();
    store.store(Box::new(git_repo())).await.unwrap();

    let results = store
        .query(&EntityQuery {
            entity_types: vec![EntityType::Context],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(results.is_empty());
}

// ---------------------------------------------------------------------------
// Query – tag filtering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_filters_by_tags() {
    let mut store = InMemoryEntityStore::new();

    let mut repo = git_repo();
    repo.metadata.tags = vec!["production".to_string(), "primary".to_string()];
    let repo_id = repo.metadata.id.clone();
    store.store(Box::new(repo)).await.unwrap();

    let mut ctx = context_entity("task B");
    ctx.metadata.tags = vec!["staging".to_string()];
    store.store(Box::new(ctx)).await.unwrap();

    let prod_results = store
        .query(&EntityQuery {
            tags: vec!["production".to_string()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(prod_results.len(), 1);
    assert_eq!(prod_results[0].entity_id, repo_id);
}

#[tokio::test]
async fn query_tag_filter_matches_any_tag_not_all() {
    let mut store = InMemoryEntityStore::new();

    let mut e1 = git_repo();
    e1.metadata.tags = vec!["alpha".to_string()];
    store.store(Box::new(e1)).await.unwrap();

    let mut e2 = context_entity("task");
    e2.metadata.tags = vec!["beta".to_string()];
    store.store(Box::new(e2)).await.unwrap();

    // Querying for either tag should match both entities (OR semantics)
    let results = store
        .query(&EntityQuery {
            tags: vec!["alpha".to_string(), "beta".to_string()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(results.len(), 2, "tag filter should use OR semantics");
}

// ---------------------------------------------------------------------------
// Query – text search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_text_search_finds_matching_entities() {
    let mut store = InMemoryEntityStore::new();
    store
        .store(Box::new(context_entity("implement OAuth login")))
        .await
        .unwrap();
    store
        .store(Box::new(context_entity("refactor database layer")))
        .await
        .unwrap();

    let results = store
        .query(&EntityQuery {
            text_query: Some("OAuth".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].relevance, 0.8);
}

#[tokio::test]
async fn query_text_search_is_case_insensitive() {
    let mut store = InMemoryEntityStore::new();
    store
        .store(Box::new(context_entity("implement OAuth login")))
        .await
        .unwrap();

    let lower = store
        .query(&EntityQuery {
            text_query: Some("oauth".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(lower.len(), 1, "text search must be case-insensitive");
}

#[tokio::test]
async fn query_text_search_excludes_non_matching_entities() {
    let mut store = InMemoryEntityStore::new();
    store
        .store(Box::new(context_entity("implement OAuth login")))
        .await
        .unwrap();

    let results = store
        .query(&EntityQuery {
            text_query: Some("completely_absent_token_xyzzy".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(results.is_empty());
}

// ---------------------------------------------------------------------------
// Query – time range filtering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_time_range_excludes_entities_outside_window() {
    let mut store = InMemoryEntityStore::new();

    // Store an entity first — it gets `created_at = Utc::now()`
    let entity = context_entity("recent task");
    let id = entity.metadata.id.clone();
    store.store(Box::new(entity)).await.unwrap();

    // A time range entirely in the past should exclude the entity
    let past_end = Utc::now() - Duration::hours(1);
    let results = store
        .query(&EntityQuery {
            time_range: Some(TimeRange {
                start: past_end - Duration::hours(1),
                end: past_end,
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        results.iter().all(|r| r.entity_id != id),
        "entity created now should not appear in a past time window"
    );
}

#[tokio::test]
async fn query_time_range_includes_entities_inside_window() {
    let mut store = InMemoryEntityStore::new();

    let entity = context_entity("current task");
    let id = entity.metadata.id.clone();
    store.store(Box::new(entity)).await.unwrap();

    // A generous window around "now" should include it
    let results = store
        .query(&EntityQuery {
            time_range: Some(TimeRange {
                start: Utc::now() - Duration::minutes(5),
                end: Utc::now() + Duration::minutes(5),
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        results.iter().any(|r| r.entity_id == id),
        "entity created within the window should be included"
    );
}

// ---------------------------------------------------------------------------
// Query – limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_limit_truncates_results() {
    let mut store = InMemoryEntityStore::new();
    for i in 0..5 {
        store
            .store(Box::new(context_entity(&format!("task {i}"))))
            .await
            .unwrap();
    }

    let results = store
        .query(&EntityQuery {
            limit: Some(3),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(results.len(), 3, "limit should cap the result count");
}

#[tokio::test]
async fn query_limit_larger_than_count_returns_all() {
    let mut store = InMemoryEntityStore::new();
    store.store(Box::new(git_repo())).await.unwrap();
    store.store(Box::new(context_entity("task"))).await.unwrap();

    let results = store
        .query(&EntityQuery {
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
}

// ---------------------------------------------------------------------------
// Relationships
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_and_retrieve_relationship() {
    let mut store = InMemoryEntityStore::new();

    let repo = git_repo();
    let branch = git_branch("main");
    let repo_id = repo.metadata.id.clone();
    let branch_id = branch.metadata.id.clone();

    store.store(Box::new(repo)).await.unwrap();
    store.store(Box::new(branch)).await.unwrap();

    store
        .create_relationship(EntityRelationship {
            from: repo_id.clone(),
            to: branch_id.clone(),
            relationship_type: RelationshipType::Contains,
            metadata: Default::default(),
        })
        .await
        .unwrap();

    // Both endpoints should see the relationship
    let from_rels = store.get_relationships(&repo_id).await.unwrap();
    assert_eq!(from_rels.len(), 1);
    assert_eq!(from_rels[0].to, branch_id);

    let to_rels = store.get_relationships(&branch_id).await.unwrap();
    assert_eq!(to_rels.len(), 1);
    assert_eq!(to_rels[0].from, repo_id);
}

#[tokio::test]
async fn create_relationship_requires_both_entities_to_exist() {
    let mut store = InMemoryEntityStore::new();
    let repo = git_repo();
    let repo_id = repo.metadata.id.clone();
    store.store(Box::new(repo)).await.unwrap();

    // Target entity does NOT exist
    let err = store
        .create_relationship(EntityRelationship {
            from: repo_id,
            to: "nonexistent-entity-id".to_string(),
            relationship_type: RelationshipType::References,
            metadata: Default::default(),
        })
        .await
        .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("not found"),
        "should reject relationship with missing target: {err}"
    );
}

#[tokio::test]
async fn create_relationship_rejects_missing_source_entity() {
    let mut store = InMemoryEntityStore::new();
    let branch = git_branch("main");
    let branch_id = branch.metadata.id.clone();
    store.store(Box::new(branch)).await.unwrap();

    let err = store
        .create_relationship(EntityRelationship {
            from: "nonexistent-source".to_string(),
            to: branch_id,
            relationship_type: RelationshipType::Modifies,
            metadata: Default::default(),
        })
        .await
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("not found"));
}

#[tokio::test]
async fn delete_relationship_removes_only_the_matching_edge() {
    let mut store = InMemoryEntityStore::new();

    let repo = git_repo();
    let branch = git_branch("main");
    let repo_id = repo.metadata.id.clone();
    let branch_id = branch.metadata.id.clone();
    store.store(Box::new(repo)).await.unwrap();
    store.store(Box::new(branch)).await.unwrap();

    // Create two relationships between the same pair
    store
        .create_relationship(EntityRelationship {
            from: repo_id.clone(),
            to: branch_id.clone(),
            relationship_type: RelationshipType::Contains,
            metadata: Default::default(),
        })
        .await
        .unwrap();
    store
        .create_relationship(EntityRelationship {
            from: repo_id.clone(),
            to: branch_id.clone(),
            relationship_type: RelationshipType::References,
            metadata: Default::default(),
        })
        .await
        .unwrap();

    // Delete only the Contains edge
    store
        .delete_relationship(&repo_id, &branch_id, RelationshipType::Contains)
        .await
        .unwrap();

    let remaining = store.get_relationships(&repo_id).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].relationship_type, RelationshipType::References);
}

#[tokio::test]
async fn get_relationships_for_entity_with_no_edges_returns_empty_vec() {
    let mut store = InMemoryEntityStore::new();
    let repo = git_repo();
    let id = repo.metadata.id.clone();
    store.store(Box::new(repo)).await.unwrap();

    let rels = store.get_relationships(&id).await.unwrap();
    assert!(rels.is_empty());
}

// ---------------------------------------------------------------------------
// Compound / lifecycle scenarios
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_update_query_delete_full_lifecycle() {
    let mut store = InMemoryEntityStore::new();

    // 1. Store
    let mut repo = git_repo();
    let id = repo.metadata.id.clone();
    store.store(Box::new(repo.clone())).await.unwrap();
    assert!(store.exists(&id).await);

    // 2. Update — change the default branch
    repo.default_branch = "release".to_string();
    store.update(Box::new(repo)).await.unwrap();

    // 3. Query — new value must be findable
    let results = store
        .query(&EntityQuery {
            text_query: Some("release".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(results.iter().any(|r| r.entity_id == id));

    // 4. Delete
    store.delete(&id).await.unwrap();
    assert!(!store.exists(&id).await);

    // 5. Query must no longer return the entity
    let after_delete = store.query(&EntityQuery::default()).await.unwrap();
    assert!(after_delete.iter().all(|r| r.entity_id != id));
}

#[tokio::test]
async fn multiple_entity_types_coexist_and_are_independently_queryable() {
    let mut store = InMemoryEntityStore::new();

    let git_id = {
        let e = git_repo();
        let id = e.metadata.id.clone();
        store.store(Box::new(e)).await.unwrap();
        id
    };
    let ctx_id = {
        let e = context_entity("build the feature");
        let id = e.metadata.id.clone();
        store.store(Box::new(e)).await.unwrap();
        id
    };

    let all = store.query(&EntityQuery::default()).await.unwrap();
    assert_eq!(all.len(), 2);

    let git_only = store
        .query(&EntityQuery {
            entity_types: vec![EntityType::Git],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(git_only.len(), 1);
    assert_eq!(git_only[0].entity_id, git_id);

    let ctx_only = store
        .query(&EntityQuery {
            entity_types: vec![EntityType::Context],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ctx_only.len(), 1);
    assert_eq!(ctx_only[0].entity_id, ctx_id);
}
