# cron job `fyhme`

```
job-id: fyhme
oldest-issue-id: one_track#2
created: 2026-05-01
runner: claude-opus-4-7 stateless one-shot
trigger: deliver oldest 15 author:DominicBurkart open issues across managed repos
```

## Per-issue disposition (nanna-coder, in scope this run)

| issue | title | canonical PR | head sha | action |
|------:|-------|-------------:|----------|--------|
| #5 | monitoring: CI health monitoring and performance metrics | #247 | `8a58969` | NO-OP — defer (non-draft, codecov success) |
| #10 | docs: Comprehensive CI maintenance and troubleshooting documentation | #254 | `d21eebc` | NO-OP — defer (non-draft, codecov success) |
| #20 | Entity Management (epic) | n/a | n/a | EPIC — open subs #23, #24 — defer to leaves |
| #23 | Entity Type: AST & Filesystem Entities | #258 | `7c2b7ed` | DEFER + flag — codecov 92.70% < 100% target |
| #24 | Entity Type: Testing & Analysis Entities | #267 | `f229f36` | DEFER + flag — codecov 80.88% < 100% target |
| #39 | Migrate from Ollama to vLLM | (this PR — plan only) | (n/a) | NEW — plan only, no code |

### Flagged gaps (constructive, for human/janitor)

- **PR #258 (#23)** is at 92.70% diff coverage; target is 100%. The uncovered lines are likely error paths in `entities/ast/` parser fallbacks. A short follow-up PR adding `nextest` cases for the AST entity discovery edges should close the gap. This CRON did not push to `peaceful-noether-vyD7d-issue-23` because it was not authored by `fyhme`.
- **PR #267 (#24)** is at 80.88% diff coverage. Wider gap; nextest cases for the test-entity discovery edge paths (cargo workspace probing, missing `tarpaulin.toml`, `nextest`-vs-`cargo test` selection) should be added. Same authorship constraint as above.

## Plan: nanna-coder #39 — Migrate from Ollama to vLLM

### Executive summary (5 bullets)

- The migration's load-bearing gap is **Rust**, not containers: `OpenAiProvider` does not exist in tree. `model/src/lib.rs` only re-exports `OllamaProvider`, and `harness/src/main.rs` hard-codes `OllamaProvider::new(OllamaConfig::default())` in five places (chat, agent, mcp-serve, models, health). `nix/containers.nix` already has a `vllmImage` wrapper (currently a thin shell wrapper that runs `vllm/vllm-openai:latest`) and HF-format entries in `modelRegistry` waiting to be wired up.
- vLLM speaks the OpenAI Chat Completions API on port 8000 (`/v1/chat/completions`, `/v1/models`, `/health`). Migration is therefore: (a) introduce a generic `OpenAiCompatProvider`; (b) gate `OllamaProvider` behind a non-default `ollama` feature; (c) point harness at `http://vllm:8000/v1`; (d) swap container in `ci.yml`'s `build-containers` matrix; (e) update `eval.yml`.
- Headline risk is **model-weight-format swap**: today `nix/containers.nix` bakes Ollama-format models (`qwen3:0.6b`, `gemma4:e4b`); vLLM consumes HF safetensors via `--model <hf_repo>`. CI eval uses `qwen3:0.6b` (`eval.yml:13`); HF equivalent is `Qwen/Qwen3-0.6B`. Parity testing must verify chat-template + tool-calling shape, not byte-equal outputs.
- CPU fallback (`vllm/vllm-openai-cpu:latest`, x86_64 only) is the documented baseline; ROCm iGPU is best-effort and stays a follow-up issue.
- **This CRON ships a plan PR only — no code.** Reasoning at the bottom.

### Migration surface inventory

