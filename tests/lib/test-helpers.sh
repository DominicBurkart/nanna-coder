#!/usr/bin/env bash
# Test helper functions for nanna-coder test suite

# Colors for output
export RED='\033[0;31m'
export GREEN='\033[0;32m'
export YELLOW='\033[1;33m'
export BLUE='\033[0;34m'
export NC='\033[0m' # No Color

# Test results tracking
export TESTS_PASSED=0
export TESTS_FAILED=0
export TOTAL_TESTS=0

# Run a test and track results
run_test() {
    local test_name="$1"
    local test_command="$2"
    local should_pass="$3"  # true/false

    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    echo -e "${BLUE}🧪 Test $TOTAL_TESTS: $test_name${NC}"
    echo "   Command: $test_command"

    if eval "$test_command" &>/dev/null; then
        if [[ "$should_pass" == "true" ]]; then
            echo -e "   ${GREEN}✅ PASSED${NC}"
            TESTS_PASSED=$((TESTS_PASSED + 1))
        else
            echo -e "   ${RED}❌ FAILED (expected failure but passed)${NC}"
            TESTS_FAILED=$((TESTS_FAILED + 1))
        fi
    else
        if [[ "$should_pass" == "false" ]]; then
            echo -e "   ${GREEN}✅ PASSED (expected failure)${NC}"
            TESTS_PASSED=$((TESTS_PASSED + 1))
        else
            echo -e "   ${RED}❌ FAILED${NC}"
            TESTS_FAILED=$((TESTS_FAILED + 1))
        fi
    fi
    echo ""
}

# Check if a dependency is available
check_dependency() {
    local dep="$1"
    local package="$2"

    if command -v "$dep" &> /dev/null; then
        echo -e "${GREEN}✅ $dep available${NC}"
        return 0
    else
        echo -e "${RED}❌ $dep not available${NC}"
        if [ -n "$package" ]; then
            echo "   Install with: nix-env -iA nixpkgs.$package"
        fi
        return 1
    fi
}

# Print test summary
print_test_summary() {
    echo ""
    echo "📊 Test Results Summary"
    echo "======================="
    echo -e "Total Tests: $TOTAL_TESTS"
    echo -e "${GREEN}Passed: $TESTS_PASSED${NC}"
    echo -e "${RED}Failed: $TESTS_FAILED${NC}"
}

# Exit with appropriate code based on test results
exit_with_results() {
    if [ $TESTS_FAILED -eq 0 ]; then
        echo ""
        echo -e "${GREEN}🎉 All tests passed!${NC}"
        exit 0
    else
        echo ""
        echo -e "${RED}⚠️  Some tests failed. Check the output above for details.${NC}"
        exit 1
    fi
}
