# Development shell configuration
# This module contains:
# - Development tools and dependencies
# - Shell environment variables
# - Git hooks setup
# - Shell aliases and initialization

{ pkgs
, lib
, rustToolchain
, self
, nixpkgs
}:

pkgs.mkShell {
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

    # Security review with Claude
    bash scripts/hooks/security-review.sh

    # Coverage check with comparison to main
    echo "📊 Checking test coverage..."
    if command -v cargo-tarpaulin >/dev/null 2>&1; then
      NEW=$(cargo tarpaulin --skip-clean --ignore-tests --out Stdout 2>&1 | grep -oP '\d+\.\d+(?=% coverage)' || echo "0.0")

      # Get main branch coverage (if possible)
      git stash -q 2>/dev/null || true
      if git checkout main -q 2>/dev/null; then
        OLD=$(cargo tarpaulin --skip-clean --ignore-tests --out Stdout 2>&1 | grep -oP '\d+\.\d+(?=% coverage)' || echo "0.0")
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
}
