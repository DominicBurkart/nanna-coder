# Cache Migration History

This document records past caching approaches that have been superseded.

## Current Approach

The project uses **Cachix exclusively** for binary caching. See [CACHE_STRATEGY.md](CACHE_STRATEGY.md) for current configuration and [../CACHIX_SETUP.md](../CACHIX_SETUP.md) for setup instructions.

## Migration History

| Period | Approach | Reason Replaced |
|--------|----------|-----------------|
| Early | `DeterminateSystems/magic-nix-cache-action` | Deprecated by DeterminateSystems (Feb 2025) |
| Interim | `nix-community/cache-nix-action@v5` | 10 GB GitHub Actions cap caused evictions on large container builds |
| Current | `cachix/cachix-action@v15` | Unlimited storage, persistent cross-CI cache, public read access |

## Alternative Approaches (Not Used)

These were evaluated but not adopted:

- **FlakeHub Cache** – good performance, paid tier required for sustained use.
- **Hybrid (GitHub cache + Cachix fallback)** – added complexity without clear benefit once Cachix was available.

## Rollback Procedure

If Cachix becomes unavailable:

```bash
# Option A: revert the cachix migration commit
git revert <cachix-migration-commit>

# Option B: temporarily comment out cachix-action steps in workflows
# and substitute nix-community/cache-nix-action@v5 with a 1 GB gc limit
```
