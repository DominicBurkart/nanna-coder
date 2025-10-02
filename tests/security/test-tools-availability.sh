#!/usr/bin/env bash
# Security tools availability tests

set -e

# Load test helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/test-helpers.sh"

echo "🧪 Security Tool Availability Tests"
echo "==================================="

# Check if security utilities are available
run_test "Security Judge Available" "nix run .#security-judge --help" "true"
run_test "Behavioral Test Available" "nix run .#security-behavioral-test --help" "true"
run_test "Threat Model Analysis Available" "nix run .#threat-model-analysis --help" "true"
run_test "Dependency Risk Profile Available" "nix run .#dependency-risk-profile --help" "true"
run_test "Adaptive Vulnix Available" "nix run .#adaptive-vulnix-scan --help" "true"
run_test "Provenance Validator Available" "nix run .#nix-provenance-validator --help" "true"
run_test "Traditional Security Available" "nix run .#traditional-security-check --help" "true"

print_test_summary
exit_with_results
