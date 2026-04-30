# Binary Cache Strategy for CI/CD

> **Scope:** high-level architecture and cache-priority framing only.
> For the operational setup (workflow wiring, cache keys, developer
> commands, troubleshooting) see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md)
> and [cachix-migration.md](./cachix-migration.md). Top-level setup
> instructions live in [../CACHIX_SETUP.md](../CACHIX_SETUP.md).

## Architecture

The project uses a single shared binary cache (Cachix, `nanna-coder.cachix.org`)
that is read by both CI runners and developer machines. Local Nix stores act
as a per-machine cache layer in front of it; there is no GitHub-Actions-cache
tier (the previous `cache-nix-action` and Magic Nix Cache layers were removed
when the project moved to Cachix-only — see [cachix-migration.md](./cachix-migration.md)).

```
Cachix (nanna-coder.cachix.org)
  - shared by CI runners and developer machines
  - public read, authenticated push (CI on main, non-fork PRs)
  ↑↓
Local Nix store (per machine)
  - populated via `nix run .#setup-cache`
```

## Cache Priority Matrix

Pushes are prioritised so the artifacts most expensive to rebuild and most
broadly reused land in the cache first. Priorities are encoded in
`flake.nix` (`binaryCacheConfig.cacheKeyPriority`); retention is managed
by Cachix and is informational here.

| Cache type        | Priority | Use case                  | Suggested retention |
|-------------------|----------|---------------------------|---------------------|
| Rust dependencies | 100      | Frequent cargo builds     | 30 days             |
| Test containers   | 90       | Integration testing       | 14 days             |
| Model cache       | 80       | AI model storage          | 60 days             |
| Build artifacts   | 60       | Release binaries          | 90 days             |
| Cross-compilation | 50       | Multi-arch builds         | 30 days             |
| Base images       | 30       | Container foundations     | 90 days             |
| System packages   | 20       | Nix package dependencies  | 180 days            |

## Push Policy

- **Push filter** — exclude `*-source` derivations and `nixpkgs.tar.gz`
  (already cached upstream). Configured on the `cachix-action` step in CI
  and documented in [cachix-migration.md](./cachix-migration.md).
- **Fork PRs** — pull only; pushes are skipped to avoid leaking auth.
- **Main / non-fork PRs** — push enabled, gated on `CACHIX_AUTH`.

## Targets

| Metric                         | Target                                  |
|--------------------------------|-----------------------------------------|
| CI cache hit rate              | >85%                                    |
| Build time vs. cold build      | >70% reduction                          |
| Cachix push time (full build)  | <5 min                                  |

Concrete per-job times and how to measure them live in
[CACHE_STRATEGY.md](./CACHE_STRATEGY.md#performance-metrics).

## Security

- `CACHIX_AUTH` is a GitHub Actions secret; only main-branch and
  non-fork-PR jobs receive it.
- All artifacts are content-addressed and signed by Cachix; the public
  signing key is pinned in `flake.nix` (`binaryCacheConfig.publicKey`).
- No model weights or other potentially sensitive payloads are pushed —
  large model files are downloaded on demand at runtime.
