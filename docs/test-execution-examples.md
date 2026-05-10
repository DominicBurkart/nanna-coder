# Test Execution Examples

Reference for what test runs look like under three environments. For canonical
test commands see [../TESTING.md](../TESTING.md); for cache details see
[CACHE_STRATEGY.md](./CACHE_STRATEGY.md).

## Local without dependencies

`cargo test --test integration_tests` runs the full suite. Tests that need
Ollama or a container runtime detect missing dependencies and skip gracefully
(no failures, no `--ignored`). Expected runtime: ~0.1 s.

Skip messages clarify the cause and how to enable full coverage locally
(install Podman + run `nix build .#ollama-qwen3`).

## Local with pre-built containers

One-time setup:

```bash
nix build .#ollama-qwen3
podman load -i $(nix build .#ollama-qwen3 --print-out-paths --no-link)/image.tar
```

Then `cargo test --test integration_tests` exercises the full path with the
qwen3:0.6b model baked in. Runtime: ~15 s first run, ~5 s with warm
containers.

## CI (GitHub Actions)

`.github/workflows/ci.yml` pre-builds containers in parallel, loads them into
Podman, and runs `cargo test --workspace`:

```yaml
- name: Pre-build test containers (cached)
  run: nix build .#ollama-base .#qwen3-model .#ollama-qwen3 --print-build-logs
- name: Load test containers into podman
  run: podman load -i $(nix build .#ollama-qwen3 --print-out-paths --no-link)/image.tar
- name: Run tests
  run: nix develop --command cargo test --workspace --verbose
```

Runtime: ~3 min on cold cache (downloads the 560 MB model once); ~30 s on
subsequent runs (Cachix hit).

## Performance comparison

| Scenario | Test time | Model download | Cache state |
|----------|-----------|----------------|-------------|
| Local (no deps) | ~0.1 s | none | n/a |
| Local (first run) | ~3 min | 560 MB | building |
| Local (cached) | ~15 s | none | hit |
| CI (first run) | ~3 min | 560 MB | building |
| CI (cached) | ~30 s | none | hit |

## Updating the model hash

```bash
nix build .#qwen3-model         # fails on placeholder; reports real sha256
# update flake.nix with the reported hash, then:
nix build .#qwen3-model         # succeeds
```

The model is content-addressed by its actual hash, so builds are bit-identical
across machines. See [`nix/README.md`](../nix/README.md) for the helper script
that automates the capture (`scripts/update-model-sha256.sh`).
