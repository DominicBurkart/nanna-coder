//! Entity types and graph for representing code artifacts and their relationships

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

/// Errors related to entity operations
#[derive(Error, Debug)]
pub enum EntityError {
    #[error("Entity not found: {0}")]
    NotFound(String),
    #[error("Invalid entity relationship: {0}")]
    InvalidRelation(String),
    #[error("Entity graph error: {0}")]
    GraphError(String),
}

pub type EntityResult<T> = Result<T, EntityError>;

/// Types of entities in the code representation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Entity {
    /// A file in the codebase
    File { path: PathBuf, content: String },
    /// A function definition
    Function {
        name: String,
        body: String,
        file_path: PathBuf,
    },
    /// A module or namespace
    Module { name: String, path: PathBuf },
    /// A struct or class definition
    Struct {
        name: String,
        fields: Vec<String>,
        file_path: PathBuf,
    },
    /// A trait or interface definition
    Trait {
        name: String,
        methods: Vec<String>,
        file_path: PathBuf,
    },
    /// A test case
    Test {
        name: String,
        body: String,
        file_path: PathBuf,
    },
    /// Documentation comment or docstring
    Documentation {
        content: String,
        associated_entity: String,
    },
}

impl Entity {
    /// Get a human-readable name for this entity
    pub fn name(&self) -> String {
        match self {
            Entity::File { path, .. } => path.display().to_string(),
            Entity::Function { name, .. } => name.clone(),
            Entity::Module { name, .. } => name.clone(),
            Entity::Struct { name, .. } => name.clone(),
            Entity::Trait { name, .. } => name.clone(),
            Entity::Test { name, .. } => name.clone(),
            Entity::Documentation {
                associated_entity, ..
            } => {
                format!("docs:{}", associated_entity)
            }
        }
    }

    /// Get the file path associated with this entity, if any
    pub fn file_path(&self) -> Option<&PathBuf> {
        match self {
            Entity::File { path, .. } => Some(path),
            Entity::Function { file_path, .. } => Some(file_path),
            Entity::Module { path, .. } => Some(path),
            Entity::Struct { file_path, .. } => Some(file_path),
            Entity::Trait { file_path, .. } => Some(file_path),
            Entity::Test { file_path, .. } => Some(file_path),
            Entity::Documentation { .. } => None,
        }
    }
}

/// Relationship types between entities
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EntityRelation {
    /// One entity contains another (e.g., module contains function)
    Contains,
    /// One entity depends on another
    DependsOn,
    /// One entity implements another (e.g., struct implements trait)
    Implements,
    /// One entity calls another
    Calls,
    /// One entity tests another
    Tests,
    /// One entity documents another
    Documents,
    /// Custom relation type for extensibility
    Custom(String),
}

/// Graph representing entities and their relationships
pub struct EntityGraph {
    graph: DiGraph<Entity, EntityRelation>,
    entity_index: HashMap<String, NodeIndex>,
}

