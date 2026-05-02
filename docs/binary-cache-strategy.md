# Binary Cache Strategy for CI/CD

> For the operational cache setup (keys, workflow wiring, troubleshooting), see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md). This document focuses on the high-level architecture and priorities.

## Overview

This document outlines the binary cache strategy used to optimize CI/CD performance and reduce build times.

## Architecture

### 1. Multi-Tier Caching System

```
┌─────────────────────────────────────────────────────────────┐
│                    Binary Cache Hierarchy                   │
├─────────────────────────────────────────────────────────────┤
│ Tier 1: Cachix Public Cache (nanna-coder.cachix.org)      │
│         - Shared across all CI runners and developers      │
│         - Persistent storage with configurable retention   │
│         - Optimized for frequent access patterns           │
├─────────────────────────────────────────────────────────────┤
│ Tier 2: Magic Nix Cache (GitHub Actions)                  │
│         - Per-job temporary caching                        │
│         - Automatic cache warming and optimization         │
│         - Zero-configuration setup                         │
├─────────────────────────────────────────────────────────────┤
│ Tier 3: Local Development Cache                           │
│         - Developer machine cache                          │
│         - Configurable via setup-cache utility             │
│         - Optional Cachix integration                      │
└─────────────────────────────────────────────────────────────┘
```

### 2. Cache Priority Matrix

| Cache Type        | Priority | Use Case                    | TTL/Retention |
|-------------------|----------|-----------------------------|---------------|
| Rust Dependencies| 100      | Frequent cargo builds       | 30 days       |
| Test Containers   | 90       | Integration testing         | 14 days       |
| Model Cache       | 80       | AI model storage            | 60 days       |
| Build Artifacts   | 60       | Release binaries            | 90 days       |
| Cross Compilation | 50       | Multi-arch builds           | 30 days       |
| Base Images       | 30       | Container foundations       | 90 days       |
| System Packages   | 20       | Nix package dependencies    | 180 days      |

## Implementation

### 1. Flake Configuration

The binary cache system is configured in `flake.nix` with the following components:

#### Cache Configuration
```nix
binaryCacheConfig = {
  cacheName = "nanna-coder";
  pushToCache = true;
  maxCacheSizeGB = 50;
  retentionDays = 30;
  maxJobs = 4;
  buildCores = 0; # Use all available cores
};
```

#### Cache Management Utilities
- `setup-cache`: Configure local development environment
- `push-cache`: Upload builds to binary cache
- `ci-cache-optimize`: Optimize CI cache settings
- `cache-analytics`: Monitor cache performance

### 2. CI/CD Integration

#### GitHub Actions Workflow Enhancement

Each CI job includes:

```yaml
- name: Configure Cachix (Binary Cache)
  uses: cachix/cachix-action@v12
  with:
    name: nanna-coder
    authToken: ${{ secrets.CACHIX_AUTH }}
    pushFilter: "(-source$|nixpkgs\.tar\.gz$)"

- name: Optimize CI cache settings
  run: nix run .#ci-cache-optimize
```

#### Cache Maintenance Job

Dedicated job for cache management:
- Pushes successful builds to cache
- Generates performance analytics
- Reports cache health metrics

### 3. Performance Optimizations

#### Build Parallelization
- Max jobs: 4 concurrent builds
- Core utilization: All available CPU cores
- Intelligent dependency ordering

#### Cache Warming Strategy
- Pre-populate development dependencies
- Prioritize frequently accessed artifacts
- Batch upload of related derivations

#### Artifact Filtering
- Exclude source tarballs from push
- Filter temporary build artifacts
- Optimize for reproducible outputs

## Usage, Monitoring, Security, Troubleshooting

For day-to-day commands (setup, push, analytics), performance targets and metrics, security model, and troubleshooting steps see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md). The content previously duplicated here has been consolidated there to avoid drift.

## Future Enhancements

### Planned Improvements
1. **Multi-Region Caching**: Geographic distribution for global teams
2. **Intelligent Warming**: ML-based cache prediction
3. **Cross-Platform Optimization**: ARM64 and x86_64 unified caching
4. **Integration APIs**: Programmatic cache management
5. **Advanced Analytics**: Cost analysis and optimization recommendations

### Performance Goals
- 95% cache hit rate target
- <1 minute average build time
- Multi-arch container support
- Real-time cache health monitoring

---

For questions or issues with the binary cache system, please refer to the cache-analytics output or contact the development team.