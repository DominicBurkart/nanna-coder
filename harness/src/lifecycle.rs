//! Container lifecycle management and orchestration.
//!
//! This module implements the container relationship graph as specified:
//!
//! ```mermaid
//! flowchart TD
//!     B([Harness]) -- Modifies --> C([Dev])
//!     B -- Queries --> n1([Model])
//!     C -- Can compile binary for --> n2([Sandbox])
//!     n2 -- Can be promoted to --> n3([Release])
//! ```
//!
//! The harness leverages Nix-in-container builds from the image-builder module
//! to create and manage the Dev, Sandbox, and Release container images.

use crate::container::{ContainerError, ContainerHandle, ContainerRuntime};
use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;

/// Errors that can occur during lifecycle operations
#[derive(Error, Debug)]
pub enum LifecycleError {
    /// Container operation failed
    #[error("Container operation failed: {0}")]
    ContainerError(#[from] ContainerError),

    /// Invalid lifecycle transition
    #[error("Invalid transition from {from:?} to {to:?}: {reason}")]
    InvalidTransition {
        from: ContainerType,
        to: ContainerType,
        reason: String,
    },

    /// Container not found
    #[error("Container {container_type:?} not found")]
    ContainerNotFound { container_type: ContainerType },

    /// Build failed
    #[error("Build failed for {container_type:?}: {reason}")]
    BuildFailed {
        container_type: ContainerType,
        reason: String,
    },

    /// Query failed
    #[error("Query to {container_type:?} failed: {reason}")]
    QueryFailed {
        container_type: ContainerType,
        reason: String,
    },
}

pub type LifecycleResult<T> = Result<T, LifecycleError>;

/// Types of containers in the system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContainerType {
    /// The harness container - orchestrates the system
    Harness,
    /// Development container - modified by harness
    Dev,
    /// Model container - queried by harness
    Model,
    /// Sandbox container - compiled from dev
    Sandbox,
    /// Release container - promoted from sandbox
    Release,
}

impl std::fmt::Display for ContainerType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerType::Harness => write!(f, "harness"),
            ContainerType::Dev => write!(f, "dev"),
            ContainerType::Model => write!(f, "model"),
            ContainerType::Sandbox => write!(f, "sandbox"),
            ContainerType::Release => write!(f, "release"),
        }
    }
}

/// Relationship types between containers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerRelationship {
    /// Container A modifies container B
    Modifies,
    /// Container A queries container B
    Queries,
    /// Container A compiles binary for container B
    CompilesFor,
    /// Container A is promoted to container B
    PromotesTo,
}

/// Container lifecycle state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerState {
    /// Container type
    pub container_type: ContainerType,
    /// Container name/ID
    pub name: String,
    /// Current status
    pub status: ContainerStatus,
    /// Image name
    pub image: String,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Container status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerStatus {
    /// Container not yet created
    NotCreated,
    /// Container is building
    Building,
    /// Container is running
    Running,
    /// Container is stopped
    Stopped,
    /// Container failed
    Failed,
}

/// Manages the lifecycle of containers and their relationships
pub struct LifecycleManager {
    /// Graph of container relationships
    relationship_graph: DiGraph<ContainerType, ContainerRelationship>,
    /// Container states
    states: HashMap<ContainerType, ContainerState>,
    /// Node index mapping
    node_index: HashMap<ContainerType, NodeIndex>,
    /// Container runtime
    runtime: ContainerRuntime,
}

impl LifecycleManager {
    /// Create a new lifecycle manager
    pub fn new(runtime: ContainerRuntime) -> Self {
        let mut manager = Self {
            relationship_graph: DiGraph::new(),
            states: HashMap::new(),
            node_index: HashMap::new(),
            runtime,
        };

        // Initialize the relationship graph
        manager.initialize_graph();
        manager
    }

    /// Initialize the container relationship graph according to the architecture
    fn initialize_graph(&mut self) {
        // Add all container types as nodes
        for container_type in [
            ContainerType::Harness,
            ContainerType::Dev,
            ContainerType::Model,
            ContainerType::Sandbox,
            ContainerType::Release,
        ] {
            let node_idx = self.relationship_graph.add_node(container_type);
            self.node_index.insert(container_type, node_idx);
        }

        // Add relationships as edges
        // Harness -> Dev (Modifies)
        self.add_relationship_internal(
            ContainerType::Harness,
            ContainerType::Dev,
            ContainerRelationship::Modifies,
        );

        // Harness -> Model (Queries)
        self.add_relationship_internal(
            ContainerType::Harness,
            ContainerType::Model,
            ContainerRelationship::Queries,
        );

        // Dev -> Sandbox (CompilesFor)
        self.add_relationship_internal(
            ContainerType::Dev,
            ContainerType::Sandbox,
            ContainerRelationship::CompilesFor,
        );

        // Sandbox -> Release (PromotesTo)
        self.add_relationship_internal(
            ContainerType::Sandbox,
            ContainerType::Release,
            ContainerRelationship::PromotesTo,
        );
    }

