# CI Performance

Where time goes in the current pipeline, and what controls it. This doc describes the existing behavior — it does not propose tuning, which is out of scope for #10.

## Where wall-clock time is spent

The critical path through `ci.yml` for a typical PR is:

```
test-matrix (slowest variant) --> build-containers --> all-checks
```

`build-matrix` runs in parallel with `build-containers` but is almost always faster. `security-scan`, `cache-maintenance`, and `release` are gated off on PRs, so they do not extend PR latency.

Within `test-matrix`, the slowest variants are:

1. **`integration-container` (Linux)** — pre-builds `ollamaImage` via `nix build .#ollamaImage`, loads it via `copyToDockerDaemon`, and runs integration tests with `--test-threads=1`. Both the image build (on cold cache) and the serialized test run are dominant.
2. **`security` (Linux)** — `cargo tarpaulin ... --timeout 1800`. Tarpaulin instruments every binary, so E2E tests run materially slower than under plain `cargo test`. The 30-minute timeout is a conscious accommodation; see the inline comment in `ci.yml`.
3. **`integration` (Linux)** — non-container integration suite.
4. **`unit` (macOS, Windows)** — slower than Linux unit because the non-Nix path installs tools via `taiki-e/install-action@v2` per job.

## What keeps it fast

### Binary cache

Every Linux job that uses Nix configures Cachix via `cachix/cachix-action@v15` with `authToken: ${{ secrets.CACHIX_AUTH }}`. The `pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"` keeps the cache from bloating with source tarballs. On a cache hit, Rust toolchains, cargo dependencies, and the Nix-built `nanna-coder` binary come straight from the binary cache.

See [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md) for cache keys and [../binary-cache-strategy.md](../binary-cache-strategy.md) for the architectural rationale.

### Cache warming

`cache-warming.yml` runs on every push to `main` that touches `flake.lock`, any `Cargo.toml`, or the workflow itself. It warms the `nanna-coder` cache under a key of the form `cachix-v1-deps-<flake hash>-<cargo hash>`, so the first PR after a lockfile bump does not pay a cold-cache cost.

### `ci-cache-optimize`

`ci.yml`'s Linux path calls `nix run .#ci-cache-optimize` (defined in [../../nix/cache.nix](../../nix/cache.nix)). This sets `max-jobs`, `cores`, `tarball-ttl`, and related flags via `NIX_CONFIG`. It runs before any heavy build step, so all subsequent Nix invocations inherit the same config.

### Matrix parallelism

- `test-matrix` runs seven variants in parallel (`fail-fast: false`), so a single slow job does not block the others.
- `build-matrix` runs four targets in parallel.
- `build-containers` runs two images in parallel.
- `cache-warming.yml`'s `warm-containers` runs `harness` and `ollama` in parallel.

### Non-Linux platforms

macOS and Windows cannot use Nix, so they install `cargo-nextest`, `cargo-audit`, `cargo-deny`, and (for security) `cargo-tarpaulin` via `taiki-e/install-action@v2`. This action downloads pre-built binaries rather than compiling from source, which is the single biggest reason those jobs are not much slower than they are.

## What slows it down

- **First PR after a lockfile bump before `cache-warming.yml` completes.** Workaround: trigger `cache-warming.yml` manually with `workflow_dispatch` and `force_rebuild=true`, then rebase the PR.
- **A pushed PR that touches `flake.nix` non-trivially.** Cache keys change and everything downstream rebuilds.
- **`integration-container` cold-cache runs.** The `ollamaImage` is big; warming helps.
- **Codecov upload on a slow network day.** `fail_ci_if_error: true` means an upload flake fails the job rather than being retried.

## What is explicitly not tuned here

Per the scope of #10, this documentation does not propose changes to runners, job topology, or caching. If you identify a regression, file a focused issue. When proposing changes, update [architecture.md](architecture.md) and [maintenance.md](maintenance.md) in the same PR so the invariants in [maintenance.md](maintenance.md) stay intact.

## Benchmarks in `cache-warming.yml`

Each job in `cache-warming.yml` computes its own elapsed time via `START_TIME=$(date +%s)` / `END_TIME=$(date +%s)` and writes a markdown summary. This is deliberately informational; no threshold fails the build.

## Related documents

- [architecture.md](architecture.md) — job graph and per-job details
- [troubleshooting.md](troubleshooting.md) — slow vs. failed diagnosis
- [maintenance.md](maintenance.md) — when to upgrade pinned actions
- [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md) — operational cache keys
- [../binary-cache-strategy.md](../binary-cache-strategy.md) — cache tier design
- [../ci-cd-pipeline.md](../ci-cd-pipeline.md) — earlier narrative on performance targets
- [../developer-experience.md](../developer-experience.md) — reproducing timings locally
