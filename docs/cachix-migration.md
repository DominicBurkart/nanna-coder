# Cachix Migration Guide

## Overview

This project uses **Cachix exclusively** for binary caching, providing unlimited storage and persistent cache across all CI runs and developer machines. See [`docs/CACHE_STRATEGY.md`](CACHE_STRATEGY.md) for the current cache architecture.

## Migration History

1. **Magic Nix Cache** (removed Feb 2025) — automatic caching by DeterminateSystems; deprecated and removed.
2. **cache-nix-action** (evaluated, not adopted) — free GitHub-native; 10 GB limit caused frequent evictions on large container builds.
3. **Cachix-only** (current) — unlimited storage, persistent, shared between CI and developers.

## Architecture

```
┌─────────────────────────────────────────┐
│         GitHub Actions CI               │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │  cachix-action@v15              │   │
│  │  - Pull: Always (public cache)  │   │
│  │  - Push: Main branch + PRs      │   │
│  │  - Skip: Fork PRs               │   │
│  └─────────────────────────────────┘   │
│              ↓↑                         │
└──────────────┼─────────────────────────┘
               │
               ↓↑
    ┌──────────────────────┐
    │  nanna-coder.cachix  │
    │  Binary Cache        │
    │  - Unlimited storage │
    │  - Public read       │
    │  - Authenticated push│
    └──────────────────────┘
               ↓↑
┌──────────────┼─────────────────────────┐
│    Developer Workstations              │
│                                        │
│  nix run .#setup-cache                 │
│  → Configures Cachix substituters      │
│  → Downloads pre-built artifacts       │
└─────────────────────────────────────────┘
```

## CI Workflow Configuration

All workflows use `cachix/cachix-action@v15`:

```yaml
- name: Configure Cachix
  uses: cachix/cachix-action@v15
  with:
    name: nanna-coder
    authToken: '${{ secrets.CACHIX_AUTH_TOKEN }}'
    pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"
    skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}
```

- **Public read**: anyone can download from cache
- **Authenticated push**: only CI with `CACHIX_AUTH_TOKEN` secret can upload
- **Fork protection**: forks read but don't push
- **Push filter**: excludes source tarballs to save bandwidth

## Developer Setup

```bash
nix run .#setup-cache   # one-time setup
nix build .#nanna-coder # builds now use Cachix automatically
nix run .#cache-analytics
```

## Cache Strategy

### What Gets Cached

| Priority | Artifact |
|----------|----------|
| High | Rust dependencies, test containers, build artifacts |
| Medium | Cross-compilation outputs, development tools |
| Excluded | Source tarballs (`*-source`), nixpkgs tarballs |

### Build Time Expectations

| Scenario | Cold Build | Cache Hit |
|----------|------------|-----------|
| Rust workspace | 10–15 min | 30–60 sec |
| Container images | 5–10 min | 1–2 min |
| Full CI pipeline | 30–45 min | 5–10 min |

## Security

- **CI push**: requires `CACHIX_AUTH_TOKEN` secret; not accessible to fork PRs
- **Public read**: no authentication required
- **Content trust**: all artifacts verified by Nix content hash and signed by Cachix

## Monitoring

```bash
nix run .#cache-analytics
```

Every CI run appends cache analytics to the GitHub Step Summary.

## Troubleshooting

### Cache not working

```bash
# Check substituters
cat ~/.config/nix/nix.conf | grep substituters
# Should include: https://nanna-coder.cachix.org

nix run .#setup-cache  # reconfigure
```

### CI not pushing to cache

1. Is `CACHIX_AUTH_TOKEN` secret configured?
2. Is the job running on a non-fork branch?
3. Check CI logs for "Pushing to cache" messages.

### Public key mismatch

Get the correct key from [app.cachix.org](https://app.cachix.org), update `flake.nix`, then run `nix run .#setup-cache`.

## References

- [Cachix Documentation](https://docs.cachix.org/)
- [cachix-action GitHub](https://github.com/cachix/cachix-action)
- [`CACHIX_SETUP.md`](../CACHIX_SETUP.md) — maintainer push-access setup
- [`docs/CACHE_STRATEGY.md`](CACHE_STRATEGY.md) — CI cache key design and workflows
