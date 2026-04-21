# Binary Cache Strategy

The project uses [Cachix](https://cachix.org/) (cache name: `nanna-coder`) as its
Nix binary cache. The cache is public-read, authenticated-push.

See [CACHIX_SETUP.md](../CACHIX_SETUP.md) for the short setup recipe; this
document covers CI integration and trade-offs.

## CI Workflow Configuration

All workflows use `cachix/cachix-action@v15`:

```yaml
- uses: cachix/cachix-action@v15
  with:
    name: nanna-coder
    authToken: '${{ secrets.CACHIX_AUTH }}'
    pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"
    skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}
```

- **Public read**: anyone can download from cache.
- **Authenticated push**: only CI with `CACHIX_AUTH` can upload.
- **Fork protection**: fork PRs read but never push.
- **Push filter**: excludes source tarballs and upstream nixpkgs archives.

## Flake Configuration

Binary cache configuration lives in [`flake.nix`](../flake.nix) via `binaryCacheConfig`:

```nix
binaryCacheConfig = {
  cacheName = "nanna-coder";
  publicKey = "nanna-coder.cachix.org-1:<key>";  # from app.cachix.org
  pushToCache = true;
  cacheKeyPriority = {
    "rust-dependencies" = 100;
    "test-containers"   = 90;
    "model-cache"       = 80;
    "build-artifacts"   = 60;
    "cross-compilation" = 50;
    "base-images"       = 30;
    "system-packages"   = 20;
  };
};
```

## Developer Setup

```bash
nix run .#setup-cache     # one-time: add Cachix substituter
nix build .#nanna-coder   # builds now pull from Cachix
nix run .#cache-analytics # inspect local store / cache config
```

## What Gets Cached

| Tier | Examples |
|---|---|
| Always | Rust deps, test containers, release binaries |
| When space allows | Cross-compilation outputs, dev tools |
| Excluded (`pushFilter`) | `*-source` derivations, `nixpkgs.tar.gz` (already on `cache.nixos.org`) |

## Performance Expectations

| Scenario | Cold | Cachix hit |
|---|---|---|
| Rust workspace | 10-15 min | 30-60 s |
| Container images | 5-10 min | 1-2 min |
| Full CI pipeline | 30-45 min | 5-10 min |

Target hit rates: Rust deps >95%, containers >90%, overall CI >85%.

## Security

- `CACHIX_AUTH` secret is set at the repo level; not accessible to fork PRs.
- Cache is marked **public** on Cachix; anyone can download.
- All artifacts are content-hashed by Nix and signed by Cachix.

## Troubleshooting

**Builds take full time, no downloads from Cachix:**

```bash
cat ~/.config/nix/nix.conf | grep substituters   # verify config
nix run .#setup-cache                            # re-apply if missing
```

**CI builds successfully but cache not updated:**

- Is `CACHIX_AUTH` set in repo secrets?
- Is the job running on main (not a fork PR)?
- Grep CI logs for `Pushing to cache`.

**Untrusted public-key error:** fetch the correct key from
[app.cachix.org/cache/nanna-coder](https://app.cachix.org/cache/nanna-coder),
update `flake.nix` (`binaryCacheConfig.publicKey`), then rerun
`nix run .#setup-cache`.

## References

- [Cachix docs](https://docs.cachix.org/)
- [cachix-action](https://github.com/cachix/cachix-action)
- [Nix substituters](https://nixos.org/manual/nix/stable/command-ref/conf-file.html#conf-substituters)
