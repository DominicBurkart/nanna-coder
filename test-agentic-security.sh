#!/usr/bin/env bash
set -e

echo "🚀 Agentic Security Testing Script"
echo "=================================="
echo ""

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test results tracking
TESTS_PASSED=0
TESTS_FAILED=0
TOTAL_TESTS=0

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

check_dependency() {
    local dep="$1"
    local package="$2"

    if command -v "$dep" &> /dev/null; then
        echo -e "${GREEN}✅ $dep available${NC}"
        return 0
    else
        echo -e "${YELLOW}⚠️  $dep not available${NC}"
        if [ -n "$package" ]; then
            echo "   Install with: nix-env -iA nixpkgs.$package"
        fi
        return 1
    fi
}

echo "📋 Dependency Check"
echo "==================="

# Check core dependencies
check_dependency "nix" || exit 1
check_dependency "jq" || exit 1
check_dependency "curl" || exit 1
check_dependency "cargo" || exit 1
check_dependency "podman" "podman" || exit 1
check_dependency "vulnix" "vulnix" || exit 1

echo ""
echo "🔧 Environment Setup"
echo "===================="

# Check if we're in a Nix development shell
if [ -n "$IN_NIX_SHELL" ]; then
    echo -e "${GREEN}✅ Running in Nix development shell${NC}"
else
    echo -e "${YELLOW}⚠️  Not in Nix development shell, entering...${NC}"
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
echo ""

echo "🧪 Security Tool Availability Tests"
echo "==================================="

# Test 1: Check if security utilities are available
run_test "Security Judge Available" "nix run .#security-judge --help" "true"
run_test "Behavioral Test Available" "nix run .#security-behavioral-test --help" "true"
run_test "Threat Model Analysis Available" "nix run .#threat-model-analysis --help" "true"
run_test "Dependency Risk Profile Available" "nix run .#dependency-risk-profile --help" "true"
run_test "Adaptive Vulnix Available" "nix run .#adaptive-vulnix-scan --help" "true"
run_test "Provenance Validator Available" "nix run .#nix-provenance-validator --help" "true"
run_test "Traditional Security Available" "nix run .#traditional-security-check --help" "true"

echo "🔒 Traditional Security Tools"
echo "============================="

# Test 2: Traditional security tools
run_test "Cargo Deny Check" "cargo deny check" "true"
run_test "Cargo Audit Check" "cargo audit" "true"

if command -v vulnix &> /dev/null; then
    run_test "Vulnix System Scan" "vulnix --system | head -10" "true"
else
    echo -e "${YELLOW}⚠️  Skipping Vulnix tests (not available)${NC}"
fi

echo "🧪 Behavioral Security Testing"
echo "==============================="

# Test 3: Behavioral security tests (these should work without Ollama)
echo -e "${BLUE}Running behavioral security test (may take 2-3 minutes)...${NC}"
if timeout 300 nix run .#security-behavioral-test; then
    echo -e "${GREEN}✅ Behavioral security test completed${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo -e "${YELLOW}⚠️  Behavioral security test timed out or failed${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi
TOTAL_TESTS=$((TOTAL_TESTS + 1))

echo ""
echo "🤖 AI-Powered Security Analysis"
echo "==============================="

# Check if Ollama is running
if curl -s http://localhost:11434/api/tags >/dev/null 2>&1; then
    echo -e "${GREEN}✅ Ollama service detected${NC}"

    # Test 4: AI security analysis
    run_test "AI Security Judge" "timeout 180 nix run .#security-judge" "true"
    run_test "AI Threat Model Analysis" "timeout 120 nix run .#threat-model-analysis" "true"
    run_test "AI Dependency Risk Profile" "timeout 120 nix run .#dependency-risk-profile" "true"
    run_test "AI Adaptive Vulnix Scan" "timeout 180 nix run .#adaptive-vulnix-scan" "true"

else
    echo -e "${YELLOW}⚠️  Ollama not running - AI tests will use fallback mode${NC}"
    echo "To test AI features:"
    echo "1. Start Ollama: nix run .#container-dev"
    echo "2. Wait for service to be ready"
    echo "3. Re-run this script"
    echo ""

    # Test fallback modes
    run_test "Security Judge (Fallback)" "timeout 60 nix run .#security-judge" "true"
    run_test "Threat Model (Fallback)" "timeout 60 nix run .#threat-model-analysis" "true"
    run_test "Vulnix Adaptive (Fallback)" "timeout 60 nix run .#adaptive-vulnix-scan" "true"
fi

echo "🔐 Provenance Validation"
echo "========================"

# Test 5: Provenance validation
run_test "Nix Provenance Validation" "nix run .#nix-provenance-validator" "true"

echo "🏗️  Build System Integration"
echo "============================"

# Test 6: Development shell integration
if [ -f ".git/hooks/pre-commit" ]; then
    echo -e "${GREEN}✅ Pre-commit hook installed${NC}"
else
    echo -e "${YELLOW}⚠️  Pre-commit hook not found${NC}"
    echo "It should be installed automatically when entering nix develop"
fi

# Test 7: Build system checks
run_test "Nix Flake Check" "nix flake check --no-build" "true"
run_test "Security Apps Listed" "nix flake show | grep -q security-judge" "true"

echo ""
echo "📊 Test Results Summary"
echo "======================="
echo -e "Total Tests: $TOTAL_TESTS"
echo -e "${GREEN}Passed: $TESTS_PASSED${NC}"
echo -e "${RED}Failed: $TESTS_FAILED${NC}"

if [ $TESTS_FAILED -eq 0 ]; then
    echo ""
    echo -e "${GREEN}🎉 All tests passed! Agentic security system is working correctly.${NC}"
    echo ""
    echo "Next steps:"
    echo "1. Test with CI: git push to trigger GitHub Actions"
    echo "2. Start Ollama for full AI features: nix run .#container-dev"
    echo "3. Run security analysis: nix run .#security-judge"
    exit 0
else
    echo ""
    echo -e "${YELLOW}⚠️  Some tests failed. Check the output above for details.${NC}"
    echo ""
    echo "Common issues:"
    echo "- Missing dependencies: run 'nix develop'"
    echo "- Ollama not running: run 'nix run .#container-dev'"
    echo "- Network connectivity issues"
    exit 1
fi