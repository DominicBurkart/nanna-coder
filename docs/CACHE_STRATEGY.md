# Nanna Coder Cache Strategy

## Overview

This document describes the caching strategy implemented to optimize CI/CD build times. The strategy focuses on maximizing cache reuse across jobs while staying within GitHub Actions' 10GB cache limit.

## Cache Architecture

### Three-Layer Cache System

```
┌─────────────────────────────────────────────────────────┐
│ Layer 1: Shared Dependencies (Pre-built on main)       │
│ - Rust toolchain (1.84.0)                              │
│ - Cargo dependencies (all crates)                      │
│ - Development tools (nextest, clippy, etc.)            │
│ Cache Key: nix-v3-deps-{flake.lock}-{Cargo.lock}      │
└─────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────┐
│ Layer 2: Job-Specific Builds                           │
│ - Test artifacts per matrix job                        │
│ - Platform-specific builds                             │
│ Cache Key: {deps-key}-{OS}-{rust}-{test-type}         │
└─────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────┐
│ Layer 3: Container Images                              │
│ - Harness container                                    │
│ - Ollama container                                     │
│ Cache Key: {deps-key}-container-{image}-{arch}        │
└─────────────────────────────────────────────────────────┘
```

## Workflows

### 1. Main CI Pipeline (`.github/workflows/ci.yml`)

**Job: `prebuild-deps`**
- Runs first, before all matrix jobs
- Builds Rust toolchain and cargo dependencies once
- Caches result with key: `nix-v3-deps-{flake.lock}-{Cargo.lock}`
- Output: `cache-key` used by downstream jobs

