# CI Caching Options

## GitHub Actions (`actions/cache`)

- Supported on both GitHub-hosted and self-hosted runners
- Cache is stored in GitHub's cloud storage (10 GB limit per repo)
- Keys can be based on OS, file hashes, or arbitrary parameters
- Works well for dependency and build artifact caching in small open-source projects

## Self-Hosted / Ephemeral Runners

- Cache is still uploaded/downloaded from GitHub's cloud storage, which adds latency
- Local or cluster-local cache backends (e.g., persistent volumes, S3) are **not** natively supported
- Third-party tools exist for local cache storage but require custom setup

## Summary

| Scenario | Feasible? | Notes |
|---|---|---|
| Cache via `actions/cache` on GitHub-hosted runners | Yes | Standard approach; cache stored in GitHub cloud |
| Local runner-level cache (like Cachix) | No (by default) | Requires custom infrastructure or external tools |

For Nix-specific binary caching in CI, this project uses [Cachix](https://cachix.org/). See [CACHIX_SETUP.md](../CACHIX_SETUP.md).

## References

- [actions/cache](https://github.com/actions/cache)
- [GitHub Actions dependency caching docs](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching)
