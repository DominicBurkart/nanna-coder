#!/usr/bin/env bash
# Behavioral security testing

set -e

# Load test helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/test-helpers.sh"

echo "🧪 Behavioral Security Testing"
echo "==============================="

# Behavioral security tests (these should work without Ollama)
echo -e "${BLUE}Running behavioral security test (may take 2-3 minutes)...${NC}"
if timeout 300 nix run .#security-behavioral-test; then
    echo -e "${GREEN}✅ Behavioral security test completed${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo -e "${YELLOW}⚠️  Behavioral security test timed out or failed${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi
TOTAL_TESTS=$((TOTAL_TESTS + 1))

print_test_summary
exit_with_results
