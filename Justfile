# Nanna-coder dev task runner
# Install just: cargo binstall just  (or via nix develop)
# Usage: just <recipe>

# Default: list available recipes
default:
    @just --list

# Run all workspace tests
test:
    cargo nextest run --workspace --all-features

# Run unit tests only
test-unit:
    cargo nextest run --workspace --lib --all-features

# Run integration tests only
test-integration:
    cargo nextest run --workspace --test '*' --all-features

# Lint with clippy (deny warnings)
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Auto-format code
fmt:
    cargo fmt --all

# Run all lint + format checks (CI-style)
check: lint fmt-check
    cargo doc --no-deps --workspace

# Security: cargo-deny (licenses, advisories, bans)
deny:
    cargo deny check

# Security: cargo-audit (advisory DB)
audit:
    cargo audit

# Coverage report (requires cargo-tarpaulin)
coverage:
    cargo tarpaulin --skip-clean --ignore-tests --out Lcov --output-dir .

# Full CI pipeline: check + test + security + coverage
ci: check test deny coverage

# Setup dev deps without Nix
setup:
    ./scripts/setup-dev.sh

# Setup CI-only deps without Nix
setup-ci:
    ./scripts/setup-dev.sh --ci

# Build the workspace in release mode
build-release:
    cargo build --workspace --release

# Clean build artifacts
clean:
    cargo clean
