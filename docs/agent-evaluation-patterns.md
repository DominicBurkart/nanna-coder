# Agent Evaluation Patterns

Evaluation framework for the full nanna-coder agent system, including subcontainers. The implementation lives at [../harness/src/agent/eval.rs](../harness/src/agent/eval.rs); for the agent control flow being evaluated, see [../AGENTS.md](../AGENTS.md) and [../ARCHITECTURE.md](../ARCHITECTURE.md). For general testing strategy, see [../TESTING.md](../TESTING.md).

## Levels

- **Unit** — individual agent components in isolation: state transitions (Planning -> Deciding -> Performing -> Checking), RAG query relevance, entity creation/modification, decision-making logic.
- **Integration** — interactions between subsystems: full control loop, LLM-agent interaction, multi-entity workflows, entity-relationship management.
- **System** — the full containerized system: model provider integration (Ollama / vLLM), observability, end-to-end task completion against real LLMs, performance under load.

## Core types

### `EvaluationScenario`

A test case with a user prompt, initial entity state, an `ExpectedOutcomes` block (min entities created, expected entity types, iteration limits, decision-quality and RAG-relevance thresholds), and a category.

### Metrics

- `execution_time` — total time to complete.
- `iterations` — control-loop cycles.
- `decision_quality` (0.0-1.0) — task completion, iteration efficiency, state correctness.
- `rag_relevance` (0.0-1.0) — average relevance of retrieved entities.
- `entity_accuracy` (0.0-1.0) — match between created and expected entities.
- `prompt_effectiveness` (0.0-1.0) — quality of LLM responses (future).

### `AgentEvaluationResult`

Per-evaluation: success/failure, all metrics above, final agent state, validation failures and warnings, optional system metrics (when observability is enabled).

## Built-in scenarios

| Constructor | Prompt | Expected | Cap |
|---|---|---|---|
| `EvaluationScenario::simple_entity_creation()` | "Create a new git repository entity" | >= 1 Git entity | 10 iters |
| `EvaluationScenario::rag_retrieval_accuracy()` | "Find all git repository entities" | RAG relevance >= 0.7 (initial: 2 Git, 1 Context) | — |
| `EvaluationScenario::multi_entity_workflow()` | "Create a git repository with associated test results" | Git + Test entities, >= 1 relationship | — |
| `EvaluationScenario::decision_quality_test()` | "Analyze existing entities and create a related context entity" | Context entity, decision quality >= 0.8 | — |

## Usage

### Basic

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

### Batch

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

### Containerized model

```rust
let config = EvaluationConfig::default()
    .with_model("qwen3:0.6b")
    .with_base_url("http://localhost:11434")
    .with_timeout(Duration::from_secs(600));

let mut evaluator = AgentEvaluator::new(config).await?;
```

### With observability

```rust
let config = EvaluationConfig {
    collect_observability: true,
    ..Default::default()
};

let mut evaluator = AgentEvaluator::new(config).await?;
let result = evaluator.evaluate(scenario).await?;

if let Some(sys) = result.system_metrics {
    println!("CPU: {:.1}%", sys.system_resources.cpu_usage_percent);
    println!("Mem: {:.1}%", sys.system_resources.memory_usage_percent);
}
```

## Custom scenarios

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

## Validation criteria

For LLM-powered scenarios, attach `ValidationCriteria` from the `model::judge` module:

```rust
use model::judge::ValidationCriteria;

let criteria = ValidationCriteria {
    min_response_length: 20,
    max_response_length: 500,
    required_keywords: vec!["repository".into(), "created".into()],
    forbidden_keywords: vec!["error".into(), "failed".into()],
    min_coherence_score: 0.7,
    min_relevance_score: 0.8,
    require_factual_accuracy: true,
    custom_validators: vec![],
};

let scenario = EvaluationScenario {
    // ...
    validation_criteria: Some(criteria),
    // ...
};
```

## Metric interpretation

### Decision quality

Composite: 50% task completion + 30% iteration efficiency + 20% state correctness.

| Range | Reading |
|---|---|
| > 0.8 | excellent: efficient and correct |
| 0.6-0.8 | good: completed but suboptimal |
| < 0.6 | poor: inefficient or incorrect |

### RAG relevance

Average relevance of retrieved entities.

| Range | Reading |
|---|---|
| > 0.8 | highly relevant |
| 0.6-0.8 | moderately relevant |
| < 0.6 | poor relevance |

### Entity accuracy

Fraction of expected entity types created. `1.0` = all expected types present.

## CI integration

```bash
cargo test --package harness --lib agent::eval

# Custom evaluation binary
cargo run --bin evaluate-agent -- \
    --scenario simple_entity_creation \
    --timeout 300 \
    --output results.json
```

## Performance considerations

- Containerized scenarios: timeouts of 300-600 s are typical.
- Batch evaluations run sequentially; parallelize at the caller if needed.
- Container-based evaluations may need significant memory.
- Observability collection adds ~5-10% overhead.

## Roadmap

LLM-powered evaluation, embedding-based semantic scoring, adversarial scenarios, regression tracking, multi-model comparison, distributed evaluation across containers.

## Best practices

- Start with unit-level scenarios; only escalate to system-level once those are stable.
- Raise quality thresholds incrementally as the agent improves.
- Keep scenario definitions versioned alongside the code they exercise.
- Watch metric trends across runs for regressions.

## Troubleshooting

**Timeouts** — increase `timeout` in `EvaluationConfig`; check container startup time and model availability.

**Low decision quality** — `max_iterations` may be too restrictive; review state transitions in logs; compare expected vs. actual final state.

**Low RAG relevance** — verify entity content matches query terms; check that initial entities are seeded; review the RAG query implementation.

**Observability errors** — initialize the tracing subscriber once; set `collect_observability: false` for simple tests; check permissions for system metrics.

## Related

- Implementation: [../harness/src/agent/eval.rs](../harness/src/agent/eval.rs)
- Agent control flow: [../AGENTS.md](../AGENTS.md)
- System architecture: [../ARCHITECTURE.md](../ARCHITECTURE.md)
- Testing strategy: [../TESTING.md](../TESTING.md)
