# Nanna Coder PoC: System Overview (historical)

> Historical PoC summary. For current architecture see [ARCHITECTURE.md](../ARCHITECTURE.md); for the current cache strategy see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md); for testing see [../TESTING.md](../TESTING.md). Subsequent work has continued on `main`; consult the current docs for the live state of the project.

## Summary

Containerized AI assistant proof-of-concept covering model caching, container infrastructure, monitoring, and automated quality validation. All planned PoC phases were delivered; the work has since merged into `main`.

### Delivered phases

| Phase | Component |
|-------|-----------|
| 1.1 | ModelJudge framework — automated quality validation, multi-criteria assessment |
| 1.2 | Container runtime detection — Podman/Docker/mock fallback, error handling |
| 2.1 | Multi-model caching — content-addressed storage, multiple model support |
| 2.2 | Binary cache strategy — Cachix integration, CI/CD optimisation |
| 3.1 | Developer experience — `dev-*` utilities, pre-commit hooks |
| 3.2 | CI/CD pipeline — parallel matrix, multi-platform |
| 4.1 | E2E integration tests — full workflow, ModelJudge coverage |
| 4.2 | Monitoring & observability — telemetry, alerting, health checks |
| 5.1 | Documentation — guides and API docs |

## Core components

- **ModelJudge** ([`model/src/judge.rs`](../model/src/judge.rs)) — API-responsiveness, response-quality, tool-calling, and consistency checks with retry/backoff. Used by E2E tests.
- **Container infrastructure** ([`harness/src/container.rs`](../harness/src/container.rs)) — Podman/Docker/mock runtimes with health checks and automatic cleanup.
- **Multi-model caching** ([`flake.nix`](../flake.nix), [`nix/containers.nix`](../nix/containers.nix)) — content-addressed storage; supports `qwen3:0.6b`, `llama3:8b`, `mistral:7b`, `gemma:2b`.
- **Monitoring & observability** (`harness/src/{monitoring,telemetry,observability}.rs`) — metrics collection, distributed tracing, multi-level alerting.

## Quick start

See [../README.md#quick-start](../README.md#quick-start). For developer commands (`dev-check`, `dev-test`, `container-dev`, etc.) see [developer-experience.md](./developer-experience.md).

## Testing strategy

See [../TESTING.md](../TESTING.md) for the current authoritative test topology. The PoC delivered unit, integration, and E2E levels for each component above.

### Notable E2E scenarios

- `test_e2e_container_to_validated_inference` — 7-phase pipeline: container → model → judge → cleanup, with mock fallback.
- `test_e2e_multi_model_comparison` — quality benchmarking across model configurations.
- `test_e2e_performance_and_reliability` — response time, throughput, threshold-based alerting.

## Production deployment notes

The PoC anticipated:

- Container registry: `ghcr.io/anthropics/nanna-coder` (harness, ollama, qwen3-container, llama3-container).
- Binary cache: Cachix (`nanna-coder.cachix.org`); see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md).
- Observability: Prometheus + Grafana, OpenTelemetry → Jaeger, structured logs, AlertManager.

## API references (PoC-era)

### ModelJudge

```rust
#[async_trait]
pub trait ModelJudge: ModelProvider {
    async fn validate_api_responsiveness(&self, latency_threshold: Duration) -> ModelResult<ValidationResult>;
    async fn validate_response_quality(&self, prompt: &str, expected_criteria: &ValidationCriteria) -> ModelResult<ValidationResult>;
    async fn validate_tool_calling(&self, tools: &[ToolDefinition]) -> ModelResult<ValidationResult>;
    async fn validate_consistency(&self, prompts: &[&str], iterations: usize) -> ModelResult<ValidationResult>;
}
```

### Observability

```rust
use harness::observability::ObservabilitySystem;

let mut obs = ObservabilitySystem::new()
    .with_service_name("my-service")
    .with_alert_policy(AlertPolicy::immediate_critical())
    .with_health_check_interval(Duration::from_secs(30));

obs.initialize().await?;
obs.start_monitoring().await?;
let status = obs.get_comprehensive_status().await?;
```

### Container API

```rust
use harness::container::{ContainerConfig, start_container_with_fallback};

let config = ContainerConfig {
    base_image: "ollama/ollama:latest".to_string(),
    test_image: Some("nanna-coder-test-ollama-qwen3:latest".to_string()),
    container_name: "my-test-container".to_string(),
    port_mapping: Some((11435, 11434)),
    model_to_pull: Some("qwen3:0.6b".to_string()),
    startup_timeout: Duration::from_secs(30),
    health_check_timeout: Duration::from_secs(10),
    env_vars: vec![("OLLAMA_MODELS".to_string(), "/models".to_string())],
    additional_args: vec!["--memory".to_string(), "2g".to_string()],
};

let handle = start_container_with_fallback(&config).await?;
// Container automatically cleaned up when handle is dropped
```

## Status

All PoC phases delivered. Subsequent work continues on `main`; consult the current docs (README, ARCHITECTURE, TESTING, CACHE_STRATEGY) for the active state of the project.
