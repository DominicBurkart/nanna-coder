# Agent Evaluation Patterns

Agent evaluation framework for the nanna-coder system. Implementation:
[`harness/src/agent/eval.rs`](../harness/src/agent/eval.rs).

## Architecture

Three evaluation levels:

### Level 1: Unit evaluations
Tests individual agent components in isolation:
- State transitions (Planning → Deciding → Performing → Checking)
- RAG query quality and relevance scoring
- Entity creation and modification
- Decision-making logic

### Level 2: Integration evaluations
Tests interaction between agent subsystems:
- Complete agent control loop execution
- LLM-agent interaction patterns
- Multi-entity workflow coordination
- Entity relationship management

### Level 3: System evaluations
Tests the full containerized system:
- Model provider integration (Ollama/vLLM)
- Observability and telemetry collection
- End-to-end task completion with real LLMs
- Performance and reliability under load

## Core Concepts

### Evaluation Scenarios

An `EvaluationScenario` defines a test case with:
- **User prompt**: The task given to the agent
- **Initial entities**: Pre-populated entity state
- **Expected outcomes**: Success criteria including:
  - Minimum entities created
  - Expected entity types
  - Iteration limits
  - Decision quality thresholds
  - RAG relevance requirements
- **Category**: Type of evaluation (entity creation, RAG, decision-making, etc.)

### Evaluation Metrics

The framework collects comprehensive metrics:
- **Execution time**: Total time to complete
- **Iterations**: Number of control loop cycles
- **Decision quality** (0.0-1.0): Based on task completion, iteration efficiency, and state correctness
- **RAG relevance** (0.0-1.0): Average relevance of retrieved entities
- **Entity accuracy** (0.0-1.0): Match between created and expected entities
- **Prompt effectiveness** (0.0-1.0): Quality of LLM prompt responses (future)

### Evaluation Results

Each evaluation produces an `AgentEvaluationResult`:
- Success/failure status
- All collected metrics
- Final agent state
- Validation failures and warnings
- Optional system metrics (when observability enabled)

## Built-in Scenarios

### Simple Entity Creation
Tests basic agent ability to create entities.
```rust
let scenario = EvaluationScenario::simple_entity_creation();
```
- **Prompt**: "Create a new git repository entity"
- **Expected**: At least 1 Git entity created
- **Max iterations**: 10

### RAG Retrieval Accuracy
Tests RAG system's ability to find relevant entities.
```rust
let scenario = EvaluationScenario::rag_retrieval_accuracy();
```
- **Prompt**: "Find all git repository entities"
- **Initial state**: 2 Git entities, 1 Context entity
- **Expected**: RAG relevance ≥ 0.7

### Multi-Entity Workflow
Tests coordination across multiple entity types.
```rust
let scenario = EvaluationScenario::multi_entity_workflow();
```
- **Prompt**: "Create a git repository with associated test results"
- **Expected**: Git + Test entities with at least 1 relationship

### Decision Quality Test
Tests agent's decision-making between QUERY and PROCEED.
```rust
let scenario = EvaluationScenario::decision_quality_test();
```
- **Prompt**: "Analyze existing entities and create a related context entity"
- **Initial state**: Git + Test entities
- **Expected**: Context entity with decision quality ≥ 0.8

## Usage Examples

### Basic Evaluation

```rust
use harness::agent::eval::{AgentEvaluator, EvaluationConfig, EvaluationScenario};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = EvaluationConfig::default()
        .with_timeout(Duration::from_secs(300));

    let mut evaluator = AgentEvaluator::new(config).await?;
    let scenario = EvaluationScenario::simple_entity_creation();
    
    let result = evaluator.evaluate(scenario).await?;
    
    println!("Success: {}", result.success);
    println!("Decision Quality: {:.2}", result.metrics.decision_quality);
    println!("RAG Relevance: {:.2}", result.metrics.rag_relevance);
    println!("Entities Created: {}", result.metrics.entities_created);
    
    Ok(())
}
```

### Batch Evaluation

```rust
let scenarios = vec![
    EvaluationScenario::simple_entity_creation(),
    EvaluationScenario::rag_retrieval_accuracy(),
    EvaluationScenario::multi_entity_workflow(),
];

let batch_result = evaluator.evaluate_batch(scenarios).await?;

println!("Total: {}", batch_result.total_scenarios);
println!("Passed: {}", batch_result.passed);
println!("Failed: {}", batch_result.failed);
```

### With Containerized Model

```rust
let config = EvaluationConfig::default()
    .with_model("qwen3:0.6b")
    .with_base_url("http://localhost:11434")
    .with_timeout(Duration::from_secs(600));

let mut evaluator = AgentEvaluator::new(config).await?;
// Evaluation will use the LLM for agent decision-making
```

### With Observability

