//! RAG (Retrieval-Augmented Generation) for querying
//!
//! This module provides functionality for querying entities using RAG techniques.
//!
//! # MVP Implementation
//! The current implementation uses simple keyword-based text search.
//! Future enhancements will include:
//! - Semantic embeddings
//! - Vector similarity search
//! - Advanced ranking algorithms

use crate::entities::{EntityQuery, EntityStore, QueryResult};
use thiserror::Error;

/// Errors related to RAG operations
#[derive(Error, Debug)]
pub enum RagError {
    #[error("RAG error: {0}")]
    QueryFailed(String),
    #[error("Entity error: {0}")]
    EntityError(#[from] crate::entities::EntityError),
}

pub type RagResult<T> = Result<T, RagError>;

/// Simple keyword-based RAG query
///
/// Performs text-based search across entities using keyword matching.
/// This is a minimal viable implementation; future versions will use
/// semantic embeddings and vector search.
///
/// # Arguments
/// * `entity_store` - The entity store to query
/// * `query_text` - The text to search for
/// * `limit` - Maximum number of results (default: 10)
///
/// # Returns
/// Vector of query results sorted by relevance
pub async fn query_entities(
    entity_store: &impl EntityStore,
    query_text: &str,
    limit: Option<usize>,
) -> RagResult<Vec<QueryResult>> {
    let query = EntityQuery {
        text_query: Some(query_text.to_string()),
        limit,
        ..Default::default()
    };

    let results = entity_store.query(&query).await?;
    Ok(results)
}

/// Extract relevant entity IDs from query results
pub fn extract_entity_ids(results: &[QueryResult]) -> Vec<String> {
    results.iter().map(|r| r.entity_id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::git::types::GitRepository;
    use crate::entities::{EntityStore, InMemoryEntityStore};

    #[tokio::test]
    async fn test_query_entities_empty_store() {
        let store = InMemoryEntityStore::new();
        let results = query_entities(&store, "test", Some(10)).await.unwrap();
        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn test_query_entities_with_matches() {
        let mut store = InMemoryEntityStore::new();

        // Add a git repository entity
        let repo = Box::new(GitRepository::new());
        store.store(repo).await.unwrap();

        // Query should find it via JSON text search
        let results = query_entities(&store, "Git", Some(10)).await.unwrap();
        assert!(!results.is_empty(), "Should find git entity");
    }

    #[tokio::test]
    async fn test_extract_entity_ids() {
        let results = vec![
            QueryResult {
                entity_id: "id1".to_string(),
                entity_type: crate::entities::EntityType::Git,
                relevance: 1.0,
                snippet: None,
            },
            QueryResult {
                entity_id: "id2".to_string(),
                entity_type: crate::entities::EntityType::Git,
                relevance: 0.8,
                snippet: None,
            },
        ];

        let ids = extract_entity_ids(&results);
        assert_eq!(ids, vec!["id1", "id2"]);
    }
}
