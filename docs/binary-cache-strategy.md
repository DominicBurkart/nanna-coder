# Binary Cache Strategy for CI/CD

> **Superseded.** This document predates the Cachix-only migration. For the current cache architecture see [`docs/CACHE_STRATEGY.md`](CACHE_STRATEGY.md). The historical analysis below is retained for reference.

---

## Overview

This document outlines the original binary cache strategy evaluated for the Nanna Coder project. The project now uses Cachix exclusively (Tier 1 below); Tiers 2 and 3 are no longer active.

## Architecture (historical)

### Cache Tier Descriptions

**Tier 1: Cachix Public Cache** (`nanna-coder.cachix.org`) — **current**
- Shared across all CI runners and developers
- Persistent storage, unlimited capacity
- Optimized for frequent access patterns

**Tier 2: Magic Nix Cache** — **removed** (deprecated Feb 2025)
- Per-job temporary caching via GitHub Actions
- Automatic cache warming and optimization

**Tier 3: Local Development Cache** — **partially active**
- Developer machine cache
- Configurable via `nix run .#setup-cache`

## Cache Priority Matrix

| Cache Type | Priority | TTL/Retention |
|------------|----------|---------------|
| Rust Dependencies | 100 | 30 days |
| Test Containers | 90 | 14 days |
| Model Cache | 80 | 60 days |
| Build Artifacts | 60 | 90 days |
| Cross Compilation | 50 | 30 days |
| Base Images | 30 | 90 days |
| System Packages | 20 | 180 days |

## Cache Management Utilities

- `nix run .#setup-cache` — configure local development environment
- `nix run .#push-cache` — upload builds to binary cache (requires `CACHIX_AUTH_TOKEN`)
- `nix run .#ci-cache-optimize` — optimize CI cache settings
- `nix run .#cache-analytics` — monitor cache performance

## Performance Targets

| Metric | Target |
|--------|--------|
| Cache hit rate | >85% for CI builds |
| Build time reduction | >70% vs. cold builds |
| Upload time | <5 minutes for full push |

## Security Considerations

- `CACHIX_AUTH_TOKEN` stored as GitHub secret; write access restricted to CI
- All cached artifacts cryptographically verified by Nix content hash
- No sensitive data cached; model caches use content-addressed storage

## References

- [`docs/CACHE_STRATEGY.md`](CACHE_STRATEGY.md) — current strategy
- [`docs/cachix-migration.md`](cachix-migration.md) — migration history
- [`CACHIX_SETUP.md`](../CACHIX_SETUP.md) — maintainer setup
