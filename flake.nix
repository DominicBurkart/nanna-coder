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
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ] (system:
      let
        # Reproducible overlays with pinned versions
        overlays = [
          (import rust-overlay)
          # Additional pinned packages can be added here
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
        rustToolchain = pkgs.rust-bin.stable."1.84.0".default.override {
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
          inherit pkgs nix2containerPkgs;
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
        };

      in
      {
        packages = {
          default = packages.nanna-coder;
          inherit (packages) nanna-coder harness;

          # Container images (production)
          inherit (containers) harnessImage ollamaImage;

          # Multi-model cache system
          inherit (containers.models) qwen3-model llama3-model mistral-model gemma-model;

          # Multi-model containers
          inherit (containers.containers) qwen3-container llama3-container mistral-container gemma-container;

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

          # TODO: Re-enable audit once advisory-db is properly configured
          # workspace-audit = craneLib.cargoAudit {
          #   inherit src;
          # };

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
    ) //
    # Add cross-platform package support for CI matrix builds
    {
      packages = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ] (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
            config.allowUnfree = false;
          };
          rustToolchain = pkgs.rust-bin.stable."1.84.0".default.override {
            extensions = [ "rust-src" "rustfmt" "clippy" "rust-analyzer" ];
          };
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

          commonBuildInputs = with pkgs; [ pkg-config openssl ];
          commonNativeBuildInputs = with pkgs; [ pkg-config ];

          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              (pkgs.lib.hasSuffix "\.rs" path) ||
              (pkgs.lib.hasSuffix "\.toml" path) ||
              (pkgs.lib.hasSuffix "\.lock" path) ||
              (type == "directory");
          };

          cargoArtifacts = craneLib.buildDepsOnly {
            inherit src;
            buildInputs = commonBuildInputs;
            nativeBuildInputs = commonNativeBuildInputs;
          };
        in
        {
          nanna-coder = craneLib.buildPackage {
            inherit src cargoArtifacts;
            buildInputs = commonBuildInputs;
            nativeBuildInputs = commonNativeBuildInputs;
            cargoBuildCommand = "cargo build --workspace --release";
            cargoCheckCommand = "cargo check --workspace";
            cargoTestCommand = "cargo test --workspace";
          };

          harness = craneLib.buildPackage {
            inherit src cargoArtifacts;
            buildInputs = commonBuildInputs;
            nativeBuildInputs = commonNativeBuildInputs;
            cargoBuildCommand = "cargo build --release --bin harness";
            cargoCheckCommand = "cargo check --bin harness";
            cargoTestCommand = "cargo test --package harness";
            installPhase = ''
              mkdir -p $out/bin
              cp target/release/harness $out/bin/
            '';
          };

          # Container images (Linux only)
          harnessImage = if pkgs.stdenv.isLinux then
            (nix2container.packages.${system}.nix2container.buildImage {
              name = "nanna-coder-harness";
              tag = "latest";
              copyToRoot = pkgs.buildEnv {
                name = "harness-env";
                paths = [
                  (craneLib.buildPackage {
                    inherit src cargoArtifacts;
                    buildInputs = commonBuildInputs;
                    nativeBuildInputs = commonNativeBuildInputs;
                    cargoBuildCommand = "cargo build --release --bin harness";
                    installPhase = ''
                      mkdir -p $out/bin
                      cp target/release/harness $out/bin/
                    '';
                  })
                  pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils
                ];
                pathsToLink = [ "/bin" "/etc" "/share" ];
              };
              config = {
                Cmd = [ "/bin/harness" ];
                Env = [
                  "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                  "RUST_LOG=info"
                  "PATH=/bin"
                ];
                WorkingDir = "/app";
                ExposedPorts = { "8080/tcp" = {}; };
              };
              maxLayers = 100;
            }) else null;

          ollamaImage = if pkgs.stdenv.isLinux then
            (nix2container.packages.${system}.nix2container.buildImage {
              name = "nanna-coder-ollama";
              tag = "latest";
              copyToRoot = pkgs.buildEnv {
                name = "ollama-env";
                paths = [ pkgs.ollama pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils ];
                pathsToLink = [ "/bin" "/etc" "/share" ];
              };
              config = {
                Cmd = [ "${pkgs.ollama}/bin/ollama" "serve" ];
                Env = [
                  "OLLAMA_HOST=0.0.0.0"
                  "OLLAMA_PORT=11434"
                  "PATH=/bin"
                ];
                WorkingDir = "/app";
                ExposedPorts = { "11434/tcp" = {}; };
                Volumes = { "/root/.ollama" = {}; };
              };
              maxLayers = 100;
            }) else null;

          # Container loading utilities for CI
          load-ollama-image = if pkgs.stdenv.isLinux then
            (pkgs.writeShellScriptBin "load-ollama-image" ''
              echo "📦 Loading ollama image using nix2container JSON format..."
              IMAGE_PATH=$(nix build .#ollamaImage --print-out-paths --no-link)
              echo "Image built at: $IMAGE_PATH"

              # Use skopeo to load the nix2container JSON format
              if command -v skopeo >/dev/null 2>&1; then
                echo "Using skopeo to load image..."
                skopeo copy nix:$IMAGE_PATH containers-storage:nanna-coder-ollama:latest
              else
                echo "Installing skopeo..."
                nix-env -iA nixpkgs.skopeo
                skopeo copy nix:$IMAGE_PATH containers-storage:nanna-coder-ollama:latest
              fi
              echo "✅ Image loaded successfully"
            '') else null;

          # Universal container loading utility for CI builds
          load-container-image = if pkgs.stdenv.isLinux then
            (pkgs.writeShellScriptBin "load-container-image" ''
              if [ $# -eq 0 ]; then
                echo "Usage: load-container-image <image-name> [tag]"
                echo "Examples:"
                echo "  load-container-image harness"
                echo "  load-container-image ollama latest"
                exit 1
              fi

              IMAGE_NAME="$1"
              TAG="''${2:-latest}"

              echo "📦 Loading container image: $IMAGE_NAME:$TAG"

              # Handle the 'result' symlink created by nix build
              if [ -L result ]; then
                IMAGE_PATH=$(readlink -f result)
                echo "📂 Image path: $IMAGE_PATH"

                # Check if it's a nix2container JSON format
                if file "$IMAGE_PATH" | grep -q "JSON"; then
                  echo "🔧 Detected nix2container JSON format, using skopeo..."

                  # Install skopeo if needed
                  if ! command -v skopeo >/dev/null 2>&1; then
                    echo "📥 Installing skopeo..."
                    nix-env -iA nixpkgs.skopeo
                  fi

                  # Use docker load with nix2container images (they are OCI compatible)
                  docker load < "$IMAGE_PATH" 2>/dev/null || {
                    echo "⚠️ Docker load failed, trying nix2container-specific approach..."
                    # For nix2container, we need to use the image name from the JSON
                    IMAGE_ID=$(docker import "$IMAGE_PATH" 2>/dev/null) || {
                      echo "❌ Failed to import nix2container image"
                      exit 1
                    }
                    echo "✅ Imported image with ID: $IMAGE_ID"
                  }
                  echo "✅ JSON image loaded successfully"
                else
                  echo "🔧 Detected tar format, using docker load..."
                  # Traditional tar format
                  docker load < "$IMAGE_PATH"
                  echo "✅ Tar image loaded via docker load"
                fi
              else
                echo "❌ Error: 'result' symlink not found"
                echo "Run 'nix build' first to create the image"
                exit 1
              fi

              # Convert repository name to lowercase for Docker compatibility
              REPO_NAME="dominicburkart/nanna-coder"
              echo "🏷️ Tagging image as: ghcr.io/$REPO_NAME/$IMAGE_NAME:$TAG"

              # Tag the loaded image appropriately for registry push
              docker tag "$IMAGE_NAME:$TAG" "ghcr.io/$REPO_NAME/$IMAGE_NAME:$TAG" 2>/dev/null || {
                echo "⚠️ Direct tag failed, trying to find loaded image..."
                # Find the loaded image by name pattern and tag it
                docker images --format "{{.Repository}}:{{.Tag}}" | grep -E "(nanna-coder|$IMAGE_NAME)" | head -1 | xargs -I {} docker tag {} "ghcr.io/$REPO_NAME/$IMAGE_NAME:$TAG"
              }

              echo "✅ Container image $IMAGE_NAME:$TAG ready for push"
            '') else null;
        }
      );
    };
}
