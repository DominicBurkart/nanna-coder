# Nix Binary Cache Migration Guide

## ✅ Migration Complete: Cachix Deprecated

**Status**: COMPLETED - All workflows migrated to cache-nix-action
**Impact**: Fully free GitHub-native caching, no external dependencies
**Benefits**: No secrets required, works with forks/PRs, 10GB cache per repo

## Migration Status

Your workflows now use:
- ✅ `nix-community/cache-nix-action@v5` - **ACTIVE** (free GitHub-native solution)
- ❌ `DeterminateSystems/magic-nix-cache-action@main` - **REMOVED** (deprecated)
- ❌ `cachix/cachix-action@v12` - **REMOVED** (external dependency eliminated)

## Workflows Updated

All CI workflows have been migrated:
- ✅ `.github/workflows/ci.yml` - Main enterprise CI (30+ parallel jobs)
- ✅ `.github/workflows/enterprise-simplified.yml` - Simplified enterprise CI
- ✅ `.github/workflows/debug-nix.yml` - Debug workflows
- ✅ `.github/workflows/cache-migration-test.yml` - Migration testing

## Cache Configuration

Each workflow now uses optimized cache keys:
- **Primary key**: `nix-{job/context}-{flake.lock hash}`
- **Restore prefixes**: `nix-{job/context}-`
- **Garbage collection**: Enabled before save
- **Storage limits**: 1GB per cache entry
- **Total GitHub cache**: 10GB per repository

## Free GitHub-Native Alternatives

### Option 1: cache-nix-action (Recommended)

**Pros:**
- ✅ Completely free (uses GitHub's 10GB cache limit)
- ✅ No secrets required
- ✅ Works with forks and pull requests
- ✅ Community maintained by nix-community
- ✅ More control over cache behavior

**Cons:**
- ⚠️ Requires GitHub Actions cache API (10GB limit per repo)
- ⚠️ Less automatic than Magic Nix Cache

**Migration:**

Replace this:
```yaml
- name: Setup Nix cache
  uses: DeterminateSystems/magic-nix-cache-action@main
```

With this:
```yaml
- name: Cache Nix store
  uses: nix-community/cache-nix-action@v5
  with:
    primary-key: nix-${{ runner.os }}-${{ hashFiles('**/flake.lock') }}
    restore-prefixes-first-match: nix-${{ runner.os }}-
    gc-before-save: true
    gc-max-store-size-linux: 1073741824  # 1GB
    gc-max-store-size-macos: 1073741824  # 1GB
```

### Option 2: FlakeHub Cache (Paid with Free Tier)

**Pros:**
- ✅ Professional binary cache service
- ✅ Better performance than GitHub cache
- ✅ Works outside CI environments
- ✅ One month free with code `FHC`
- ✅ Free for open source projects (request at support@flakehub.com)

**Migration:**
```yaml
- name: Setup FlakeHub cache
  uses: DeterminateSystems/flakehub-cache-action@v1
```

### Option 3: Keep Cachix (Paid)

**Pros:**
- ✅ Already working in your workflows
- ✅ Professional service with team features
- ✅ Most mature Nix binary cache solution

**Cons:**
- ❌ Requires paid subscription
- ❌ Needs `CACHIX_AUTH` secret configuration

## Migration Strategy

### Phase 1: Test Alternative (In Progress)
- [x] Test `cache-nix-action` with new workflow
- [ ] Monitor performance compared to Magic Nix Cache
- [ ] Verify cache hit rates and build time improvements

### Phase 2: Gradual Migration (Before Feb 1, 2025)
1. Update simplified enterprise workflow first
2. Monitor for any issues or performance regressions
3. Update main enterprise CI workflow
4. Update all other workflows

### Phase 3: Cleanup (After Migration)
1. Remove Magic Nix Cache references
2. Update documentation
3. Configure optimal cache settings

## Implementation Examples

### For Small Projects (Recommended)
Use `cache-nix-action` for completely free caching:

```yaml
steps:
- uses: actions/checkout@v4
- uses: DeterminateSystems/nix-installer-action@main
- uses: nix-community/cache-nix-action@v5
  with:
    primary-key: nix-${{ runner.os }}-${{ hashFiles('**/flake.lock') }}
    restore-prefixes-first-match: nix-${{ runner.os }}-
    gc-before-save: true
```

### For Professional Projects
Consider FlakeHub Cache for better performance:

```yaml
steps:
- uses: actions/checkout@v4
- uses: DeterminateSystems/nix-installer-action@main
- uses: DeterminateSystems/flakehub-cache-action@v1
```

### Hybrid Approach
Combine free and paid solutions:

```yaml
steps:
- uses: actions/checkout@v4
- uses: DeterminateSystems/nix-installer-action@main

# Primary cache: Free GitHub cache
- uses: nix-community/cache-nix-action@v5
  with:
    primary-key: nix-${{ runner.os }}-${{ hashFiles('**/flake.lock') }}

# Fallback cache: Cachix (only when token available)
- uses: cachix/cachix-action@v12
  if: secrets.CACHIX_AUTH != ''
  with:
    name: nanna-coder
    authToken: ${{ secrets.CACHIX_AUTH }}
```

## Performance Expectations

Based on community feedback:

| Solution | Setup Complexity | Performance | Cost |
|----------|------------------|-------------|------|
| cache-nix-action | Low | Good | Free |
| FlakeHub Cache | Low | Excellent | Paid |
| Cachix | Medium | Excellent | Paid |
| Magic Nix Cache | None | Good | Free (until Feb 2025) |

## Next Steps

1. **Immediate**: Test the new `cache-migration-test.yml` workflow
2. **This Week**: Choose primary alternative (recommend cache-nix-action)
3. **Before Jan 15**: Migrate all workflows
4. **Before Feb 1**: Remove all Magic Nix Cache references

## Support

- **cache-nix-action**: [GitHub Issues](https://github.com/nix-community/cache-nix-action/issues)
- **FlakeHub Cache**: support@flakehub.com
- **Cachix**: [Documentation](https://docs.cachix.org/)

## Testing

Run the migration test workflow to compare performance:
```bash
# Workflow: .github/workflows/cache-migration-test.yml
# This tests cache-nix-action vs no-cache baseline
```