# CI/CD Pipeline

Reference for the parallel CI/CD pipeline at `.github/workflows/ci.yml`. For
cache details see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md); for the
infrastructure self-tests see [ci/integration-tests.md](./ci/integration-tests.md).

## Pipeline matrix (~30 parallel jobs)

| Stage | Dimensions |
|-------|-----------|
| Test matrix | OS (Ubuntu, macOS, Windows) × Rust (stable, beta, limited nightly) × test-type (unit, integration, lint, security) |
| Build matrix | x86_64-linux (Nix), aarch64-linux (Nix cross), x86_64-darwin (cargo), aarch64-darwin (cargo cross) |
| Container matrix | harness, ollama (x86_64 + aarch64); qwen3-container, llama3-container (x86_64 only) |
| Performance | Benchmarks (main only), cache maintenance (main only), CI summary (always) |

## Test types

| Type | Command | Platforms |
|------|---------|-----------|
| Unit | `cargo nextest run --workspace --lib` | all |
| Integration | `nix run .#container-test` | Linux only (containers required) |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all -- --check` | all |
| Security | `cargo audit`, `cargo deny check`, `cargo tarpaulin` (coverage uploaded to Codecov, flagged by Rust version) | Linux only |

Platform notes: Linux uses Nix for reproducible builds + full Cachix integration;
macOS uses direct rustup + cargo (no container support); Windows runs unit and
lint only (security tooling unavailable).

## Build strategy

- **Linux** (Ubuntu): native Nix (`nix build .#nanna-coder`); aarch64-linux via
  Nix cross with cargo fallback.
- **macOS**: native cargo (`cargo build --release`); aarch64-darwin via
  `--target aarch64-apple-darwin`.

Artifacts named `nanna-coder-{target}`; missing-file uploads warn rather than fail.

## Container builds

Pushed to `ghcr.io/dominicburkart/nanna-coder`. Tags: `latest{-arm64}` and
`{sha}{-arm64}`. PRs build but do not push. Authentication: GitHub token.

Trivy vulnerability scanning runs on built images and uploads SARIF results.

## Performance jobs

- **`benchmark`** (main only): cargo bench + criterion via
  `benchmark-action/github-action-benchmark`. Stored on GitHub Pages; alerts
  on >200% regression and comments on PRs.
- **`cache-maintenance`** (main only): pushes successful builds to Cachix and
  emits cache analytics to `$GITHUB_STEP_SUMMARY`.

## Release pipeline

Triggered on GitHub release events. Builds all four platform targets in parallel
and uploads artifacts named `harness-{target}` to the release assets.

## Performance targets

| Metric | Target |
|--------|--------|
| Cache hit rate | >85% |
| Full pipeline | <20 min |
| Container build | <10 min/image |
| Binary build | <5 min/target |

## Local reproduction

```bash
nix develop --command cargo nextest run    # tests
nix build .#nanna-coder                    # main binary
nix build .#qwen3-container                # container
nix flake check                            # all checks
```

## Configuration entry points

- `.github/workflows/ci.yml` — main pipeline
- `.github/workflows/cache-warming.yml` — `main`-branch cache pre-population
- `.github/workflows/ci-integration.yml` — CI infrastructure self-tests (see
  [ci/integration-tests.md](./ci/integration-tests.md))
- `flake.nix` — build configuration
- `Cargo.toml` — workspace + Rust configuration
