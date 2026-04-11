# Nix Binary Cache Migration Guide

> **Superseded.** This document describes an intermediate migration to `cache-nix-action` that was subsequently replaced. The current cache strategy uses Cachix exclusively. See [`docs/cachix-migration.md`](cachix-migration.md) and [`docs/CACHE_STRATEGY.md`](CACHE_STRATEGY.md) for up-to-date information.

---

## Historical Context

This document records the evaluation of alternatives to `DeterminateSystems/magic-nix-cache-action` (deprecated Feb 2025). The project ultimately adopted Cachix; the analysis below is retained for reference.

## Migration Status (historical)

- ❌ `DeterminateSystems/magic-nix-cache-action@main` — **removed** (deprecated)
- ❌ `cachix/cachix-action@v12` — evaluated then re-adopted at v15 (see current docs)
- ✅ Current: `cachix/cachix-action@v15` — see [`docs/cachix-migration.md`](cachix-migration.md)

## Free GitHub-Native Alternatives (evaluated)

### Option 1: cache-nix-action

**Pros:**
- Free (uses GitHub's 10 GB cache limit)
- No secrets required
- Works with forks and pull requests
- Community-maintained by nix-community

**Cons:**
- 10 GB repo-wide limit causes evictions on large container builds
- Less automatic than Magic Nix Cache

```yaml
- name: Cache Nix store
  uses: nix-community/cache-nix-action@v5
  with:
    primary-key: nix-${{ runner.os }}-${{ hashFiles('**/flake.lock') }}
    restore-prefixes-first-match: nix-${{ runner.os }}-
    gc-before-save: true
    gc-max-store-size-linux: 1073741824  # 1 GB
    gc-max-store-size-macos: 1073741824  # 1 GB
```

### Option 2: FlakeHub Cache

```yaml
- name: Setup FlakeHub cache
  uses: DeterminateSystems/flakehub-cache-action@v1
```

Free for open source (request at support@flakehub.com).

### Option 3: Cachix (selected)

See [`docs/cachix-migration.md`](cachix-migration.md).

## Performance Comparison

| Solution | Setup | Performance | Cost |
|----------|-------|-------------|------|
| cache-nix-action | Low | Good | Free |
| FlakeHub Cache | Low | Excellent | Paid/free OSS |
| Cachix | Medium | Excellent | Paid/free OSS |
| Magic Nix Cache | None | Good | Free (deprecated) |

## Support

- **cache-nix-action**: [GitHub Issues](https://github.com/nix-community/cache-nix-action/issues)
- **FlakeHub Cache**: support@flakehub.com
- **Cachix**: [Documentation](https://docs.cachix.org/)
