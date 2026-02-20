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
        rustToolchain = pkgs.rust-bin.stable."1.93.0".default.override {
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
        containerConfig = import ./nix/container-config.nix {
          lib = pkgs.lib;
        };

        packages = import ./nix/packages.nix {
          inherit pkgs craneLib src;
          lib = pkgs.lib;
        };

        configs = import ./nix/configs.nix {
          inherit pkgs;
        };

        containers = import ./nix/containers.nix {
          inherit pkgs nix2containerPkgs containerConfig;
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
            COVERAGE=$(cargo tarpaulin --skip-clean --ignore-tests --out Stdout 2>&1 | \
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
          rustToolchain = pkgs.rust-bin.stable."1.93.0".default.override {
            extensions = [ "rust-src" "rustfmt" "clippy" "rust-analyzer" ];
          };
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

          commonBuildInputs = with pkgs; [ pkg-config openssl libssh2 zlib ];
          commonNativeBuildInputs = with pkgs; [ pkg-config stdenv.cc ];

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

          /** Load Ollama container using nix2container's copyToDockerDaemon

          This is the recommended method for loading nix2container images.
          Uses skopeo internally with the nix: transport.

          # Usage

          ```bash
          # Load ollama image
          nix run .#load-ollama-image

          # Verify it loaded
          docker images | grep nanna-coder-ollama

          # Run the container
          docker run -d -p 11434:11434 nanna-coder-ollama:latest
          ```

          # How It Works

          1. nix2container builds JSON description (not tarball)
          2. copyToDockerDaemon uses skopeo with nix: transport
          3. Image loaded directly into Docker daemon
          4. No intermediate files created

          # Troubleshooting

          If loading fails:
          - Check Docker daemon: `docker info`
          - Verify image built: `nix build .#ollamaImage --print-out-paths`
          - Check disk space: `df -h`

          # Benefits

          - Fast (no tar extraction)
          - Works with both Docker and Podman
          - Handles all format complexities internally
          - Official nix2container approach

          # See Also

          - Configuration: nix/container-config.nix
          - For CI usage: .github/workflows/ci.yml:529-536
          - nix2container: https://github.com/nlewo/nix2container
          */
          load-ollama-image = if pkgs.stdenv.isLinux then
            (pkgs.writeShellApplication {
              name = "load-ollama-image";
              text = ''
                echo "📦 Loading ollama image using nix2container's copyToDockerDaemon..."

                # Use nix2container's built-in loading method (handles skopeo internally)
                if ! nix run .#ollamaImage.copyToDockerDaemon; then
                  echo "❌ Failed to load ollama image"
                  echo "💡 Ensure Docker/Podman daemon is running"
                  exit 1
                fi

                echo "✅ Ollama image loaded successfully"
                echo "🐳 Run: docker run -d -p 11434:11434 nanna-coder-ollama:latest"
              '';
            }) else null;
        }
      );
    };
}