| Layer | File | Change |
|---|---|---|
| Rust provider trait | `model/src/provider.rs` | None (already abstract) |
| Rust impls | `model/src/{ollama.rs,lib.rs,config.rs}` (+ new `model/src/openai.rs`) | Add `openai.rs`; gate `ollama` feature off-by-default; add `OpenAiConfig` (base_url, api_key optional, timeout, reuse `default_temperature`/`default_max_tokens`) |
| Crate manifest | `model/Cargo.toml` | `default-features` flip from `["ollama"]` to `["openai"]` (in phase 4) |
| Harness wiring | `harness/src/main.rs` (5 sites: chat / models / health / run_agent / run_mcp_server), `harness/src/task.rs`, `harness/src/agent/*` | Replace `OllamaProvider::new(OllamaConfig::default())` with provider selection from `NANNA_MODEL_PROVIDER={vllm,ollama}` env (default `vllm`). Update default `--model` clap defaults from `qwen3:0.6b` / `llama3.1:8b` to `Qwen/Qwen3-0.6B` |
| Containers | `nix/containers.nix` | Promote `vllmImage` to a real `nix2container.buildImage` (CPU variant first); add `vllm-qwen3-container` baking `Qwen/Qwen3-0.6B` weights; gate `ollamaImage` & co. on a `legacy = true` flag |
| Container config | `nix/container-config.nix` | Add `images.vllm`, `vllmRef`, model variants |
| CI build matrix | `.github/workflows/ci.yml` (`build-containers`, ~L309 & ~L333: `image: [harness, ollama]`) | Add `vllm` to matrix while keeping `ollama` until phase 4. Flip integration-container port probe (~L163) from `:11434` to `:8000` |
| CI eval | `.github/workflows/eval.yml` | Replace `curl -fsSL https://ollama.com/install.sh \| sh` block with `podman run -d -p 8000:8000 vllm/vllm-openai-cpu:latest --model Qwen/Qwen3-0.6B`; add `provider` workflow_dispatch input |
| CI install | `.github/workflows/install-test.yml`, `scripts/install.{sh,ps1}` | Touched by in-flight PRs #321/#335 — limit changes here to port `11434`→`8000` health-probe sites |
| Tests | `harness/tests/*`, `model/tests/*` | Add `harness/tests/provider_parity_test.rs` (vLLM vs `OllamaProvider` mock for chat shape + tool-call schema). Update any test pinning port 11434 |

### Phased plan with reproducible feedback loops

| Phase | Scope | Done when (reproducible local + CI) | Parallel? |
|------:|-------|--------------------------------------|-----------|
| **1. OpenAI-compat provider crate** | Add `model/src/openai.rs` implementing `ModelProvider` against `/v1/chat/completions`. Gate `ollama` feature non-default. | `nix develop --command cargo nextest run -p model --features openai --no-default-features` and `cargo clippy -p model --all-features -- -D warnings`. Add wiremock-based fixture test for `/v1/chat/completions` request/response. | Yes — isolated to `model/` crate |
| **2. vLLM container in Nix** | Promote `vllmImage` from shell wrapper to real `nix2container.buildImage` (CPU variant first), plus `vllm-qwen3-container` pre-staging `Qwen/Qwen3-0.6B` weights. Keep `ollamaImage` & `qwen3-container` co-existent. | `nix build .#vllmImage`; `nix run .#vllmImage.copyToDockerDaemon`; `podman run --rm -d -p 8000:8000 nanna-coder-vllm:latest && curl -fsS localhost:8000/v1/models` returns 200 with `Qwen/Qwen3-0.6B`. | Yes — isolated to `nix/` |
| **3. Harness wiring + parity test** | Provider selection via `NANNA_MODEL_PROVIDER` env (default `vllm`). New `harness/tests/provider_parity_test.rs` runs same prompt through both providers, asserts non-empty `ChatResponse.choices[0].message.content` and tool-call schema parity behind a `#[cfg(feature = "parity")]` gate. New CI lane in `ci-integration.yml` brings up both containers. | `cargo nextest run -p harness --test provider_parity_test --features parity`; CI lane green. | Sequential — must wait for phase 2 (parity test needs a running vLLM container) |
| **4. Default flip** | `default-features = ["openai"]` in `model/Cargo.toml`; clap defaults `qwen3:0.6b` → `Qwen/Qwen3-0.6B`; `ci.yml` build matrix drops `ollama` (Ollama becomes opt-in via `--features ollama --no-default-features`). | Full `nix flake check`; `cargo nextest run --workspace --all-features`; codecov target unchanged (`tarpaulin.toml`/`codecov.yml`); `eval.yml` green on vLLM. | Sequential — must wait for phase 3's parity test on main |
| **5. ROCm iGPU variant** *(follow-up)* | `vllmImage-rocm` variant consuming `nix/gpu-support.nix::rocmSupport.rocmLibraries`. File as new sub-issue. | `nix build .#vllmImage-rocm` build smoke. Runtime testing requires AMD hardware not in GHA. | Independent; new PR after phase 4 lands |

### Risks (expanded)

