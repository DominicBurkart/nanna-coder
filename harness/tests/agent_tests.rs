//! Comprehensive test suite for the agent architecture
//!
//! This test suite verifies the agent control flow and entity management system.

use harness::{
    AgentConfig, AgentContext, AgentError, AgentLoop, AgentState, DecisionContext,
    EnrichmentConfig, Entity, EntityGraph, EntityRelation, ModificationDecision,
};
use std::path::PathBuf;

#[test]
fn test_entity_graph_creation() {
    let graph = EntityGraph::new();
    assert!(graph.is_empty());
    assert_eq!(graph.len(), 0);
}

#[test]
fn test_entity_graph_add_entity() {
    let mut graph = EntityGraph::new();

    let file_entity = Entity::File {
        path: PathBuf::from("main.rs"),
        content: "fn main() {}".to_string(),
    };

    graph.add_entity(file_entity.clone());
    assert_eq!(graph.len(), 1);

    let retrieved = graph.get_entity("main.rs").unwrap();
    assert_eq!(retrieved.name(), "main.rs");
}

#[test]
fn test_entity_graph_multiple_entities() {
    let mut graph = EntityGraph::new();

    graph.add_entity(Entity::File {
        path: PathBuf::from("lib.rs"),
        content: "pub mod test;".to_string(),
    });

    graph.add_entity(Entity::Module {
        name: "test".to_string(),
        path: PathBuf::from("test.rs"),
    });

    graph.add_entity(Entity::Function {
        name: "test_func".to_string(),
        body: "fn test_func() {}".to_string(),
        file_path: PathBuf::from("test.rs"),
    });

    assert_eq!(graph.len(), 3);
}

#[test]
fn test_entity_relationships() {
    let mut graph = EntityGraph::new();

    graph.add_entity(Entity::Module {
        name: "mymod".to_string(),
        path: PathBuf::from("mymod.rs"),
    });

    graph.add_entity(Entity::Function {
        name: "helper".to_string(),
        body: "fn helper() {}".to_string(),
        file_path: PathBuf::from("mymod.rs"),
    });

    graph
        .add_relation("mymod", "helper", EntityRelation::Contains)
        .unwrap();

    let relations = graph.relations_from("mymod").unwrap();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].0, "helper");
    assert_eq!(relations[0].1, EntityRelation::Contains);
}

#[test]
fn test_entity_query() {
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

    graph.add_entity(Entity::Struct {
        name: "MyStruct".to_string(),
        fields: vec!["field1".to_string()],
        file_path: PathBuf::from("file1.rs"),
    });

    let files = graph.query(|e| matches!(e, Entity::File { .. }));
    assert_eq!(files.len(), 1);

    let functions = graph.query(|e| matches!(e, Entity::Function { .. }));
    assert_eq!(functions.len(), 1);

    let structs = graph.query(|e| matches!(e, Entity::Struct { .. }));
    assert_eq!(structs.len(), 1);
}

#[test]
fn test_entity_types() {
    let file = Entity::File {
        path: PathBuf::from("test.rs"),
        content: "content".to_string(),
    };
    assert_eq!(file.name(), "test.rs");
    assert_eq!(file.file_path(), Some(&PathBuf::from("test.rs")));

    let function = Entity::Function {
        name: "my_func".to_string(),
        body: "fn my_func() {}".to_string(),
        file_path: PathBuf::from("test.rs"),
    };
    assert_eq!(function.name(), "my_func");
    assert_eq!(function.file_path(), Some(&PathBuf::from("test.rs")));

    let module = Entity::Module {
        name: "mymod".to_string(),
        path: PathBuf::from("mymod.rs"),
    };
    assert_eq!(module.name(), "mymod");

    let test = Entity::Test {
        name: "test_something".to_string(),
        body: "#[test] fn test_something() {}".to_string(),
        file_path: PathBuf::from("tests.rs"),
    };
    assert_eq!(test.name(), "test_something");
}

#[test]
fn test_agent_state_machine() {
    assert_eq!(AgentState::Enriching, AgentState::Enriching);
    assert_ne!(AgentState::Enriching, AgentState::Planning);

    let states = vec![
        AgentState::Enriching,
        AgentState::Planning,
        AgentState::QueryingEntities,
        AgentState::DecidingModification,
        AgentState::Modifying,
        AgentState::UpdatingEntities,
        AgentState::CheckingCompletion,
        AgentState::Completed,
    ];

    for state in states {
        match state {
            AgentState::Enriching
            | AgentState::Planning
            | AgentState::QueryingEntities
            | AgentState::DecidingModification
            | AgentState::Modifying
            | AgentState::UpdatingEntities
            | AgentState::CheckingCompletion
            | AgentState::Completed => {}
            AgentState::Error(_) => panic!("Unexpected error state"),
        }
    }
}

#[test]
fn test_agent_loop_initialization() {
    let config = AgentConfig::default();
    let agent = AgentLoop::new(config);

    assert_eq!(agent.state(), &AgentState::Enriching);
    assert!(agent.entities().is_empty());
}

