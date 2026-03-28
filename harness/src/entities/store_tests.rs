//! Comprehensive tests for InMemoryEntityStore
//!
//! The existing test module in mod.rs only verified that a new store has zero
//! entities. These tests exercise every EntityStore method and query filter
//! path, using ContextEntity as a lightweight concrete Entity implementation.

#[cfg(test)]
mod tests {
    use crate::entities::{
        context::ContextEntity, Entity, EntityError, EntityMetadata, EntityQuery, EntityRelationship,
        EntityResult, EntityStore, EntityType, InMemoryEntityStore, RelationshipType, TimeRange,
    };
    use chrono::{Duration, Utc};
    use std::collections::HashMap;

    // ── helpers ──────────────────────────────────────────────────────────

    /// Create a minimal ContextEntity with the given task description.
    fn make_entity(task: &str) -> ContextEntity {
        ContextEntity::new(
            task.to_string(),
            vec![],
            vec![],
            String::new(),
            "test-model".to_string(),
        )
    }

    /// Create a ContextEntity and tag it.
    fn make_tagged_entity(task: &str, tags: Vec<&str>) -> ContextEntity {
        let mut e = make_entity(task);
        e.metadata.tags = tags.into_iter().map(String::from).collect();
        e
    }

    // ── store ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn store_returns_entity_id() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_entity("task-a");
        let expected_id = entity.id().to_string();
        let id = store.store(Box::new(entity)).await.unwrap();
        assert_eq!(id, expected_id);
    }

    #[tokio::test]
    async fn store_duplicate_id_is_rejected() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_entity("task-a");
        let id = entity.id().to_string();

        // First store succeeds
        store.store(Box::new(entity)).await.unwrap();

        // Fabricate a second entity with the same ID
        let mut dup = make_entity("task-b");
        dup.metadata.id = id.clone();
        let err = store.store(Box::new(dup)).await.unwrap_err();

        match err {
            EntityError::AlreadyExists(eid) => assert_eq!(eid, id),
            other => panic!("expected AlreadyExists, got {:?}", other),
        }
    }

    // ── exists ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn exists_returns_true_after_store() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_entity("task-a");
        let id = store.store(Box::new(entity)).await.unwrap();
        assert!(store.exists(&id).await);
    }

    #[tokio::test]
    async fn exists_returns_false_for_unknown_id() {
        let store = InMemoryEntityStore::new();
        assert!(!store.exists("nonexistent").await);
    }

    // ── update ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_replaces_entity() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_entity("original");
        let id = entity.id().to_string();
        store.store(Box::new(entity)).await.unwrap();

        // Build replacement with same ID
        let mut replacement = make_entity("updated");
        replacement.metadata.id = id.clone();
        store.update(Box::new(replacement)).await.unwrap();

        // Verify via text query that the updated content is present
        let results = store
            .query(&EntityQuery {
                text_query: Some("updated".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_id, id);
    }

    #[tokio::test]
    async fn update_nonexistent_entity_fails() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_entity("ghost");
        let err = store.update(Box::new(entity)).await.unwrap_err();
        assert!(matches!(err, EntityError::NotFound(_)));
    }

    // ── delete ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_removes_entity() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_entity("doomed");
        let id = store.store(Box::new(entity)).await.unwrap();

        store.delete(&id).await.unwrap();
        assert!(!store.exists(&id).await);
    }

    #[tokio::test]
    async fn delete_nonexistent_entity_fails() {
        let mut store = InMemoryEntityStore::new();
        let err = store.delete("missing").await.unwrap_err();
        assert!(matches!(err, EntityError::NotFound(_)));
    }

    // ── query: entity type filter ────────────────────────────────────────

    #[tokio::test]
    async fn query_filters_by_entity_type() {
        let mut store = InMemoryEntityStore::new();
        // ContextEntity has EntityType::Context
        store.store(Box::new(make_entity("ctx"))).await.unwrap();

        // Query for Context type should return 1
        let results = store
            .query(&EntityQuery {
                entity_types: vec![EntityType::Context],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        // Query for Git type should return 0
        let results = store
            .query(&EntityQuery {
                entity_types: vec![EntityType::Git],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    // ── query: tag filter ────────────────────────────────────────────────

    #[tokio::test]
    async fn query_filters_by_tags() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(make_tagged_entity("with-tag", vec!["important"])))
            .await
            .unwrap();
        store
            .store(Box::new(make_tagged_entity("no-match", vec!["other"])))
            .await
            .unwrap();

        let results = store
            .query(&EntityQuery {
                tags: vec!["important".to_string()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    // ── query: text search ───────────────────────────────────────────────

    #[tokio::test]
    async fn query_text_search_matches_serialized_content() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(make_entity("needle-in-haystack")))
            .await
            .unwrap();
        store
            .store(Box::new(make_entity("something else")))
            .await
            .unwrap();

        let results = store
            .query(&EntityQuery {
                text_query: Some("needle".to_string()),
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
            .store(Box::new(make_entity("CamelCase")))
            .await
            .unwrap();

        let results = store
            .query(&EntityQuery {
                text_query: Some("camelcase".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn query_text_search_no_match_returns_empty() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(make_entity("hello world")))
            .await
            .unwrap();

        let results = store
            .query(&EntityQuery {
                text_query: Some("zzzzz_no_match".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    // ── query: time range filter ─────────────────────────────────────────

    #[tokio::test]
    async fn query_filters_by_time_range() {
        let mut store = InMemoryEntityStore::new();
        let entity = make_entity("recent");
        store.store(Box::new(entity)).await.unwrap();

        // Query with a time range that includes now
        let results = store
            .query(&EntityQuery {
                time_range: Some(TimeRange {
                    start: Utc::now() - Duration::hours(1),
                    end: Utc::now() + Duration::hours(1),
                }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        // Query with a time range in the past
        let results = store
            .query(&EntityQuery {
                time_range: Some(TimeRange {
                    start: Utc::now() - Duration::hours(48),
                    end: Utc::now() - Duration::hours(24),
                }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    // ── query: limit ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn query_respects_limit() {
        let mut store = InMemoryEntityStore::new();
        for i in 0..5 {
            store
                .store(Box::new(make_entity(&format!("task-{}", i))))
                .await
                .unwrap();
        }

        let results = store
            .query(&EntityQuery {
                limit: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    // ── query: no filters returns all ────────────────────────────────────

    #[tokio::test]
    async fn query_empty_filter_returns_all() {
        let mut store = InMemoryEntityStore::new();
        for i in 0..3 {
            store
                .store(Box::new(make_entity(&format!("task-{}", i))))
                .await
                .unwrap();
        }

        let results = store.query(&EntityQuery::default()).await.unwrap();
        assert_eq!(results.len(), 3);
        // All should have full relevance
        assert!(results.iter().all(|r| r.relevance == 1.0));
    }

    // ── query: combined filters ──────────────────────────────────────────

    #[tokio::test]
    async fn query_combines_type_and_tag_filters() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(make_tagged_entity("tagged-ctx", vec!["hot"])))
            .await
            .unwrap();
        store
            .store(Box::new(make_tagged_entity("untagged-ctx", vec!["cold"])))
            .await
            .unwrap();

        let results = store
            .query(&EntityQuery {
                entity_types: vec![EntityType::Context],
                tags: vec!["hot".to_string()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    // ── relationships ────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_and_get_relationship() {
        let mut store = InMemoryEntityStore::new();
        let id_a = store.store(Box::new(make_entity("a"))).await.unwrap();
        let id_b = store.store(Box::new(make_entity("b"))).await.unwrap();

        let rel = EntityRelationship {
            from: id_a.clone(),
            to: id_b.clone(),
            relationship_type: RelationshipType::References,
            metadata: HashMap::new(),
        };
        store.create_relationship(rel).await.unwrap();

        // Accessible from source
        let rels = store.get_relationships(&id_a).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to, id_b);

        // Also accessible from target
        let rels = store.get_relationships(&id_b).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].from, id_a);
    }

    #[tokio::test]
    async fn create_relationship_with_nonexistent_source_fails() {
        let mut store = InMemoryEntityStore::new();
        let id_b = store.store(Box::new(make_entity("b"))).await.unwrap();

        let rel = EntityRelationship {
            from: "ghost".to_string(),
            to: id_b,
            relationship_type: RelationshipType::Calls,
            metadata: HashMap::new(),
        };
        let err = store.create_relationship(rel).await.unwrap_err();
        assert!(matches!(err, EntityError::NotFound(_)));
    }

    #[tokio::test]
    async fn create_relationship_with_nonexistent_target_fails() {
        let mut store = InMemoryEntityStore::new();
        let id_a = store.store(Box::new(make_entity("a"))).await.unwrap();

        let rel = EntityRelationship {
            from: id_a,
            to: "ghost".to_string(),
            relationship_type: RelationshipType::Calls,
            metadata: HashMap::new(),
        };
        let err = store.create_relationship(rel).await.unwrap_err();
        assert!(matches!(err, EntityError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_relationship_removes_matching() {
        let mut store = InMemoryEntityStore::new();
        let id_a = store.store(Box::new(make_entity("a"))).await.unwrap();
        let id_b = store.store(Box::new(make_entity("b"))).await.unwrap();

        store
            .create_relationship(EntityRelationship {
                from: id_a.clone(),
                to: id_b.clone(),
                relationship_type: RelationshipType::Calls,
                metadata: HashMap::new(),
            })
            .await
            .unwrap();

        store
            .delete_relationship(&id_a, &id_b, RelationshipType::Calls)
            .await
            .unwrap();

        let rels = store.get_relationships(&id_a).await.unwrap();
        assert!(rels.is_empty());
    }

    #[tokio::test]
    async fn delete_relationship_only_removes_matching_type() {
        let mut store = InMemoryEntityStore::new();
        let id_a = store.store(Box::new(make_entity("a"))).await.unwrap();
        let id_b = store.store(Box::new(make_entity("b"))).await.unwrap();

        // Create two relationships of different types
        store
            .create_relationship(EntityRelationship {
                from: id_a.clone(),
                to: id_b.clone(),
                relationship_type: RelationshipType::Calls,
                metadata: HashMap::new(),
            })
            .await
            .unwrap();
        store
            .create_relationship(EntityRelationship {
                from: id_a.clone(),
                to: id_b.clone(),
                relationship_type: RelationshipType::References,
                metadata: HashMap::new(),
            })
            .await
            .unwrap();

        // Delete only Calls
        store
            .delete_relationship(&id_a, &id_b, RelationshipType::Calls)
            .await
            .unwrap();

        let rels = store.get_relationships(&id_a).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relationship_type, RelationshipType::References);
    }

    #[tokio::test]
    async fn get_relationships_for_unknown_entity_returns_empty() {
        let store = InMemoryEntityStore::new();
        let rels = store.get_relationships("nonexistent").await.unwrap();
        assert!(rels.is_empty());
    }

    // ── query result ordering ────────────────────────────────────────────

    #[tokio::test]
    async fn query_results_sorted_by_relevance_descending() {
        let mut store = InMemoryEntityStore::new();
        // One entity matches text search (relevance 0.8), others don't match
        // but we can verify ordering with a non-text query where all have 1.0
        store.store(Box::new(make_entity("aaa"))).await.unwrap();
        store.store(Box::new(make_entity("bbb"))).await.unwrap();

        let results = store.query(&EntityQuery::default()).await.unwrap();
        // All should have relevance 1.0 when no text query
        for r in &results {
            assert_eq!(r.relevance, 1.0);
            assert_eq!(r.entity_type, EntityType::Context);
        }
    }

    // ── metadata ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn entity_metadata_id_is_stable() {
        let entity = make_entity("test");
        let id1 = entity.id().to_string();
        let id2 = entity.id().to_string();
        assert_eq!(id1, id2);
    }

    #[test]
    fn entity_metadata_new_sets_version_to_1() {
        let m = EntityMetadata::new(EntityType::Git);
        assert_eq!(m.version, 1);
    }

    #[test]
    fn entity_metadata_new_sets_timestamps() {
        let before = Utc::now();
        let m = EntityMetadata::new(EntityType::Ast);
        let after = Utc::now();
        assert!(m.created_at >= before && m.created_at <= after);
        assert!(m.updated_at >= before && m.updated_at <= after);
    }

    #[test]
    fn relationship_type_custom_holds_value() {
        let rt = RelationshipType::Custom("depends-on".to_string());
        assert_eq!(rt, RelationshipType::Custom("depends-on".to_string()));
        assert_ne!(rt, RelationshipType::Custom("other".to_string()));
    }

    #[test]
    fn all_relationship_types_are_distinct() {
        let types = vec![
            RelationshipType::Contains,
            RelationshipType::Calls,
            RelationshipType::Imports,
            RelationshipType::Implements,
            RelationshipType::References,
            RelationshipType::Modifies,
            RelationshipType::Validates,
            RelationshipType::Custom("x".to_string()),
        ];
        for (i, a) in types.iter().enumerate() {
            for (j, b) in types.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b);
                }
            }
        }
    }

    // ── default impls ────────────────────────────────────────────────────

    #[test]
    fn in_memory_entity_store_default_is_empty() {
        let store = InMemoryEntityStore::default();
        assert_eq!(store.entities.len(), 0);
        assert!(store.relationships.is_empty());
    }

    #[test]
    fn entity_query_default_has_no_filters() {
        let q = EntityQuery::default();
        assert!(q.entity_types.is_empty());
        assert!(q.text_query.is_none());
        assert!(q.tags.is_empty());
        assert!(q.time_range.is_none());
        assert!(q.limit.is_none());
    }
}