    /// Add a relationship between containers
    fn add_relationship_internal(
        &mut self,
        from: ContainerType,
        to: ContainerType,
        relationship: ContainerRelationship,
    ) {
        if let (Some(&from_idx), Some(&to_idx)) =
            (self.node_index.get(&from), self.node_index.get(&to))
        {
            self.relationship_graph
                .add_edge(from_idx, to_idx, relationship);
        }
    }

    /// Modify a dev container (Harness -> Dev relationship)
    ///
    /// # Implementation Note
    /// The actual modification logic is not yet defined. This will involve:
    /// - Using Nix-in-container builds from image-builder
    /// - Applying code changes
    /// - Rebuilding the dev container
    pub async fn modify_dev_container(
        &mut self,
        _modifications: DevModifications,
    ) -> LifecycleResult<()> {
        unimplemented!("Dev container modification requires further problem definition")
    }

    /// Query the model container (Harness -> Model relationship)
    ///
    /// # Implementation Note
    /// The actual query logic is not yet defined. This will involve:
    /// - Sending requests to the model API
    /// - Handling responses
    /// - Managing context and conversation state
    pub async fn query_model(&self, _query: ModelQuery) -> LifecycleResult<ModelResponse> {
        unimplemented!("Model querying requires further problem definition")
    }

    /// Compile binary from dev to sandbox (Dev -> Sandbox relationship)
    ///
    /// # Implementation Note
    /// The actual compilation logic is not yet defined. This will involve:
    /// - Using Nix builds within the dev container
    /// - Extracting compiled binaries
    /// - Creating a sandbox container with the binary
    /// - Running safety checks
    pub async fn compile_to_sandbox(
        &mut self,
        _build_spec: BuildSpec,
    ) -> LifecycleResult<ContainerHandle> {
        unimplemented!("Dev to sandbox compilation requires further problem definition")
    }

    /// Promote sandbox to release (Sandbox -> Release relationship)
    ///
    /// # Implementation Note
    /// The actual promotion logic is not yet defined. This will involve:
    /// - Running validation tests on sandbox
    /// - Creating release container
    /// - Tagging and versioning
    /// - Publishing to registry
    pub async fn promote_to_release(
        &mut self,
        _promotion_spec: PromotionSpec,
    ) -> LifecycleResult<ContainerHandle> {
        unimplemented!("Sandbox to release promotion requires further problem definition")
    }

    /// Get the state of a container
    pub fn get_container_state(&self, container_type: ContainerType) -> Option<&ContainerState> {
        self.states.get(&container_type)
    }

    /// Get all container states
    pub fn get_all_states(&self) -> &HashMap<ContainerType, ContainerState> {
        &self.states
    }

    /// Check if a transition is valid
    pub fn is_valid_transition(&self, from: ContainerType, to: ContainerType) -> bool {
        if let (Some(&from_idx), Some(&to_idx)) =
            (self.node_index.get(&from), self.node_index.get(&to))
        {
            self.relationship_graph
                .find_edge(from_idx, to_idx)
                .is_some()
        } else {
            false
        }
    }

    /// Get the container runtime
    pub fn runtime(&self) -> &ContainerRuntime {
        &self.runtime
    }
}

/// Modifications to apply to a dev container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevModifications {
    /// Files to modify
    pub file_changes: HashMap<String, String>,
    /// Dependencies to add
    pub dependencies: Vec<String>,
    /// Build commands
    pub build_commands: Vec<String>,
}

/// Query to send to the model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelQuery {
    /// Query text
    pub prompt: String,
    /// Optional context
    pub context: HashMap<String, String>,
    /// Temperature setting
    pub temperature: Option<f32>,
}

/// Response from the model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    /// Response text
    pub text: String,
    /// Metadata
    pub metadata: HashMap<String, String>,
}

/// Specification for building a sandbox from dev
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildSpec {
    /// Source container
    pub source: String,
    /// Build target
    pub target: String,
    /// Nix build expression
    pub nix_expr: String,
    /// Timeout
    pub timeout: Duration,
}

