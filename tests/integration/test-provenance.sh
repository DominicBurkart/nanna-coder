#!/usr/bin/env bash
# Provenance validation tests

set -e

# Load test helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/test-helpers.sh"

echo "🔐 Provenance Validation"
echo "========================"

# Provenance validation
run_test "Nix Provenance Validation" "nix run .#nix-provenance-validator" "true"

print_test_summary
exit_with_results
