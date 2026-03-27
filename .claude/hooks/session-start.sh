#!/bin/bash
set -euo pipefail

# Only run in remote (Claude Code on the web) environments
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

# Install Rust dev dependencies via cargo-binstall.
# Tool list is kept in sync with nix/dev-shell.nix and .github/workflows/ci.yml.
exec "$CLAUDE_PROJECT_DIR/scripts/setup-dev.sh"
