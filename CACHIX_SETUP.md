# Cachix Setup Instructions

## One-Time Setup (Maintainer)

### 1. Create Cachix Cache

```bash
# Install cachix if not already installed
nix profile install nixpkgs#cachix

# Login to Cachix
cachix authtoken

# Create the cache
cachix create nanna-coder
```

### 2. Obtain Keys

After creating the cache at https://app.cachix.org/cache/nanna-coder:

1. **Public Signing Key**: Found in cache settings
   - Format: `nanna-coder.cachix.org-1:<base64-key>`
   - This will be added to `flake.nix`

2. **Auth Token**: Found in cache settings → Auth tokens
   - Click "Generate token" for CI usage
   - This will be added to GitHub secrets

### 3. Configure GitHub Secret

Repository Settings → Secrets and variables → Actions → New repository secret:

- **Name**: `CACHIX_AUTH_TOKEN`
- **Value**: [auth token from step 2]

### 4. Update flake.nix

Replace the placeholder in `flake.nix` line ~824:

```nix
# Replace:
echo "trusted-public-keys = ... nanna-coder.cachix.org-1:AAAAAAAAAA..."

# With actual key:
echo "trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= nanna-coder.cachix.org-1:<YOUR_ACTUAL_KEY>"
```

## For Contributors

### Option A: Use Public Cache (Read-Only)

```bash
# Configure to pull from Cachix
nix run .#setup-cache
```

This allows you to download pre-built artifacts without authentication.

### Option B: Full Setup (Push Access)

If you're a maintainer with push access:

```bash
# Get auth token from maintainer
export CACHIX_AUTH_TOKEN="<your-token>"

# Configure cache
nix run .#setup-cache

# Your builds will now push to Cachix
nix run .#push-cache
```

## Verification

Test that cache is working:

```bash
# Check cache info
nix run .#cache-analytics

# Try building with cache
nix build .#nanna-coder

# Check if artifacts are being pulled from Cachix
# (should show downloads from nanna-coder.cachix.org)
```

## Troubleshooting

### "Cache not found" error

The cache may not be publicly accessible yet. Check:
1. Cache is set to "Public" in Cachix dashboard
2. Public key is correctly configured in nix.conf
3. Network connectivity to cachix.org

### Builds not using cache

Check substituters configuration:

```bash
cat ~/.config/nix/nix.conf | grep substituters
# Should include: https://nanna-coder.cachix.org
```

### CI not pushing to cache

Verify in GitHub Actions logs:
1. `CACHIX_AUTH_TOKEN` secret is configured
2. Cachix action shows "Pushing to cache" messages
3. No authentication errors in logs
