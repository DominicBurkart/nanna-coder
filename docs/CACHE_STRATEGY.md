# Nanna Coder Cache Strategy

## Overview

This document describes the caching strategy implemented to optimize CI/CD build times. The strategy uses Cachix binary cache for unlimited storage and maximizes cache reuse across jobs.

## Cache Architecture

### Three-Layer Cache System

```
┌─────────────────────────────────────────────────────────┐
│ Layer 1: Shared Dependencies (Pre-built on main)       │
│ - Rust toolchain (1.84.0)                              │
│ - Cargo dependencies (all crates)                      │
│ - Development tools (nextest, clippy, etc.)            │
│ Cache Key: cachix-v1-deps-{flake.lock}-{Cargo.lock}   │
│ Storage: Cachix Binary Cache (Unlimited)               │
└─────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────┐
│ Layer 2: Job-Specific Builds                           │
│ - Test artifacts per matrix job                        │
│ - Platform-specific builds                             │
│ Cache Key: {deps-key}-{OS}-{rust}-{test-type}         │
│ Storage: Cachix Binary Cache (Unlimited)               │
└─────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────┐
│ Layer 3: Container Images                              │
│ - Harness container                                    │
│ - Ollama container                                     │
│ Cache Key: {deps-key}-container-{image}-{arch}        │
│ Storage: Cachix Binary Cache (Unlimited)               │
└─────────────────────────────────────────────────────────┘
```

**Note:** Cachix provides unlimited binary cache storage, eliminating the constraints of GitHub Actions' 10GB cache limit.

## Workflows

### 1. Main CI Pipeline (`.github/workflows/ci.yml`)

**Job: `prebuild-deps`**
- Runs first, before all matrix jobs
- Builds Rust toolchain and cargo dependencies once
- Pushes to Cachix binary cache with key: `cachix-v1-deps-{flake.lock}-{Cargo.lock}`
- Uses `cachix/cachix-action@v15` for authentication and push
- Output: `cache-key` used by downstream jobs

**Job: `test-matrix`**
- Depends on `prebuild-deps`
- Pulls shared dependency cache from Cachix
- Adds job-specific artifacts to Cachix cache
- Uses `cachix/cachix-action@v15` for authentication and push
- Cache restoration is automatic via Cachix

**Job: `build-containers`**
- Depends on `prebuild-deps` and `test-matrix`
- Leverages shared dependency cache from Cachix
- Optimized for container layer reuse
- Pushes container artifacts to Cachix
- Uses `cachix/cachix-action@v15` for authentication and push

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
cachix-v1-deps-{flake-hash}-{cargo-hash}
│         │     │            │
│         │     │            └─ First 16 chars of Cargo.lock SHA256
│         │     └─ First 16 chars of flake.lock SHA256
│         └─ Cache strategy version (increment to invalidate all)
└─ Cachix-specific prefix
```

### Cachix Authentication

All workflows using Cachix require authentication to push to the binary cache:

- **Secret Required**: `CACHIX_AUTH_TOKEN` must be configured in repository secrets
- **Cache Name**: `nanna-coder` (configured in workflows)
- **Action**: `cachix/cachix-action@v15`

Example workflow configuration:
```yaml
- uses: cachix/cachix-action@v15
  with:
    name: nanna-coder
    authToken: '${{ secrets.CACHIX_AUTH_TOKEN }}'
```

### Cachix Dashboard

Monitor cache contents and statistics at:
- **URL**: https://nanna-coder.cachix.org
- **Features**:
  - View all cached store paths
  - Check cache size and usage statistics
  - Browse recent pushes and pulls
  - Monitor cache hit rates
  - Manage cache retention policies

## Cache Size Management

### Storage Capacity

- **Cachix Binary Cache**: Unlimited storage
- **No Size Constraints**: Unlike GitHub Actions cache (10GB limit), Cachix allows unrestricted caching
- **Automatic Management**: Cachix handles cache retention and cleanup automatically
- **No Manual GC Required**: No need to manage garbage collection or size limits

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
| Cache hit rate | >80% | Cachix dashboard analytics |
| PR build time reduction | >30% | Baseline vs optimized comparison |
| Dependency restore time | <2min | Time from cache pull start to completion |
| Storage efficiency | Unlimited | Cachix provides unlimited storage |

### Monitoring

**Cachix Dashboard** (https://nanna-coder.cachix.org):
- Real-time cache statistics
- Push/pull activity timeline
- Storage usage metrics
- Cache hit/miss rates
- Recent build artifacts

**CI Job Summary** includes:
- Cachix push/pull status per job
- Build duration breakdown
- Cache key information
- Link to Cachix dashboard

**Cache Analytics Tool** (`nix run .#cache-analytics`):
```bash
# Run locally or in CI
nix run .#cache-analytics

# Shows:
# - Nix store size and contents
# - Largest store paths
# - Build dependency breakdown
# - Optimization recommendations
```

## Maintenance

### Cache Invalidation

**Automatic Invalidation:**
- Dependency changes (`flake.lock`, `Cargo.lock`) result in new cache keys
- Cache version bump (`cachix-v1` → `cachix-v2`)
- Cachix retains all cache entries indefinitely (no automatic eviction)

