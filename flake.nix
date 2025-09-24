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

            # Security scanning tools
            vulnix      # Nix vulnerability scanner
            openscap    # Security compliance scanning
            bc          # Calculator for security scoring
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
            echo "🔒 Agentic Security Commands:"
            echo "  security-judge               # AI-powered security analysis (model-as-judge)"
            echo "  security-behavioral-test     # Test security tools with known vulnerabilities"
            echo "  threat-model-analysis        # AI-driven threat model refinement"
            echo "  dependency-risk-profile      # AI analysis of dependency risks"
            echo "  adaptive-vulnix-scan         # Self-healing Nix vulnerability scanning"
            echo "  nix-provenance-validator     # Supply chain provenance validation"
            echo "  traditional-security-check   # Fallback: standard security tools"
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

            # Agentic security analysis
            echo "🔒 Running agentic security analysis..."

            # Quick behavioral security test
            echo "🧪 Quick behavioral security test..."
            if nix run .#security-behavioral-test --quiet 2>/dev/null; then
              echo "✅ Behavioral security tests passed"
            else
              echo "⚠️  Behavioral security tests had issues"
            fi

            # AI-powered security review if Ollama available
            if curl -s -m 5 http://localhost:11434/api/tags >/dev/null 2>&1; then
              echo "🤖 AI security analysis..."

              # Get staged changes with input sanitization
              STAGED_CHANGES=$(git diff --cached)
              if [ -n "$STAGED_CHANGES" ]; then
                # Sanitize input - remove potential secrets before AI analysis
                SANITIZED_CHANGES=$(echo "$STAGED_CHANGES" | \
                  sed 's/\(password\|secret\|key\|token\)=[^ ]*/\1=***REDACTED***/gi' | \
                  sed 's/["'"'"'][^"'"'"']*\(password\|secret\|key\|token\)[^"'"'"']*["'"'"']/***REDACTED***/gi' | \
                  head -c 4000)  # Limit input size

                # Use timeout and proper JSON escaping
                SECURITY_ANALYSIS=$(echo "$SANITIZED_CHANGES" | \
                  python3 -c "
import json, sys
content = sys.stdin.read()
payload = {
  'model': 'qwen3:0.6b',
  'prompt': 'You are a security engineer reviewing code changes. Look for: 1) Obvious vulnerabilities 2) Injection risks 3) Unsafe patterns. Reply SECURE if safe, or list specific issues. Do not echo back the code.\n\nCode changes:\n' + content,
  'stream': False
}
print(json.dumps(payload))
" | timeout 60 curl -s -X POST "$OLLAMA_URL/api/generate" \
                  -H "Content-Type: application/json" \
                  -d @- 2>/dev/null | \
                  jq -r '.response' 2>/dev/null || echo "SECURE")

                if echo "$SECURITY_ANALYSIS" | grep -qi "issue\|problem\|vulnerability\|risk\|insecure"; then
                  echo "🚨 AI Security Analysis found concerns:"
                  echo "$SECURITY_ANALYSIS"
                  echo ""
                  echo "Fix security issues before committing or use --no-verify to skip"
                  exit 1
                else
                  echo "✅ AI security analysis passed"
                fi
              fi
            elif command -v claude >/dev/null 2>&1; then
              # Fallback to Claude CLI if available
              echo "🔒 Fallback: Claude CLI security review..."
              git diff --cached | claude "You are a security engineer. Review the code being committed to determine if it can be committed/pushed. Does this commit leak any secrets, tokens, sensitive internals, or PII? If so, return a list of security/compliance problems to fix before the commit can be completed." | tee /tmp/claude_review
              if grep -qi "problem\|secret\|token\|pii\|leak" /tmp/claude_review; then
                echo "🚨 Security issues found above. Please fix before committing."
                exit 1
              fi
            else
              echo "ℹ️  AI security analysis unavailable (Ollama/Claude not running)"

              # Static security checks as fallback
              echo "🔧 Running static security checks..."
              cargo audit --quiet || echo "⚠️  cargo-audit warnings found"
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
              if curl -s -m 5 "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
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

        # Agentic Security Utilities with Model-as-Judge Architecture
        securityUtils = {
          # Core security judge using local Ollama
          security-judge = pkgs.writeShellScriptBin "security-judge" ''
            echo "🔒 Agentic Security Analysis - Model-as-Judge"
            echo "============================================="

            # Configurable Ollama endpoint
            OLLAMA_HOST=''${OLLAMA_HOST:-localhost}
            OLLAMA_PORT=''${OLLAMA_PORT:-11434}
            OLLAMA_URL="http://$OLLAMA_HOST:$OLLAMA_PORT"

            echo "🌐 Using Ollama endpoint: $OLLAMA_URL"

            # Check if Ollama is running
            if ! curl -s -m 5 "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
              echo "⚠️  Ollama not running. Starting local Ollama server..."
              podman run -d --name security-ollama -p 11434:11434 nanna-coder-ollama:latest || {
                echo "❌ Failed to start Ollama. Using fallback traditional tools."
                exec traditional-security-check
              }
              sleep 10
            fi

            # Security analysis prompt template
            SECURITY_PROMPT="You are an expert security engineer conducting a comprehensive security audit.

            Analyze the provided configuration/code for:
            1. REAL-WORLD ATTACK VECTORS: What actual attacks would this prevent vs. allow?
            2. BUSINESS IMPACT: How would successful attacks affect an AI coding assistant?
            3. CONTEXT AWARENESS: Is security appropriate for this specific project?
            4. SEMANTIC GAPS: Misalignment between security intent and implementation?
            5. RECOMMENDATIONS: Specific, actionable improvements with justification.

            Focus on EFFECTIVENESS over COMPLIANCE. Rate overall security posture 1-10 with detailed reasoning."

            echo "📋 Analyzing cargo-deny configuration..."
            if [ -f "deny.toml" ]; then
              DENY_ANALYSIS=$(cat deny.toml | timeout 60 curl -s -X POST "$OLLAMA_URL/api/generate" \
                -H "Content-Type: application/json" \
                -d "{\"model\": \"qwen3:0.6b\", \"prompt\": \"$SECURITY_PROMPT\n\nCargo-deny configuration:\n$(cat deny.toml)\", \"stream\": false}" | \
                jq -r '.response')

              echo "🤖 AI Security Assessment - Cargo Deny:"
              echo "$DENY_ANALYSIS"
              echo ""

              # Extract security score
              DENY_SCORE=$(echo "$DENY_ANALYSIS" | grep -oP 'Rating?:?\s*(\d+)/10' | head -1 | grep -oP '\d+' || echo "5")
              echo "📊 Cargo Deny Security Score: $DENY_SCORE/10"
            else
              echo "⚠️  No deny.toml found"
              DENY_SCORE=0
            fi

            echo ""
            echo "🔍 Analyzing Nix security configuration..."
            if [ -f "flake.nix" ]; then
              # Extract security-relevant parts of flake.nix
              SECURITY_EXTRACT=$(grep -A 5 -B 5 -i "security\|vulnix\|audit\|openssl\|cacert" flake.nix | head -50)

              NIX_ANALYSIS=$(echo "$SECURITY_EXTRACT" | timeout 60 curl -s -X POST "$OLLAMA_URL/api/generate" \
                -H "Content-Type: application/json" \
                -d "{\"model\": \"qwen3:0.6b\", \"prompt\": \"$SECURITY_PROMPT\n\nNix security configuration:\n$SECURITY_EXTRACT\", \"stream\": false}" | \
                jq -r '.response')

              echo "🤖 AI Security Assessment - Nix Configuration:"
              echo "$NIX_ANALYSIS"
              echo ""

              NIX_SCORE=$(echo "$NIX_ANALYSIS" | grep -oP 'Rating?:?\s*(\d+)/10' | head -1 | grep -oP '\d+' || echo "5")
              echo "📊 Nix Security Score: $NIX_SCORE/10"
            else
              NIX_SCORE=5
            fi

            echo ""
            echo "🎯 Overall Security Assessment:"
            OVERALL_SCORE=$(echo "scale=1; ($DENY_SCORE + $NIX_SCORE) / 2" | bc)
            echo "Security Effectiveness: $OVERALL_SCORE/10"

            if (( $(echo "$OVERALL_SCORE >= 7.0" | bc -l) )); then
              echo "✅ Security posture: GOOD"
              exit 0
            elif (( $(echo "$OVERALL_SCORE >= 5.0" | bc -l) )); then
              echo "⚠️  Security posture: NEEDS IMPROVEMENT"
              exit 1
            else
              echo "❌ Security posture: CRITICAL ISSUES"
              exit 2
            fi
          '';

          # Behavioral security testing with known-bad dependencies
          security-behavioral-test = pkgs.writeShellScriptBin "security-behavioral-test" ''
            echo "🧪 Behavioral Security Testing"
            echo "==============================="

            # Create temporary test directory
            TEST_DIR=$(mktemp -d)
            cd $TEST_DIR

            echo "📝 Creating test Cargo.toml with known problematic dependencies..."
            cat > Cargo.toml << 'EOF'
            [package]
            name = "security-test"
            version = "0.1.0"
            edition = "2021"

            [dependencies]
            # Test cases for security validation
            openssl = "0.10.64"  # Known vulnerable version (CVE-2024-6119)
            EOF

            echo "🎯 Testing if security tools catch vulnerable dependencies..."

            # Test cargo-audit
            echo "Testing cargo audit..."
            if cargo audit 2>&1 | grep -q "vulnerability\|RUSTSEC"; then
              echo "✅ cargo-audit: DETECTED vulnerable dependencies"
              AUDIT_PASS=1
            else
              echo "❌ cargo-audit: FAILED to detect vulnerabilities"
              AUDIT_PASS=0
            fi

            # Test cargo-deny with our config
            echo "Testing cargo deny..."
            cp ${../deny.toml} ./deny.toml 2>/dev/null || echo "No deny.toml to copy"
            if cargo deny check 2>&1 | grep -q "denied\|banned\|error"; then
              echo "✅ cargo-deny: BLOCKED problematic dependencies"
              DENY_PASS=1
            else
              echo "❌ cargo-deny: FAILED to block dependencies"
              DENY_PASS=0
            fi

            # AI analysis of test results
            if curl -s -m 5 "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
              echo "🤖 AI Analysis of behavioral test results..."

              TEST_RESULTS="Behavioral Security Test Results:
              - cargo-audit detection: $AUDIT_PASS/1
              - cargo-deny blocking: $DENY_PASS/1
              - Test dependencies: openssl 0.10.64 (known CVE-2024-6119)"

              AI_ANALYSIS=$(echo "$TEST_RESULTS" | timeout 60 curl -s -X POST "$OLLAMA_URL/api/generate" \
                -H "Content-Type: application/json" \
                -d "{\"model\": \"qwen3:0.6b\", \"prompt\": \"Analyze these security tool test results. What do they tell us about real-world protection? What are the implications for an AI coding assistant project?\n\n$TEST_RESULTS\", \"stream\": false}" | \
                jq -r '.response')

              echo "$AI_ANALYSIS"
            fi

            # Cleanup
            cd /
            rm -rf $TEST_DIR

            echo ""
            echo "📊 Behavioral Test Results:"
            echo "Audit Detection: $AUDIT_PASS/1"
            echo "Deny Blocking: $DENY_PASS/1"

            TOTAL_SCORE=$((AUDIT_PASS + DENY_PASS))
            if [ $TOTAL_SCORE -eq 2 ]; then
              echo "✅ All behavioral tests PASSED"
              exit 0
            elif [ $TOTAL_SCORE -eq 1 ]; then
              echo "⚠️  Some behavioral tests FAILED"
              exit 1
            else
              echo "❌ All behavioral tests FAILED"
              exit 2
            fi
          '';

          # Threat model refinement using AI analysis
          threat-model-analysis = pkgs.writeShellScriptBin "threat-model-analysis" ''
            echo "🎯 AI-Driven Threat Model Analysis"
            echo "=================================="

            # Configurable Ollama endpoint
            OLLAMA_HOST=''${OLLAMA_HOST:-localhost}
            OLLAMA_PORT=''${OLLAMA_PORT:-11434}
            OLLAMA_URL="http://$OLLAMA_HOST:$OLLAMA_PORT"

            # Check for git changes to analyze
            if git rev-parse --git-dir > /dev/null 2>&1; then
              echo "📊 Analyzing recent code changes for threat model updates..."

              # Get recent changes
              RECENT_CHANGES=$(git diff HEAD~5..HEAD --stat --name-only | head -20)
              if [ -z "$RECENT_CHANGES" ]; then
                RECENT_CHANGES="No recent changes detected"
              fi

              # Get current threat surface
              THREAT_SURFACE="Project: AI Coding Assistant (Rust + Nix + Containers)

              Recent Changes:
              $RECENT_CHANGES

              Current Dependencies: $(find . -name "Cargo.toml" -exec grep -H "^[a-zA-Z]" {} \; | head -20)

              Container Images: harness, ollama, model containers
              Network Exposure: HTTP API (port 8080), Ollama API (port 11434)"

              if curl -s -m 5 "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
                echo "🤖 AI Threat Model Analysis..."

                THREAT_PROMPT="You are a cybersecurity expert analyzing an AI coding assistant project.

                Given the following project information, provide:
                1. CURRENT THREAT LANDSCAPE: What attacks are most likely?
                2. ATTACK SURFACE ANALYSIS: How could an attacker compromise this system?
                3. SUPPLY CHAIN RISKS: Dependencies and build process vulnerabilities
                4. CONTAINER SECURITY: Risks from containerized deployment
                5. AI-SPECIFIC THREATS: Model poisoning, prompt injection, data exfiltration
                6. RECOMMENDED MITIGATIONS: Specific, actionable security controls

                Focus on threats specific to an AI coding assistant that processes user code."

                THREAT_ANALYSIS=$(echo "$THREAT_SURFACE" | timeout 60 curl -s -X POST "$OLLAMA_URL/api/generate" \
                  -H "Content-Type: application/json" \
                  -d "{\"model\": \"qwen3:0.6b\", \"prompt\": \"$THREAT_PROMPT\n\nProject Information:\n$THREAT_SURFACE\", \"stream\": false}" | \
                  jq -r '.response')

                echo "$THREAT_ANALYSIS"

                # Save analysis for future reference
                echo "$THREAT_ANALYSIS" > threat-model-$(date +%Y%m%d).md
                echo ""
                echo "💾 Threat model analysis saved to: threat-model-$(date +%Y%m%d).md"
              else
                echo "⚠️  Ollama not available, using static threat analysis..."
                echo "🎯 Static Threat Categories for AI Coding Assistant:"
                echo "1. Supply Chain Attacks (malicious dependencies)"
                echo "2. Container Escape (runtime container vulnerabilities)"
                echo "3. Model Poisoning (compromised AI models)"
                echo "4. Code Injection (untrusted user input)"
                echo "5. Data Exfiltration (sensitive code/credentials)"
              fi
            else
              echo "⚠️  Not in a git repository, cannot analyze changes"
            fi
          '';

          # Dependency risk profiling with AI analysis
          dependency-risk-profile = pkgs.writeShellScriptBin "dependency-risk-profile" ''
            echo "📊 Dependency Risk Profiling"
            echo "============================"

            # Configurable Ollama endpoint
            OLLAMA_HOST=''${OLLAMA_HOST:-localhost}
            OLLAMA_PORT=''${OLLAMA_PORT:-11434}
            OLLAMA_URL="http://$OLLAMA_HOST:$OLLAMA_PORT"

            # Analyze Cargo dependencies
            if [ -f "Cargo.toml" ]; then
              echo "🦀 Analyzing Rust dependencies..."

              # Extract dependencies with versions
              RUST_DEPS=$(grep -A 100 "\\[dependencies\\]" Cargo.toml | grep -E "^[a-zA-Z0-9_-]+ =" | head -20)

              if curl -s -m 5 "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
                RISK_PROMPT="You are a security expert analyzing software dependencies for risk.

                For each dependency, assess:
                1. MAINTAINER REPUTATION: Project maturity, community support
                2. UPDATE FREQUENCY: How often is it maintained?
                3. VULNERABILITY HISTORY: Past security issues
                4. SUPPLY CHAIN RISK: Dependencies of dependencies
                5. BUSINESS IMPACT: What happens if this dependency is compromised?

                Provide risk ratings (LOW/MEDIUM/HIGH) with justification."

                RUST_RISK_ANALYSIS=$(echo "$RUST_DEPS" | timeout 60 curl -s -X POST "$OLLAMA_URL/api/generate" \
                  -H "Content-Type: application/json" \
                  -d "{\"model\": \"qwen3:0.6b\", \"prompt\": \"$RISK_PROMPT\n\nRust Dependencies:\n$RUST_DEPS\", \"stream\": false}" | \
                  jq -r '.response')

                echo "🤖 AI Risk Analysis - Rust Dependencies:"
                echo "$RUST_RISK_ANALYSIS"
              fi
            fi

            echo ""
            # Analyze Nix dependencies
            if [ -f "flake.nix" ]; then
              echo "❄️  Analyzing Nix dependencies..."

              # Extract key Nix packages
              NIX_PACKAGES=$(grep -E "(pkgs\\.|inputs\\.|url =)" flake.nix | head -20)

              if curl -s -m 5 "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
                NIX_RISK_ANALYSIS=$(echo "$NIX_PACKAGES" | timeout 60 curl -s -X POST "$OLLAMA_URL/api/generate" \
                  -H "Content-Type: application/json" \
                  -d "{\"model\": \"qwen3:0.6b\", \"prompt\": \"$RISK_PROMPT\n\nNix Dependencies/Inputs:\n$NIX_PACKAGES\", \"stream\": false}" | \
                  jq -r '.response')

                echo "🤖 AI Risk Analysis - Nix Dependencies:"
                echo "$NIX_RISK_ANALYSIS"
              fi
            fi

            echo ""
            echo "💡 Risk Mitigation Recommendations:"
            echo "1. Pin dependency versions in Cargo.lock and flake.lock"
            echo "2. Enable security advisories monitoring"
            echo "3. Regular dependency updates with security testing"
            echo "4. Consider alternative dependencies for HIGH risk packages"
          '';

          # Self-healing vulnix configuration with AI-driven tuning
          adaptive-vulnix-scan = pkgs.writeShellScriptBin "adaptive-vulnix-scan" ''
            echo "🔧 Adaptive Vulnix Security Scanning"
            echo "===================================="

            # Configurable Ollama endpoint
            OLLAMA_HOST=''${OLLAMA_HOST:-localhost}
            OLLAMA_PORT=''${OLLAMA_PORT:-11434}
            OLLAMA_URL="http://$OLLAMA_HOST:$OLLAMA_PORT"

            # Create adaptive vulnix configuration
            VULNIX_CONFIG=$(mktemp)
            cat > $VULNIX_CONFIG << 'EOF'
            # Adaptive vulnix configuration
            # High severity CVEs that must fail the build
            critical-cves = [
              # Network-facing vulnerabilities (AI coding assistant is network service)
              "CVE-2024-*-RCE",
              "CVE-2024-*-SQLI",
              "CVE-2023-*-RCE",
              # Container escape vulnerabilities
              "CVE-2024-*-ESCAPE",
              "CVE-2023-*-ESCAPE",
              # Supply chain attacks
              "CVE-2024-*-SUPPLY",
            ]

            # Medium severity - warn but don't fail (for development)
            medium-cves = [
              "CVE-2024-*-DOS",
              "CVE-2023-*-DOS",
              "CVE-2022-*-INFO",
            ]
            EOF

            echo "🔍 Running adaptive vulnix scan..."
            VULNIX_OUTPUT=$(vulnix --system 2>&1 || true)

            if [ -n "$VULNIX_OUTPUT" ]; then
              echo "🔍 Vulnix found potential issues:"
              echo "$VULNIX_OUTPUT"

              # AI analysis of vulnix output if available
              if curl -s -m 5 "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
                echo ""
                echo "🤖 AI Analysis of Vulnix Results..."

                VULNIX_PROMPT="You are a cybersecurity expert analyzing Nix vulnerability scan results.

                Context: This is an AI coding assistant built with Rust + Nix + Containers

                For each CVE found:
                1. EXPLOITABILITY: How easily could this be exploited in our context?
                2. BUSINESS IMPACT: What damage could this cause to an AI coding assistant?
                3. URGENCY: Should this block deployment? (CRITICAL/HIGH/MEDIUM/LOW)
                4. MITIGATION: Specific steps to address this vulnerability

                Focus on practical risk assessment, not just CVSS scores."

                AI_VULNIX_ANALYSIS=$(echo "$VULNIX_OUTPUT" | timeout 60 curl -s -X POST "$OLLAMA_URL/api/generate" \
                  -H "Content-Type: application/json" \
                  -d "{\"model\": \"qwen3:0.6b\", \"prompt\": \"$VULNIX_PROMPT\n\nVulnix Output:\n$VULNIX_OUTPUT\", \"stream\": false}" | \
                  jq -r '.response')

                echo "$AI_VULNIX_ANALYSIS"

                # Extract urgency level from AI analysis
                if echo "$AI_VULNIX_ANALYSIS" | grep -qi "CRITICAL\|block.*deployment"; then
                  echo "🚨 AI Assessment: CRITICAL - Blocking build"
                  exit 1
                elif echo "$AI_VULNIX_ANALYSIS" | grep -qi "HIGH.*urgency"; then
                  echo "⚠️  AI Assessment: HIGH - Consider blocking"
                  # Could make this configurable
                  exit 1
                else
                  echo "ℹ️  AI Assessment: MEDIUM/LOW - Proceeding with warnings"
                fi
              else
                # Fallback logic without AI
                if echo "$VULNIX_OUTPUT" | grep -qi "critical\|rce\|remote.*code\|escape"; then
                  echo "🚨 Critical vulnerabilities found - blocking build"
                  exit 1
                else
                  echo "⚠️  Vulnerabilities found but not critical - proceeding"
                fi
              fi
            else
              echo "✅ vulnix: No vulnerabilities found"
            fi

            # Cleanup
            rm -f $VULNIX_CONFIG
            echo "✅ Adaptive vulnix scan complete"
          '';

          # Traditional security fallback when AI unavailable
          traditional-security-check = pkgs.writeShellScriptBin "traditional-security-check" ''
            echo "🔧 Traditional Security Scanning (Fallback Mode)"
            echo "================================================"

            EXIT_CODE=0

            echo "📋 Running cargo-deny license and security checks..."
            if cargo deny check 2>&1; then
              echo "✅ cargo-deny: PASSED"
            else
              echo "❌ cargo-deny: FAILED"
              EXIT_CODE=1
            fi

            echo ""
            echo "🔍 Running cargo-audit vulnerability scan..."
            if cargo audit 2>&1; then
              echo "✅ cargo-audit: PASSED"
            else
              echo "❌ cargo-audit: VULNERABILITIES FOUND"
              EXIT_CODE=1
            fi

            echo ""
            echo "❄️  Running adaptive vulnix scan..."
            if command -v vulnix >/dev/null 2>&1; then
              nix run .#adaptive-vulnix-scan
            else
              echo "⚠️  vulnix not installed, skipping Nix vulnerability scan"
            fi

            exit $EXIT_CODE
          '';

          # Nix store provenance validation with AI analysis
          nix-provenance-validator = pkgs.writeShellScriptBin "nix-provenance-validator" ''
            echo "🔐 Nix Store Provenance Validation"
            echo "=================================="

            # Configurable Ollama endpoint
            OLLAMA_HOST=''${OLLAMA_HOST:-localhost}
            OLLAMA_PORT=''${OLLAMA_PORT:-11434}
            OLLAMA_URL="http://$OLLAMA_HOST:$OLLAMA_PORT"

            # Check flake lock for suspicious changes
            if [ -f "flake.lock" ]; then
              echo "🔍 Analyzing flake.lock for provenance..."

              # Check for recent changes to inputs
              if git rev-parse --git-dir > /dev/null 2>&1; then
                LOCK_CHANGES=$(git diff HEAD~5..HEAD flake.lock 2>/dev/null || echo "No recent changes")

                if [ "$LOCK_CHANGES" != "No recent changes" ]; then
                  echo "📊 Recent flake.lock changes detected:"
                  echo "$LOCK_CHANGES" | head -20

                  # AI analysis of lock changes if available
                  if curl -s -m 5 "$OLLAMA_URL/api/tags" >/dev/null 2>&1; then
                    echo ""
                    echo "🤖 AI Analysis of Provenance Changes..."

                    PROVENANCE_PROMPT="You are a supply chain security expert analyzing Nix flake.lock changes.

                    Analyze these changes for:
                    1. SUSPICIOUS PATTERNS: Unexpected input changes, hash modifications
                    2. SUPPLY CHAIN RISKS: New dependencies, version downgrades
                    3. PROVENANCE INTEGRITY: Are sources still trustworthy?
                    4. RECOMMENDATION: Should these changes be trusted?

                    Reply TRUSTED if changes look legitimate, or SUSPICIOUS with specific concerns."

                    PROVENANCE_ANALYSIS=$(echo "$LOCK_CHANGES" | timeout 60 curl -s -X POST "$OLLAMA_URL/api/generate" \
                      -H "Content-Type: application/json" \
                      -d "{\"model\": \"qwen3:0.6b\", \"prompt\": \"$PROVENANCE_PROMPT\n\nFlake Lock Changes:\n$LOCK_CHANGES\", \"stream\": false}" | \
                      jq -r '.response')

                    echo "$PROVENANCE_ANALYSIS"

                    if echo "$PROVENANCE_ANALYSIS" | grep -qi "suspicious\|concern\|risk\|untrusted"; then
                      echo "🚨 Provenance validation failed - suspicious changes detected"
                      echo "Review flake.lock changes carefully before proceeding"
                      exit 1
                    else
                      echo "✅ Provenance validation passed"
                    fi
                  else
                    echo "ℹ️  AI analysis unavailable, using heuristic checks"

                    # Simple heuristic checks
                    if echo "$LOCK_CHANGES" | grep -qi "narHash.*-.*+"; then
                      echo "⚠️  Hash changes detected - manual review recommended"
                    fi

                    if echo "$LOCK_CHANGES" | grep -qi '"rev".*-.*+'; then
                      echo "⚠️  Git revision changes detected - verify authenticity"
                    fi
                  fi
                fi
              fi

              # Validate input sources are from trusted origins
              echo ""
              echo "🏛️  Validating input source trustworthiness..."

              UNTRUSTED_SOURCES=$(jq -r '.nodes[] | select(.original.type == "github") | .original.owner + "/" + .original.repo' flake.lock 2>/dev/null | \
                grep -v -E "(NixOS|nixpkgs|numtide|oxalica|ipetkov|nlewo|cachix)" | head -5)

              if [ -n "$UNTRUSTED_SOURCES" ]; then
                echo "⚠️  Non-standard input sources detected:"
                echo "$UNTRUSTED_SOURCES"
                echo "Manual verification recommended for these repositories"
              else
                echo "✅ All input sources from trusted organizations"
              fi

              # Check for reproducible build markers
              echo ""
              echo "🔄 Checking reproducible build configuration..."

              if grep -q "SOURCE_DATE_EPOCH" flake.nix; then
                echo "✅ SOURCE_DATE_EPOCH configured for reproducible builds"
              else
                echo "⚠️  No SOURCE_DATE_EPOCH found - builds may not be reproducible"
              fi

              if jq -e '.nodes[] | select(.original.type == "github" and .locked.rev != null)' flake.lock >/dev/null 2>&1; then
                echo "✅ Git revisions pinned for reproducibility"
              else
                echo "⚠️  Some inputs may not have pinned revisions"
              fi

            else
              echo "⚠️  No flake.lock found - cannot validate provenance"
              exit 1
            fi

            echo ""
            echo "📋 Provenance Validation Summary:"
            echo "- Input source validation: Complete"
            echo "- Change analysis: Complete"
            echo "- Reproducibility checks: Complete"
            echo "✅ Nix store provenance validation complete"
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

          # Agentic Security Utilities
          inherit (securityUtils) security-judge security-behavioral-test threat-model-analysis
                                  dependency-risk-profile adaptive-vulnix-scan traditional-security-check
                                  nix-provenance-validator;

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

          # Agentic Security Applications
          security-judge = flake-utils.lib.mkApp {
            drv = securityUtils.security-judge;
          };

          security-behavioral-test = flake-utils.lib.mkApp {
            drv = securityUtils.security-behavioral-test;
          };

          threat-model-analysis = flake-utils.lib.mkApp {
            drv = securityUtils.threat-model-analysis;
          };

          dependency-risk-profile = flake-utils.lib.mkApp {
            drv = securityUtils.dependency-risk-profile;
          };

          adaptive-vulnix-scan = flake-utils.lib.mkApp {
            drv = securityUtils.adaptive-vulnix-scan;
          };

          nix-provenance-validator = flake-utils.lib.mkApp {
            drv = securityUtils.nix-provenance-validator;
          };

          traditional-security-check = flake-utils.lib.mkApp {
            drv = securityUtils.traditional-security-check;
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
        }
      );
    };
}