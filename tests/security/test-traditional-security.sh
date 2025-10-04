#!/usr/bin/env bash
# Traditional security tools tests

set -e

# Load test helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/test-helpers.sh"

echo "🔒 Traditional Security Tools"
echo "============================="

# Traditional security tools
run_test "Cargo Deny Check" "cargo deny check" "true"
run_test "Cargo Audit Check" "cargo audit" "true"

if command -v vulnix &> /dev/null; then
    run_test "Vulnix System Scan" "vulnix --system | head -10" "true"
else
    echo -e "${YELLOW}⚠️  Skipping Vulnix tests (not available)${NC}"
fi

print_test_summary
exit_with_results
