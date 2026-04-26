# Cachix Migration (Historical)

The project moved to Cachix-only binary caching after evaluating GitHub Actions cache and `cache-nix-action`. For the current strategy and operational details, see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md). For the prior `cache-nix-action` interim, see [cache-migration-guide.md](./cache-migration-guide.md).

## Migration timeline

1. **Magic Nix Cache** — deprecated upstream Feb 2025; removed.
2. **`cache-nix-action`** — used briefly; bound by GitHub's 10 GB per-repo cache and frequent evictions on container builds.
3. **Cachix** — current. Unlimited storage, persistent across runs, shared between CI and developers.

## CI workflow shape

```yaml
- uses: cachix/cachix-action@v15
  with:
    name: nanna-coder
    authToken: '${{ secrets.CACHIX_AUTH }}'
    pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"
    skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}
```

- Public read; authenticated push gated on `CACHIX_AUTH`.
- Fork PRs read but never push (`skipPush`).
- Push filter drops source tarballs and `nixpkgs.tar.gz`.

## Flake configuration

`flake.nix` -> `binaryCacheConfig` holds `cacheName`, `publicKey`, `pushToCache`, and a `cacheKeyPriority` map (rust deps highest, system packages lowest). The same priorities are listed in [binary-cache-strategy.md](./binary-cache-strategy.md).

## Developer setup

```bash
nix run .#setup-cache    # one-time
nix build .#nanna-coder
nix run .#cache-analytics
```

## Trust and access

- Artifacts: content-addressed, signed by Cachix, public-key verified on download.
- Secrets: `CACHIX_AUTH` is repo-scoped and unavailable to fork PRs.

## Migration checklist (completed 2025)

- [x] Cache created at app.cachix.org
- [x] Public signing key added to `flake.nix`
- [x] `CACHIX_AUTH` added as repo secret
- [x] All workflows updated to `cachix/cachix-action@v15`
- [x] `cache-nix-action` references removed
- [x] Developer-side `setup-cache` validated

## Cost reference

| Tier | Storage | Bandwidth | Cost |
|---|---|---|---|
| GitHub Actions cache | 10 GB | unlimited | free |
| Cachix Free | 5 GB | 10 GB/mo | free |
| Cachix Pro | unlimited | unlimited | $29/mo |

## Troubleshooting (migration-specific)

**Untrusted public key**

1. Get the current key from [app.cachix.org/cache/nanna-coder](https://app.cachix.org/cache/nanna-coder).
2. Update `publicKey` in `flake.nix` (search for `nanna-coder.cachix.org`).
3. Re-run `nix run .#setup-cache`.

For day-to-day cache operation issues (auth failures, cache misses), see the troubleshooting section of [CACHE_STRATEGY.md](./CACHE_STRATEGY.md).

## References

- [Cachix docs](https://docs.cachix.org/)
- [cachix-action](https://github.com/cachix/cachix-action)
- [Nix substituters](https://nixos.org/manual/nix/stable/command-ref/conf-file.html#conf-substituters)
- Setup: [../CACHIX_SETUP.md](../CACHIX_SETUP.md)
