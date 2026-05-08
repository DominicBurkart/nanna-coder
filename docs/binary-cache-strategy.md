# Binary Cache Strategy (superseded)

> Superseded. The active cache strategy lives in [CACHE_STRATEGY.md](./CACHE_STRATEGY.md); the migration history is in [cachix-migration.md](./cachix-migration.md). This file remains as a redirect because earlier docs link here.

The earlier multi-tier framing (Cachix + Magic Nix Cache + local) no longer reflects reality: Magic Nix Cache was deprecated in February 2025 and the project now uses Cachix exclusively. See [cachix-migration.md](./cachix-migration.md#migration-history) for the timeline.

## Where to look

| Topic | Document |
|---|---|
| CI cache wiring, keys, troubleshooting | [CACHE_STRATEGY.md](./CACHE_STRATEGY.md) |
| Migration history & rationale | [cachix-migration.md](./cachix-migration.md) |
| Maintainer push setup | [../CACHIX_SETUP.md](../CACHIX_SETUP.md) |
| Flake-level cache config | [`nix/cache.nix`](../nix/cache.nix) |
| Cache priorities (current) | `binaryCacheConfig.cacheKeyPriority` in [`nix/cache.nix`](../nix/cache.nix#L21) |
