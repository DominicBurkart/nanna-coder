//! Tests for entity types that lack inline test modules.
//!
//! Coverage target: harness/src/entities/test/types.rs (TestEntity).

use harness::entities::{Entity, EntityStore, EntityType, InMemoryEntityStore};

#[test]
fn test_entity_new_has_test_type() {
    use harness::entities::test::TestEntity;
    let entity = TestEntity::new();
    assert_eq!(entity.entity_type(), EntityType::Test);
}

#[test]
fn test_entity_default_has_test_type() {
    use harness::entities::test::TestEntity;
    let entity = TestEntity::default();
    assert_eq!(entity.entity_type(), EntityType::Test);
}

#[test]
fn test_entity_metadata_ref() {
    use harness::entities::test::TestEntity;
    let entity = TestEntity::new();
    assert_eq!(entity.metadata().entity_type, EntityType::Test);
}

#[test]
fn test_entity_metadata_mut() {
    use harness::entities::test::TestEntity;
    let mut entity = TestEntity::new();
    entity.metadata_mut().version = 2;
    assert_eq!(entity.metadata().version, 2);
}

#[test]
fn test_entity_to_json_is_valid() {
    use harness::entities::test::TestEntity;
    let entity = TestEntity::new();
    let json = entity
        .to_json()
        .expect("TestEntity should serialize to JSON");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("JSON should parse back");
    assert_eq!(parsed["entity_type"], "Test");
}

#[tokio::test]
async fn test_entity_stored_and_found_by_kind() {
    use harness::entities::test::TestEntity;

    let mut store = InMemoryEntityStore::new();
    let entity = Box::new(TestEntity::new()) as Box<dyn Entity>;
    let id = store.store(entity).await.expect("store should succeed");

    assert!(store.exists(&id).await);

    let by_kind = store
        .list_by_kind(EntityType::Test)
        .await
        .expect("list_by_kind should succeed");
    assert_eq!(by_kind, vec![id]);
}
