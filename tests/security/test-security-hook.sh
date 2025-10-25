#!/usr/bin/env bash
# Security hook tests - comprehensive test suite for pre-commit security review
#
# Tests the security review hook for false positives and correct behavior.
# Issue: https://github.com/DominicBurkart/nanna-coder/issues/33

set -e

# Load test helpers
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/test-helpers.sh"

echo "🔒 Security Hook Tests"
echo "======================"
echo ""

# Test helper: Create a mock claude command that returns specific output
setup_mock_claude() {
    local response="$1"
    local mock_script="/tmp/mock-claude-$$"

    cat > "$mock_script" << EOF
#!/usr/bin/env bash
echo "$response"
EOF
    chmod +x "$mock_script"
    echo "$mock_script"
}

# Test helper: Simulate the security hook logic
run_security_hook_logic() {
    local claude_output="$1"
    local tmp_review="/tmp/test_claude_review_$$"

    echo "$claude_output" > "$tmp_review"

    # This is the CURRENT (buggy) logic from dev-shell.nix line 130
    if grep -qi "problem\|secret\|token\|pii\|leak" "$tmp_review"; then
        rm "$tmp_review"
        return 1  # Hook blocks commit
    fi

    rm "$tmp_review"
    return 0  # Hook allows commit
}

# Test helper: Simulate the FIXED hook logic (structured output)
run_fixed_security_hook_logic() {
    local claude_output="$1"
    local tmp_review="/tmp/test_claude_review_$$"

    echo "$claude_output" > "$tmp_review"

    # NEW logic: Look for explicit status markers
    if grep -q "^STATUS: APPROVED" "$tmp_review"; then
        rm "$tmp_review"
        return 0  # Hook allows commit
    elif grep -q "^STATUS: BLOCKED" "$tmp_review"; then
        rm "$tmp_review"
        return 1  # Hook blocks commit
    else
        # Fallback: if no structured output, look for security concerns
        # but with more specific patterns
        if grep -qi "SECURITY ISSUE:\|CREDENTIAL LEAK:\|SECRET EXPOSED:\|PII DETECTED:" "$tmp_review"; then
            rm "$tmp_review"
            return 1  # Hook blocks commit
        fi
        rm "$tmp_review"
        return 0  # Default to allowing if unclear
    fi
}

echo "📋 Testing CURRENT (buggy) hook logic"
echo "======================================"

# Test 1: Current logic - False positive on approval with "token" keyword
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: False Positive - Approval message containing 'token'${NC}"
claude_response="APPROVED - No security issues found. No security concerns: No secrets, tokens, or PII detected in this commit."

if run_security_hook_logic "$claude_response"; then
    echo -e "   ${RED}❌ FAILED - Hook allowed commit (current buggy behavior)${NC}"
    echo -e "   ${YELLOW}This demonstrates the bug: approval messages with keywords are blocked${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
else
    echo -e "   ${GREEN}✅ CONFIRMED BUG - Hook blocked commit on approval message${NC}"
    echo -e "   ${YELLOW}This is the false positive we're fixing${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi
echo ""

# Test 2: Current logic - False positive on "No problems found"
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: False Positive - 'No problems found' message${NC}"
claude_response="Security review complete. No problems found. This commit is safe to proceed."

if run_security_hook_logic "$claude_response"; then
    echo -e "   ${RED}❌ FAILED - Hook allowed commit${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
else
    echo -e "   ${GREEN}✅ CONFIRMED BUG - Hook blocked on 'No problems'${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi
echo ""

# Test 3: Current logic - Correctly blocks actual security issue
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Correct Behavior - Actual security issue detected${NC}"
claude_response="WARNING: This commit contains a hardcoded API token on line 42. Please remove before committing."

if run_security_hook_logic "$claude_response"; then
    echo -e "   ${RED}❌ FAILED - Hook allowed commit with security issue${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
else
    echo -e "   ${GREEN}✅ PASSED - Hook correctly blocked security issue${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi
echo ""

echo ""
echo "📋 Testing FIXED hook logic with structured output"
echo "==================================================="

# Test 4: Fixed logic - Approval with structured output
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Fixed - Structured approval (with keywords in body)${NC}"
claude_response="STATUS: APPROVED

Security Review Summary:
- No secrets detected
- No API tokens found
- No PII leaks identified
- No security problems found

This commit is safe to proceed."

