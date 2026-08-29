# Nix Binary Cache Migration Guide (historical)

> **Superseded.** The project now uses Cachix exclusively — see [cachix-migration.md](./cachix-migration.md) and [CACHE_STRATEGY.md](./CACHE_STRATEGY.md) for the current strategy. This document is retained only for historical context on the `cache-nix-action` interim step.

## Original status (pre-Cachix adoption)

Workflows briefly used:

- `nix-community/cache-nix-action@v5` — interim, GitHub-native (10 GB per-repo limit).
- `DeterminateSystems/magic-nix-cache-action@main` — removed (upstream deprecation, Feb 2025).
- `cachix/cachix-action` — adopted later as the sole solution (currently `@v15`, see [cachix-migration.md](./cachix-migration.md)).

## Workflows updated (interim)

All CI workflows were migrated through the `cache-nix-action` step before settling on Cachix:

- `.github/workflows/ci.yml`
- `.github/workflows/enterprise-simplified.yml`
- `.github/workflows/debug-nix.yml`
- `.github/workflows/cache-migration-test.yml`

Cache keys at the time used `nix-{job}-{flake.lock hash}` with restore prefixes, GC before save, and 1 GB per-entry / 10 GB total limits.

## Alternatives evaluated

| Solution | Setup | Performance | Cost |
|---|---|---|---|
| `cache-nix-action` | Low | Good | Free (10 GB GHA cache) |
| FlakeHub Cache | Low | Excellent | Paid (free for OSS on request) |
| Cachix | Medium | Excellent | Paid (free tier 5 GB) |
| Magic Nix Cache | None | Good | Free (deprecated Feb 2025) |

The hybrid approach (free cache + Cachix fallback) was evaluated but **not adopted**; see [cachix-migration.md](./cachix-migration.md) for the chosen single-cache setup. Note the snippet `if: secrets.CACHIX_AUTH != ''` shown in old drafts is not valid GHA expression syntax (correct form: `if: ${{ secrets.CACHIX_AUTH != '' }}`).

## Migration phases (historical)

1. Tested `cache-nix-action` against a no-cache baseline; monitored hit rates.
2. Migrated simplified workflow, then main CI, then remaining workflows.
3. Removed Magic Nix Cache references; updated docs.

The project subsequently moved to Cachix-only.

## Support

- `cache-nix-action`: <https://github.com/nix-community/cache-nix-action/issues>
- FlakeHub Cache: support@flakehub.com
- Cachix: <https://docs.cachix.org/>
