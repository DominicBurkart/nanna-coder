# Binary Cache Strategy (High-Level)

> Operational setup (keys, workflow wiring, troubleshooting) lives in [CACHE_STRATEGY.md](./CACHE_STRATEGY.md). Migration history lives in [cachix-migration.md](./cachix-migration.md). This document covers only the cache tiering and priority model.

## Cache tiers

| Tier | Location | Scope |
|------|----------|-------|
| 1 | Cachix (`nanna-coder.cachix.org`) | Shared across CI runners and developers; persistent, unlimited storage |
| 2 | GitHub Actions cache | Per-job temporary cache, used as a fallback layer |
| 3 | Local Nix store | Developer machine; optionally wired to Cachix via `nix run .#setup-cache` |

## Priority matrix

Cache priorities are configured in `flake.nix` under `binaryCacheConfig.cacheKeyPriority`. Higher priority means cached first and evicted last.

| Cache type | Priority | Use case | Retention |
|------------|----------|----------|-----------|
| Rust dependencies | 100 | Frequent cargo builds | 30 days |
| Test containers | 90 | Integration testing | 14 days |
| Model cache | 80 | AI model storage | 60 days |
| Build artifacts | 60 | Release binaries | 90 days |
| Cross-compilation | 50 | Multi-arch builds | 30 days |
| Base images | 30 | Container foundations | 90 days |
| System packages | 20 | Nix package dependencies | 180 days |

## Push filter

Cachix excludes source tarballs and nixpkgs archives via:

```yaml
pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"
```

This saves bandwidth without sacrificing reproducibility — sources are already cached upstream on `cache.nixos.org`.

## Security

- `CACHIX_AUTH` is a repository secret; only main-repo CI can push.
- Fork PRs read from cache but cannot push (`skipPush` is set when `github.event.pull_request.head.repo.fork` is true).
- All cached artifacts are content-hashed by Nix and signed by Cachix.

## Targets

- Cache hit rate: >85% for CI builds.
- Build time reduction: >70% vs. cold builds.
- Upload time: <5 minutes for a full push.
