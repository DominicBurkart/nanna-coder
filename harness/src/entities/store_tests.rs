//! Comprehensive tests for InMemoryEntityStore
//!
//! The InMemoryEntityStore previously had only a single trivial test checking
//! initial emptiness. These tests cover the full CRUD lifecycle, query filtering
//! (by type, tags, time range, text search, limit), relationship management,
//! and error paths.

#[cfg(test)]
mod tests {
    use crate::entities::{
        context::ContextEntity, test::TestEntity, Entity, EntityError, EntityQuery,
        EntityRelationship, EntityStore, EntityType, InMemoryEntityStore, RelationshipType,
        TimeRange,
    };
    use std::collections::HashMap;

    // -- helpers --

    fn context_entity(task: &str) -> ContextEntity {
        ContextEntity::new(
            task.to_string(),
            vec![],
            vec![],
            String::new(),
            "test-model".to_string(),
        )
    }

    fn tagged_context(task: &str, tags: Vec<&str>) -> ContextEntity {
        let mut e = context_entity(task);
        e.metadata.tags = tags.into_iter().map(String::from).collect();
        e
    }

    // -- store / exists / delete --

    #[tokio::test]
    async fn store_and_exists() {
        let mut store = InMemoryEntityStore::new();
        let entity = context_entity("task-1");
        let id = entity.id().to_string();

        assert!(!store.exists(&id).await);
        let returned_id = store.store(Box::new(entity)).await.unwrap();
        assert_eq!(returned_id, id);
        assert!(store.exists(&id).await);
    }

    #[tokio::test]
    async fn store_duplicate_entity_errors() {
        let mut store = InMemoryEntityStore::new();
        let entity = context_entity("dup");
        let id = entity.id().to_string();

        let mut dup = context_entity("dup2");
        dup.metadata.id = id.clone();

        store.store(Box::new(entity)).await.unwrap();
        let err = store.store(Box::new(dup)).await.unwrap_err();
        assert!(matches!(err, EntityError::AlreadyExists(_)));
    }

    #[tokio::test]
    async fn delete_existing_entity() {
        let mut store = InMemoryEntityStore::new();
        let entity = context_entity("to-delete");
        let id = store.store(Box::new(entity)).await.unwrap();

        assert!(store.exists(&id).await);
        store.delete(&id).await.unwrap();
        assert!(!store.exists(&id).await);
    }

    #[tokio::test]
    async fn delete_nonexistent_entity_errors() {
        let mut store = InMemoryEntityStore::new();
        let err = store.delete("no-such-id").await.unwrap_err();
        assert!(matches!(err, EntityError::NotFound(_)));
    }

    // -- update --

    #[tokio::test]
    async fn update_existing_entity() {
        let mut store = InMemoryEntityStore::new();
        let entity = context_entity("original");
        let id = entity.id().to_string();
        store.store(Box::new(entity)).await.unwrap();

        let mut updated = context_entity("updated");
        updated.metadata.id = id.clone();
        store.update(Box::new(updated)).await.unwrap();

        assert!(store.exists(&id).await);
    }

    #[tokio::test]
    async fn update_nonexistent_entity_errors() {
        let mut store = InMemoryEntityStore::new();
        let entity = context_entity("ghost");
        let err = store.update(Box::new(entity)).await.unwrap_err();
        assert!(matches!(err, EntityError::NotFound(_)));
    }

    // -- query: entity type filter --

