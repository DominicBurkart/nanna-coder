#!/usr/bin/env bash
# Dependency check tests for security tooling

set -e

# Load test helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/test-helpers.sh"

echo "📋 Dependency Check"
echo "==================="

# Check core dependencies
check_dependency "nix" || exit 1
check_dependency "jq" || exit 1
check_dependency "curl" || exit 1
check_dependency "cargo" || exit 1
check_dependency "podman" "podman" || exit 1
check_dependency "vulnix" "vulnix" || exit 1

echo -e "${GREEN}✅ All dependencies available${NC}"
