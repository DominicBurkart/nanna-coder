# Cachix Migration History

The project uses **Cachix exclusively** for binary caching. For current setup,
keys, and troubleshooting see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md) (CI
strategy) and [../CACHIX_SETUP.md](../CACHIX_SETUP.md) (contributor + maintainer
setup).

## Previous approaches

1. **Magic Nix Cache** (deprecated 2025-02) — automatic caching by
   DeterminateSystems; removed upstream.
2. **cache-nix-action** — free GitHub-native caching; replaced because the
   10 GB per-repo limit caused frequent evictions on large container builds.
3. **Cachix-only** (current) — unlimited storage, persistent across CI runs,
   shared between CI and developer machines.

## Migration checklist (completed)

- [x] Create Cachix cache at app.cachix.org/cache/nanna-coder
- [x] Add `CACHIX_AUTH` to GitHub secrets and `nix/cache.nix` public key
- [x] Migrate workflows to `cachix/cachix-action@v15`
- [x] Remove `cache-nix-action` references
- [x] Verify developer setup via `nix run .#setup-cache`

## Fork PR protection

Forks **read** from Cachix (faster builds) but **cannot push**, controlled by
`skipPush: ${{ github.event.pull_request.head.repo.fork }}` on the
`cachix-action` step. See [CACHE_STRATEGY.md](./CACHE_STRATEGY.md) for the full
workflow snippet.
