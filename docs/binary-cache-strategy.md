# Binary Cache Strategy for CI/CD

> **Canonical reference:** [CACHE_STRATEGY.md](./CACHE_STRATEGY.md) covers the operational setup (keys, workflow wiring, troubleshooting). This document only records the strategic priorities and any policy that does not belong in the operational doc.

## Cache Priority Matrix

Used to decide what to push first when CI runners are bandwidth-constrained.

| Cache Type        | Priority | Use Case                    | Retention |
|-------------------|----------|-----------------------------|-----------|
| Rust dependencies | 100      | Frequent cargo builds       | 30 days   |
| Test containers   | 90       | Integration testing         | 14 days   |
| Model cache       | 80       | AI model storage            | 60 days   |
| Build artifacts   | 60       | Release binaries            | 90 days   |
| Cross-compilation | 50       | Multi-arch builds           | 30 days   |
| Base images       | 30       | Container foundations       | 90 days   |
| System packages   | 20       | Nix package dependencies    | 180 days  |

The same priorities are encoded in `flake.nix` under `binaryCacheConfig.cacheKeyPriority`.

## Performance Targets

- Cache hit rate: >85% on CI builds.
- Cold-vs-warm build-time reduction: >70%.
- Push filter excludes `*-source` derivations and `nixpkgs.tar.gz` to keep the cache focused on compiled artifacts.

## Security & Trust

- `CACHIX_AUTH` is stored as a GitHub Actions secret; only repo CI can push.
- Public read access is allowed (open-source project).
- All artifacts are content-addressed and signed by Cachix; Nix verifies the public key on download.
- Fork PRs read but never push (`skipPush` parameter on `cachix/cachix-action`).

## Future Enhancements

Tracked, not committed to:

1. Multi-region caching for geographically distributed contributors.
2. Predictive cache warming (build-graph-aware prefetch).
3. Unified ARM64 + x86_64 layer reuse for container images.
4. Cost / bandwidth reporting tied to the cache-analytics utility.
