# CI Performance

The goal is keeping PR wall time low without giving up correctness. This
document lists the tuning knobs in the current pipeline and the observations
that justify them.

## Wall-time budget

Rough targets for the primary pipeline
([`ci.yml`](../../.github/workflows/ci.yml)) on a cache-warm PR:

| Job | Target | Notes |
|-----|--------|-------|
| `test-matrix (unit, ubuntu-latest)` | < 5 min | Cachix should have Rust deps warm. |
| `test-matrix (lint, ubuntu-latest)` | < 5 min | Cachix should have Rust deps warm. |
| `test-matrix (security, ubuntu-latest)` | < 30 min | Tarpaulin ceiling `--timeout 1800`. |
| `test-matrix (integration, ubuntu-latest)` | < 15 min | |
| `test-matrix (integration-container, ubuntu-latest)` | < 20 min | Includes image build/load. |
| `test-matrix (unit, macos-latest)` | < 10 min | No Nix cache; native rustup. |
| `test-matrix (unit, windows-latest)` | < 10 min | Same. |
| `build-matrix (all four)` | < 15 min each | |
| `build-containers (harness, ollama)` | < 15 min each | |
| `all-checks` | seconds | Just the coverage audit + fan-in. |

These are targets, not guarantees. Cold-cache runs (first PR after a
`flake.lock`/`Cargo.lock` change) will overshoot; that's what
[`cache-warming.yml`](../../.github/workflows/cache-warming.yml) exists to
prevent for the next PR.

## The tuning knobs, ranked by impact

### 1. Cachix cache warmth (highest impact)

The single biggest lever. Cachix (`nanna-coder`) is populated by every Linux
job that installs Nix, and pre-populated on every `main` push touching
lockfiles by
[`cache-warming.yml`](../../.github/workflows/cache-warming.yml).

Symptoms of cold cache:

- `nix build .#nanna-coder` step running 10+ min instead of seconds.
- `nix develop --command …` step showing dependency compilation instead of
  jumping straight into cargo.

Recovery:

- Trigger `cache-warming.yml` manually via `workflow_dispatch` with
  `force_rebuild=true`.
- Verify hit rate at the Cachix dashboard; see baseline in
  [`docs/binary-cache-strategy.md`](../binary-cache-strategy.md).

### 2. `pushFilter` in `cachix-action`

Every `cachix/cachix-action@v15` invocation sets:

```yaml
pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"
```

This excludes the very large, low-reuse source tarballs from being pushed
back to Cachix. Removing this pattern would blow up storage without
improving hit rate.

### 3. `nix run .#ci-cache-optimize`

Runs at the start of every Linux job in `ci.yml`. Tunes local Nix store
settings for CI cache reuse.

### 4. `test-threads=1` on container integration tests

`test-matrix (integration-container)` runs
`cargo nextest run … --test-threads=1`. This is a correctness constraint
(shared Docker daemon, fixed ports), not a performance choice. Trying to
parallelize it will flake, not speed up.

### 5. Nextest over `cargo test`

Every test cell uses `cargo nextest run`. Nextest parallelizes test
execution more aggressively than `cargo test` and gives cleaner output for
failures. Installed via Nix on Linux and via
`taiki-e/install-action@v2` on macOS/Windows.

### 6. Native toolchain on macOS/Windows

macOS and Windows use `dtolnay/rust-toolchain@master` +
`taiki-e/install-action` rather than Nix. On those platforms the Nix install
cost dominates the actual work — measured empirically as a wash on macOS and
a strict loss on Windows.

### 7. Prebuilt containers for integration-container cell

`test-matrix (integration-container)` runs `nix build .#ollamaImage` and
loads it via `copyToDockerDaemon` before the test step. This front-loads the
container preparation into a single upfront cost instead of a per-test
cost.

### 8. `--workspace --all-features` on tarpaulin

Explained in [`architecture.md`](architecture.md#test-matrix): without this
flag, feature-gated modules read as 0% coverage and codecov rejects the PR,
forcing a re-run. The "extra" work is cheaper than a fail-and-retry.

### 9. Concurrency groups on `ci-integration.yml`

```yaml
concurrency:
  group: ci-integration-${{ github.ref }}
  cancel-in-progress: true
```

Pushing a new commit to the same branch cancels the previous
`ci-integration` run. Same trick is worth adding to any future workflow
where the newest commit's result is the only one that matters.

## Diagnosing a slow run

1. Open the run summary. Sort jobs by duration.
2. For the slowest job, open its log and look for either:
   - Long `nix build …` steps (→ cache miss; check Cachix).
   - Long `cargo build` steps (→ dependency graph change or no incremental
     cache).
   - Long test steps (→ profile locally; check for `#[ignore]` tests that
     should have stayed ignored).
3. Compare against the previous green run on `main` — a 2× regression
   almost always maps to a specific commit.

## Known slow paths

- **First PR after a `Cargo.lock` update.** Expect cold Rust dep cache. The
  next PR benefits from cache warming.
- **First PR after a `flake.lock` update.** Same, for Nix deps. Cache
  warming triggers on `flake.lock` changes.
- **Integration-container cell.** Image build + load + serialized tests.
  This is inherent, not a tuning failure.
- **Nightly install E2E** ([`install-nightly.yml`](../../.github/workflows/install-nightly.yml))
  runs up to 90 min with a multi-GB model pull. Not on PR path.

## What performance we deliberately do not chase

- **Skipping macOS/Windows on non-user-facing changes.** Docs-only PRs
  still run the full matrix. The occasional wasted minutes are cheaper than
  the risk of a docs PR accidentally reformatting a workflow YAML.
- **Sharding test-matrix by test count.** Unit-test count is small enough
  that startup cost would dominate.
- **Removing `--all-features` from tarpaulin.** See point 8 above.

## Instrumentation gap

The pipeline currently has no first-class build-time telemetry outside the
GitHub Actions UI. Issue #5 tracks adding metrics collection; the
first-slice workflow is proposed in
[`proposed-ci-metrics-workflow.md`](proposed-ci-metrics-workflow.md)
pending a maintainer with `workflows` permission installing it into
`.github/workflows/ci-metrics.yml`. Once installed, per-job wall-time
snapshots appear as the workflow's step summary and as
`ci-metrics-<run_id>` artifacts.

## See also

- [`architecture.md`](architecture.md) — the pipeline structure being tuned.
- [`maintenance.md`](maintenance.md) — cache and toolchain hygiene.
- [`troubleshooting.md`](troubleshooting.md) — when slow becomes broken.
- [`docs/CACHE_STRATEGY.md`](../CACHE_STRATEGY.md)
- [`docs/binary-cache-strategy.md`](../binary-cache-strategy.md)
