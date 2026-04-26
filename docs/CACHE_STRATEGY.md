# Cache Strategy

Operational reference for the Cachix-based CI/CD cache. For high-level priorities and policy, see [binary-cache-strategy.md](./binary-cache-strategy.md). For migration history, see [cache-migration-guide.md](./cache-migration-guide.md).

## Layers

1. **Shared dependencies** — Rust toolchain (1.84.0), cargo dependencies, dev tools. Built once on `main`. Key: `cachix-v1-deps-{flake.lock}-{Cargo.lock}`.
2. **Job-specific builds** — test artifacts and platform-specific builds. Key: `{deps-key}-{OS}-{rust}-{test-type}`.
3. **Container images** — harness, ollama, model containers. Key: `{deps-key}-container-{image}-{arch}`.

All layers are stored in the public `nanna-coder` Cachix cache. Cachix has no fixed size cap — old entries are not auto-evicted.

## Workflows

### `.github/workflows/ci.yml`

- `prebuild-deps` — first job; builds toolchain and cargo deps; pushes to Cachix; emits `cache-key`.
- `test-matrix` — pulls `prebuild-deps` cache; pushes job-specific artifacts.
- `build-containers` — depends on the two above; pushes container artifacts.

### `.github/workflows/cache-warming.yml`

Pre-populates caches on `main`. Triggers: push to `main`, `flake.lock` / `Cargo.lock` / `Cargo.toml` changes, manual dispatch (`force_rebuild`).

Jobs: `warm-dependencies`, `warm-containers`, `warm-cross-platform` (currently disabled).

## Cache Key Format

```
cachix-v1-deps-{flake-hash-16}-{cargo-hash-16}
```

Bump the prefix (`cachix-v1` -> `cachix-v2`) to force a global re-warm.

## Authentication

`cachix/cachix-action@v15` requires the repo secret `CACHIX_AUTH`. Cache name is `nanna-coder`.

```yaml
- uses: cachix/cachix-action@v15
  with:
    name: nanna-coder
    authToken: '${{ secrets.CACHIX_AUTH }}'
```

Dashboard: https://nanna-coder.cachix.org

## What is and isn't cached

Cached: Rust toolchain, cargo deps, container base layers, test/build artifacts.

Excluded: `*-source` derivations, `nixpkgs.tar.gz`, temporary build files, git data.

## Cache Invalidation

Automatic on `flake.lock` / `Cargo.lock` changes (new key). Manual:

```bash
# Bump version
sed -i 's/cachix-v1/cachix-v2/g' .github/workflows/*.yml

# Force re-warm
gh workflow run cache-warming.yml -f force_rebuild=true
```

## Troubleshooting

**Auth failure**

```bash
gh secret list | grep CACHIX
cachix authtoken <token>
cachix use nanna-coder
gh run view --log | grep -i 'cachix.*auth'
```

**Cache not used**

```bash
gh run view --log | grep cache-key
git status flake.lock Cargo.lock
grep -A 5 cachix-action .github/workflows/*.yml
```

**Slow build despite Cachix**

```bash
nix run .#cache-analytics
nix build .#nanna-coder --print-build-logs
gh run view --log | grep -i cachix
```

## Local Use

```bash
# read-only, no auth
nix run .#setup-cache
nix develop
cargo build --workspace
```

For maintainer push setup, see [../CACHIX_SETUP.md](../CACHIX_SETUP.md).

## Performance Targets

| Metric | Target |
|---|---|
| Cache hit rate | >80% |
| PR build-time reduction | >30% |
| Dependency restore | <2 min |

## References

- [Cachix docs](https://docs.cachix.org/)
- [cachix-action](https://github.com/cachix/cachix-action)
- [Cache dashboard](https://nanna-coder.cachix.org)
- Tracking: [issue #18](https://github.com/DominicBurkart/nanna-coder/issues/18)
