//! Correlation helpers for test entities.
//!
//! Provides convenience functions for connecting test-run entities back to the
//! git commit they were executed against via a
//! [`RelationshipType::Validates`] edge.

use crate::entities::{EntityRelationship, EntityResult, EntityStore, RelationshipType};
use std::collections::HashMap;

/// Create a `Validates` relationship from `test_run_id` to `commit_entity_id`.
///
/// Both entities must already be present in the store; otherwise the
/// underlying store returns [`crate::entities::EntityError::NotFound`].
pub async fn correlate_with_commit<S: EntityStore + ?Sized>(
    store: &mut S,
    commit_entity_id: &str,
    test_run_id: &str,
) -> EntityResult<()> {
    let rel = EntityRelationship {
        from: test_run_id.to_string(),
        to: commit_entity_id.to_string(),
        relationship_type: RelationshipType::Validates,
        metadata: HashMap::new(),
    };
    store.create_relationship(rel).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::test::types::TestRunEntity;
    use crate::entities::{git::types::GitRepository, Entity, EntityError, InMemoryEntityStore};

    #[tokio::test]
    async fn test_correlate_creates_relationship() {
        let mut store = InMemoryEntityStore::new();

        let commit = GitRepository::new("url".to_string(), "main".to_string());
        let commit_id = commit.metadata().id.clone();
        store
            .store(Box::new(commit) as Box<dyn Entity>)
            .await
            .unwrap();

        let run = TestRunEntity::new("cargo".to_string());
        let run_id = run.metadata().id.clone();
        store.store(Box::new(run) as Box<dyn Entity>).await.unwrap();

        correlate_with_commit(&mut store, &commit_id, &run_id)
            .await
            .expect("correlate");

        let rels = store.get_relationships(&run_id).await.unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].from, run_id);
        assert_eq!(rels[0].to, commit_id);
        assert_eq!(rels[0].relationship_type, RelationshipType::Validates);
    }

    #[tokio::test]
    async fn test_correlate_missing_entity_errors() {
        let mut store = InMemoryEntityStore::new();
        let err = correlate_with_commit(&mut store, "missing-commit", "missing-run")
            .await
            .expect_err("should fail");
        assert!(matches!(err, EntityError::NotFound(_)));
    }
}
