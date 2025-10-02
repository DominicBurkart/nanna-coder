#!/usr/bin/env bash
# Build system integration tests

set -e

# Load test helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/test-helpers.sh"

echo "🏗️  Build System Integration"
echo "============================"

# Development shell integration
if [ -f ".git/hooks/pre-commit" ]; then
    echo -e "${GREEN}✅ Pre-commit hook installed${NC}"
else
    echo -e "${YELLOW}⚠️  Pre-commit hook not found${NC}"
    echo "It should be installed automatically when entering nix develop"
fi

# Build system checks
run_test "Nix Flake Check" "nix flake check --no-build" "true"
run_test "Security Apps Listed" "nix flake show | grep -q security-judge" "true"

print_test_summary
exit_with_results
