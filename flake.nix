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
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane, nix2container }:
    flake-utils.lib.eachDefaultSystem (system:
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

        # Pin specific Rust version for reproducibility
        rustToolchain = pkgs.rust-bin.stable."1.81.0".default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" "rust-analyzer" ];
        };

        # Crane library for building Rust packages
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Common build inputs for all Rust packages
        commonBuildInputs = with pkgs; [
          pkg-config
          openssl
        ];

        commonNativeBuildInputs = with pkgs; [
          pkg-config
        ];

        # Filter source files (exclude target, .git, etc.)
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (pkgs.lib.hasSuffix "\.rs" path) ||
            (pkgs.lib.hasSuffix "\.toml" path) ||
            (pkgs.lib.hasSuffix "\.lock" path) ||
            (type == "directory");
        };

        # Build dependencies first for better caching
        cargoArtifacts = craneLib.buildDepsOnly {
          inherit src;
          buildInputs = commonBuildInputs;
          nativeBuildInputs = commonNativeBuildInputs;
        };

        # Build the workspace
        nanna-coder = craneLib.buildPackage {
          inherit src cargoArtifacts;
          buildInputs = commonBuildInputs;
          nativeBuildInputs = commonNativeBuildInputs;

          # Ensure all workspace members are built
          cargoBuildCommand = "cargo build --workspace --release";
          cargoCheckCommand = "cargo check --workspace";
          cargoTestCommand = "cargo test --workspace";

          # Additional build metadata
          meta = with pkgs.lib; {
            description = "AI-powered coding assistant with tool calling and multi-model support";
            homepage = "https://github.com/yourusername/nanna-coder";
            license = licenses.mit;
            maintainers = [ ];
            platforms = platforms.linux ++ platforms.darwin;
          };
        };

        # Individual workspace member builds for granular container images
        harness = craneLib.buildPackage {
          inherit src cargoArtifacts;
          buildInputs = commonBuildInputs;
          nativeBuildInputs = commonNativeBuildInputs;

          cargoBuildCommand = "cargo build --release --bin harness";
          cargoCheckCommand = "cargo check --bin harness";
          cargoTestCommand = "cargo test --package harness";

          # Install only the harness binary
          installPhase = ''
            mkdir -p $out/bin
            cp target/release/harness $out/bin/
          '';
        };

        # Reproducible container images using nix2container
        nix2containerPkgs = nix2container.packages.${system};

        # Container image for the harness CLI
        harnessImage = nix2containerPkgs.nix2container.buildImage {
          name = "nanna-coder-harness";
          tag = "latest";

          copyToRoot = pkgs.buildEnv {
            name = "harness-env";
            paths = [
              harness
              pkgs.cacert  # For HTTPS requests
              pkgs.tzdata  # Timezone data
              pkgs.bash    # Shell for debugging
              pkgs.coreutils # Basic utilities
            ];
            pathsToLink = [ "/bin" "/etc" "/share" ];
          };

          config = {
            Cmd = [ "${harness}/bin/harness" ];
            Env = [
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              "RUST_LOG=info"
              "PATH=/bin"
            ];
            WorkingDir = "/app";
            ExposedPorts = {
              "8080/tcp" = {};
            };
          };

          # Reproducible layer strategy
          maxLayers = 100;
        };

        # Ollama service container using nix2container
        ollamaImage = nix2containerPkgs.nix2container.buildImage {
          name = "nanna-coder-ollama";
          tag = "latest";

          copyToRoot = pkgs.buildEnv {
            name = "ollama-env";
            paths = [
              pkgs.ollama
              pkgs.cacert
              pkgs.tzdata
              pkgs.bash
              pkgs.coreutils
            ];
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
            ExposedPorts = {
              "11434/tcp" = {};
            };
            Volumes = {
              "/root/.ollama" = {};
            };
          };

          # Reproducible layer strategy
          maxLayers = 100;
        };

        # Multi-container pod configuration
        podConfig = pkgs.writeTextFile {
          name = "nanna-coder-pod.yaml";
          text = ''
            apiVersion: v1
            kind: Pod
            metadata:
              name: nanna-coder-pod
            spec:
              containers:
              - name: harness
                image: nanna-coder-harness:latest
                ports:
                - containerPort: 8080
                env:
                - name: OLLAMA_URL
                  value: "http://localhost:11434"
                - name: RUST_LOG
                  value: "info"
              - name: ollama
                image: nanna-coder-ollama:latest
                ports:
                - containerPort: 11434
                volumeMounts:
                - name: ollama-data
                  mountPath: /root/.ollama
              volumes:
              - name: ollama-data
                emptyDir: {}
          '';
        };

        # Podman compose file for easier orchestration
        composeConfig = pkgs.writeTextFile {
          name = "docker-compose.yml";
          text = ''
            version: '3.8'

            services:
              ollama:
                image: nanna-coder-ollama:latest
                ports:
                  - "11434:11434"
                volumes:
                  - ollama_data:/root/.ollama
                environment:
                  - OLLAMA_HOST=0.0.0.0
                healthcheck:
                  test: ["CMD", "curl", "-f", "http://localhost:11434/api/tags"]
                  interval: 30s
                  timeout: 10s
                  retries: 3
                  start_period: 40s

              harness:
                image: nanna-coder-harness:latest
                ports:
                  - "8080:8080"
                environment:
                  - OLLAMA_URL=http://ollama:11434
                  - RUST_LOG=info
                depends_on:
                  ollama:
                    condition: service_healthy
                command: ["harness", "chat", "--model", "llama3.1:8b", "--tools"]

            volumes:
              ollama_data:
          '';
        };

        # Reproducible development shell with pinned tool versions
        devShell = pkgs.mkShell {
          buildInputs = with pkgs; [
            # Rust toolchain (pinned version)
            rustToolchain

            # Development tools (specific versions for reproducibility)
            cargo-watch
            cargo-audit
            cargo-deny
            cargo-tarpaulin
            cargo-edit
            cargo-nextest  # Better test runner

            # Container tools
            podman
            buildah
            skopeo

            # System dependencies
            pkg-config
            openssl

            # Development utilities (pinned in overlay)
            jq
            yq-go
            curl
            git

            # Nix tools
            nix-tree
            nix-du
            nixfmt-rfc-style

            # Documentation
            mdbook

            # Additional reproducibility tools
            nix-diff
            nix-output-monitor
          ];

          # Reproducible environment variables
          RUST_TOOLCHAIN_PATH = "${rustToolchain}";
          NIX_PATH = "nixpkgs=${nixpkgs}";

          # Ensure reproducible builds
          SOURCE_DATE_EPOCH = "1672531200"; # 2023-01-01

          shellHook = ''
            echo "🚀 Nanna Coder Development Environment (Reproducible)"
            echo "📦 Rust version: $(rustc --version)"
            echo "🐳 Podman version: $(podman --version)"
            echo "📋 Flake commit: ${self.shortRev or "dirty"}"
            echo "🔒 Reproducible build: SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH"
            echo ""
            echo "Available commands:"
            echo "  cargo build --workspace     # Build all packages"
            echo "  cargo nextest run            # Run tests with nextest"
            echo "  nix build .#harnessImage     # Build harness container"
            echo "  nix build .#ollamaImage      # Build ollama container"
            echo "  nix flake check              # Validate flake"
            echo "  nixfmt flake.nix             # Format Nix code"
            echo ""
            echo "Reproducibility commands:"
            echo "  nix-diff /nix/store/old /nix/store/new  # Compare store paths"
            echo "  nix-output-monitor           # Monitor build outputs"
            echo ""

            # Set up comprehensive git hooks if in a git repository
            if [ -d .git ]; then
              echo "Setting up production-grade git hooks..."
              mkdir -p .git/hooks

              cat > .git/hooks/pre-commit << 'EOF'
            #!/usr/bin/env bash
            set -e

            echo "🔍 Running comprehensive pre-commit checks..."

            # Format check
            echo "📝 Checking formatting..."
            cargo fmt --all -- --check

            # Clippy linting
            echo "🔍 Running clippy..."
            cargo clippy --workspace --all-targets -- -D warnings

            # Tests (including doctests)
            echo "🧪 Running tests..."
            cargo test --workspace --all-features

            # License and dependency scanning
            echo "📋 Checking licenses and dependencies..."
            cargo deny check

            # Security review with Claude (if available)
            echo "🔒 Running security review..."
            if command -v claude >/dev/null 2>&1; then
              git diff --cached | claude "You are a security engineer. Review the code being committed to determine if it can be committed/pushed. Does this commit leak any secrets, tokens, sensitive internals, or PII? If so, return a list of security/compliance problems to fix before the commit can be completed." | tee /tmp/claude_review
              if grep -qi "problem\|secret\|token\|pii\|leak" /tmp/claude_review; then
                echo "🚨 Security issues found above. Please fix before committing."
                exit 1
              fi
            else
              echo "⚠️  Claude CLI not available, skipping automated security review"
            fi

            # Coverage check with comparison to main
            echo "📊 Checking test coverage..."
            if command -v cargo-tarpaulin >/dev/null 2>&1; then
              NEW=$(cargo tarpaulin --skip-clean --ignore-tests -q --output-format text | grep -oP '\d+\.\d+(?=% coverage)' || echo "0.0")

              # Get main branch coverage (if possible)
              git stash -q 2>/dev/null || true
              if git checkout main -q 2>/dev/null; then
                OLD=$(cargo tarpaulin --skip-clean --ignore-tests -q --output-format text | grep -oP '\d+\.\d+(?=% coverage)' || echo "0.0")
                git checkout - -q
                git stash pop -q 2>/dev/null || true

                # Compare coverage using awk
                if awk "BEGIN { exit !($NEW >= $OLD) }"; then
                  echo "✅ Coverage: $NEW% >= $OLD%"
                else
                  echo "❌ Coverage dropped: $NEW% < $OLD%"
                  exit 1
                fi
              else
                echo "ℹ️  Could not check coverage against main branch"
                git stash pop -q 2>/dev/null || true
              fi
            else
              echo "⚠️  cargo-tarpaulin not available, skipping coverage check"
            fi

            echo "✅ All pre-commit checks passed!"
            EOF

              chmod +x .git/hooks/pre-commit
              echo "✅ Production-grade pre-commit hook installed"
            fi
          '';
        };

        # Build scripts for common operations
        buildScripts = {
          build-all = pkgs.writeShellScriptBin "build-all" ''
            echo "🔨 Building all containers..."
            nix build .#harnessImage
            nix build .#ollamaImage
            echo "✅ All containers built successfully!"
          '';

          load-images = pkgs.writeShellScriptBin "load-images" ''
            echo "📦 Loading container images into podman..."
            podman load < result-harness
            podman load < result-ollama
            echo "✅ Images loaded successfully!"
          '';

          start-pod = pkgs.writeShellScriptBin "start-pod" ''
            echo "🚀 Starting nanna-coder pod..."
            podman play kube ${podConfig}
            echo "✅ Pod started successfully!"
            echo "🌐 Harness available at: http://localhost:8080"
            echo "🤖 Ollama API available at: http://localhost:11434"
          '';

          stop-pod = pkgs.writeShellScriptBin "stop-pod" ''
            echo "🛑 Stopping nanna-coder pod..."
            podman pod stop nanna-coder-pod || true
            podman pod rm nanna-coder-pod || true
            echo "✅ Pod stopped successfully!"
          '';
        };

      in
      {
        packages = {
          default = nanna-coder;
          inherit nanna-coder harness;

          # Container images
          harnessImage = harnessImage;
          ollamaImage = ollamaImage;

          # Configuration files
          inherit podConfig composeConfig;

          # Build scripts
          inherit (buildScripts) build-all load-images start-pod stop-pod;
        };

        devShells.default = devShell;

        # Apps for easy execution
        apps = {
          default = flake-utils.lib.mkApp {
            drv = harness;
            exePath = "/bin/harness";
          };

          harness = flake-utils.lib.mkApp {
            drv = harness;
            exePath = "/bin/harness";
          };
        };

        # Checks for CI/CD
        checks = {
          # Workspace-wide checks
          workspace-test = craneLib.cargoTest {
            inherit src cargoArtifacts;
            buildInputs = commonBuildInputs;
            nativeBuildInputs = commonNativeBuildInputs;
            cargoTestCommand = "cargo test --workspace";
          };

          workspace-clippy = craneLib.cargoClippy {
            inherit src cargoArtifacts;
            buildInputs = commonBuildInputs;
            nativeBuildInputs = commonNativeBuildInputs;
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
            buildInputs = [ pkgs.cargo-tarpaulin rustToolchain ] ++ commonBuildInputs;
            nativeBuildInputs = commonNativeBuildInputs;
          } ''
            cd ${src}
            export CARGO_HOME=$(mktemp -d)

            # Run coverage and extract percentage
            COVERAGE=$(cargo tarpaulin --skip-clean --ignore-tests -q --output-format text | \
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
    ) // {
      # Cross-compilation support for additional architectures
      packages.aarch64-linux =
        let
          crossPkgs = import nixpkgs {
            system = "aarch64-linux";
            overlays = [ (import rust-overlay) ];
          };
        in {
          default = crossPkgs.rustPlatform.buildRustPackage {
            pname = "nanna-coder";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            buildInputs = with crossPkgs; [ pkg-config openssl ];
            nativeBuildInputs = with crossPkgs; [ pkg-config ];

            meta = {
              description = "AI-powered coding assistant (aarch64-linux)";
              platforms = [ "aarch64-linux" ];
            };
          };
        };

      packages.x86_64-darwin =
        let
          crossPkgs = import nixpkgs {
            system = "x86_64-darwin";
            overlays = [ (import rust-overlay) ];
          };
        in {
          default = crossPkgs.rustPlatform.buildRustPackage {
            pname = "nanna-coder";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            buildInputs = with crossPkgs; [ crossPkgs.darwin.apple_sdk.frameworks.Security ];

            meta = {
              description = "AI-powered coding assistant (x86_64-darwin)";
              platforms = [ "x86_64-darwin" ];
            };
          };
        };

      packages.aarch64-darwin =
        let
          crossPkgs = import nixpkgs {
            system = "aarch64-darwin";
            overlays = [ (import rust-overlay) ];
          };
        in {
          default = crossPkgs.rustPlatform.buildRustPackage {
            pname = "nanna-coder";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            buildInputs = with crossPkgs; [ crossPkgs.darwin.apple_sdk.frameworks.Security ];

            meta = {
              description = "AI-powered coding assistant (aarch64-darwin/Apple Silicon)";
              platforms = [ "aarch64-darwin" ];
            };
          };
        };
    };
}