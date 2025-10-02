#!/usr/bin/env bash
# Environment setup tests

set -e

# Load test helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/test-helpers.sh"

echo "🔧 Environment Setup"
echo "===================="

# Check if we're in a Nix development shell
if [ -n "$IN_NIX_SHELL" ]; then
    echo -e "${GREEN}✅ Running in Nix development shell${NC}"
else
    echo -e "${RED}❌ Not in Nix development shell${NC}"
    echo "Run: nix develop"
    echo "Then run this script again"
    exit 1
fi

# Check if we're in the right directory
if [ ! -f "flake.nix" ] || [ ! -f "deny.toml" ]; then
    echo -e "${RED}❌ Not in project root directory${NC}"
    echo "Please run from the directory containing flake.nix and deny.toml"
    exit 1
fi

echo -e "${GREEN}✅ Environment setup complete${NC}"
