# vLLM Migration Plan (issue #39)

> **Status: research artifact / spike.** This document is the deliverable for
> a CRON pass on issue #39 ("Migrate from Ollama to vLLM"). It is intentionally
> *not* an implementation PR — the migration is a multi-PR epic and several of
> the dependent slices are in flight as separate pull requests. The role of
> this doc is to make the sequencing, decisions, and risks explicit so any
> future pass (human or agent) can pick up the next slice without re-deriving
> the plan.
>
> - job-id: nnkMo
> - oldest-issue-id: 2
> - Refs #39

## 1. Goal

Replace the Ollama-based model container with a vLLM-based container while
preserving the architecture documented in
[ARCHITECTURE.md](../ARCHITECTURE.md) — i.e. a `ModelProvider`-shaped
service that the harness talks to over HTTP.

The motivation is verbatim from the issue:

- Broader GPU acceleration trajectory over the next several years.
- Better resource management (paged attention, continuous batching, tensor
  parallelism) than Ollama.
- Best-effort container-based ROCm iGPU support upstream.
- A clean CPU fallback for environments without a usable GPU.

## 2. Current footprint (as of `main` @ commit referenced by this branch)

The `ModelProvider` trait already exists as an abstraction:

```
model/src/provider.rs   trait ModelProvider { chat, list_models, health_check, provider_name }
```

so the *trait surface* is migration-ready. The coupling is in **call sites**
and **config types**, not the trait itself.

### 2.1 Rust files referencing Ollama

| File | What it owns |
| --- | --- |
| `model/src/lib.rs` | `pub mod ollama;`, `pub use ollama::OllamaProvider;`, `pub use config::OllamaConfig;`. Default Cargo feature is `ollama`. |
| `model/src/ollama.rs` (~1100 LOC) | `OllamaProvider` impls `ModelProvider` **and** `ModelJudge`. ~500 LOC of tool-call parsing. |
| `model/src/config.rs` | Only `OllamaConfig` + `ModelDefaults`. No provider-agnostic config type. |
| `model/src/judge.rs` | `ModelJudge` trait with default-impl helpers hard-bound to `OllamaProvider`. |
| `harness/src/main.rs` | 6+ sites construct `OllamaProvider::new(OllamaConfig::default())`; subcommand handlers take `&OllamaProvider` concretely (not `&dyn ModelProvider`). |
| `harness/src/agent/mod.rs` | Test-only `OllamaProvider::with_default_config()` calls. |
| `harness/src/container.rs` | `base_image: "ollama/ollama:latest"`, port `11434`, `ollama pull`. |
| `harness/src/eval/runner.rs` | Eval pipeline assumes Ollama call shape. |
| Integration tests | `model/tests/ollama_chat_integration.rs`, `harness/tests/integration_tests.rs`, `harness/tests/dev_container_integration.rs`, `harness/tests/eval_runner_tests.rs`, `harness/tests/nanna_self_dev_integration.rs` all assume `:11434`. |

### 2.2 Nix / container references

| File | Reference |
| --- | --- |
| `nix/containers.nix` | `ollamaImage`, `vllmImage`, `vllmImageMimo`, `vllmImageQwen`. The vLLM side already exists as wrapper scripts around `vllm/vllm-openai:latest`. |
| `flake.nix` | Apps: `nix run .#ollama-start`, `nix run .#vllm-start`, `vllm-start-mimo`, `vllm-start-qwen`. |
| `nix/scripts.nix`, `nix/configs.nix`, `nix/gpu-support.nix` | Ollama-shaped helpers. |

### 2.3 CI workflows

`.github/workflows/{ci.yml,cache-warming.yml,eval.yml}` reference Ollama
through container build matrices and integration jobs. None reference vLLM
yet.

## 3. Existing in-flight work that this plan depends on

| PR / Issue | What it gives us |
| --- | --- |
| **#42** (merged Feb 26) | Phase 3 vLLM container Nix infra — `vllmImage`, port 8000, HF cache mount. **Container side is already built.** |
| **#115** (merged Mar 28) | Removed the dead `model/src/vllm.rs` (a never-compiled `VLLMProvider` skeleton). Currently **no Rust-side vLLM provider** in the tree. |
| **#140** (open, draft) | `OpenAICompatProvider` implementing `ModelProvider` against any OpenAI-compatible endpoint (vLLM, LiteLLM, OpenRouter, Ollama `/v1`), plus `GatewayConfig` enum and `--provider` / `--api-base` / `--api-key` CLI flags. **This is the natural seam** — once landed, vLLM becomes a config flip rather than a code rewrite. |
| **#41** (merged) | Phase 2 LLM intelligence — downstream consumer of `ModelProvider`. |
| **#43** (merged) | Eval framework — downstream consumer of `ModelProvider`. |

## 4. Decision matrix: Ollama vs vLLM (no migration vs migration)

