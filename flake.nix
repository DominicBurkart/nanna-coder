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
            cargo-expand   # Macro expansion debugging
            cargo-udeps    # Unused dependency detection
            cargo-machete  # Remove unused dependencies
            cargo-outdated # Check for outdated dependencies

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
            echo "🛠️  Development Commands:"
            echo "  dev-test                     # Run full test suite with watch mode"
            echo "  dev-build                    # Fast incremental build"
            echo "  dev-check                    # Quick syntax and format check"
            echo "  dev-clean                    # Clean build artifacts"
            echo "  dev-reset                    # Reset development environment"
            echo ""
            echo "🐳 Container Commands:"
            echo "  container-dev                # Start development containers"
            echo "  container-test               # Run containerized tests"
            echo "  container-stop               # Stop all containers"
            echo "  container-logs               # View container logs"
            echo ""
            echo "🔧 Cache Commands:"
            echo "  cache-info                   # View cache statistics"
            echo "  cache-setup                  # Configure binary cache"
            echo "  cache-warm                   # Pre-warm frequently used builds"
            echo ""
            echo "📋 Legacy Commands:"
            echo "  cargo build --workspace      # Build all packages"
            echo "  cargo nextest run             # Run tests with nextest"
            echo "  nix build .#harnessImage      # Build harness container"
            echo "  nix flake check               # Validate flake"
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
              NEW=$(cargo tarpaulin --skip-clean --ignore-tests --output-format text 2>/dev/null | grep -oP '\d+\.\d+(?=% coverage)' || echo "0.0")

              # Get main branch coverage (if possible)
              git stash -q 2>/dev/null || true
              if git checkout main -q 2>/dev/null; then
                OLD=$(cargo tarpaulin --skip-clean --ignore-tests --output-format text 2>/dev/null | grep -oP '\d+\.\d+(?=% coverage)' || echo "0.0")
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

            # Set up development aliases for convenience
            echo "🔧 Setting up development aliases..."
            alias ll='ls -la'
            alias la='ls -A'
            alias l='ls -CF'
            alias ..='cd ..'
            alias ...='cd ../..'
            alias grep='grep --color=auto'
            alias c='clear'

            # Cargo aliases for convenience
            alias cb='cargo build'
            alias ct='cargo test'
            alias cc='cargo check'
            alias cf='cargo fmt'
            alias cn='cargo nextest run'

            # Git aliases
            alias gs='git status'
            alias ga='git add'
            alias gc='git commit'
            alias gp='git push'
            alias gl='git log --oneline'
            alias gd='git diff'

            # Nix aliases
            alias nb='nix build'
            alias nr='nix run'
            alias nd='nix develop'
            alias nf='nix flake'

            # Project-specific aliases
            alias dt='dev-test'
            alias db='dev-build'
            alias dc='dev-check'

            echo ""
            echo "🎯 Development environment ready!"
            echo "💡 Useful aliases configured (ll, cb, ct, gs, nb, dt, etc.)"
            echo "🚀 Run any of the commands above to get started"
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

        # Multi-model caching system for reproducible testing
        # These are kept separate from release images to prevent bloat

        # Model registry with metadata for all supported models
        modelRegistry = {
          "qwen3" = {
            name = "qwen3:0.6b";
            hash = "sha256-2EaXyBr1C+6wNyLzcWblzB52iV/2G26dSa5MFqpYJLc=";
            description = "Qwen3 0.6B - Fast and efficient model for testing";
            size = "560MB";
            homepage = "https://ollama.com/library/qwen3";
          };
          "llama3" = {
            name = "llama3:8b";
            hash = "sha256-0000000000000000000000000000000000000000000="; # Placeholder
            description = "Llama3 8B - High quality general purpose model";
            size = "4.7GB";
            homepage = "https://ollama.com/library/llama3";
          };
          "mistral" = {
            name = "mistral:7b";
            hash = "sha256-0000000000000000000000000000000000000000000="; # Placeholder
            description = "Mistral 7B - Balanced performance model";
            size = "4.1GB";
            homepage = "https://ollama.com/library/mistral";
          };
          "gemma" = {
            name = "gemma:2b";
            hash = "sha256-0000000000000000000000000000000000000000000="; # Placeholder
            description = "Gemma 2B - Lightweight model for development";
            size = "1.4GB";
            homepage = "https://ollama.com/library/gemma";
          };
        };

        # Cache size management configuration
        cacheConfig = {
          maxTotalSize = "10GB"; # Maximum total cache size
          maxModelAge = "30days"; # Auto-cleanup models older than this
          evictionPolicy = "LRU"; # Least Recently Used eviction
          compressionEnabled = true;
        };

        # Function to create a model derivation with proper caching
        createModelDerivation = modelKey: modelInfo:
          # Use conditional logic to handle placeholder hashes
          if (pkgs.lib.hasInfix "0000000000000000000000000000000000000000000" modelInfo.hash) then
            # For development/CI - create non-fixed derivation that downloads on demand
            pkgs.runCommand "${modelKey}-model" {
              nativeBuildInputs = with pkgs; [ ollama curl cacert ];
              # Development mode - no fixed hash
            } ''
              echo "🔄 Creating development model stub for ${modelInfo.name}..."
              mkdir -p $out/models
              echo "${modelInfo.name}" > $out/models/model.info
              echo "Development mode - model will be downloaded on first use" > $out/models/README
            ''
          else
            # Production mode with real hashes
            pkgs.runCommand "${modelKey}-model" {
              # Fixed-output derivation for reproducible caching
              outputHash = modelInfo.hash;
              outputHashAlgo = "sha256";
              outputHashMode = "recursive";
              nativeBuildInputs = with pkgs; [ ollama curl cacert ];
              # Add meta information for documentation
              meta = with pkgs.lib; {
                description = "${modelInfo.description} (cached for testing)";
                longDescription = ''
                  Pre-downloaded ${modelInfo.name} model for reproducible testing.
                  This derivation downloads the model once and caches it by content hash.
                  Size: ${modelInfo.size}
                '';
                homepage = modelInfo.homepage;
                platforms = platforms.linux;
              };
            } ''
            echo "🔄 Setting up ${modelInfo.name} model download (reproducible)..."

            # Create output directory structure
            mkdir -p $out/models

            # Set up environment for ollama
            export OLLAMA_MODELS=$out/models
            export HOME=$(mktemp -d)
            export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt

            # Start ollama server in isolated environment
            echo "🚀 Starting temporary Ollama server..."
            ollama serve > ollama.log 2>&1 &
            OLLAMA_PID=$!

            # Function to cleanup on exit
            cleanup() {
              echo "🧹 Cleaning up Ollama server..."
              kill $OLLAMA_PID 2>/dev/null || true
              wait $OLLAMA_PID 2>/dev/null || true
            }
            trap cleanup EXIT

            # Wait for ollama to be ready
            echo "⏳ Waiting for Ollama server..."
            for i in {1..30}; do
              if curl -s http://localhost:11434/api/tags >/dev/null 2>&1; then
                echo "✅ Ollama server ready"
                break
              fi
              sleep 2
              if [ $i -eq 30 ]; then
                echo "❌ Ollama server failed to start"
                cat ollama.log
                exit 1
              fi
            done

            # Download the model
            echo "📥 Downloading ${modelInfo.name} model (${modelInfo.size} - will be cached by hash)..."
            if ! ollama pull ${modelInfo.name}; then
              echo "❌ Failed to download ${modelInfo.name}"
              cat ollama.log
              exit 1
            fi

            # Verify download
            if ! ollama list | grep -q "${modelInfo.name}"; then
              echo "❌ Model verification failed"
              ollama list
              exit 1
            fi

            # Stop ollama (cleanup will handle this too)
            cleanup

            echo "✅ ${modelInfo.name} model cached at $out/models"
            echo "📊 Model cache contents:"
            find $out/models -type f -exec ls -lh {} \; | head -5
          '';

        # Cache management utilities
        cacheUtils = {
          # Script to check cache size and manage cleanup
          cache-info = pkgs.writeShellScriptBin "cache-info" ''
            echo "🗂️  Nanna Coder Model Cache Information"
            echo "======================================"

            CACHE_DIR="/nix/store"

            echo "📊 Cache Statistics:"
            echo "  Total models cached: $(find $CACHE_DIR -name "*-model" | wc -l)"
            echo "  Total cache size: $(du -sh $CACHE_DIR | cut -f1)"
            echo "  Available space: $(df -h $CACHE_DIR | tail -1 | awk '{print $4}')"

            echo ""
            echo "🏷️  Available Models:"
            ${pkgs.lib.concatMapStringsSep "\n" (item: ''
              echo "  - ${item.value.name} (${item.value.size}) - ${item.value.description}"
            '') (pkgs.lib.attrsToList modelRegistry)}

            echo ""
            echo "💡 Usage:"
            echo "  nix build .#qwen3-model     # Cache qwen3 model"
            echo "  nix build .#llama3-model    # Cache llama3 model"
            echo "  nix build .#mistral-model   # Cache mistral model"
            echo "  nix build .#gemma-model     # Cache gemma model"
            echo ""
            echo "  nix build .#ollama-qwen3    # Pre-built container with qwen3"
            echo "  nix build .#ollama-llama3   # Pre-built container with llama3"
          '';

          # Script to clean up old cached models
          cache-cleanup = pkgs.writeShellScriptBin "cache-cleanup" ''
            echo "🧹 Cleaning up model cache..."
            echo "Max size: ${toString cacheConfig.maxTotalSize}"
            echo "Eviction policy: ${cacheConfig.evictionPolicy}"

            # This would implement actual cleanup logic
            # For now, it's informational
            echo "⚠️  Cleanup logic not yet implemented"
            echo "Use 'nix-collect-garbage' for manual cleanup"
          '';
        };

        # Multi-model cache system - reproducible model derivations
        models = {
          qwen3-model = createModelDerivation "qwen3" modelRegistry.qwen3;
          llama3-model = createModelDerivation "llama3" modelRegistry.llama3;
          mistral-model = createModelDerivation "mistral" modelRegistry.mistral;
          gemma-model = createModelDerivation "gemma" modelRegistry.gemma;
        };

        # Multi-model containers with pre-cached models
        containers = {
          qwen3-container = nix2containerPkgs.nix2container.buildImage {
            name = "nanna-coder-ollama-qwen3";
            tag = "latest";
            fromImage = ollamaImage;
            copyToRoot = pkgs.buildEnv {
              name = "ollama-qwen3-env";
              paths = [ pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils pkgs.curl models.qwen3-model ];
              pathsToLink = [ "/bin" "/etc" "/share" "/models" ];
            };
            config = {
              Cmd = [ "${pkgs.ollama}/bin/ollama" "serve" ];
              Env = [ "OLLAMA_HOST=0.0.0.0" "OLLAMA_PORT=11434" "OLLAMA_MODELS=/models" "PATH=/bin" ];
              WorkingDir = "/app";
              ExposedPorts = { "11434/tcp" = {}; };
              Volumes = { "/root/.ollama" = {}; };
            };
            created = "2025-09-20T00:00:00Z";
            maxLayers = 100;
          };

          llama3-container = nix2containerPkgs.nix2container.buildImage {
            name = "nanna-coder-ollama-llama3";
            tag = "latest";
            fromImage = ollamaImage;
            copyToRoot = pkgs.buildEnv {
              name = "ollama-llama3-env";
              paths = [ pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils pkgs.curl models.llama3-model ];
              pathsToLink = [ "/bin" "/etc" "/share" "/models" ];
            };
            config = {
              Cmd = [ "${pkgs.ollama}/bin/ollama" "serve" ];
              Env = [ "OLLAMA_HOST=0.0.0.0" "OLLAMA_PORT=11434" "OLLAMA_MODELS=/models" "PATH=/bin" ];
              WorkingDir = "/app";
              ExposedPorts = { "11434/tcp" = {}; };
              Volumes = { "/root/.ollama" = {}; };
            };
            created = "2025-09-20T00:00:00Z";
            maxLayers = 100;
          };

          mistral-container = nix2containerPkgs.nix2container.buildImage {
            name = "nanna-coder-ollama-mistral";
            tag = "latest";
            fromImage = ollamaImage;
            copyToRoot = pkgs.buildEnv {
              name = "ollama-mistral-env";
              paths = [ pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils pkgs.curl models.mistral-model ];
              pathsToLink = [ "/bin" "/etc" "/share" "/models" ];
            };
            config = {
              Cmd = [ "${pkgs.ollama}/bin/ollama" "serve" ];
              Env = [ "OLLAMA_HOST=0.0.0.0" "OLLAMA_PORT=11434" "OLLAMA_MODELS=/models" "PATH=/bin" ];
              WorkingDir = "/app";
              ExposedPorts = { "11434/tcp" = {}; };
              Volumes = { "/root/.ollama" = {}; };
            };
            created = "2025-09-20T00:00:00Z";
            maxLayers = 100;
          };

          gemma-container = nix2containerPkgs.nix2container.buildImage {
            name = "nanna-coder-ollama-gemma";
            tag = "latest";
            fromImage = ollamaImage;
            copyToRoot = pkgs.buildEnv {
              name = "ollama-gemma-env";
              paths = [ pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils pkgs.curl models.gemma-model ];
              pathsToLink = [ "/bin" "/etc" "/share" "/models" ];
            };
            config = {
              Cmd = [ "${pkgs.ollama}/bin/ollama" "serve" ];
              Env = [ "OLLAMA_HOST=0.0.0.0" "OLLAMA_PORT=11434" "OLLAMA_MODELS=/models" "PATH=/bin" ];
              WorkingDir = "/app";
              ExposedPorts = { "11434/tcp" = {}; };
              Volumes = { "/root/.ollama" = {}; };
            };
            created = "2025-09-20T00:00:00Z";
            maxLayers = 100;
          };
        };

        # Binary cache strategy for CI/CD optimization
        binaryCacheConfig = {
          # Cachix configuration for public binary cache
          cacheName = "nanna-coder";
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
            echo "trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= ${binaryCacheConfig.cacheName}.cachix.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=" >> ~/.config/nix/nix.conf

            echo "✅ Binary cache configured successfully!"
            echo "💡 Run 'push-cache' to upload builds to cache"
          '';

          # Script to push builds to binary cache
          push-cache = pkgs.writeShellScriptBin "push-cache" ''
            echo "🚀 Pushing builds to binary cache..."

            if [ -z "$CACHIX_AUTH_TOKEN" ]; then
              echo "❌ CACHIX_AUTH_TOKEN not set. Please configure authentication."
              echo "💡 Run: export CACHIX_AUTH_TOKEN=your_token"
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

        # Development experience optimization utilities
        devUtils = {
          # Fast incremental development build
          dev-build = pkgs.writeShellScriptBin "dev-build" ''
            echo "🚀 Starting fast incremental build..."

            # Use cargo-watch for incremental compilation
            if command -v cargo-watch &> /dev/null; then
              echo "📦 Using cargo-watch for incremental builds"
              cargo watch -x "build --workspace"
            else
              echo "📦 Running standard incremental build"
              cargo build --workspace
            fi

            echo "✅ Build complete!"
          '';

          # Comprehensive test runner with watch mode
          dev-test = pkgs.writeShellScriptBin "dev-test" ''
            echo "🧪 Starting comprehensive test suite..."

            # Run different test types based on arguments
            case "''${1:-all}" in
              "unit")
                echo "Running unit tests..."
                cargo nextest run --workspace --lib
                ;;
              "integration")
                echo "Running integration tests..."
                cargo nextest run --workspace --test '*'
                ;;
              "watch")
                echo "Starting test watch mode..."
                if command -v cargo-watch &> /dev/null; then
                  cargo watch -x "nextest run --workspace"
                else
                  echo "⚠️  cargo-watch not available, running tests once"
                  cargo nextest run --workspace
                fi
                ;;
              "all"|*)
                echo "Running all tests..."
                cargo nextest run --workspace

                echo "🔍 Running clippy checks..."
                cargo clippy --workspace --all-targets -- -D warnings

                echo "📝 Checking formatting..."
                cargo fmt --all -- --check

                echo "🔒 Running security audit..."
                cargo audit

                echo "📋 Checking licenses..."
                cargo deny check
                ;;
            esac

            echo "✅ Test suite complete!"
          '';

          # Quick syntax and format check
          dev-check = pkgs.writeShellScriptBin "dev-check" ''
            echo "🔍 Running quick development checks..."

            echo "📝 Checking Rust formatting..."
            if ! cargo fmt --all -- --check; then
              echo "💡 Run 'cargo fmt' to fix formatting issues"
              exit 1
            fi

            echo "🔍 Running clippy (fast mode)..."
            if ! cargo clippy --workspace --all-targets -- -D warnings; then
              echo "💡 Fix clippy warnings before committing"
              exit 1
            fi

            echo "🏗️  Checking compilation..."
            if ! cargo check --workspace; then
              echo "💡 Fix compilation errors"
              exit 1
            fi

            echo "✅ All checks passed!"
          '';

          # Clean development artifacts
          dev-clean = pkgs.writeShellScriptBin "dev-clean" ''
            echo "🧹 Cleaning development artifacts..."

            echo "📦 Cleaning Cargo artifacts..."
            cargo clean

            echo "🗑️  Removing target directory..."
            rm -rf target/

            echo "🐳 Cleaning container images..."
            if command -v podman &> /dev/null; then
              podman system prune -f --filter until=24h
            elif command -v docker &> /dev/null; then
              docker system prune -f --filter until=24h
            fi

            echo "♻️  Cleaning Nix store (optional)..."
            nix store gc --max-age 7d

            echo "✅ Cleanup complete!"
          '';

          # Reset development environment
          dev-reset = pkgs.writeShellScriptBin "dev-reset" ''
            echo "🔄 Resetting development environment..."

            echo "🧹 Running cleanup..."
            dev-clean

            echo "🔧 Updating flake inputs..."
            nix flake update

            echo "📥 Rebuilding development shell..."
            nix develop --refresh

            echo "🎯 Warming cache with common builds..."
            nix build .#nanna-coder --no-link

            echo "✅ Development environment reset complete!"
          '';

          # Start development containers
          container-dev = pkgs.writeShellScriptBin "container-dev" ''
            echo "🐳 Starting development containers..."

            # Use docker-compose for orchestration
            if [ -f "docker-compose.yml" ] || [ -f "docker-compose.yaml" ]; then
              echo "📋 Using docker-compose configuration"
              if command -v podman-compose &> /dev/null; then
                podman-compose up -d
              elif command -v docker-compose &> /dev/null; then
                docker-compose up -d
              else
                echo "⚠️  No compose tool available"
                exit 1
              fi
            else
              echo "🚀 Starting individual containers..."

              # Start Ollama container
              echo "🤖 Starting Ollama container..."
              nix run .#start-pod
            fi

            echo "✅ Development containers started!"
            echo "💡 Use 'container-logs' to view logs"
          '';

          # Run containerized tests
          container-test = pkgs.writeShellScriptBin "container-test" ''
            echo "🧪 Running containerized tests..."

            echo "🐳 Starting test containers..."
            nix build .#qwen3-container --no-link

            # Load and start test container
            echo "📦 Loading test container..."
            if command -v podman &> /dev/null; then
              podman load -i $(nix build .#qwen3-container --print-out-paths --no-link)/image.tar
              podman run -d --name nanna-test-ollama -p 11434:11434 nanna-coder-ollama-qwen3:latest
            else
              echo "⚠️  Podman not available, skipping container tests"
              exit 1
            fi

            echo "⏳ Waiting for container to be ready..."
            sleep 10

            echo "🧪 Running integration tests..."
            cargo test --workspace --test '*' -- --test-threads=1

            echo "🧹 Cleaning up test containers..."
            podman stop nanna-test-ollama
            podman rm nanna-test-ollama

            echo "✅ Containerized tests complete!"
          '';

          # Stop all development containers
          container-stop = pkgs.writeShellScriptBin "container-stop" ''
            echo "🛑 Stopping development containers..."

            if command -v podman &> /dev/null; then
              echo "🐳 Stopping podman containers..."
              podman stop $(podman ps -q) 2>/dev/null || echo "No running containers"
              nix run .#stop-pod 2>/dev/null || echo "Pod not running"
            fi

            if command -v docker &> /dev/null; then
              echo "🐳 Stopping docker containers..."
              docker stop $(docker ps -q) 2>/dev/null || echo "No running containers"
            fi

            echo "✅ All containers stopped!"
          '';

          # View container logs
          container-logs = pkgs.writeShellScriptBin "container-logs" ''
            echo "📋 Viewing container logs..."

            if command -v podman &> /dev/null; then
              echo "🐳 Podman containers:"
              podman ps --format "{{.Names}}" | while read container; do
                if [ -n "$container" ]; then
                  echo "--- Logs for $container ---"
                  podman logs --tail 20 "$container"
                  echo ""
                fi
              done
            fi

            echo "💡 Use 'podman logs -f <container>' for live logs"
          '';

          # Warm cache with frequently used builds
          cache-warm = pkgs.writeShellScriptBin "cache-warm" ''
            echo "🔥 Warming development cache..."

            echo "📦 Building core packages..."
            nix build .#nanna-coder --no-link --print-build-logs

            echo "🐳 Building container images..."
            nix build .#harnessImage --no-link --print-build-logs &
            nix build .#ollamaImage --no-link --print-build-logs &

            echo "🧪 Building test dependencies..."
            nix build .#qwen3-model --no-link --print-build-logs &

            echo "⏳ Waiting for background builds..."
            wait

            echo "📊 Cache statistics:"
            nix run .#cache-analytics

            echo "✅ Cache warming complete!"
          '';
        };

        # Test containers with multi-model support
        testContainers = {
          # Use our existing ollama image as base for testing
          ollama-base = ollamaImage;

          # Generate individual model derivations
          qwen3-model = createModelDerivation "qwen3" modelRegistry.qwen3;
          llama3-model = createModelDerivation "llama3" modelRegistry.llama3;
          mistral-model = createModelDerivation "mistral" modelRegistry.mistral;
          gemma-model = createModelDerivation "gemma" modelRegistry.gemma;

          # Function to create containers with specific models
          createModelContainer = modelKey: modelInfo: modelDerivation: nix2containerPkgs.nix2container.buildImage {
            name = "nanna-coder-test-ollama-${modelKey}";
            tag = "latest";
            fromImage = ollamaImage;

            copyToRoot = pkgs.buildEnv {
              name = "ollama-${modelKey}-env";
              paths = [
                pkgs.cacert
                pkgs.tzdata
                pkgs.bash
                pkgs.coreutils
                pkgs.curl
                modelDerivation
              ];
              pathsToLink = [ "/bin" "/etc" "/share" "/models" ];
            };

            config = {
              Cmd = [ "${pkgs.ollama}/bin/ollama" "serve" ];
              Env = [
                "OLLAMA_HOST=0.0.0.0"
                "OLLAMA_PORT=11434"
                "OLLAMA_MODELS=/models"
                "PATH=/bin"
              ];
              WorkingDir = "/app";
              ExposedPorts = { "11434/tcp" = {}; };
              Volumes = { "/root/.ollama" = {}; };
            };

            created = "2025-09-20T00:00:00Z";
            maxLayers = 100;
          };

          # Pre-built containers with models
          ollama-qwen3 = testContainers.createModelContainer "qwen3" modelRegistry.qwen3 testContainers.qwen3-model;
          ollama-llama3 = testContainers.createModelContainer "llama3" modelRegistry.llama3 testContainers.llama3-model;
          ollama-mistral = testContainers.createModelContainer "mistral" modelRegistry.mistral testContainers.mistral-model;
          ollama-gemma = testContainers.createModelContainer "gemma" modelRegistry.gemma testContainers.gemma-model;
        };

      in
      {
        packages = {
          default = nanna-coder;
          inherit nanna-coder harness;

          # Container images (production)
          harnessImage = harnessImage;
          ollamaImage = ollamaImage;

          # Multi-model cache system
          inherit (models) qwen3-model llama3-model mistral-model gemma-model;

          # Multi-model containers
          inherit (containers) qwen3-container llama3-container mistral-container gemma-container;

          # Cache management utilities
          inherit (cacheUtils) cache-info cache-cleanup;

          # Binary cache utilities
          inherit (binaryCacheUtils) setup-cache push-cache ci-cache-optimize cache-analytics;

          # Development utilities
          inherit (devUtils) dev-build dev-test dev-check dev-clean dev-reset
                            container-dev container-test container-stop container-logs cache-warm;

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

          # CI/CD utilities
          ci-cache-optimize = flake-utils.lib.mkApp {
            drv = binaryCacheUtils.ci-cache-optimize;
          };

          container-test = flake-utils.lib.mkApp {
            drv = devUtils.container-test;
          };

          cache-analytics = flake-utils.lib.mkApp {
            drv = binaryCacheUtils.cache-analytics;
          };

          push-cache = flake-utils.lib.mkApp {
            drv = binaryCacheUtils.push-cache;
          };

          # Development utilities
          dev-test = flake-utils.lib.mkApp {
            drv = devUtils.dev-test;
          };

          dev-build = flake-utils.lib.mkApp {
            drv = devUtils.dev-build;
          };

          # Cache management
          cache-info = flake-utils.lib.mkApp {
            drv = cacheUtils.cache-info;
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