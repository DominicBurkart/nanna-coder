# Binary cache configuration and utilities
# This module contains:
# - Cachix binary cache configuration
# - Cache management utilities (setup, push, optimize)
# - CI/CD cache optimization scripts

{ pkgs
, lib
, rustToolchain
}:

let
  # Binary cache strategy for CI/CD optimization
  binaryCacheConfig = {
    # Cachix configuration for public binary cache
    cacheName = "nanna-coder";
    publicKey = "nanna-coder.cachix.org-1:U/8OwBxzrmKhrghm7KtNA3cRnYR5ioKlB637gbc2BF4=";
    pushToCache = true;

    # Cache priorities optimized for CI performance
    cacheKeyPriority = {
      # High priority - frequently changing, cache first
      "rust-dependencies" = 100;
      "test-containers" = 90;
      "model-cache" = 80;

      # Medium priority - moderately changing
      "build-artifacts" = 60;
      "cross-compilation" = 50;

      # Low priority - rarely changing, cache last
      "base-images" = 30;
      "system-packages" = 20;
    };

    # Cache size management for CI runners
    maxCacheSizeGB = 50;
    retentionDays = 30;

    # Parallel build optimization
    maxJobs = 4;
    buildCores = 0; # Use all available cores
  };

  # Binary cache management utilities
  binaryCacheUtils = {
    # Script to configure cachix for the project
    setup-cache = pkgs.writeShellScriptBin "setup-cache" ''
      echo "🔧 Setting up Nanna Coder binary cache..."

      # Install cachix if not available
      if ! command -v cachix &> /dev/null; then
        echo "📦 Installing cachix..."
        nix-env -iA nixpkgs.cachix
      fi

      # Configure nanna-coder cache
      echo "📥 Configuring cache: ${binaryCacheConfig.cacheName}"
      cachix use ${binaryCacheConfig.cacheName}

      # Add to nix configuration
      echo "✏️  Adding to nix.conf..."
      mkdir -p ~/.config/nix
      echo "substituters = https://cache.nixos.org https://${binaryCacheConfig.cacheName}.cachix.org" >> ~/.config/nix/nix.conf
      echo "trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= ${binaryCacheConfig.publicKey}" >> ~/.config/nix/nix.conf

      echo "✅ Binary cache configured successfully!"
      echo "💡 Run 'push-cache' to upload builds to cache"
    '';

    # Script to push builds to binary cache
    push-cache = pkgs.writeShellScriptBin "push-cache" ''
      echo "🚀 Pushing builds to binary cache..."

      if [ -z "$CACHIX_AUTH" ]; then
        echo "❌ CACHIX_AUTH not set. Please configure authentication."
        echo "💡 Run: export CACHIX_AUTH=your_token"
        exit 1
      fi

      echo "📦 Building and pushing core packages..."
      nix build .#nanna-coder --print-build-logs
      cachix push ${binaryCacheConfig.cacheName} $(nix path-info .#nanna-coder)

      echo "🐳 Building and pushing container images..."
      nix build .#harnessImage --print-build-logs
      cachix push ${binaryCacheConfig.cacheName} $(nix path-info .#harnessImage)

      nix build .#ollamaImage --print-build-logs
      cachix push ${binaryCacheConfig.cacheName} $(nix path-info .#ollamaImage)

      echo "🧪 Building and pushing test containers..."
      nix build .#qwen3-container --print-build-logs
      cachix push ${binaryCacheConfig.cacheName} $(nix path-info .#qwen3-container)

      echo "📊 Cache statistics:"
      cachix info ${binaryCacheConfig.cacheName}

      echo "✅ All builds pushed to cache successfully!"
    '';

    # Script to optimize CI cache usage
    ci-cache-optimize = pkgs.writeShellScriptBin "ci-cache-optimize" ''
      echo "⚡ Optimizing CI cache usage..."

      # Set optimal Nix settings for CI
      export NIX_CONFIG="
        max-jobs = ${toString binaryCacheConfig.maxJobs}
        cores = ${toString binaryCacheConfig.buildCores}
        substitute = true
        builders-use-substitutes = true
        experimental-features = nix-command flakes
        keep-outputs = true
        keep-derivations = true
        tarball-ttl = 300
      "

      echo "🔧 Nix configuration optimized:"
      echo "  Max jobs: ${toString binaryCacheConfig.maxJobs}"
      echo "  Build cores: ${toString binaryCacheConfig.buildCores}"
      echo "  Cache TTL: 300s"

      # Pre-populate cache with build dependencies
      echo "📥 Pre-populating build dependencies..."
      nix develop --command echo "Development environment loaded"

      echo "🎯 Building test dependencies..."
      nix build .#qwen3-model --no-link --print-build-logs

      echo "✅ CI cache optimization complete!"
    '';

    # Script to analyze cache hit rates and performance
    cache-analytics = pkgs.writeShellScriptBin "cache-analytics" ''
      echo "📊 Binary Cache Analytics"
      echo "========================"

      echo "🎯 Cache Information:"
      if command -v cachix &> /dev/null; then
        cachix info ${binaryCacheConfig.cacheName} || echo "⚠️  Cache not configured"
      else
        echo "⚠️  Cachix not installed"
      fi

      echo ""
      echo "💾 Local Nix Store Stats:"
      echo "  Store size: $(du -sh /nix/store 2>/dev/null | cut -f1 || echo 'N/A')"
      echo "  Optimization available: $(nix store optimise --dry-run 2>/dev/null || echo 'Command not available in this Nix version')"

      echo ""
      echo "🔍 Build Dependencies Analysis:"
      echo "  Rust toolchain: $(nix path-info ${rustToolchain} 2>/dev/null | wc -l) paths"
      echo "  Container deps: $(nix path-info .#ollamaImage --derivation 2>/dev/null | wc -l) derivations"

      echo ""
      echo "💡 Optimization Recommendations:"
      if [ -f ~/.config/nix/nix.conf ]; then
        if grep -q "${binaryCacheConfig.cacheName}" ~/.config/nix/nix.conf; then
          echo "  ✅ Binary cache configured"
        else
          echo "  ⚠️  Run 'setup-cache' to configure binary cache"
        fi
      else
        echo "  ⚠️  Run 'setup-cache' to configure binary cache"
      fi

      echo "  💡 Consider running 'ci-cache-optimize' for better performance"
    '';
  };

in
{
  inherit binaryCacheConfig binaryCacheUtils;
}
