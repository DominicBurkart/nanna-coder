# CI/CD Pipeline

The CI pipeline lives in `.github/workflows/ci.yml`. For the cache strategy that backs it, see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md). For test scope and commands, see [../TESTING.md](../TESTING.md).

## Job graph

```
test-matrix    (unit, integration, lint, security)
build-matrix   (x86_64-linux, aarch64-linux, x86_64-darwin, aarch64-darwin)
build-containers (harness, ollama, qwen3-container, llama3-container)
benchmark        (main only)
cache-maintenance (main only)
ci-summary       (always; aggregates status)
release          (on GitHub release events)
```

The total matrix is roughly 30 parallel jobs.

## `test-matrix`

Runs across `{ubuntu, macos, windows} x {stable, beta, (nightly limited)} x {unit, integration, lint, security}`, fail-fast disabled.

| Test type | Command | Platforms |
|---|---|---|
| unit | `cargo nextest run --workspace --lib` | all |
| integration | `nix run .#container-test` | linux only (containers required) |
| lint | `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` | all |
| security | `cargo audit`, `cargo deny check`, `cargo tarpaulin` | linux only |

Per-platform notes:

- **Linux** — Nix-based builds, full Cachix integration, all test types.
- **macOS** — direct Rust toolchain, cargo-installed tools, no containers (integration skipped).
- **Windows** — direct Rust toolchain, unit + lint only; security tooling has limited Windows support.

Coverage results upload to Codecov with a Rust-version flag.

## `build-matrix`

| Target | Builder | Notes |
|---|---|---|
| x86_64-linux | `nix build .#nanna-coder` | native |
| aarch64-linux | nix cross-compile | falls back to native if cross fails |
| x86_64-darwin | `cargo build --release` | native |
| aarch64-darwin | `cargo build --target aarch64-apple-darwin --release` | cross |

Artifacts are named `nanna-coder-{target}` and uploaded with warning-on-missing.

## `build-containers`

| Image | Architectures |
|---|---|
| harness | x86_64, aarch64 |
| ollama | x86_64, aarch64 |
| qwen3-container | x86_64 |
| llama3-container | x86_64 |

Pushes to `ghcr.io` using the workflow's `GITHUB_TOKEN`. Tags: `latest{-arm64}`, `<sha>{-arm64}`. Push is skipped on pull requests.

## Performance jobs

- **`benchmark`** — runs on pushes to `main`. Uses `cargo bench` with Criterion; results stored on GitHub Pages via `benchmark-action`. Regression alert threshold: 200%.
- **`cache-maintenance`** — pushes to `main` only. Runs `cache-analytics`, pushes to Cachix, summarizes in the GitHub Step Summary.

## Release pipeline

Triggered on GitHub release events. Builds `harness-{target}` for the four targets above and uploads them as release assets.

## `ci-summary`

Always runs. Aggregates job statuses into a markdown table in the Step Summary, surfaces artifact links, and reports failures.

## Caching

See [CACHE_STRATEGY.md](./CACHE_STRATEGY.md) for keys, TTLs, and the push filter (`(-source$|nixpkgs\.tar\.gz$)`). Other caches in play:

- Cargo dependency cache (per-runner).
- Container layer cache (Docker / podman).

## Security gates

Supply chain:

- `cargo audit` — vulnerability scanning.
- `cargo deny check` — license compliance.
- Pinned dependencies (`Cargo.lock`, `flake.lock`).

Containers:

- Trivy scan with SARIF upload.
- Multi-stage builds for minimal attack surface.

Access:

- Minimal-permission `GITHUB_TOKEN` per job.
- Branch protection requires the `ci-summary` check.
- Secrets are repo-scoped; fork PRs cannot access them.

## Performance targets

| Metric | Target |
|---|---|
| Cache hit rate | >85% |
| Total pipeline time | <20 min |
| Container build per image | <10 min |
| Binary build per target | <5 min |
| Cache storage | <50 GB |

## Local reproduction

```bash
nix develop --command cargo nextest run
nix run .#dev-check
nix build .#nanna-coder
nix flake check
nix build .#qwen3-container
nix run .#container-test
```

CI investigation:

```bash
nix run .#cache-analytics
nix path-info --json .#nanna-coder
nix flake show
nix flake metadata
```

## Troubleshooting

**Cache miss** — verify `CACHIX_AUTH` is set, cache keys are content-addressed correctly, and push filters do not exclude the artifact.

**Cross-compilation failure** — confirm target support in `flake.nix`; the workflow falls back to native compilation on failure.

**Container build failure** — check `GITHUB_TOKEN` permissions, base-image availability, and runner disk/memory.

For pipeline change requests, open an issue.
