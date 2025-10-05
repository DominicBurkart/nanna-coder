# Cachix Setup

## Maintainer Setup

1. Visit [app.cachix.org/cache/nanna-coder](https://app.cachix.org/cache/nanna-coder) for complete setup instructions
2. Add GitHub secret `CACHIX_AUTH` with your auth token from Cachix dashboard
3. Update `nix/cache.nix` with the public signing key from Cachix

## Contributor Usage

### Read-Only (Pull from Cache)
```bash
nix run .#setup-cache  # Configure cache for faster builds
```

### Push Access (Maintainers)
```bash
export CACHIX_AUTH="<your-token>"
nix run .#push-cache
```

For detailed documentation, troubleshooting, and advanced configuration, see the [Cachix documentation](https://docs.cachix.org/).