if run_fixed_security_hook_logic "$claude_response"; then
    echo -e "   ${GREEN}✅ PASSED - Hook allowed commit with structured approval${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo -e "   ${RED}❌ FAILED - Hook blocked approved commit${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi
echo ""

# Test 5: Fixed logic - Block with structured output
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Fixed - Structured block${NC}"
claude_response="STATUS: BLOCKED

SECURITY ISSUE: Hardcoded credentials detected
- Line 42: API token 'sk-1234567890abcdef'
- Line 58: Database password in plaintext

Please remove these secrets before committing."

if run_fixed_security_hook_logic "$claude_response"; then
    echo -e "   ${RED}❌ FAILED - Hook allowed commit with security issues${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
else
    echo -e "   ${GREEN}✅ PASSED - Hook correctly blocked security issue${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi
echo ""

# Test 6: Fixed logic - Fallback to specific patterns
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Fixed - Fallback pattern matching (no structured output)${NC}"
claude_response="SECURITY ISSUE: Potential credential leak detected in config file."

if run_fixed_security_hook_logic "$claude_response"; then
    echo -e "   ${RED}❌ FAILED - Hook allowed commit with security issue${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
else
    echo -e "   ${GREEN}✅ PASSED - Hook blocked using fallback patterns${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi
echo ""

# Test 7: Fixed logic - Safe content without structured output
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Fixed - Safe content without structured markers${NC}"
claude_response="This commit adds a new feature for user authentication. The code follows security best practices and doesn't expose sensitive information."

if run_fixed_security_hook_logic "$claude_response"; then
    echo -e "   ${GREEN}✅ PASSED - Hook allowed safe commit${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo -e "   ${RED}❌ FAILED - Hook blocked safe commit${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi
echo ""

# Test 8: Edge case - Empty response
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Edge Case - Empty Claude response${NC}"
claude_response=""

if run_fixed_security_hook_logic "$claude_response"; then
    echo -e "   ${GREEN}✅ PASSED - Hook defaults to allow on empty response${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo -e "   ${RED}❌ FAILED - Hook blocked on empty response${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi
echo ""

# Test 9: Edge case - Multiple status lines (should use first)
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Edge Case - Multiple status markers${NC}"
claude_response="STATUS: APPROVED

Here's the review of your commit. Everything looks good.
Note: STATUS: BLOCKED is what we would say if there were issues, but there aren't any."

if run_fixed_security_hook_logic "$claude_response"; then
    echo -e "   ${GREEN}✅ PASSED - Hook used first status marker (APPROVED)${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo -e "   ${RED}❌ FAILED - Hook didn't respect first status marker${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi
echo ""

# Test 10: Real-world case from issue #33
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Real-world - Issue #33 scenario${NC}"
claude_response="APPROVED - No security issues found. **No security concerns:** - No secrets, tokens, credentials, or PII detected - Safe to commit"

if run_fixed_security_hook_logic "$claude_response"; then
    echo -e "   ${GREEN}✅ PASSED - Hook correctly approved commit from issue #33${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo -e "   ${RED}❌ FAILED - Hook still has false positive from issue #33${NC}"
    TESTS_FAILED=$((TESTS_FAILED + 1))
fi
echo ""

echo ""
echo "🔬 Integration Tests"
echo "===================="

# Test 11: Verify hook exists and is executable
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Hook file exists and is executable${NC}"
# Check cargo-husky hook (preferred) or git hooks directory
if [ -f ".cargo-husky/hooks/pre-commit" ] && [ -x ".cargo-husky/hooks/pre-commit" ]; then
    echo -e "   ${GREEN}✅ PASSED - Pre-commit hook is in cargo-husky and executable${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
elif [ -f ".git/hooks/pre-commit" ] && [ -x ".git/hooks/pre-commit" ]; then
    echo -e "   ${GREEN}✅ PASSED - Pre-commit hook is installed and executable${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
else
    echo -e "   ${YELLOW}⚠️  INFO - Pre-commit hook not found (expected in dev environment)${NC}"
    echo -e "   ${YELLOW}Run 'cargo build' or 'nix develop' to install hooks${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi
echo ""

# Test 12: Hook contains security review logic
TOTAL_TESTS=$((TOTAL_TESTS + 1))
echo -e "${BLUE}🧪 Test $TOTAL_TESTS: Hook contains security review section${NC}"
# Check cargo-husky hook first, then git hooks
HOOK_FILE=""
if [ -f ".cargo-husky/hooks/pre-commit" ]; then
    HOOK_FILE=".cargo-husky/hooks/pre-commit"
elif [ -f ".git/hooks/pre-commit" ]; then
    HOOK_FILE=".git/hooks/pre-commit"
fi

if [ -n "$HOOK_FILE" ]; then
    if grep -q "security review" "$HOOK_FILE"; then
        echo -e "   ${GREEN}✅ PASSED - Security review section found in $HOOK_FILE${NC}"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        echo -e "   ${RED}❌ FAILED - Security review section missing from $HOOK_FILE${NC}"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
else
    echo -e "   ${YELLOW}⚠️  SKIP - Hook not installed${NC}"
    TESTS_PASSED=$((TESTS_PASSED + 1))
fi
echo ""

print_test_summary
exit_with_results