```rust
let config = EvaluationConfig {
    collect_observability: true,
    ..Default::default()
};

let mut evaluator = AgentEvaluator::new(config).await?;
let result = evaluator.evaluate(scenario).await?;

if let Some(sys_metrics) = result.system_metrics {
    println!("CPU Usage: {:.1}%", sys_metrics.system_resources.cpu_usage_percent);
    println!("Memory Usage: {:.1}%", sys_metrics.system_resources.memory_usage_percent);
}
```

## Custom Scenarios

Create custom evaluation scenarios for specific test cases:

```rust
use harness::agent::eval::{EvaluationScenario, EvaluationCategory, ExpectedOutcomes};
use harness::entities::EntityType;

let custom_scenario = EvaluationScenario {
    id: "custom_test".to_string(),
    name: "Custom Test Scenario".to_string(),
    description: "Tests specific agent behavior".to_string(),
    user_prompt: "Your custom prompt here".to_string(),
    initial_entities: vec![EntityType::Git, EntityType::Context],
    expected_outcomes: ExpectedOutcomes {
        min_entities_created: 2,
        expected_entity_types: vec![EntityType::Test, EntityType::Ast],
        min_decision_quality: 0.75,
        min_rag_relevance: 0.80,
        max_allowed_iterations: 15,
        ..Default::default()
    },
    validation_criteria: None,
    max_iterations: 20,
    category: EvaluationCategory::Custom("your_category".to_string()),
};
```

## Validation Criteria

For LLM-powered agents, add validation criteria:

```rust
use model::judge::ValidationCriteria;

let criteria = ValidationCriteria {
    min_response_length: 20,
    max_response_length: 500,
    required_keywords: vec!["repository".to_string(), "created".to_string()],
    forbidden_keywords: vec!["error".to_string(), "failed".to_string()],
    min_coherence_score: 0.7,
    min_relevance_score: 0.8,
    require_factual_accuracy: true,
    custom_validators: vec![],
};

let scenario = EvaluationScenario {
    // ... other fields ...
    validation_criteria: Some(criteria),
    // ...
};
```

## Metrics Interpretation

### Decision Quality (0.0 - 1.0)
Composite score based on:
- **Task completion** (50%): Did the agent complete the task?
- **Iteration efficiency** (30%): How many iterations were needed?
- **State correctness** (20%): Did it reach the expected final state?

**Interpretation:**
- `> 0.8`: Excellent - efficient and correct
- `0.6 - 0.8`: Good - completed but could be more efficient
- `< 0.6`: Poor - inefficient or incorrect

### RAG Relevance (0.0 - 1.0)
Average relevance score of retrieved entities.

**Interpretation:**
- `> 0.8`: Highly relevant results
- `0.6 - 0.8`: Moderately relevant
- `< 0.6`: Poor relevance

### Entity Accuracy (0.0 - 1.0)
Percentage of expected entity types that were created.

**Interpretation:**
- `1.0`: All expected entities created
- `< 1.0`: Some expected entities missing

## Integration with CI/CD

The evaluation framework can be integrated into CI pipelines:

```bash
# Run evaluations as part of test suite
cargo test --package harness --lib agent::eval

# Custom evaluation binary
cargo run --bin evaluate-agent -- \
    --scenario simple_entity_creation \
    --timeout 300 \
    --output results.json
```

## Performance Considerations

- **Timeouts**: Set appropriate timeouts for containerized scenarios (300-600s)
- **Parallelization**: Batch evaluations run sequentially; parallelize manually if needed
- **Resource limits**: Container-based evaluations may require significant memory
- **Observability overhead**: Collecting system metrics adds ~5-10% overhead

## Future Enhancements

1. **LLM-Powered Evaluations**: Full integration with model providers for decision-making
2. **Semantic Evaluation**: Use embeddings to evaluate response quality
3. **Adversarial Testing**: Scenarios designed to challenge agent robustness
4. **Performance Benchmarking**: Track metrics over time for regression detection
5. **Multi-Model Comparison**: Evaluate same scenarios across different LLMs
6. **Distributed Evaluation**: Run evaluations across multiple containers in parallel

## Best practices

1. Start with unit-level scenarios before system-level tests.
2. Raise quality thresholds gradually as the agent improves.
3. When a scenario fails, document why and adjust expectations.
4. Version scenarios alongside code changes.
5. Watch for metric degradation over time.
6. Cover error handling and boundary conditions explicitly.

## Troubleshooting

| Symptom | Try |
|---------|-----|
| Evaluation timeout | Increase `timeout` in `EvaluationConfig`; verify container startup + model availability |
| Low decision quality | Check `max_iterations`; review state transitions in logs; compare expected vs. actual final state |
| Low RAG relevance | Verify entity content matches query terms; confirm initial entities created; review RAG query impl |
| Observability errors | Initialize tracing subscriber once; set `collect_observability: false` for simple tests; check metric-collection permissions |

## Related

- [../AGENTS.md](../AGENTS.md) — agent control flow
- [../ARCHITECTURE.md](../ARCHITECTURE.md) — system architecture
- [../TESTING.md](../TESTING.md) — testing strategy
- [`harness/src/agent/eval.rs`](../harness/src/agent/eval.rs) — implementation
