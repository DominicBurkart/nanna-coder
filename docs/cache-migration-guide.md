# Nix Binary Cache Migration Guide (Historical)

> **Superseded.** The project now uses Cachix — see [cachix-migration.md](./cachix-migration.md) and [CACHE_STRATEGY.md](./CACHE_STRATEGY.md) for the current strategy. This document is retained only for historical context on the `cache-nix-action` interim step.

## Context

Before adopting Cachix, the project briefly used `nix-community/cache-nix-action@v5` (GitHub-native, 10 GB per-repo limit) after `DeterminateSystems/magic-nix-cache-action` was deprecated upstream in February 2025.

The interim configuration used cache keys of the form `nix-{job}-{flake.lock hash}` with restore prefixes and a 1 GB per-entry GC limit.

## Outcome

The 10 GB per-repo GitHub Actions cache limit proved insufficient for the container workload, leading to frequent evictions. The project migrated to Cachix for unlimited storage; see [cachix-migration.md](./cachix-migration.md) for the current setup.