| Axis | Ollama (status quo) | vLLM (post-migration) |
| --- | --- | --- |
| Wire protocol | Custom `/api/chat`, `/api/tags`, `/api/generate` | OpenAI-compatible `/v1/chat/completions`, `/v1/models` |
| Tool-call format | Ollama-specific `tool_calls` shape; ~500 LOC parsing in `ollama.rs` | Standard OpenAI `tool_calls`; reusable across providers |
| Streaming tool-calls | Ollama-specific deltas | Standard OpenAI deltas |
| Default GPU support | NVIDIA via CUDA, basic ROCm | NVIDIA via CUDA, ROCm via container best-effort, CPU container |
| iGPU (the stated motivation) | Not supported in any clean way | Best-effort upstream container path |
| Resource management | Single-request per model load; basic | Paged attention, continuous batching, tensor parallel |
| Cold start | Fast (model files local, copy-on-load) | Slower (HF download + weight load) |
| Model identifiers | `qwen3:0.6b`, `gemma4:e4b` (Ollama registry) | `Qwen/Qwen3-Coder-30B-A3B-Instruct` (HF Hub) |
| Small-model story for unit tests | Trivially fast (`qwen2.5:0.5b`) | Acceptable but slower; needs CI matrix tuning |
| Default port | 11434 | 8000 |
| Readiness probe | `/api/tags` 200 | `/v1/models` 200 |
| Container image | `ollama/ollama:latest` | `vllm/vllm-openai:latest` |
| API key | None (local trust) | Optional (`OPENAI_API_KEY`); secret-redaction needed |
| Latency p50 (chat) | Lower for small models | Higher cold-start, comparable warm |
| Maintenance burden | One in-tree provider | Migration cost up front; smaller in-tree footprint after |