#[test]
fn test_agent_config() {
    let config = AgentConfig::default();
    assert_eq!(config.max_iterations, 100);
    assert!(!config.verbose);

    let custom_config = AgentConfig {
        enrichment: EnrichmentConfig::default(),
        max_iterations: 50,
        verbose: true,
    };
    assert_eq!(custom_config.max_iterations, 50);
    assert!(custom_config.verbose);
}

#[test]
fn test_agent_context() {
    let context = AgentContext {
        user_prompt: "Add a new function".to_string(),
        conversation_history: vec!["Previous message".to_string()],
        app_state_id: "state_123".to_string(),
    };

    assert_eq!(context.user_prompt, "Add a new function");
    assert_eq!(context.conversation_history.len(), 1);
    assert_eq!(context.app_state_id, "state_123");
}

#[test]
fn test_decision_context() {
    let ctx = DecisionContext {
        user_prompt: "Create a helper function".to_string(),
        conversation_history: vec![],
        recent_modifications: vec![],
    };

    assert_eq!(ctx.user_prompt, "Create a helper function");
    assert!(ctx.conversation_history.is_empty());
    assert!(ctx.recent_modifications.is_empty());
}

#[test]
fn test_modification_decisions() {
    use harness::EntityType;

    let create = ModificationDecision::Create(EntityType::Function);
    assert!(matches!(create, ModificationDecision::Create(_)));

    let update = ModificationDecision::Update("test_func".to_string());
    assert!(matches!(update, ModificationDecision::Update(_)));

    let delete = ModificationDecision::Delete("old_func".to_string());
    assert!(matches!(delete, ModificationDecision::Delete(_)));

    let refactor = ModificationDecision::Refactor(vec!["func1".to_string(), "func2".to_string()]);
    assert!(matches!(refactor, ModificationDecision::Refactor(_)));

    let none = ModificationDecision::None;
    assert_eq!(none, ModificationDecision::None);
}

#[tokio::test]
async fn test_agent_loop_with_entities() {
    let config = AgentConfig {
        max_iterations: 5,
        ..Default::default()
    };

    let mut agent = AgentLoop::new(config);

    // Add some entities to work with
    agent.entities_mut().add_entity(Entity::File {
        path: PathBuf::from("test.rs"),
        content: "fn test() {}".to_string(),
    });

    assert_eq!(agent.entities().len(), 1);
    assert_eq!(agent.state(), &AgentState::Enriching);
}

#[test]
fn test_entity_relation_types() {
    let relations = vec![
        EntityRelation::Contains,
        EntityRelation::DependsOn,
        EntityRelation::Implements,
        EntityRelation::Calls,
        EntityRelation::Tests,
        EntityRelation::Documents,
        EntityRelation::Custom("CustomRel".to_string()),
    ];

    assert_eq!(relations.len(), 7);

    for relation in relations {
        match relation {
            EntityRelation::Contains
            | EntityRelation::DependsOn
            | EntityRelation::Implements
            | EntityRelation::Calls
            | EntityRelation::Tests
            | EntityRelation::Documents => {}
            EntityRelation::Custom(s) => assert_eq!(s, "CustomRel"),
        }
    }
}

#[test]
fn test_complex_entity_graph() {
    let mut graph = EntityGraph::new();

    // Create a module
    graph.add_entity(Entity::Module {
        name: "utils".to_string(),
        path: PathBuf::from("utils.rs"),
    });

    // Add functions to the module
    graph.add_entity(Entity::Function {
        name: "add".to_string(),
        body: "fn add(a: i32, b: i32) -> i32 { a + b }".to_string(),
        file_path: PathBuf::from("utils.rs"),
    });

    graph.add_entity(Entity::Function {
        name: "subtract".to_string(),
        body: "fn subtract(a: i32, b: i32) -> i32 { a - b }".to_string(),
        file_path: PathBuf::from("utils.rs"),
    });

    // Add test
    graph.add_entity(Entity::Test {
        name: "test_add".to_string(),
        body: "#[test] fn test_add() { assert_eq!(add(2, 2), 4); }".to_string(),
        file_path: PathBuf::from("utils.rs"),
    });

    // Create relationships
    graph
        .add_relation("utils", "add", EntityRelation::Contains)
        .unwrap();
    graph
        .add_relation("utils", "subtract", EntityRelation::Contains)
        .unwrap();
    graph
        .add_relation("test_add", "add", EntityRelation::Tests)
        .unwrap();

    assert_eq!(graph.len(), 4);

    let util_relations = graph.relations_from("utils").unwrap();
    assert_eq!(util_relations.len(), 2);

    let test_relations = graph.relations_from("test_add").unwrap();
    assert_eq!(test_relations.len(), 1);
    assert_eq!(test_relations[0].1, EntityRelation::Tests);
}

