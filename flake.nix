{
  description = "Nanna Coder - AI-powered coding assistant with containerized Rust services";

  inputs = {
    # Pin to specific commit for reproducibility
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # For reproducible container builds
    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Binary cache management
    cachix = {
      url = "github:cachix/cachix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane, nix2container, cachix }:
    # Support multiple systems for cross-platform CI
    nixpkgs.lib.recursiveUpdate (flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ] (system:
      let
        # Overlays
        overlays = [
          (import rust-overlay)
        ];
        pkgs = import nixpkgs {
          inherit system overlays;
          config = {
            # Allow unfree packages if needed (e.g., for some development tools)
            allowUnfree = false;
            # Ensure reproducible builds
            allowBroken = false;
          };
        };

        # Pin specific Rust version for reproducibility (supports edition 2024)
        rustToolchain = pkgs.rust-bin.stable."1.87.0".default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" "rust-analyzer" ];
        };

        # Crane library for building Rust packages
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Filter source files (exclude target, .git, etc.)
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (pkgs.lib.hasSuffix "\.rs" path) ||
            (pkgs.lib.hasSuffix "\.toml" path) ||
            (pkgs.lib.hasSuffix "\.lock" path) ||
            (type == "directory");
        };

        # Reproducible container images using nix2container
        nix2containerPkgs = nix2container.packages.${system};

        # Import modular components
        packages = import ./nix/packages.nix {
          inherit pkgs craneLib src;
          lib = pkgs.lib;
        };

        configs = import ./nix/configs.nix {
          inherit pkgs;
        };

        containers = import ./nix/containers.nix {
          inherit pkgs nix2containerPkgs rustToolchain;
          lib = pkgs.lib;
          harness = packages.harness;
        };

        cache = import ./nix/cache.nix {
          inherit pkgs rustToolchain;
          lib = pkgs.lib;
        };

        scripts = import ./nix/scripts.nix {
          inherit pkgs rustToolchain;
          lib = pkgs.lib;
          podConfig = configs.podConfig;
          modelRegistry = containers.modelRegistry;
          binaryCacheConfig = cache.binaryCacheConfig;
          cacheConfig = configs.cacheConfig;
        };

        devShell = import ./nix/dev-shell.nix {
          inherit pkgs rustToolchain self nixpkgs;
          lib = pkgs.lib;
        };

        apps = import ./nix/apps.nix {
          inherit flake-utils;
          harness = packages.harness;
          binaryCacheUtils = cache.binaryCacheUtils;
          devUtils = scripts.devUtils;
          cacheUtils = scripts.cacheUtils;
          vllmImage = containers.vllmImage { };
          vllmImageMimo = containers.vllmImage { model = "XiaomiMiMo/MiMo-V2-Flash"; };
          vllmImageQwen = containers.vllmImage { model = "Qwen/Qwen3-Coder-30B-A3B-Instruct"; };
        };

      in
      {
        packages = {
          default = packages.nanna-coder;
          inherit (packages) nanna-coder harness;

          # Container images (production)
          inherit (containers) harnessImage ollamaImage devContainerImage;
          
          # vLLM containers with different models
          vllmImage = containers.vllmImage { };  # Default: MiMo-V2-Flash
          vllmImageMimo = containers.vllmImage { model = "XiaomiMiMo/MiMo-V2-Flash"; };
          vllmImageQwen = containers.vllmImage { model = "Qwen/Qwen3-Coder-30B-A3B-Instruct"; };

          # Multi-model cache system (Ollama - legacy)
          inherit (containers.models) qwen3-model gemma-model;
          inherit (containers.strictModels) qwen3-model-strict gemma-model-strict;

          # Multi-model containers (Ollama - legacy)
          inherit (containers.containers) qwen3-container gemma-container;

          # Cache management utilities
          inherit (scripts.cacheUtils) cache-info cache-cleanup;

          # Binary cache utilities
          inherit (cache.binaryCacheUtils) setup-cache push-cache ci-cache-optimize cache-analytics;

          # Development utilities
          inherit (scripts.devUtils) dev-build dev-test dev-check dev-clean dev-reset
                                      container-dev container-test container-stop container-logs cache-warm;

          # Configuration files
          inherit (configs) podConfig composeConfig;

          # Build scripts
          inherit (scripts.buildScripts) build-all load-images start-pod stop-pod;
        };

        devShells.default = devShell;

        # Apps for easy execution
        inherit apps;

        # Checks for CI/CD
        checks = {
          # Workspace-wide checks
          workspace-test = craneLib.cargoTest {
            inherit src;
            cargoArtifacts = packages.cargoArtifacts;
            buildInputs = packages.commonBuildInputs;
            nativeBuildInputs = packages.commonNativeBuildInputs;
            cargoTestCommand = "cargo test --workspace";
          };

          workspace-clippy = craneLib.cargoClippy {
            inherit src;
            cargoArtifacts = packages.cargoArtifacts;
            buildInputs = packages.commonBuildInputs;
            nativeBuildInputs = packages.commonNativeBuildInputs;
            cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
          };

          workspace-fmt = craneLib.cargoFmt {
            inherit src;
          };

          # workspace-audit omitted: craneLib.cargoAudit requires a mandatory
      # advisory-db parameter; use `cargo audit` in CI shell steps instead.

          workspace-deny = pkgs.runCommand "cargo-deny-check" {
            buildInputs = [ pkgs.cargo-deny rustToolchain ];
          } ''
            cd ${src}
            export CARGO_HOME=$(mktemp -d)
            cargo deny check
            touch $out
          '';

          workspace-coverage = pkgs.runCommand "cargo-tarpaulin-coverage" {
            buildInputs = [ pkgs.cargo-tarpaulin rustToolchain ] ++ packages.commonBuildInputs;
            nativeBuildInputs = packages.commonNativeBuildInputs;
          } ''
            cd ${src}
            export CARGO_HOME=$(mktemp -d)

            # Run coverage and extract percentage
            COVERAGE=$(cargo tarpaulin --skip-clean --ignore-tests --output-format text 2>/dev/null | \
                      grep -oP '\d+\.\d+(?=% coverage)' || echo "0.0")

            # Minimum coverage threshold (can be adjusted)
            MIN_COVERAGE="70.0"

            # Compare coverage using awk since bc might not be available
            if awk "BEGIN { exit !($COVERAGE >= $MIN_COVERAGE) }"; then
              echo "✅ Coverage: $COVERAGE% >= $MIN_COVERAGE%"
              echo "$COVERAGE" > $out
            else
              echo "❌ Coverage too low: $COVERAGE% < $MIN_COVERAGE%"
              exit 1
            fi
          '';
        };
      }
    )) # end eachSystem
    # Merge additional Linux-only container loading utilities not defined in eachSystem
    {
      packages = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ] (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
            config.allowUnfree = false;
          };
        in
        {
          # Container loading utilities for CI (Linux only)
          load-ollama-image = if pkgs.stdenv.isLinux then
            (pkgs.writeShellApplication {
              name = "load-ollama-image";
              runtimeInputs = [ pkgs.skopeo ];
              text = ''
                echo "Loading ollama image using nix2container JSON format..."
                IMAGE_PATH=$(nix build .#ollamaImage --print-out-paths --no-link)
                echo "Image built at: $IMAGE_PATH"
                skopeo copy "nix:$IMAGE_PATH" containers-storage:nanna-coder-ollama:latest
                echo "Image loaded successfully"
              '';
            }) else null;

          load-container-image = if pkgs.stdenv.isLinux then
            (pkgs.writeShellApplication {
              name = "load-container-image";
              runtimeInputs = with pkgs; [ skopeo docker_28 file ];
              text = ''
                if [ $# -eq 0 ]; then
                  echo "Usage: load-container-image <image-name> [tag]"
                  exit 1
                fi

                IMAGE_NAME="$1"
                TAG="''${2:-latest}"

                echo "Loading container image: $IMAGE_NAME:$TAG"

                if [ -L result ]; then
                  IMAGE_PATH=$(readlink -f result)

                  if file "$IMAGE_PATH" | grep -q "JSON"; then
                    docker load < "$IMAGE_PATH" 2>/dev/null || {
                      IMAGE_ID=$(docker import "$IMAGE_PATH" 2>/dev/null) || {
                        echo "Failed to import nix2container image"
                        exit 1
                      }
                      echo "Imported image with ID: $IMAGE_ID"
                    }
                  else
                    docker load < "$IMAGE_PATH"
                  fi
                else
                  echo "Error: 'result' symlink not found. Run 'nix build' first."
                  exit 1
                fi

                REPO_NAME="dominicburkart/nanna-coder"
                docker tag "$IMAGE_NAME:$TAG" "ghcr.io/$REPO_NAME/$IMAGE_NAME:$TAG" 2>/dev/null || {
                  docker images --format "{{.Repository}}:{{.Tag}}" | grep -E "(nanna-coder|$IMAGE_NAME)" | head -1 | xargs -I {} docker tag {} "ghcr.io/$REPO_NAME/$IMAGE_NAME:$TAG"
                }

                echo "Container image $IMAGE_NAME:$TAG ready for push"
              '';
            }) else null;
        }
      );
    }; # end recursiveUpdate second arg
}