impl EntityGraph {
    /// Create a new empty entity graph
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            entity_index: HashMap::new(),
        }
    }

    /// Add an entity to the graph
    pub fn add_entity(&mut self, entity: Entity) -> NodeIndex {
        let name = entity.name();
        let node = self.graph.add_node(entity);
        self.entity_index.insert(name, node);
        node
    }

    /// Get an entity by name
    pub fn get_entity(&self, name: &str) -> EntityResult<&Entity> {
        let node = self
            .entity_index
            .get(name)
            .ok_or_else(|| EntityError::NotFound(name.to_string()))?;
        Ok(&self.graph[*node])
    }

    /// Get a mutable reference to an entity by name
    pub fn get_entity_mut(&mut self, name: &str) -> EntityResult<&mut Entity> {
        let node = self
            .entity_index
            .get(name)
            .ok_or_else(|| EntityError::NotFound(name.to_string()))?;
        Ok(&mut self.graph[*node])
    }

    /// Add a relationship between two entities
    pub fn add_relation(
        &mut self,
        from: &str,
        to: &str,
        relation: EntityRelation,
    ) -> EntityResult<()> {
        let from_node = self
            .entity_index
            .get(from)
            .ok_or_else(|| EntityError::NotFound(from.to_string()))?;
        let to_node = self
            .entity_index
            .get(to)
            .ok_or_else(|| EntityError::NotFound(to.to_string()))?;
        self.graph.add_edge(*from_node, *to_node, relation);
        Ok(())
    }

    /// Get all entities
    pub fn entities(&self) -> Vec<&Entity> {
        self.graph.node_weights().collect()
    }

    /// Get all relations from an entity
    pub fn relations_from(&self, name: &str) -> EntityResult<Vec<(String, EntityRelation)>> {
        let node = self
            .entity_index
            .get(name)
            .ok_or_else(|| EntityError::NotFound(name.to_string()))?;

        let mut relations = Vec::new();
        for edge in self.graph.edges(*node) {
            let target = edge.target();
            let target_entity = &self.graph[target];
            relations.push((target_entity.name(), edge.weight().clone()));
        }
        Ok(relations)
    }

    /// Query entities by a predicate function
    pub fn query<F>(&self, predicate: F) -> Vec<&Entity>
    where
        F: Fn(&Entity) -> bool,
    {
        self.graph
            .node_weights()
            .filter(|entity| predicate(entity))
            .collect()
    }

    /// Get the number of entities in the graph
    pub fn len(&self) -> usize {
        self.graph.node_count()
    }

    /// Check if the graph is empty
    pub fn is_empty(&self) -> bool {
        self.graph.node_count() == 0
    }
}

impl Default for EntityGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_entity_graph() {
        let graph = EntityGraph::new();
        assert_eq!(graph.len(), 0);
        assert!(graph.is_empty());
    }

    #[test]
    fn test_add_entity() {
        let mut graph = EntityGraph::new();
        let entity = Entity::File {
            path: PathBuf::from("test.rs"),
            content: "fn main() {}".to_string(),
        };

        graph.add_entity(entity.clone());
        assert_eq!(graph.len(), 1);

        let retrieved = graph.get_entity("test.rs").unwrap();
        assert_eq!(retrieved, &entity);
    }

    #[test]
    fn test_add_relation() {
        let mut graph = EntityGraph::new();

        let module = Entity::Module {
            name: "mymod".to_string(),
            path: PathBuf::from("mymod.rs"),
        };
        let function = Entity::Function {
            name: "my_func".to_string(),
            body: "fn my_func() {}".to_string(),
            file_path: PathBuf::from("mymod.rs"),
        };

        graph.add_entity(module);
        graph.add_entity(function);

        graph
            .add_relation("mymod", "my_func", EntityRelation::Contains)
            .unwrap();

        let relations = graph.relations_from("mymod").unwrap();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].0, "my_func");
        assert_eq!(relations[0].1, EntityRelation::Contains);
    }

    #[test]
    fn test_query_entities() {
        let mut graph = EntityGraph::new();

        graph.add_entity(Entity::File {
            path: PathBuf::from("file1.rs"),
            content: "".to_string(),
        });
        graph.add_entity(Entity::Function {
            name: "func1".to_string(),
            body: "".to_string(),
            file_path: PathBuf::from("file1.rs"),
        });

        let files = graph.query(|e| matches!(e, Entity::File { .. }));
        assert_eq!(files.len(), 1);

        let functions = graph.query(|e| matches!(e, Entity::Function { .. }));
        assert_eq!(functions.len(), 1);
    }

    #[test]
    fn test_entity_not_found() {
        let graph = EntityGraph::new();
        let result = graph.get_entity("nonexistent");
        assert!(result.is_err());
        match result.unwrap_err() {
            EntityError::NotFound(name) => assert_eq!(name, "nonexistent"),
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_entity_name() {
        let entity = Entity::Function {
            name: "test_func".to_string(),
            body: "fn test_func() {}".to_string(),
            file_path: PathBuf::from("test.rs"),
        };
        assert_eq!(entity.name(), "test_func");
    }

    #[test]
    fn test_entity_file_path() {
        let entity = Entity::Function {
            name: "test_func".to_string(),
            body: "fn test_func() {}".to_string(),
            file_path: PathBuf::from("test.rs"),
        };
        assert_eq!(entity.file_path(), Some(&PathBuf::from("test.rs")));
    }
}
