#!/usr/bin/env bash
# AI-powered security analysis tests

set -e

# Load test helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/test-helpers.sh"

echo "🤖 AI-Powered Security Analysis"
echo "==============================="

# Check if Ollama is running
if curl -s http://localhost:11434/api/tags >/dev/null 2>&1; then
    echo -e "${GREEN}✅ Ollama service detected${NC}"

    # AI security analysis
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

print_test_summary
exit_with_results
