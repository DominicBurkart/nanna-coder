# CI Performance

How to measure CI performance, where the time goes, and what to tune.

## Where the time goes (warm cache, typical PR)

Approximate wall-clock budget on a fully warm Cachix:

| Job | Time | Notes |
|-----|------|-------|
| `test-matrix/unit (ubuntu)` | ~3 min | bottlenecked by nextest startup |
| `test-matrix/unit (macos)` | ~6 min | no Nix; slower toolchain bootstrap |
| `test-matrix/unit (windows)` | ~8 min | slowest matrix entry |
| `test-matrix/integration` | ~5 min | container pre-pull included |
| `test-matrix/integration-container` | ~10 min | Ollama + dev container |
| `test-matrix/lint` | ~2 min | clippy dominates |
| `test-matrix/security` | ~15 min | **tarpaulin** dominates |
| `build-matrix` (each) | ~4 min | linker dominates |
| `build-containers` (each) | ~3 min | mostly skopeo copy |
| `security-scan` | ~2 min | Trivy scan |
| **Total wall-clock (parallel)** | **~18 min** | gated by slowest matrix entry |

Cold cache (first build after lock-file change): +20–30 min.

## Performance Levers

### 1. Cache hit rate (highest leverage)

Cachix is the dominant cost factor. A 1% drop in hit rate adds
multi-minute rebuilds.

- **Monitor**: Cachix dashboard → "Cache hit rate" graph.
- **Tune**: ensure `pushFilter` is `(-source$|nixpkgs\.tar\.gz$)` (the
  default; don't push source tarballs).
- **Warm**: `cache-warming.yml` runs on lock-file changes; trigger it
  manually after large dependency bumps.

### 2. `tarpaulin` runtime

Coverage instrumentation makes tests 2–4× slower. The `--timeout 1800`
budget is real and we've hit it before.

- **Don't**: add `#[ignore]` tests to skip coverage; they still appear
  as uncovered in codecov/patch.
- **Do**: keep slow tests fast by mocking expensive setup. Profile with
  `cargo nextest run -- --test-threads=1`.
- **Last resort**: split tarpaulin into a separate workflow that runs
  in parallel with `test-matrix`.

### 3. Container builds

`nix2container` images are reproducible — a rebuild on cache hit is
~30s. A miss is several minutes.

- **Monitor**: `nix log` output for "querying paths" vs "building".
- **Tune**: keep the harness image's `contents:` list lean. Each item
  is a separate cache key.

### 4. Matrix breadth

Every matrix entry adds queue time and runner-minute cost.

- **Audit**: do we really need `windows-latest` for unit tests?
  Probably yes (Windows-specific bugs exist), but the calculus is
  worth revisiting if the bill grows.

### 5. `nextest` parallelism

Default is `num_cpus`. CI runners have 2–4 cores; this is usually fine.
Integration-container tests run with `--test-threads=1` because they
share a single Ollama instance.

## Measuring

`ci-metrics.yml` (issue #5) writes a per-run summary including:
- Total wall-clock
- Per-job duration
- Cache hit/miss inferred from log markers

For deeper analysis, the GitHub Actions API exposes per-step timing:

```bash
gh api repos/:owner/:repo/actions/runs/$RUN_ID/timing
gh api repos/:owner/:repo/actions/runs/$RUN_ID/jobs
```

These power any external dashboard work tracked in issue #5.

## Targets

Service-level goals we aim for:

| Metric | Target | Source |
|--------|--------|--------|
| PR wall-clock (warm cache) | < 20 min | Actions UI |
| PR wall-clock (cold cache) | < 50 min | Actions UI |
| Cachix hit rate | > 90% | Cachix dashboard |
| `tarpaulin` runtime | < 1500s | step log |
| Flake rate (false reds) | < 1% of runs | manual triage |

When any of these regresses, file an issue tagged `ci-perf`.

## See Also

- [`docs/CACHE_STRATEGY.md`](../CACHE_STRATEGY.md) — cache key design
- [`docs/binary-cache-strategy.md`](../binary-cache-strategy.md) — Cachix usage
