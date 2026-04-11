# Nanna Coder Cache Strategy

## Overview

This document describes the CI/CD caching strategy. The project uses Cachix binary cache for unlimited storage and maximum cache reuse across jobs. For migration history and setup details see [`docs/cachix-migration.md`](cachix-migration.md) and [`CACHIX_SETUP.md`](../CACHIX_SETUP.md).

## Cache Architecture

### Three-Layer Cache System

```
┌─────────────────────────────────────────────────────────┐
│ Layer 1: Shared Dependencies (Pre-built on main)       │
│ - Rust toolchain                                       │
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

## Workflows

### Main CI Pipeline (`.github/workflows/ci.yml`)

**Job: `prebuild-deps`**
- Runs first, before all matrix jobs
- Builds Rust toolchain and cargo dependencies once
- Pushes to Cachix with key: `cachix-v1-deps-{flake.lock}-{Cargo.lock}`
- Output: `cache-key` used by downstream jobs

**Job: `test-matrix`**
- Depends on `prebuild-deps`
- Pulls shared dependency cache from Cachix
- Adds job-specific artifacts to Cachix cache

**Job: `build-containers`**
- Depends on `prebuild-deps` and `test-matrix`
- Leverages shared dependency cache
- Pushes container artifacts to Cachix

All jobs use `cachix/cachix-action@v15`:
```yaml
- uses: cachix/cachix-action@v15
  with:
    name: nanna-coder
    authToken: '${{ secrets.CACHIX_AUTH_TOKEN }}'
```

### Cache Warming (`.github/workflows/cache-warming.yml`)

Pre-populates caches on `main` to accelerate PR builds. Triggers on push to `main`, changes to `flake.lock`/`Cargo.lock`/`Cargo.toml`, or manual dispatch.

## Cache Key Design

```
cachix-v1-deps-{flake-hash}-{cargo-hash}
│         │     │            │
│         │     │            └─ First 16 chars of Cargo.lock SHA256
│         │     └─ First 16 chars of flake.lock SHA256
│         └─ Cache strategy version (increment to invalidate all)
└─ Cachix-specific prefix
```

## Cache Size Management

- **Storage**: Unlimited (Cachix manages retention automatically)
- **No manual GC required**

### What Gets Cached

| Priority | Artifact | Approx. Size |
|----------|----------|--------------|
| High | Rust toolchain | ~1.5 GB |
| High | Cargo dependencies | ~500 MB–1 GB |
| High | Container base layers | ~800 MB |
| Medium | Test binaries | ~200 MB/job |
| Medium | Build artifacts | ~300 MB |
| Excluded | Source tarballs (`-source`) | — |
| Excluded | nixpkgs archives | — |

## Performance Metrics

| Metric | Target |
|--------|--------|
| Cache hit rate | >80% |
| PR build time reduction | >30% |
| Dependency restore time | <2 min |

Monitor at [nanna-coder.cachix.org](https://nanna-coder.cachix.org).

## Maintenance

### Cache Invalidation

Dependency changes (`flake.lock`, `Cargo.lock`) automatically produce new cache keys. To force full invalidation:

```bash
# Bump cache version in all workflows
sed -i 's/cachix-v1/cachix-v2/g' .github/workflows/*.yml

# Force rebuild via cache warming
gh workflow run cache-warming.yml -f force_rebuild=true
```

## Troubleshooting

### Cachix authentication failed
```bash
gh secret list | grep CACHIX_AUTH_TOKEN
cachix authtoken <your-token>
cachix use nanna-coder
```

### Cache not being used
```bash
# Check dashboard
open https://nanna-coder.cachix.org

# Verify lock files are committed
git status flake.lock Cargo.lock
```

### Slow builds despite Cachix
```bash
nix run .#cache-analytics
nix build .#nanna-coder --print-build-logs
```

### Unable to push to Cachix
Verify `CACHIX_AUTH_TOKEN` has write access to the `nanna-coder` cache, then check workflow logs for push errors.

## References

- [Cachix Documentation](https://docs.cachix.org/)
- [Cachix GitHub Action](https://github.com/cachix/cachix-action)
- [Nanna Coder Cachix Dashboard](https://nanna-coder.cachix.org)
- [`docs/cachix-migration.md`](cachix-migration.md) - Migration history and architecture
- [`CACHIX_SETUP.md`](../CACHIX_SETUP.md) - Maintainer setup instructions
