#!/usr/bin/env bash
# Setup dev dependencies for nanna-coder without Nix.
# Works in CI, Claude background agents, or any rustup-based environment.
#
# Usage:
#   ./scripts/setup-dev.sh          # install all dev tools
#   ./scripts/setup-dev.sh --ci     # install only CI-essential tools
set -euo pipefail

CI_ONLY=false
if [[ "${1:-}" == "--ci" ]]; then
  CI_ONLY=true
fi

# Ensure rustup is available (rust-toolchain.toml handles the version)
if ! command -v rustup &>/dev/null; then
  echo "Installing rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
  # shellcheck source=/dev/null
  source "$HOME/.cargo/env"
fi

# Trigger rust-toolchain.toml auto-install
echo "Syncing Rust toolchain from rust-toolchain.toml..."
rustup show active-toolchain

# Install cargo-binstall for fast binary installs (no compilation)
if ! command -v cargo-binstall &>/dev/null; then
  echo "Installing cargo-binstall..."
  curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
fi

# CI-essential tools — keep in sync with nix/dev-shell.nix
echo "Installing CI tools..."
cargo binstall --no-confirm \
  cargo-nextest \
  cargo-deny \
  cargo-audit \
  cargo-tarpaulin

if [[ "$CI_ONLY" == "false" ]]; then
  echo "Installing additional dev tools..."
  cargo binstall --no-confirm \
    cargo-watch \
    cargo-edit \
    cargo-expand \
    cargo-udeps \
    cargo-machete \
    cargo-outdated \
    just
fi

echo "Dev environment ready."
