#!/usr/bin/env bash
# Main test runner for nanna-coder project
# Runs all test suites in a modular fashion

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Load test helpers
source "$SCRIPT_DIR/lib/test-helpers.sh"

echo "🚀 Nanna Coder Test Suite"
echo "=================================="
echo ""

# Track overall results
OVERALL_FAILED=0

# Change to project root
cd "$PROJECT_ROOT"

# Run dependency checks
echo ""
echo "======================================"
if bash "$SCRIPT_DIR/security/test-dependencies.sh"; then
    echo -e "${GREEN}✅ Dependency checks passed${NC}"
else
    echo -e "${RED}❌ Dependency checks failed${NC}"
    OVERALL_FAILED=1
fi

# Run environment checks
echo ""
echo "======================================"
if bash "$SCRIPT_DIR/security/test-environment.sh"; then
    echo -e "${GREEN}✅ Environment checks passed${NC}"
else
    echo -e "${RED}❌ Environment checks failed${NC}"
    OVERALL_FAILED=1
fi

# Run security tool availability tests
echo ""
echo "======================================"
if bash "$SCRIPT_DIR/security/test-tools-availability.sh"; then
    echo -e "${GREEN}✅ Tool availability tests passed${NC}"
else
    echo -e "${RED}❌ Tool availability tests failed${NC}"
    OVERALL_FAILED=1
fi

# Run traditional security tests
echo ""
echo "======================================"
if bash "$SCRIPT_DIR/security/test-traditional-security.sh"; then
    echo -e "${GREEN}✅ Traditional security tests passed${NC}"
else
    echo -e "${RED}❌ Traditional security tests failed${NC}"
    OVERALL_FAILED=1
fi

# Run behavioral security tests
echo ""
echo "======================================"
if bash "$SCRIPT_DIR/security/test-behavioral-security.sh"; then
    echo -e "${GREEN}✅ Behavioral security tests passed${NC}"
else
    echo -e "${RED}❌ Behavioral security tests failed${NC}"
    OVERALL_FAILED=1
fi

# Run AI security tests
echo ""
echo "======================================"
if bash "$SCRIPT_DIR/security/test-ai-security.sh"; then
    echo -e "${GREEN}✅ AI security tests passed${NC}"
else
    echo -e "${RED}❌ AI security tests failed${NC}"
    OVERALL_FAILED=1
fi

# Run provenance tests
echo ""
echo "======================================"
if bash "$SCRIPT_DIR/integration/test-provenance.sh"; then
    echo -e "${GREEN}✅ Provenance tests passed${NC}"
else
    echo -e "${RED}❌ Provenance tests failed${NC}"
    OVERALL_FAILED=1
fi

# Run build system tests
echo ""
echo "======================================"
if bash "$SCRIPT_DIR/integration/test-build-system.sh"; then
    echo -e "${GREEN}✅ Build system tests passed${NC}"
else
    echo -e "${RED}❌ Build system tests failed${NC}"
    OVERALL_FAILED=1
fi

# Final summary
echo ""
echo "======================================"
echo "📊 Overall Test Summary"
echo "======================================"

if [ $OVERALL_FAILED -eq 0 ]; then
    echo ""
    echo -e "${GREEN}🎉 All test suites passed!${NC}"
    echo ""
    echo "Next steps:"
    echo "1. Test with CI: git push to trigger GitHub Actions"
    echo "2. Start Ollama for full AI features: nix run .#container-dev"
    echo "3. Run security analysis: nix run .#security-judge"
    exit 0
else
    echo ""
    echo -e "${RED}⚠️  Some test suites failed. Check the output above for details.${NC}"
    echo ""
    echo "Common issues:"
    echo "- Missing dependencies: run 'nix develop'"
    echo "- Ollama not running: run 'nix run .#container-dev'"
    echo "- Network connectivity issues"
    exit 1
fi
