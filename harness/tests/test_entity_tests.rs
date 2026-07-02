use harness::entities::{Entity, EntityType};
use harness::entities::test::types::TestEntity;

#[test]
fn test_entity_new_has_correct_type() {
    let e = TestEntity::new();
    assert_eq!(e.metadata().entity_type, EntityType::Test);
}

#[test]
fn test_entity_default_equals_new() {
    let a = TestEntity::new();
    let b = TestEntity::default();
    assert_eq!(a.metadata().entity_type, b.metadata().entity_type);
}

#[test]
fn test_entity_to_json_is_valid() {
    let e = TestEntity::new();
    let json = e.to_json().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.is_object());
}

#[test]
fn test_entity_metadata_accessor() {
    let e = TestEntity::new();
    let m = e.metadata();
    assert_eq!(m.entity_type, EntityType::Test);
}

#[test]
fn test_entity_metadata_mut_accessor() {
    let mut e = TestEntity::new();
    let m = e.metadata_mut();
    assert_eq!(m.entity_type, EntityType::Test);
}