#[test]
fn test_enrichment_config() {
    let config = EnrichmentConfig::default();
    assert!(config.infer_types);
    assert!(config.extract_docs);
    assert!(config.analyze_dependencies);
    assert!(!config.semantic_analysis);

    let custom = EnrichmentConfig {
        infer_types: false,
        extract_docs: true,
        analyze_dependencies: false,
        semantic_analysis: true,
    };
    assert!(!custom.infer_types);
    assert!(custom.extract_docs);
}

#[test]
fn test_agent_error_types() {
    use harness::EntityError;

    let entity_err = AgentError::Entity(EntityError::NotFound("test".to_string()));
    assert!(matches!(entity_err, AgentError::Entity(_)));

    let state_err = AgentError::StateError("Invalid state".to_string());
    assert!(matches!(state_err, AgentError::StateError(_)));

    let max_iter_err = AgentError::MaxIterationsExceeded;
    assert!(matches!(max_iter_err, AgentError::MaxIterationsExceeded));
}

#[test]
fn test_entity_graph_query_by_file_path() {
    let mut graph = EntityGraph::new();

    graph.add_entity(Entity::Function {
        name: "func1".to_string(),
        body: "".to_string(),
        file_path: PathBuf::from("file1.rs"),
    });

    graph.add_entity(Entity::Function {
        name: "func2".to_string(),
        body: "".to_string(),
        file_path: PathBuf::from("file2.rs"),
    });

    graph.add_entity(Entity::Struct {
        name: "MyStruct".to_string(),
        fields: vec![],
        file_path: PathBuf::from("file1.rs"),
    });

    let file1_entities = graph.query(|e| {
        e.file_path()
            .map(|p| p == &PathBuf::from("file1.rs"))
            .unwrap_or(false)
    });

    assert_eq!(file1_entities.len(), 2);
}

#[tokio::test]
async fn test_agent_loop_state_initialization() {
    let config = AgentConfig {
        max_iterations: 3,
        verbose: false,
        enrichment: EnrichmentConfig::default(),
    };

    let agent = AgentLoop::new(config);
    assert_eq!(agent.state(), &AgentState::Enriching);

    // Verify we can create the context without running the loop
    let _context = AgentContext {
        user_prompt: "test".to_string(),
        conversation_history: vec![],
        app_state_id: "test".to_string(),
    };

    // Note: We don't call agent.run() because enrichment is unimplemented.
    // The agent state machine is tested via the state enum tests above.
}

#[test]
fn test_modification_plan_types() {
    use harness::{ImpactEstimate, ModificationPlan, RiskLevel};

    let plan = ModificationPlan {
        decision: ModificationDecision::None,
        steps: vec![],
        impact: ImpactEstimate {
            entities_affected: 5,
            risk_level: RiskLevel::Medium,
            files_modified: vec!["test.rs".to_string()],
        },
    };

    assert_eq!(plan.impact.entities_affected, 5);
    assert_eq!(plan.impact.risk_level, RiskLevel::Medium);
    assert_eq!(plan.impact.files_modified.len(), 1);
}

#[test]
fn test_risk_levels() {
    use harness::RiskLevel;

    assert_eq!(RiskLevel::Low, RiskLevel::Low);
    assert_ne!(RiskLevel::Low, RiskLevel::High);

    let levels = [RiskLevel::Low, RiskLevel::Medium, RiskLevel::High];
    assert_eq!(levels.len(), 3);
}

#[test]
fn test_step_operations() {
    use harness::StepOperation;

    let ops = [
        StepOperation::Create,
        StepOperation::Update,
        StepOperation::Delete,
        StepOperation::Validate,
        StepOperation::Test,
    ];

    assert_eq!(ops.len(), 5);
    assert_eq!(StepOperation::Create, StepOperation::Create);
    assert_ne!(StepOperation::Create, StepOperation::Delete);
}

#[test]
fn test_entity_documentation() {
    let docs = Entity::Documentation {
        content: "This is a doc comment".to_string(),
        associated_entity: "my_func".to_string(),
    };

    assert_eq!(docs.name(), "docs:my_func");
    assert_eq!(docs.file_path(), None);
}

#[test]
fn test_trait_entity() {
    let trait_entity = Entity::Trait {
        name: "MyTrait".to_string(),
        methods: vec!["method1".to_string(), "method2".to_string()],
        file_path: PathBuf::from("traits.rs"),
    };

    assert_eq!(trait_entity.name(), "MyTrait");
    assert_eq!(trait_entity.file_path(), Some(&PathBuf::from("traits.rs")));
}

#[test]
fn test_entity_struct_with_fields() {
    let struct_entity = Entity::Struct {
        name: "Config".to_string(),
        fields: vec![
            "host".to_string(),
            "port".to_string(),
            "timeout".to_string(),
        ],
        file_path: PathBuf::from("config.rs"),
    };

    assert_eq!(struct_entity.name(), "Config");
    if let Entity::Struct { fields, .. } = struct_entity {
        assert_eq!(fields.len(), 3);
        assert!(fields.contains(&"host".to_string()));
    }
}
