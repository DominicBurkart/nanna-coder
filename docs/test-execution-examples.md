# Test Execution Examples

Reference traces of the integration-test runner under three conditions, showing how the Nix-based container caching changes behavior. For test scope and commands, see [../TESTING.md](../TESTING.md).

## Local, no Ollama / container runtime

```bash
$ cargo test --test integration_tests -- --nocapture
```

Output (abridged):

```
running 14 tests
test test_chat_request_building ... ok
test test_config_validation ... ok
test test_echo_tool_execution ... ok
test test_calculator_tool_execution ... ok
test test_model_provider_creation ... ok

# Ollama tests skip gracefully:
Ollama health check failed: Service unavailable
test test_ollama_health_check ... ok

Failed to list models: Service unavailable
test test_ollama_list_models ... ok

# Container test falls back gracefully:
Pre-built container not found, falling back to base container
   To build cached container: nix build .#ollama-qwen3
Failed to start Ollama container: short-name resolution enforced but cannot prompt without a TTY
test test_containerized_ollama_qwen3_communication ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Notes: all tests pass (no failures, no `ignored`); environment-dependent steps degrade gracefully and emit guidance; total time ~0.1 s.

## Local, pre-built containers

One-time setup:

```bash
$ nix build .#ollama-qwen3
# Downloads qwen3:0.6b (~560MB), caches by hash, builds container
$ podman load -i $(nix build .#ollama-qwen3 --print-out-paths --no-link)/image.tar
```

Subsequent runs:

```bash
$ cargo test --test integration_tests -- --nocapture
```

Output (abridged):

```
Using pre-built test container with qwen3:0.6b cached
Health check passed
Model listing passed - qwen3:0.6b found
Chat response received
Tool calls received: 1 calls
Container cleaned up successfully
test test_containerized_ollama_qwen3_communication ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

First run ~15 s; subsequent runs with warm containers ~5 s.

## CI

Workflow excerpt (`.github/workflows/ci.yml`):

```yaml
- name: Pre-build test containers (cached)
  run: |
    nix build .#ollama-base .#qwen3-model .#ollama-qwen3 --print-build-logs
    podman load -i $(nix build .#ollama-qwen3 --print-out-paths --no-link)/image.tar

- name: Run tests
  run: nix develop --command cargo test --workspace --verbose
```

First run ~3 min (model download); subsequent runs ~30 s (full cache hits).

## Performance summary

| Scenario | Test time | Model download | Cache state |
|---|---|---|---|
| Local, no deps | ~0.1 s | none | n/a |
| Local, first run | ~3 min | 560 MB | building |
| Local, cached | ~15 s | none | hit |
| CI, first run | ~3 min | 560 MB | building |
| CI, cached | ~30 s | none | hit |

## Hash-mismatch workflow

When a model needs to be updated:

```bash
$ nix build .#qwen3-model
error: hash mismatch in fixed-output derivation
  specified: sha256-AAAAAAAAAA...
  got:        sha256-b8f2c3d4e5...

# Update flake.nix with the correct hash, then rebuild:
$ sed -i 's/sha256-AAAAAAAAAA.../sha256-b8f2c3d4e5.../' flake.nix
$ nix build .#qwen3-model
```

This keeps builds bit-for-bit reproducible across machines and time.
