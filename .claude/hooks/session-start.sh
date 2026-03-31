#!/bin/bash
set -euo pipefail

# Only run in remote (Claude Code on the web) environments
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

# Install Nix if not present
if ! command -v nix &>/dev/null; then
  sh <(curl --proto '=https' --tlsv1.2 -L https://nixos.org/nix/install) --daemon --yes
  # shellcheck source=/dev/null
  . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
fi

# Warm the devShell so nix develop is fast on first use
nix develop --command true

# Make nix available for the rest of the session
echo '. /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh' >> "$CLAUDE_ENV_FILE"
