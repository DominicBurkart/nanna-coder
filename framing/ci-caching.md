# CI Caching Notes

GitHub Actions provides caching via `actions/cache` (10 GB per repo, stored in GitHub's cloud).

Key constraints:
- Works on both GitHub-hosted and self-hosted runners, but self-hosted runners still upload/download from GitHub's cloud storage, which adds latency.
- Local runner-level caching (like Cachix provides for Nix) is not natively supported; it requires external tooling.
- For Nix-heavy workloads with large container images the 10 GB cap causes frequent evictions.

**Decision for this project**: Use Cachix for unlimited, persistent binary cache. See [CACHIX_SETUP.md](../CACHIX_SETUP.md) and [docs/CACHE_STRATEGY.md](../docs/CACHE_STRATEGY.md).