1. **`OpenAiProvider` does not exist.** Issue prompt assumed PR #140 had landed — code search shows it has not. Migration must build it.
2. **Tool-call parity.** vLLM emits OpenAI-style `tool_calls` only when the chat template supports it. `Qwen/Qwen3-0.6B` does, but `ChatMessage::tool_response` in `model/src/types.rs` must round-trip via both providers; phase 3 parity test must exercise the calculator tool path.
3. **vLLM cold-start.** Model load is 30–90s on CPU; CI eval timeouts will need bumping or `--load-format dummy` for smoke tests.
4. **Tokenizer / chat-template drift.** Qwen3 HF chat template differs subtly from Ollama's baked template, which can break tool-call JSON serialization.
5. **Architecture constraint.** `vllm/vllm-openai-cpu:latest` is x86_64-only — confirm `runs-on: ubuntu-latest` arch matches; ARM macOS dev loop is broken without a remote-build escape hatch.
6. **Memory footprint.** vLLM CPU on `Qwen3-0.6B` needs ~4 GB RAM; GitHub-hosted runners may OOM on cold-start + test load.
7. **HF auth.** `Qwen3-0.6B` is permissive, but document the `HF_TOKEN` story for future gated models.
8. **Cachix size.** Baking HF weights into a Nix fixed-output derivation pushes multi-GB blobs to `nanna-coder.cachix.org`. Mitigation: only push small `Qwen3-0.6B` (~1.2 GB) for CI; leave 30 B variants runtime-pull-only.
9. **Container build time.** Upstream vLLM image is ~8 GB. Phase 2 must use `fromImage = vllmUpstream` rather than rebuild from scratch.
10. **In-flight conflicts.** PR #335 touches Ollama port-collision logic; PR #290 removes the `:11434` probe. Base on post-#290 main; rebase if #335 lands first.

### Non-goals

- Streaming completions (`/v1/chat/completions` with `stream=true`).
- Structured-output / JSON-mode parity.
- Vision / multimodal inputs.
- Multi-LoRA serving.
- Online benchmarking / throughput tuning.

### Swarm decomposition

- **Worktree A** (phase 1): `model/` crate. Pure Rust. No CI dependency.
- **Worktree B** (phase 2): `nix/` containers. Pure Nix. No Rust dependency.
- **Worktree C** (phase 3): merges A + B; harness wiring + parity test. Must serialize after **B** (parity test needs running vLLM image), naturally also after A.
- **Worktree D** (phase 4): default flip + CI matrix. Must serialize after **C**'s parity test passes on main.
- **Worktree E** (phase 5, follow-up issue): ROCm. Independent; opens after D.

### CI / static-analysis / feedback-loop matrix

Phase | Local cmd | CI workflow checked
---|---|---
1 | `nix develop --command cargo nextest run -p model --features openai --no-default-features` | `ci.yml::test`, `clippy`, `Test Suite`
2 | `nix build .#vllmImage && podman run --rm -d -p 8000:8000 nanna-coder-vllm:latest && curl -fsS localhost:8000/v1/models` | `ci.yml::build-containers` (matrix-extended)
3 | `cargo nextest run -p harness --test provider_parity_test --features parity` | new `ci-integration.yml::provider-parity`
4 | `nix flake check && cargo nextest run --workspace --all-features` | full `ci.yml`, `eval.yml`

### Why this CRON does not ship code

Three reasons:

1. **Cross-cutting change.** Migration crosses Rust crate boundaries (introduce `OpenAiProvider`), Nix containers refactor, CI matrix change, and a model-format swap. Bundling them in one autonomous PR violates the repo's recent surgical-PR precedent (#321, #246, #223).
2. **Plan premise was factually wrong.** Issue #39's working assumption that PR #140 had shipped an OpenAI-compat provider is **incorrect** (`OpenAiProvider` is not in tree). A human should confirm intent before a stateless CRON spends compute building one.
3. **Eval rebaseline needed.** Swapping GGUF/Ollama for HF safetensors changes evaluation outputs in ways that need a human-evaluated parity baseline before any default-flip lands. Phase 3's parity test is the gate.

The temptation to ship phase 1 alone (the `OpenAiProvider` crate) is real — it is genuinely isolated. But a stateless one-shot CRON cannot guarantee that wiremock fixtures, feature-gating churn in `model/Cargo.toml`, and downstream `harness` compile breakage all land green in one shot, and the surgical-PR precedent argues for a human-staged rollout once intent is confirmed.

### Promotion / janitor

Per CRON contract clause (3): **planner jobs never promote**. This PR is intentionally NOT labeled `ready-for-review`. Janitor decides when phase 1 may begin and on which branch.

### Idempotency / fingerprint

Re-running this CRON on the same trigger produces the same no-op: an existing open PR with header `job-id: fyhme` referencing `oldest-issue-id: one_track#2` is detected before any agent spawns. Janitor closes duplicates on sight.
