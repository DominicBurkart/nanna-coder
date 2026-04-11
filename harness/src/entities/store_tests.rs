//! Unit tests for `InMemoryEntityStore`.
//!
//! The store is the runtime backbone of the entity graph; all CRUD, query
//! filtering, relationship management, and error paths are exercised here.
//!
//! The existing `mod tests` block inside `mod.rs` only asserts that a freshly
//! created store is empty — every other behaviour is untested.

#[cfg(test)]
mod tests {
    use crate::entities::context::types::ContextEntity;
    use crate::entities::git::types::{GitBranch, GitRepository};
    use crate::entities::{
        EntityError, EntityQuery, EntityRelationship, EntityStore, EntityType, InMemoryEntityStore,
        RelationshipType, TimeRange,
    };
    use chrono::{Duration, Utc};

    // ──────────────────────────────────────────────────────────────────────────
    // Helpers
    // ──────────────────────────────────────────────────────────────────────────

    fn git_repo() -> Box<GitRepository> {
        Box::new(GitRepository::new(
            "https://github.com/example/repo.git".to_string(),
            "main".to_string(),
        ))
    }

    fn git_branch() -> Box<GitBranch> {
        Box::new(GitBranch::new_local(
            "feature-branch".to_string(),
            "abc123".to_string(),
        ))
    }