**Job: `test-matrix`**
- Depends on `prebuild-deps`
- Restores shared dependency cache
- Adds job-specific artifacts to cache
- Cache key hierarchy:
  1. Exact: `{deps-key}-{OS}-{rust}-{test-type}` (this job's cache)
  2. Fallback: `{deps-key}-{OS}-` (same OS, any test type)
  3. Fallback: `{deps-key}-` (shared deps from prebuild)
  4. Fallback: `nix-v3-deps-` (any version of shared deps)

**Job: `build-containers`**
- Depends on `prebuild-deps` and `test-matrix`
- Leverages shared dependency cache
- Optimized for container layer reuse
- Cache key hierarchy:
  1. Exact: `{deps-key}-container-{image}-{arch}`
  2. Fallback: `{deps-key}-container-{image}-` (same image, any arch)
  3. Fallback: `{deps-key}-container-` (any container)
  4. Fallback: `{deps-key}-` (shared deps)

### 2. Cache Warming (`.github/workflows/cache-warming.yml`)

**Purpose:** Pre-populate caches on `main` branch to accelerate PR builds

**Triggers:**
- Push to `main` branch
- Changes to `flake.lock`, `Cargo.lock`, or `Cargo.toml`
- Manual dispatch (with force rebuild option)

**Jobs:**
1. **`warm-dependencies`**: Builds core dependencies
2. **`warm-containers`**: Builds container images in parallel
3. **`warm-cross-platform`**: (Disabled) Cross-compilation caches

**Benefits:**
- PR builds start with pre-warmed dependency cache
- First PR build after dependency update is fast
- Reduces compute waste from repeated builds

## Cache Key Design

### Key Components

```
nix-v3-deps-{flake-hash}-{cargo-hash}
│       │    │            │
│       │    │            └─ First 16 chars of Cargo.lock SHA256
│       │    └─ First 16 chars of flake.lock SHA256
│       └─ Cache strategy version (increment to invalidate all)
└─ Nix-specific prefix
```

### Cache Key Hierarchy Benefits

1. **Exact Match**: Fastest, uses exact cache for this job
2. **Partial Match**: Reuses most artifacts, rebuilds only changed parts
3. **Shared Deps**: Reuses expensive dependencies, rebuilds project
4. **Fallback**: Better than nothing, still saves some time

## Cache Size Management

### Limits

- **GitHub Actions Cache Limit**: 10GB total per repository
- **Per-Job Allocation**:
  - `prebuild-deps`: 2GB (Rust toolchain + dependencies)
  - `test-matrix` jobs: 1GB each
  - `build-containers`: 2GB per container
  - GC triggers when approaching limits

### What Gets Cached

✅ **High Value (Always Cached)**
- Rust toolchain (1.84.0) - ~1.5GB, rarely changes
- Cargo dependencies - ~500MB-1GB, changes occasionally
- Container base layers - ~800MB, stable

✅ **Medium Value (Conditionally Cached)**
- Test binaries - ~200MB per job, changes with code
- Build artifacts - ~300MB, changes frequently

❌ **Excluded (Never Cached)**
- Source tarballs (`-source` suffix)
- nixpkgs archives (`nixpkgs.tar.gz`)
- Temporary build files
- Git repository data

## Performance Metrics

### Target Metrics

| Metric | Target | How Measured |
|--------|--------|--------------|
| Cache hit rate | >80% | `steps.nix-cache.outputs.cache-hit` |
| PR build time reduction | >30% | Baseline vs optimized comparison |
| Dependency restore time | <5min | Time from cache restore start to completion |
| Storage efficiency | <10GB total | Sum of all active caches |

### Monitoring

**CI Job Summary** includes:
- Cache hit/miss status per job
- Cache restore timing
- Build duration breakdown
- Cache key information

**Cache Analytics Tool** (`nix run .#cache-analytics`):
```bash
# Run locally or in CI
nix run .#cache-analytics

# Shows:
# - Cache hit/miss analysis
# - Nix store size and contents
# - Largest store paths
# - Build dependency breakdown
# - Optimization recommendations
```

## Maintenance

### Cache Invalidation

**Automatic Invalidation:**
- Dependency changes (`flake.lock`, `Cargo.lock`)
- Cache version bump (`nix-v3` → `nix-v4`)
- GitHub Actions automatic eviction (unused for 7 days)

**Manual Invalidation:**
```bash
# Bump cache version in workflows
sed -i 's/nix-v3/nix-v4/g' .github/workflows/*.yml

# Force rebuild via cache warming
gh workflow run cache-warming.yml -f force_rebuild=true
```

### Troubleshooting

**Problem: Cache Miss on Every Build**
```bash
# Check if keys are generating consistently
gh run view --log | grep "Cache Key:"

# Verify flake.lock and Cargo.lock are committed
git status flake.lock Cargo.lock
```

**Problem: Cache Size Limit Exceeded**
```bash
# Check current usage
gh cache list --limit 100

# Manually delete old caches
gh cache delete <cache-key>

# Or bump version to invalidate all
# (see Manual Invalidation above)
```

**Problem: Slow Builds Despite Cache Hits**
```bash
# Run analytics to identify bottlenecks
nix run .#cache-analytics

# Check if derivations are being rebuilt
nix build .#nanna-coder --print-build-logs
```

## Best Practices

### For Contributors

1. **Keep dependencies up to date**
   ```bash
   nix flake update
   cargo update
   # Commit lock files together
   ```

2. **Test locally with Nix**
   ```bash
   # Use Nix develop to match CI environment
   nix develop
   cargo build --workspace
   ```

3. **Monitor cache in PRs**
   - Check CI summary for cache hit rates
   - Look for "CACHE HIT" vs "CACHE MISS" messages
   - Report persistent cache misses as issues

### For Maintainers

1. **Merge dependency updates promptly**
   - Triggerse cache warming on `main`
   - Benefits all subsequent PRs

2. **Monitor cache usage**
   ```bash
   gh cache list --limit 50 | head -20
   ```

3. **Tune cache sizes if needed**
   - Adjust `gc-max-store-size-*` in workflows
   - Balance between cache hit rate and size limits

## Migration Guide

### From Old Cache Strategy (nix-v2)

The new strategy (nix-v3) is backward compatible via fallback keys:

```yaml
restore-prefixes-first-match: |
  nix-v3-deps-
  nix-v2-Linux-  # Falls back to old strategy
```

First build after merge:
- May see CACHE MISS as v3 keys don't exist yet
- `prebuild-deps` populates new cache structure
- Subsequent builds use v3 efficiently

### Rollback Procedure

If issues arise:

1. Revert cache version in workflows:
   ```bash
   git revert <cache-optimization-commit>
   ```

2. Or disable prebuild-deps job:
   ```yaml
   prebuild-deps:
     if: false  # Disable optimization
   ```

3. Jobs fall back to old `nix-v2` cache strategy

## References

- [GitHub Actions Cache Documentation](https://docs.github.com/en/actions/using-workflows/caching-dependencies-to-speed-up-workflows)
- [nix-community/cache-nix-action](https://github.com/nix-community/cache-nix-action)
- [Cachix Documentation](https://docs.cachix.org/)
- [Issue #18: Cache Strategy Evaluation](https://github.com/DominicBurkart/nanna-coder/issues/18)
