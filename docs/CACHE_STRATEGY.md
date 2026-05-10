# Nanna Coder Cache Strategy

Operational reference for the Cachix-backed CI/CD cache. For migration history
see [cachix-migration.md](./cachix-migration.md); for the contributor/maintainer
quick path see [../CACHIX_SETUP.md](../CACHIX_SETUP.md).

## Cache architecture

Three cache layers, all stored in the `nanna-coder` Cachix cache (unlimited
storage, no GitHub Actions 10 GB cap):

| Layer | Contents | Cache key |
|-------|----------|-----------|
| 1. Shared dependencies | Rust toolchain (1.84.0), cargo deps, dev tools (nextest, clippy, …) | `cachix-v1-deps-{flake.lock}-{Cargo.lock}` |
| 2. Job-specific builds | Per-matrix-job test artifacts; platform-specific builds | `{deps-key}-{OS}-{rust}-{test-type}` |
| 3. Container images    | Harness, Ollama containers | `{deps-key}-container-{image}-{arch}` |

Cache keys: prefix is `cachix-v1` (bump to invalidate everything); the suffixes
are the first 16 chars of the lockfile SHA256s.

## Workflows

### `.github/workflows/ci.yml`

- **`prebuild-deps`** — runs first; builds toolchain + cargo deps once and
  pushes to Cachix. Outputs `cache-key` for downstream jobs.
- **`test-matrix`** — depends on `prebuild-deps`; pulls shared deps and adds
  job-specific artifacts.
- **`build-containers`** — depends on `prebuild-deps` + `test-matrix`; reuses
  shared deps, optimised for container layer reuse.

All jobs use `cachix/cachix-action@v15`.

### `.github/workflows/cache-warming.yml`

Pre-populates caches on `main` to accelerate PR builds. Triggers: push to
`main`, changes to `flake.lock`/`Cargo.lock`/`Cargo.toml`, manual dispatch
(with optional force rebuild).

Jobs: `warm-dependencies`, `warm-containers` (parallel),
`warm-cross-platform` (currently disabled).

## Cachix configuration

```yaml
- uses: cachix/cachix-action@v15
  with:
    name: nanna-coder
    authToken: '${{ secrets.CACHIX_AUTH }}'
    pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"
    skipPush: ${{ github.event.pull_request.head.repo.fork }}
```

- **Public read**: anyone can pull from cache (no auth required).
- **Authenticated push**: requires `CACHIX_AUTH` repo secret.
- **Fork PRs**: read-only — forks can't push (security).
- **Push filter**: excludes `*-source` derivations and `nixpkgs.tar.gz`.

Dashboard: <https://nanna-coder.cachix.org>.

## What gets cached

| Priority | Items | Notes |
|----------|-------|-------|
| High | Rust toolchain (~1.5 GB), cargo deps (~500 MB–1 GB), container base layers (~800 MB) | Always cached |
| Medium | Test binaries (~200 MB/job), build artifacts (~300 MB) | Conditional |
| Excluded | Source tarballs, `nixpkgs.tar.gz`, temp build files, git data | Push filter |

## Performance targets

| Metric | Target | Source |
|--------|--------|--------|
| Cache hit rate | >80% (overall), >95% (rust deps), >90% (containers) | Cachix dashboard |
| PR build time reduction | >30% vs cold | Cold-vs-warm comparison |
| Dependency restore time | <2 min | CI step timing |

Expected wall-clock with full cache hit: rust workspace 30–60 s (cold:
10–15 min); container images 1–2 min (cold: 5–10 min); full CI pipeline
5–10 min (cold: 30–45 min).

## Cache analytics

Local diagnostic:

```bash
nix run .#cache-analytics
```

Reports nix store size, largest paths, build dependency breakdown,
optimisation hints. CI runs the same command and pipes output to
`$GITHUB_STEP_SUMMARY`.

## Cache invalidation

- **Automatic**: any change to `flake.lock` or `Cargo.lock` produces a new
  cache key. Cachix retains old entries but they're unused.
- **Manual**: bump the version prefix:
  ```bash
  sed -i 's/cachix-v1/cachix-v2/g' .github/workflows/*.yml
  gh workflow run cache-warming.yml -f force_rebuild=true
  ```

## Troubleshooting

**Cachix authentication failed**

```bash
gh secret list | grep CACHIX            # verify secret is configured
cachix authtoken <your-token>           # test auth locally
gh run view --log | grep -i 'cachix.*auth'
```

**Cache not being used**

```bash
gh run view --log | grep 'cache-key'    # verify keys generate consistently
git status flake.lock Cargo.lock        # ensure lockfiles are committed
grep -A 5 'cachix-action' .github/workflows/*.yml
```

**Slow builds despite Cachix**

```bash
nix run .#cache-analytics                       # find bottlenecks
nix build .#nanna-coder --print-build-logs      # see what's being rebuilt
gh run view --log | grep -i cachix              # confirm hits in CI
```

**Public-key mismatch**

1. Get the current key from app.cachix.org/cache/nanna-coder.
2. Update `nix/cache.nix`.
3. Re-run `nix run .#setup-cache` locally.

## Best practices

For contributors:

1. Keep lockfiles up to date together: `nix flake update && cargo update`.
2. Configure local cache once: `cachix use nanna-coder` (or `nix run
   .#setup-cache`).
3. Match CI environment locally: `nix develop`.

For maintainers:

1. Merge dependency updates promptly — they trigger cache warming on `main`
   and benefit subsequent PRs.
2. Monitor cache usage on the Cachix dashboard.
3. Rotate `CACHIX_AUTH` periodically: `gh secret set CACHIX_AUTH`.

## References

- [Cachix docs](https://docs.cachix.org/)
- [Cachix GitHub Action](https://github.com/cachix/cachix-action)
- [`nanna-coder` Cachix dashboard](https://nanna-coder.cachix.org)
- [Issue #18](https://github.com/DominicBurkart/nanna-coder/issues/18) — cache strategy evaluation