    #[tokio::test]
    async fn query_filters_by_entity_type() {
        let mut store = InMemoryEntityStore::new();
        store.store(Box::new(context_entity("ctx"))).await.unwrap();
        store.store(Box::new(TestEntity::new())).await.unwrap();

        let results = store
            .query(&EntityQuery {
                entity_types: vec![EntityType::Context],
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_type, EntityType::Context);
    }

    #[tokio::test]
    async fn query_without_type_filter_returns_all() {
        let mut store = InMemoryEntityStore::new();
        store.store(Box::new(context_entity("a"))).await.unwrap();
        store.store(Box::new(TestEntity::new())).await.unwrap();

        let results = store.query(&EntityQuery::default()).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    // -- query: tag filter --

    #[tokio::test]
    async fn query_filters_by_tags() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(tagged_context("tagged", vec!["important"])))
            .await
            .unwrap();
        store
            .store(Box::new(tagged_context("untagged", vec!["irrelevant"])))
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

    // -- query: time range filter --

    #[tokio::test]
    async fn query_filters_by_time_range() {
        let mut store = InMemoryEntityStore::new();
        let entity = context_entity("timed");
        let created = entity.metadata().created_at;
        store.store(Box::new(entity)).await.unwrap();

        let inclusive_range = TimeRange {
            start: created - chrono::TimeDelta::seconds(1),
            end: created + chrono::TimeDelta::seconds(1),
        };
        let results = store
            .query(&EntityQuery {
                time_range: Some(inclusive_range),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(results.len(), 1);

        let exclusive_range = TimeRange {
            start: created - chrono::TimeDelta::hours(2),
            end: created - chrono::TimeDelta::hours(1),
        };
        let results = store
            .query(&EntityQuery {
                time_range: Some(exclusive_range),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    // -- query: text search --

    #[tokio::test]
    async fn query_text_search_matches_json_content() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(context_entity("fix the parser bug")))
            .await
            .unwrap();
        store
            .store(Box::new(context_entity("add new feature")))
            .await
            .unwrap();

        let results = store
            .query(&EntityQuery {
                text_query: Some("parser".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(
            results[0].relevance > 0.5,
            "matched entity should have high relevance"
        );
    }

    #[tokio::test]
    async fn query_text_search_is_case_insensitive() {
        let mut store = InMemoryEntityStore::new();
        store
            .store(Box::new(context_entity("Parser Bug")))
            .await
            .unwrap();

        let results = store
            .query(&EntityQuery {
                text_query: Some("parser".to_string()),
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
            .store(Box::new(context_entity("hello world")))
            .await
            .unwrap();

        let results = store
            .query(&EntityQuery {
                text_query: Some("zzzznonexistent".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(results.is_empty());
    }

    // -- query: limit --

    #[tokio::test]
    async fn query_respects_limit() {
        let mut store = InMemoryEntityStore::new();
        for i in 0..5 {
            store
                .store(Box::new(context_entity(&format!("task-{}", i))))
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

    // -- query: combined filters --

    #[tokio::test]
    async fn query_combines_type_and_tag_filters() {
        let mut store = InMemoryEntityStore::new();

        store
            .store(Box::new(tagged_context("ctx-match", vec!["urgent"])))
            .await
            .unwrap();
        store
            .store(Box::new(tagged_context("ctx-nomatch", vec!["low"])))
            .await
            .unwrap();
        let mut test_ent = TestEntity::new();
        test_ent.metadata.tags = vec!["urgent".to_string()];
        store.store(Box::new(test_ent)).await.unwrap();

        let results = store
            .query(&EntityQuery {
                entity_types: vec![EntityType::Context],
                tags: vec!["urgent".to_string()],
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_type, EntityType::Context);
    }

    // -- relationships --

    #[tokio::test]
    async fn create_and_get_relationship() {
        let mut store = InMemoryEntityStore::new();
        let e1 = context_entity("source");
        let e2 = context_entity("target");
        let id1 = e1.id().to_string();
        let id2 = e2.id().to_string();

        store.store(Box::new(e1)).await.unwrap();
        store.store(Box::new(e2)).await.unwrap();

        let rel = EntityRelationship {
            from: id1.clone(),
            to: id2.clone(),
            relationship_type: RelationshipType::References,
            metadata: HashMap::new(),
        };
        store.create_relationship(rel).await.unwrap();

        let rels = store.get_relationships(&id1).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to, id2);

        let rels = store.get_relationships(&id2).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].from, id1);
    }

    #[tokio::test]
    async fn create_relationship_fails_for_missing_entity() {
        let mut store = InMemoryEntityStore::new();
        let e1 = context_entity("only-one");
        let id1 = e1.id().to_string();
        store.store(Box::new(e1)).await.unwrap();

        let rel = EntityRelationship {
            from: id1,
            to: "nonexistent".to_string(),
            relationship_type: RelationshipType::Contains,
            metadata: HashMap::new(),
        };
        let err = store.create_relationship(rel).await.unwrap_err();
        assert!(matches!(err, EntityError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_relationship() {
        let mut store = InMemoryEntityStore::new();
        let e1 = context_entity("a");
        let e2 = context_entity("b");
        let id1 = e1.id().to_string();
        let id2 = e2.id().to_string();

        store.store(Box::new(e1)).await.unwrap();
        store.store(Box::new(e2)).await.unwrap();

        store
            .create_relationship(EntityRelationship {
                from: id1.clone(),
                to: id2.clone(),
                relationship_type: RelationshipType::Calls,
                metadata: HashMap::new(),
            })
            .await
            .unwrap();

        store
            .delete_relationship(&id1, &id2, RelationshipType::Calls)
            .await
            .unwrap();

        let rels = store.get_relationships(&id1).await.unwrap();
        assert!(rels.is_empty());
    }

    #[tokio::test]
    async fn delete_relationship_only_removes_matching_type() {
        let mut store = InMemoryEntityStore::new();
        let e1 = context_entity("x");
        let e2 = context_entity("y");
        let id1 = e1.id().to_string();
        let id2 = e2.id().to_string();

        store.store(Box::new(e1)).await.unwrap();
        store.store(Box::new(e2)).await.unwrap();

        store
            .create_relationship(EntityRelationship {
                from: id1.clone(),
                to: id2.clone(),
                relationship_type: RelationshipType::Calls,
                metadata: HashMap::new(),
            })
            .await
            .unwrap();
        store
            .create_relationship(EntityRelationship {
                from: id1.clone(),
                to: id2.clone(),
                relationship_type: RelationshipType::References,
                metadata: HashMap::new(),
            })
            .await
            .unwrap();

        store
            .delete_relationship(&id1, &id2, RelationshipType::Calls)
            .await
            .unwrap();

        let rels = store.get_relationships(&id1).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relationship_type, RelationshipType::References);
    }

    #[tokio::test]
    async fn get_relationships_for_unrelated_entity_returns_empty() {
        let mut store = InMemoryEntityStore::new();
        let entity = context_entity("loner");
        let id = entity.id().to_string();
        store.store(Box::new(entity)).await.unwrap();

        let rels = store.get_relationships(&id).await.unwrap();
        assert!(rels.is_empty());
    }

    // -- query: relevance --

    #[tokio::test]
    async fn query_results_have_full_relevance_without_text_query() {
        let mut store = InMemoryEntityStore::new();
        store.store(Box::new(context_entity("a"))).await.unwrap();

        let results = store.query(&EntityQuery::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            (results[0].relevance - 1.0).abs() < f64::EPSILON,
            "without text query, relevance should be 1.0"
        );
    }

    // -- entity metadata --

    #[tokio::test]
    async fn entity_metadata_ids_are_unique() {
        let e1 = context_entity("first");
        let e2 = context_entity("second");
        assert_ne!(e1.id(), e2.id());
    }

    #[tokio::test]
    async fn entity_type_accessor_matches_metadata() {
        let ctx = context_entity("ctx");
        assert_eq!(ctx.entity_type(), EntityType::Context);

        let test = TestEntity::new();
        assert_eq!(test.entity_type(), EntityType::Test);
    }
}