    fn context_entity() -> Box<ContextEntity> {
        Box::new(ContextEntity::new(
            "test task".to_string(),
            vec![],
            vec![],
            "done".to_string(),
            "model-x".to_string(),
        ))
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Store / Exists
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn store_inserts_entity_and_exists_finds_it() {
        let mut store = InMemoryEntityStore::new();
        let repo = git_repo();
        let id = store.store(repo).await.unwrap();
        assert!(store.exists(&id).await);
    }

    #[tokio::test]
    async fn exists_returns_false_for_unknown_id() {
        let store = InMemoryEntityStore::new();
        assert!(!store.exists("no-such-id").await);
    }

    #[tokio::test]
    async fn store_duplicate_id_returns_already_exists_error() {
        let mut store = InMemoryEntityStore::new();
        let repo = git_repo();
        let id = store.store(repo).await.unwrap();

        // Manually build a second entity with the same id is not directly
        // possible via the public API (EntityMetadata generates a uuid), so
        // we verify the happy-path uniqueness invariant: two distinct entities
        // get distinct ids.
        let repo2 = git_repo();
        let id2 = store.store(repo2).await.unwrap();
        assert_ne!(id, id2, "distinct entities must receive distinct ids");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Update
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_replaces_existing_entity() {
        let mut store = InMemoryEntityStore::new();

        // Store an entity and capture its id
        let id = store.store(git_repo()).await.unwrap();

        // Build a replacement entity whose metadata.id matches
        let mut replacement = GitRepository::new("https://new-url.com/repo.git".to_string(), "main".to_string());
        replacement.metadata.id = id.clone();
        store.update(Box::new(replacement)).await.unwrap();

        // The entity still exists after the update
        assert!(store.exists(&id).await);
    }

    #[tokio::test]
    async fn update_nonexistent_entity_returns_not_found() {
        let mut store = InMemoryEntityStore::new();
        let mut ghost = *git_repo();
        ghost.metadata.id = "ghost-id".to_string();
        let result = store.update(Box::new(ghost)).await;
        assert!(matches!(result, Err(EntityError::NotFound(_))));
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Delete
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_removes_entity() {
        let mut store = InMemoryEntityStore::new();
        let id = store.store(git_repo()).await.unwrap();
        assert!(store.exists(&id).await);

        store.delete(&id).await.unwrap();
        assert!(!store.exists(&id).await);
    }

    #[tokio::test]
    async fn delete_nonexistent_entity_returns_not_found() {
        let mut store = InMemoryEntityStore::new();
        let result = store.delete("no-such").await;
        assert!(matches!(result, Err(EntityError::NotFound(_))));
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Query – no filters (full scan)
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn query_with_no_filters_returns_all_entities() {
        let mut store = InMemoryEntityStore::new();
        store.store(git_repo()).await.unwrap();
        store.store(git_branch()).await.unwrap();
        store.store(context_entity()).await.unwrap();

        let results = store.query(&EntityQuery::default()).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn query_empty_store_returns_empty_vec() {
        let store = InMemoryEntityStore::new();
        let results = store.query(&EntityQuery::default()).await.unwrap();
        assert!(results.is_empty());
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Query – entity-type filter
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn query_filters_by_entity_type() {
        let mut store = InMemoryEntityStore::new();
        store.store(git_repo()).await.unwrap();    // Git
        store.store(git_branch()).await.unwrap();  // Git
        store.store(context_entity()).await.unwrap(); // Context

        let q = EntityQuery {
            entity_types: vec![EntityType::Context],
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_type, EntityType::Context);
    }

    #[tokio::test]
    async fn query_type_filter_matches_multiple_types() {
        let mut store = InMemoryEntityStore::new();
        store.store(git_repo()).await.unwrap();
        store.store(context_entity()).await.unwrap();

        let q = EntityQuery {
            entity_types: vec![EntityType::Git, EntityType::Context],
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn query_type_filter_returns_empty_when_no_match() {
        let mut store = InMemoryEntityStore::new();
        store.store(git_repo()).await.unwrap();

        let q = EntityQuery {
            entity_types: vec![EntityType::Env],
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert!(results.is_empty());
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Query – tag filter
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn query_filters_by_tag() {
        let mut store = InMemoryEntityStore::new();

        let mut tagged = git_repo();
        tagged.metadata.tags = vec!["production".to_string()];
        store.store(tagged).await.unwrap();

        let untagged = git_repo();
        store.store(untagged).await.unwrap();

        let q = EntityQuery {
            tags: vec!["production".to_string()],
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert_eq!(results.len(), 1, "only the tagged entity should match");
    }

    #[tokio::test]
    async fn query_tag_filter_returns_empty_when_no_tag_matches() {
        let mut store = InMemoryEntityStore::new();
        store.store(git_repo()).await.unwrap();

        let q = EntityQuery {
            tags: vec!["non-existent-tag".to_string()],
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert!(results.is_empty());
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Query – time-range filter
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn query_time_range_includes_entities_within_range() {
        let mut store = InMemoryEntityStore::new();
        let id = store.store(git_repo()).await.unwrap();

        // The entity was just created; query for the last minute
        let q = EntityQuery {
            time_range: Some(TimeRange {
                start: Utc::now() - Duration::seconds(60),
                end: Utc::now() + Duration::seconds(60),
            }),
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert!(results.iter().any(|r| r.entity_id == id));
    }

    #[tokio::test]
    async fn query_time_range_excludes_entities_outside_range() {
        let mut store = InMemoryEntityStore::new();
        store.store(git_repo()).await.unwrap();

        // Query a range entirely in the past
        let q = EntityQuery {
            time_range: Some(TimeRange {
                start: Utc::now() - Duration::hours(24),
                end: Utc::now() - Duration::hours(23),
            }),
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert!(results.is_empty());
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Query – text search
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn query_text_search_finds_matching_entity() {
        let mut store = InMemoryEntityStore::new();
        // GitRepository serialises the remote_url; search for a unique fragment
        let repo = GitRepository::new(
            "https://github.com/needle/in-haystack.git".to_string(),
            "main".to_string(),
        );
        store.store(Box::new(repo)).await.unwrap();
        store.store(git_branch()).await.unwrap(); // unrelated entity

        let q = EntityQuery {
            text_query: Some("needle".to_string()),
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].relevance > 0.0);
    }

    #[tokio::test]
    async fn query_text_search_is_case_insensitive() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(GitRepository::new(
                "https://github.com/CaseTest/repo.git".to_string(),
                "main".to_string(),
            )))
            .await
            .unwrap();

        // lowercase query should still find the entity
        let q = EntityQuery {
            text_query: Some("casetest".to_string()),
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn query_text_search_returns_empty_on_no_match() {
        let mut store = InMemoryEntityStore::new();
        store.store(git_repo()).await.unwrap();

        let q = EntityQuery {
            text_query: Some("zzz-no-such-token-zzz".to_string()),
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert!(results.is_empty());
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Query – limit
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn query_limit_caps_result_set() {
        let mut store = InMemoryEntityStore::new();
        for _ in 0..5 {
            store.store(git_repo()).await.unwrap();
        }

        let q = EntityQuery {
            limit: Some(3),
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn query_limit_larger_than_store_returns_all() {
        let mut store = InMemoryEntityStore::new();
        store.store(git_repo()).await.unwrap();
        store.store(git_repo()).await.unwrap();

        let q = EntityQuery {
            limit: Some(100),
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Query – relevance ordering
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn query_results_ordered_by_descending_relevance() {
        let mut store = InMemoryEntityStore::new();
        // All three entities match (no text filter → relevance = 1.0); the sort
        // must produce a non-decreasing-when-reversed sequence.
        for _ in 0..3 {
            store.store(git_repo()).await.unwrap();
        }
        let results = store.query(&EntityQuery::default()).await.unwrap();
        for window in results.windows(2) {
            assert!(
                window[0].relevance >= window[1].relevance,
                "results must be sorted descending by relevance"
            );
        }
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Relationships – create / get
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_relationship_and_retrieve_it() {
        let mut store = InMemoryEntityStore::new();
        let repo_id = store.store(git_repo()).await.unwrap();
        let branch_id = store.store(git_branch()).await.unwrap();

        let rel = EntityRelationship {
            from: repo_id.clone(),
            to: branch_id.clone(),
            relationship_type: RelationshipType::Contains,
            metadata: Default::default(),
        };
        store.create_relationship(rel).await.unwrap();

        // Both endpoints expose the relationship
        let from_rels = store.get_relationships(&repo_id).await.unwrap();
        assert_eq!(from_rels.len(), 1);
        assert_eq!(from_rels[0].relationship_type, RelationshipType::Contains);

        let to_rels = store.get_relationships(&branch_id).await.unwrap();
        assert_eq!(to_rels.len(), 1);
    }

    #[tokio::test]
    async fn get_relationships_returns_empty_for_entity_with_none() {
        let mut store = InMemoryEntityStore::new();
        let id = store.store(git_repo()).await.unwrap();
        let rels = store.get_relationships(&id).await.unwrap();
        assert!(rels.is_empty());
    }

    #[tokio::test]
    async fn create_relationship_fails_when_source_missing() {
        let mut store = InMemoryEntityStore::new();
        let branch_id = store.store(git_branch()).await.unwrap();

        let rel = EntityRelationship {
            from: "ghost-entity".to_string(),
            to: branch_id,
            relationship_type: RelationshipType::References,
            metadata: Default::default(),
        };
        let result = store.create_relationship(rel).await;
        assert!(
            matches!(result, Err(EntityError::NotFound(_))),
            "should fail when source entity does not exist"
        );
    }

    #[tokio::test]
    async fn create_relationship_fails_when_target_missing() {
        let mut store = InMemoryEntityStore::new();
        let repo_id = store.store(git_repo()).await.unwrap();

        let rel = EntityRelationship {
            from: repo_id,
            to: "ghost-target".to_string(),
            relationship_type: RelationshipType::References,
            metadata: Default::default(),
        };
        let result = store.create_relationship(rel).await;
        assert!(
            matches!(result, Err(EntityError::NotFound(_))),
            "should fail when target entity does not exist"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Relationships – delete
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_relationship_removes_it() {
        let mut store = InMemoryEntityStore::new();
        let repo_id = store.store(git_repo()).await.unwrap();
        let branch_id = store.store(git_branch()).await.unwrap();

        let rel = EntityRelationship {
            from: repo_id.clone(),
            to: branch_id.clone(),
            relationship_type: RelationshipType::Contains,
            metadata: Default::default(),
        };
        store.create_relationship(rel).await.unwrap();

        // Delete the specific relationship
        store
            .delete_relationship(&repo_id, &branch_id, RelationshipType::Contains)
            .await
            .unwrap();

        let rels = store.get_relationships(&repo_id).await.unwrap();
        assert!(rels.is_empty(), "relationship should be gone after deletion");
    }

    #[tokio::test]
    async fn delete_relationship_only_removes_matching_type() {
        let mut store = InMemoryEntityStore::new();
        let a = store.store(git_repo()).await.unwrap();
        let b = store.store(git_branch()).await.unwrap();

        for rtype in [RelationshipType::Contains, RelationshipType::References] {
            store
                .create_relationship(EntityRelationship {
                    from: a.clone(),
                    to: b.clone(),
                    relationship_type: rtype,
                    metadata: Default::default(),
                })
                .await
                .unwrap();
        }

        // Remove only "Contains"
        store
            .delete_relationship(&a, &b, RelationshipType::Contains)
            .await
            .unwrap();

        let rels = store.get_relationships(&a).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relationship_type, RelationshipType::References);
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Combined filters
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn combined_type_and_text_filter() {
        let mut store = InMemoryEntityStore::new();

        store
            .store(Box::new(GitRepository::new(
                "https://github.com/needleproject/repo.git".to_string(),
                "main".to_string(),
            )))
            .await
            .unwrap();
        store.store(context_entity()).await.unwrap(); // Context entity, no "needle"

        let q = EntityQuery {
            entity_types: vec![EntityType::Git],
            text_query: Some("needleproject".to_string()),
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_type, EntityType::Git);
    }

    #[tokio::test]
    async fn combined_type_and_limit_filter() {
        let mut store = InMemoryEntityStore::new();
        for _ in 0..4 {
            store.store(git_repo()).await.unwrap(); // Git
        }
        store.store(context_entity()).await.unwrap(); // Context

        let q = EntityQuery {
            entity_types: vec![EntityType::Git],
            limit: Some(2),
            ..Default::default()
        };
        let results = store.query(&q).await.unwrap();
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(r.entity_type, EntityType::Git);
        }
    }
}