**Decision: migrate.** The deciding factor is that the OpenAI-compatible
wire protocol (#140) is *also* useful as a generic gateway provider —
LiteLLM, OpenRouter, hosted vLLM, even Ollama's own `/v1` endpoint all
work through the same code path. The migration buys broader provider
optionality for free.

## 5. Phased plan

Each phase below is a single PR. Phases are designed so that each one
can land independently behind a feature flag, with no flag day at the
end.

### Phase 1 — Land #140 (OpenAI-compat gateway provider) — NOT IN THIS PR

**Pre-req for everything else.** Until #140 is mergeable, there is no
Rust-side `ModelProvider` impl that speaks vLLM's wire protocol.

Acceptance:

- `OpenAICompatProvider` impls `ModelProvider`.
- Round-trip test against a CPU-only vLLM container in CI (gated, opt-in).
- `--provider`, `--api-base`, `--api-key` flags wired through `harness`.
- Clippy + CI green on the PR.

Risk: tool-call parsing for OpenAI-format streaming deltas needs a
real-server round-trip test, not just unit tests against fixtures.

### Phase 2 — `ModelJudge` blanket impl over `ModelProvider`

Pure refactor. ~150 LOC. Eliminates the per-provider duplication that
made the deleted `model/src/vllm.rs` (#115) collapse under its own
weight.

Acceptance:

- `ModelJudge` becomes a trait with default methods that call
  `ModelProvider::chat` with judge-specific prompts.
- `OllamaProvider`'s bespoke `ModelJudge` impl is removed in favour of
  the blanket impl.
- All existing judge tests pass unchanged.

### Phase 3 — De-concretize harness call sites

Pure refactor on top of Phase 1. ~200 LOC.

Acceptance:

- `harness/src/main.rs::{single_chat, interactive_chat, run_agent,
  run_mcp_server, list_models, health_check}` take
  `Arc<dyn ModelProvider>` instead of `&OllamaProvider`.
- `agent/mod.rs` test helpers take `Arc<dyn ModelProvider>` or use a
  small trait-object test fake.
- `GatewayConfig` from CLI (#140) is wired down to all call sites.
- Existing Ollama integration tests still pass — Ollama is still the
  default until Phase 6.

### Phase 4 — vLLM container integration in `harness/src/container.rs`

Acceptance:

- `ContainerKind::Vllm` variant (or a generalized `ModelContainer` that
  parameterizes image / port / readiness / pull).
- Tests against the existing `nix run .#vllm-start` apps (built in #42).
- Ollama path remains green — the two coexist.

Risk: weight pull is dramatically slower than `ollama pull`. The CI
container-load test should pin to a small model
(`Qwen/Qwen2.5-0.5B-Instruct` or similar) to keep wall-clock under 5
minutes.

### Phase 5 — CI matrix + integration tests

Acceptance:

- New `model/tests/openai_compat_chat_integration.rs` (or similar)
  duplicates `ollama_chat_integration.rs` against the CPU-only vLLM
  container.
- A new `integration-vllm` job in `.github/workflows/ci.yml`,
  ROCm/CUDA paths gated as opt-in (CPU-only by default in CI).
- Eval timeouts in `harness/src/agent/eval.rs::EvaluationConfig` are
  bumped to accommodate vLLM cold-start.
- Eval runner picks a small model for fast unit eval.

Risk: CI runtime budget. The CPU-only vLLM container is heavy; weigh
caching the HF model under cachix vs accepting the cost.

### Phase 6 — Flip the default provider

The flag day. Single PR.

Acceptance:

- `model/Cargo.toml` `default = ["openai-compat"]` (or rename feature).
- `OllamaConfig::default()` callers in `main.rs` / `container.rs`
  default to a vLLM-shaped `GatewayConfig`.
- README, ARCHITECTURE.md, TESTING.md, `docs/poc-system-overview.md`
  updated.
- Migration note in CHANGELOG (or release notes equivalent).

Risk: every downstream `cargo` invocation in CI changes its default
feature set; cache hashes flip. Plan a cache-warming rebuild.

### Phase 7 — Remove Ollama (optional)

Acceptance:

- Delete `model/src/ollama.rs`, `OllamaConfig`, `ollama-rs` dep,
  `nix/containers.nix` `ollamaImage`, all Ollama-specific CI/cache
  jobs, integration tests.
- Optionally keep the OpenAI-compat path pointed at Ollama's `/v1`
  endpoint as a documented user-supported fallback (free — costs no
  extra code).

Risk: branch protection / required-checks list in GitHub references
Ollama-named jobs; coordinate with admin.

## 6. Risk register

| Risk | Severity | Mitigation |
| --- | --- | --- |
| Tool-call protocol divergence (Ollama vs OpenAI streaming `tool_calls`) | High | Phase 1 must include round-trip tests against a real vLLM server; do not merge on unit-test fixtures alone. |
| `ModelJudge` duplication regresses (as in deleted `vllm.rs`) | High | Phase 2 *must* land before any vLLM-specific judge code. Treat duplication as a merge blocker, not a follow-up. |
| Container topology changes simultaneously (port, image, pull, readiness, model id) | Medium | Phase 4 introduces a `ContainerKind` abstraction so each axis is parameterized, not hard-coded twice. |
| vLLM cold-start regresses CI wall-clock budget | Medium | Pin to a 0.5B-class HF model in CI; cache weights via cachix; bump eval timeouts. |
| ROCm iGPU path is best-effort upstream | Medium | CPU container is the CI default; GPU is opt-in only. |
| Default-feature flip cascades into every CI cache hash | Medium | Schedule a cache-warming run immediately before/after Phase 6 lands. |
| `--api-key` flag introduces a new secret surface | Low | Confirm secret-redaction (#232 work) covers `--api-key` and `OPENAI_API_KEY`. Add a redaction unit test in Phase 1. |
| Branch protection's required-checks list references Ollama-named jobs | Low | Coordinate with repo admin in Phase 7. |
| Small-model parity for unit tests | Low | `Qwen/Qwen2.5-0.5B-Instruct` is the closest analogue to `qwen2.5:0.5b`; eval rebaseline expected. |

## 7. Migration checklist

Use this as a per-PR quick-check during execution.

- [ ] Phase 1: #140 lands, OpenAI-compat provider mergeable green.
- [ ] Phase 1: Real-server round-trip test against CPU vLLM in CI.
- [ ] Phase 1: Secret-redaction covers `--api-key` / `OPENAI_API_KEY`.
- [ ] Phase 2: `ModelJudge` blanket impl replaces per-provider impls.
- [ ] Phase 2: All existing judge tests pass unchanged.
- [ ] Phase 3: All harness subcommands take `Arc<dyn ModelProvider>`.
- [ ] Phase 3: `GatewayConfig` flows from CLI to all call sites.
- [ ] Phase 4: `ContainerKind::Vllm` variant added, Ollama coexists.
- [ ] Phase 4: Container-load smoke test pinned to small HF model.
- [ ] Phase 5: `integration-vllm` job in `ci.yml`, CPU-only by default.
- [ ] Phase 5: Eval timeouts bumped for vLLM cold-start.
- [ ] Phase 5: Eval runner has a small-model path for fast iteration.
- [ ] Phase 6: Default Cargo feature flipped to `openai-compat`.
- [ ] Phase 6: README / ARCHITECTURE.md / TESTING.md updated.
- [ ] Phase 6: Cache-warming rerun scheduled around the flip.
- [ ] Phase 7 (optional): Ollama code, deps, CI jobs, integration tests
  removed.
- [ ] Phase 7 (optional): Branch-protection required-checks list updated.

## 8. Out of scope

- Hosted-provider auth (anything beyond a single optional `--api-key`).
- Multi-model routing / cost-aware dispatch (LiteLLM-style).
- Tensor-parallel / multi-GPU configuration. CI is CPU-only.
- ROCm tuning beyond accepting the upstream container as-is.
- Replacing the eval framework's prompt / scoring layer.

## 9. References

- Issue: <https://github.com/DominicBurkart/nanna-coder/issues/39>
- ARCHITECTURE: [`../ARCHITECTURE.md`](../ARCHITECTURE.md)
- vLLM CPU install:
  <https://docs.vllm.ai/en/v0.6.1/getting_started/cpu-installation.html>
- PR #140 (OpenAI-compat gateway): seam for the migration.
- PR #42 (merged): vLLM container Nix infra.
- PR #115 (merged): removed the dead `vllm.rs` skeleton.
