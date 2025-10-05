# Cachix Migration Guide

## Overview

This project uses **Cachix exclusively** for binary caching, providing unlimited storage and persistent cache across all CI runs and developer machines.

## Migration History

### Previous Approaches

1. **Magic Nix Cache** (deprecated Feb 2025)
   - Automatic caching by DeterminateSystems
   - Deprecated and removed

2. **cache-nix-action** (replaced)
   - Free GitHub-native caching
   - Limited to 10GB per repository
   - Frequent evictions on large container builds

3. **Current: Cachix-only** (implemented)
   - Unlimited storage
   - Persistent across CI runs
   - Shared between CI and developers
   - No deprecated dependencies

## Architecture

### Cachix Integration Points

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
│                                         │
│  nix run .#setup-cache                 │
│  → Configures Cachix substituters      │
│  → Downloads pre-built artifacts       │
└─────────────────────────────────────────┘
```

## Implementation Details

### CI Workflow Configuration

All workflows now use `cachix/cachix-action@v15`:

```yaml
- name: Configure Cachix
  uses: cachix/cachix-action@v15
  with:
    name: nanna-coder
    authToken: '${{ secrets.CACHIX_AUTH_TOKEN }}'
    pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"
    skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}
```

**Key Features:**
- **Public read access**: Anyone can download from cache
- **Authenticated push**: Only CI with `CACHIX_AUTH_TOKEN` can upload
- **Fork protection**: Forks read but don't push (security)
- **Push filter**: Excludes source tarballs to save bandwidth

### Flake.nix Configuration

Binary cache configuration in `flake.nix`:

```nix
binaryCacheConfig = {
  cacheName = "nanna-coder";
  publicKey = "nanna-coder.cachix.org-1:<REAL_KEY>";  # From app.cachix.org
  pushToCache = true;

  cacheKeyPriority = {
    "rust-dependencies" = 100;    # Cache first
    "test-containers" = 90;
    "model-cache" = 80;
    "build-artifacts" = 60;
    "cross-compilation" = 50;
    "base-images" = 30;
    "system-packages" = 20;       # Cache last
  };
};
```

### Developer Setup

Developers can configure Cachix locally:

```bash
# One-time setup
nix run .#setup-cache

# Builds now use Cachix automatically
nix build .#nanna-coder

# Verify cache is working
nix run .#cache-analytics
```

## Cache Strategy

### What Gets Cached

**High Priority (Always cached):**
- Rust dependencies (cargo artifacts)
- Test containers (small, frequently used)
- Build artifacts (binaries)

**Medium Priority (Cached when space available):**
- Cross-compilation outputs
- Development tools

**Low Priority (Excluded from Cachix):**
- Source tarballs (filtered out)
- Large model files (downloaded on-demand)
- nixpkgs tarballs (already cached upstream)

### Push Filter Rationale

```yaml
pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"
```

**Excludes:**
- `*-source` derivations (save bandwidth)
- `nixpkgs.tar.gz` files (redundant with upstream cache)

**Includes:**
- Compiled binaries
- Container images
- Build dependencies

## Performance Expectations

### Build Times (with Cachix)

| Scenario | Cold Build | Cachix Cache Hit |
|----------|------------|------------------|
| Rust workspace | 10-15 min | 30-60 sec |
| Container images | 5-10 min | 1-2 min |
| Full CI pipeline | 30-45 min | 5-10 min |

### Cache Hit Rates (Target)

- Rust dependencies: >95%
- Container images: >90%
- Overall CI: >85%

## Security

### Authentication

**CI Push Access:**
- Requires `CACHIX_AUTH_TOKEN` secret
- Only configured in repository settings
- Not accessible to fork PRs

**Public Read Access:**
- Anyone can download from cache
- No authentication required
- Cache is marked as "Public" on Cachix

### Fork PR Protection

Fork PRs:
- ✅ Can read from Cachix (faster builds)
- ❌ Cannot push to Cachix (security)
- Configured via `skipPush` parameter

### Content Trust

All cached artifacts:
- ✅ Verified by Nix content hash
- ✅ Signed by Cachix
- ✅ Public key verified on download

## Monitoring

### Cache Analytics

Check cache performance:

```bash
nix run .#cache-analytics
```

**Reports:**
- Cachix cache info
- Local store statistics
- Configuration validation

### CI Integration

Every CI run includes cache analytics in the job summary:

```yaml
- name: Cache analytics and reporting
  run: |
    echo "## 📊 Binary Cache Performance Report" >> $GITHUB_STEP_SUMMARY
    nix run .#cache-analytics >> $GITHUB_STEP_SUMMARY
```

## Troubleshooting

### Cache Not Working

**Symptom:** Builds take full time, no downloads from Cachix

**Diagnosis:**
```bash
# Check substituters configuration
cat ~/.config/nix/nix.conf | grep substituters

# Should include:
# substituters = https://cache.nixos.org https://nanna-coder.cachix.org
```

**Solution:**
```bash
nix run .#setup-cache
```

### CI Not Pushing to Cache

**Symptom:** CI builds successfully but cache not updated

**Check:**
1. Is `CACHIX_AUTH_TOKEN` secret configured?
2. Is job running on main branch (not fork PR)?
3. Check CI logs for "Pushing to cache" messages

**Debug:**
```bash
# In CI workflow, add diagnostic step:
- name: Debug Cachix
  run: |
    echo "Auth token configured: ${{ secrets.CACHIX_AUTH_TOKEN != '' }}"
    echo "Is fork PR: ${{ github.event.pull_request.head.repo.fork }}"
```

### Public Key Mismatch

**Symptom:** Error about untrusted public key

**Solution:**
1. Get correct public key from app.cachix.org
2. Update `flake.nix` line 779
3. Update local config: `nix run .#setup-cache`

## Cost Analysis

### Cachix Free Tier

**Limits:**
- 5 GB storage
- 10 GB/month bandwidth

**Recommended for:**
- Small projects
- Open source projects
- Personal development

### Cachix Pro

**Features:**
- Unlimited storage
- Unlimited bandwidth
- Priority support

**Recommended for:**
- Large projects (>5GB artifacts)
- High traffic projects
- Enterprise use

### Cost Comparison

| Solution | Storage | Bandwidth | Cost |
|----------|---------|-----------|------|
| GitHub Actions Cache | 10GB | Unlimited | Free |
| Cachix Free | 5GB | 10GB/month | Free |
| Cachix Pro | Unlimited | Unlimited | $29/month |

**Optimization Tips:**
- Use `pushFilter` to exclude large artifacts
- Monitor bandwidth with cache-analytics
- Consider hybrid approach for very large projects

## Migration Checklist

- [x] Create Cachix cache at app.cachix.org
- [x] Obtain public signing key
- [x] Update flake.nix with real public key
- [x] Add CACHIX_AUTH_TOKEN to GitHub secrets
- [x] Update all workflows to use cachix-action@v15
- [x] Remove cache-nix-action references
- [x] Test cache in CI
- [x] Verify developer setup works
- [x] Monitor cache hit rates
- [x] Document setup process

## References

- [Cachix Documentation](https://docs.cachix.org/)
- [cachix-action GitHub](https://github.com/cachix/cachix-action)
- [Nix Binary Cache Documentation](https://nixos.org/manual/nix/stable/command-ref/conf-file.html#conf-substituters)
- [CACHIX_SETUP.md](../CACHIX_SETUP.md) - Setup instructions
