{
  description = "nanna-coder — AI coding assistant with eval harness";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, nix2container }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config.allowUnfree = true;
        };
        nix2containerPkgs = nix2container.packages.${system};

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "clippy" "rustfmt" ];
        };

        buildInputs = with pkgs; [
          openssl
          libssh2
          zlib
          pkg-config
        ] ++ lib.optionals pkgs.stdenv.isLinux [
          pkgs.libgit2
        ] ++ lib.optionals pkgs.stdenv.isDarwin [
          pkgs.libiconv
          pkgs.darwin.apple_sdk.frameworks.Security
          pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
        ];

        nativeBuildInputs = with pkgs; [
          rustToolchain
          pkg-config
          git
        ];

        # Build the harness binary
        harness = pkgs.rustPlatform.buildRustPackage {
          pname = "harness";
          version = "0.1.0";
          src = ./harness;
          cargoLock.lockFile = ./harness/Cargo.lock;
          inherit buildInputs nativeBuildInputs;
          LIBGIT2_NO_VENDOR = if pkgs.stdenv.isLinux then "1" else "0";
          PKG_CONFIG_PATH = pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" buildInputs;
        };

        # Container images
        containers = import ./nix/containers.nix {
          inherit pkgs nix2containerPkgs harness rustToolchain;
          lib = pkgs.lib;
        };

        # CI optimization script
        ciCacheOptimize = pkgs.writeShellApplication {
          name = "ci-cache-optimize";
          runtimeInputs = with pkgs; [ git nix ];
          text = ''
            echo "Optimizing CI cache settings..."
            # Configure Nix to use binary cache effectively
            mkdir -p ~/.config/nix
            cat >> ~/.config/nix/nix.conf << 'EOF'
            substituters = https://cache.nixos.org https://nanna-coder.cachix.org
            trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= nanna-coder.cachix.org-1:placeholder
            EOF
            echo "Cache optimization complete"
          '';
        };

        # Cache analytics script
        cacheAnalytics = pkgs.writeShellApplication {
          name = "cache-analytics";
          runtimeInputs = with pkgs; [ nix jq ];
          text = ''
            echo "### Nix Binary Cache Analytics"
            echo ""
            echo "**Cache Hit Rate:** Analyzing recent builds..."
            echo ""

            # Try to get cache statistics from Cachix if available
            if command -v cachix &> /dev/null; then
              echo "**Cachix Integration:** Available"
            else
              echo "**Cachix Integration:** Not installed"
            fi

            echo ""
            echo "**Key Derivations:**"
            echo "| Derivation | Cached? |"
            echo "|-----------|--------|"

            for drv in harness ollama; do
              if nix build .#${drv}Image --dry-run 2>&1 | grep -q 'will be built'; then
                echo "| ${drv} | No (will build) |"
              else
                echo "| ${drv} | Yes |"
              fi
            done 2>/dev/null || echo "| Unable to check | N/A |"
          '';
        };

      in
      {
        packages = {
          default = harness;
          nanna-coder = harness;
          inherit harness;

          inherit (containers)
            harnessImage
            ollamaImage
            devContainerImage;

          ci-cache-optimize = ciCacheOptimize;
          cache-analytics = cacheAnalytics;
        };

        # Expose container attributes directly for nix run .#harnessImage.copyToDockerDaemon
        inherit (containers) harnessImage ollamaImage devContainerImage;

        devShells.default = pkgs.mkShell {
          inherit buildInputs nativeBuildInputs;
          buildInputs = buildInputs ++ (with pkgs; [
            cargo-nextest
            cargo-audit
            cargo-deny
            cargo-tarpaulin
            git
            jq
            yq-go
          ]);
          shellHook = ''
            echo "nanna-coder dev environment"
            echo "Rust: $(rustc --version)"
            echo "Cargo: $(cargo --version)"
          '';
          LIBGIT2_NO_VENDOR = if pkgs.stdenv.isLinux then "1" else "0";
          PKG_CONFIG_PATH = pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" buildInputs;
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      }
    );
}
