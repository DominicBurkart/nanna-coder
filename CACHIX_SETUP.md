# Cachix Setup

Binary-cache setup. Contributors only need the read-only path; maintainers configure push.

## Contributor Usage (read-only)

```bash
nix run .#setup-cache  # Configure cache for faster builds
```

## Maintainer Setup (push)

1. Visit [app.cachix.org/cache/nanna-coder](https://app.cachix.org/cache/nanna-coder) for setup instructions.
2. Add GitHub secret `CACHIX_AUTH` with your auth token from the Cachix dashboard.
3. Update `nix/cache.nix` with the public signing key from Cachix.

```bash
export CACHIX_AUTH="<your-token>"
nix run .#push-cache
```

See also [docs/CACHE_STRATEGY.md](docs/CACHE_STRATEGY.md) for the CI strategy and the [Cachix docs](https://docs.cachix.org/) for advanced configuration.