**Manual Invalidation:**
```bash
# Bump cache version in workflows
sed -i 's/cachix-v1/cachix-v2/g' .github/workflows/*.yml

# Force rebuild via cache warming
gh workflow run cache-warming.yml -f force_rebuild=true

# Note: Old cache entries remain in Cachix but won't be used
```

### Troubleshooting

**Problem: Cachix Authentication Failed**
```bash
# Verify CACHIX_AUTH_TOKEN secret is configured
gh secret list | grep CACHIX

# Test authentication locally
cachix authtoken <your-token>
cachix use nanna-coder

# Check workflow logs for auth errors
gh run view --log | grep -i "cachix.*auth"
```

**Problem: Cache Not Being Used**
```bash
# Check Cachix dashboard for cache contents
open https://nanna-coder.cachix.org

# Verify cache keys are generating consistently
gh run view --log | grep "cache-key"

# Verify flake.lock and Cargo.lock are committed
git status flake.lock Cargo.lock

# Check if Cachix action is properly configured
grep -A 5 "cachix-action" .github/workflows/*.yml
```

**Problem: Slow Builds Despite Cachix**
```bash
# Run analytics to identify bottlenecks
nix run .#cache-analytics

# Check if derivations are being rebuilt unnecessarily
nix build .#nanna-coder --print-build-logs

# Verify Cachix is being hit in workflow logs
gh run view --log | grep -i "cachix"

# Check Cachix dashboard for recent pulls
open https://nanna-coder.cachix.org
```

**Problem: Unable to Push to Cachix**
```bash
# Verify push permissions for CACHIX_AUTH_TOKEN
# Token must have write access to nanna-coder cache

# Check workflow logs for push errors
gh run view --log | grep -i "cachix.*push\|cachix.*error"

# Verify cache name matches in workflow
grep "name: nanna-coder" .github/workflows/*.yml
```

## Best Practices

### For Contributors

1. **Keep dependencies up to date**
   ```bash
   nix flake update
   cargo update
   # Commit lock files together
   ```

2. **Test locally with Nix and Cachix**
   ```bash
   # Configure Cachix locally (read-only, no auth needed)
   cachix use nanna-coder

   # Use Nix develop to match CI environment
   nix develop
   cargo build --workspace
   ```

3. **Monitor cache in PRs**
   - Check CI logs for Cachix push/pull activity
   - Visit Cachix dashboard to verify artifacts
   - Report authentication or push failures as issues

### For Maintainers

1. **Merge dependency updates promptly**
   - Triggers cache warming on `main`
   - Benefits all subsequent PRs
   - Check Cachix dashboard after merge

2. **Monitor cache usage via Cachix Dashboard**
   ```bash
   # Open dashboard in browser
   open https://nanna-coder.cachix.org

   # View statistics:
   # - Total cache size (unlimited)
   # - Recent push/pull activity
   # - Cache hit rates
   # - Popular store paths
   ```

3. **Manage Cachix authentication**
   - Rotate `CACHIX_AUTH_TOKEN` periodically
   - Verify token has write permissions
   - Update secret if authentication issues arise:
     ```bash
     gh secret set CACHIX_AUTH_TOKEN
     ```

## Migration Guide

### From GitHub Actions Cache to Cachix

The migration from GitHub Actions cache to Cachix provides:
- **Unlimited storage** vs 10GB GitHub Actions limit
- **Faster cache restoration** via CDN distribution
- **Better reliability** with dedicated binary cache infrastructure
- **Enhanced monitoring** via Cachix dashboard

Migration steps:
1. Configure `CACHIX_AUTH_TOKEN` repository secret
2. Update workflows to use `cachix/cachix-action@v15`
3. Change cache key prefix from `nix-v3-deps-` to `cachix-v1-deps-`
4. Remove GitHub Actions cache configuration (e.g., `gc-max-store-size`)

First build after migration:
- Cachix cache will be empty initially
- `prebuild-deps` job populates Cachix with dependencies
- Subsequent builds pull from Cachix automatically
- Old GitHub Actions cache can be safely ignored/deleted

### Rollback Procedure

If issues arise with Cachix:

1. Revert to GitHub Actions cache:
   ```bash
   git revert <cachix-migration-commit>
   ```

2. Or temporarily disable Cachix:
   ```yaml
   # Comment out cachix-action steps in workflows
   # - uses: cachix/cachix-action@v15
   #   with:
   #     name: nanna-coder
   #     authToken: '${{ secrets.CACHIX_AUTH_TOKEN }}'
   ```

3. Monitor rollback impact:
   - Builds will be slower without cache
   - Consider increasing GitHub Actions cache allocation
   - Re-enable cache warming workflow

## References

- [Cachix Documentation](https://docs.cachix.org/)
- [Cachix GitHub Action](https://github.com/cachix/cachix-action)
- [Nanna Coder Cachix Dashboard](https://nanna-coder.cachix.org)
- [GitHub Actions Cache Documentation](https://docs.github.com/en/actions/using-workflows/caching-dependencies-to-speed-up-workflows)
- [Issue #18: Cache Strategy Evaluation](https://github.com/DominicBurkart/nanna-coder/issues/18)