/// Specification for promoting sandbox to release
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionSpec {
    /// Source sandbox container
    pub source: String,
    /// Release version
    pub version: String,
    /// Release tags
    pub tags: Vec<String>,
    /// Validation checks to run
    pub validation_checks: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_type_display() {
        assert_eq!(ContainerType::Harness.to_string(), "harness");
        assert_eq!(ContainerType::Dev.to_string(), "dev");
        assert_eq!(ContainerType::Model.to_string(), "model");
        assert_eq!(ContainerType::Sandbox.to_string(), "sandbox");
        assert_eq!(ContainerType::Release.to_string(), "release");
    }

    #[test]
    fn test_lifecycle_manager_creation() {
        let manager = LifecycleManager::new(ContainerRuntime::None);
        assert_eq!(manager.relationship_graph.node_count(), 5);
        // 4 relationships: Harness->Dev, Harness->Model, Dev->Sandbox, Sandbox->Release
        assert_eq!(manager.relationship_graph.edge_count(), 4);
    }

    #[test]
    fn test_valid_transitions() {
        let manager = LifecycleManager::new(ContainerRuntime::None);

        // Valid transitions
        assert!(manager.is_valid_transition(ContainerType::Harness, ContainerType::Dev));
        assert!(manager.is_valid_transition(ContainerType::Harness, ContainerType::Model));
        assert!(manager.is_valid_transition(ContainerType::Dev, ContainerType::Sandbox));
        assert!(manager.is_valid_transition(ContainerType::Sandbox, ContainerType::Release));

        // Invalid transitions
        assert!(!manager.is_valid_transition(ContainerType::Dev, ContainerType::Harness));
        assert!(!manager.is_valid_transition(ContainerType::Model, ContainerType::Dev));
        assert!(!manager.is_valid_transition(ContainerType::Sandbox, ContainerType::Dev));
        assert!(!manager.is_valid_transition(ContainerType::Release, ContainerType::Sandbox));
    }

    #[test]
    fn test_container_state_creation() {
        let state = ContainerState {
            container_type: ContainerType::Dev,
            name: "dev-container-1".to_string(),
            status: ContainerStatus::Running,
            image: "nanna-dev:latest".to_string(),
            metadata: HashMap::new(),
        };

        assert_eq!(state.container_type, ContainerType::Dev);
        assert_eq!(state.status, ContainerStatus::Running);
    }

    #[test]
    fn test_dev_modifications() {
        let mods = DevModifications {
            file_changes: HashMap::from([("main.rs".to_string(), "fn main() {}".to_string())]),
            dependencies: vec!["tokio".to_string()],
            build_commands: vec!["cargo build".to_string()],
        };

        assert_eq!(mods.file_changes.len(), 1);
        assert_eq!(mods.dependencies.len(), 1);
    }

    #[test]
    fn test_model_query() {
        let query = ModelQuery {
            prompt: "Explain recursion".to_string(),
            context: HashMap::new(),
            temperature: Some(0.7),
        };

        assert_eq!(query.prompt, "Explain recursion");
        assert_eq!(query.temperature, Some(0.7));
    }

    #[test]
    fn test_build_spec() {
        let spec = BuildSpec {
            source: "dev-1".to_string(),
            target: "sandbox-1".to_string(),
            nix_expr: "{ pkgs }: pkgs.hello".to_string(),
            timeout: Duration::from_secs(300),
        };

        assert_eq!(spec.source, "dev-1");
        assert_eq!(spec.timeout, Duration::from_secs(300));
    }

    #[tokio::test]
    async fn test_modify_dev_container_unimplemented() {
        let mut manager = LifecycleManager::new(ContainerRuntime::None);
        let mods = DevModifications {
            file_changes: HashMap::new(),
            dependencies: Vec::new(),
            build_commands: Vec::new(),
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(manager.modify_dev_container(mods))
        }));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_query_model_unimplemented() {
        let manager = LifecycleManager::new(ContainerRuntime::None);
        let query = ModelQuery {
            prompt: "test".to_string(),
            context: HashMap::new(),
            temperature: None,
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(manager.query_model(query))
        }));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_compile_to_sandbox_unimplemented() {
        let mut manager = LifecycleManager::new(ContainerRuntime::None);
        let spec = BuildSpec {
            source: "dev".to_string(),
            target: "sandbox".to_string(),
            nix_expr: "test".to_string(),
            timeout: Duration::from_secs(60),
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(manager.compile_to_sandbox(spec))
        }));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_promote_to_release_unimplemented() {
        let mut manager = LifecycleManager::new(ContainerRuntime::None);
        let spec = PromotionSpec {
            source: "sandbox".to_string(),
            version: "1.0.0".to_string(),
            tags: vec!["latest".to_string()],
            validation_checks: vec![],
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(manager.promote_to_release(spec))
        }));
        assert!(result.is_err());
    }
}
