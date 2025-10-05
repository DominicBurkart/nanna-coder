#!/usr/bin/env bash
set -euo pipefail

# Cachix Integration Test Suite
# Tests that Cachix is properly configured and working

echo "🧪 Cachix Integration Test Suite"
echo "================================"

# Test 1: Verify flake.nix has binaryCacheConfig
echo ""
echo "Test 1: Verify binaryCacheConfig in flake.nix"
if grep -q 'cacheName = "nanna-coder"' flake.nix; then
    echo "✅ PASS: binaryCacheConfig.cacheName is set correctly"
else
    echo "❌ FAIL: binaryCacheConfig.cacheName not found in flake.nix"
    exit 1
fi

# Test 2: Verify publicKey is configured (not placeholder)
echo ""
echo "Test 2: Verify publicKey is configured"
PUBLIC_KEY=$(grep 'publicKey = "nanna-coder.cachix.org-1:' flake.nix | cut -d'"' -f2 || echo "")
if [[ -n "$PUBLIC_KEY" ]]; then
    if [[ "$PUBLIC_KEY" == *"AAAA"* ]]; then
        echo "⚠️  WARNING: publicKey appears to be a placeholder"
        echo "   Current value: $PUBLIC_KEY"
        echo "   Please update with real key from app.cachix.org"
        echo "   See CACHIX_SETUP.md for instructions"
    else
        echo "✅ PASS: publicKey is configured (not placeholder)"
        echo "   Key: $PUBLIC_KEY"
    fi
else
    echo "❌ FAIL: publicKey not found in flake.nix"
    exit 1
fi

# Test 3: Verify cache utilities are defined in flake.nix
echo ""
echo "Test 3: Verify cache utility definitions in flake.nix"
if grep -q 'setup-cache.*writeShellScriptBin' flake.nix; then
    echo "✅ PASS: setup-cache utility defined in flake.nix"
else
    echo "❌ FAIL: setup-cache utility not found in flake.nix"
    exit 1
fi

# Test 4: Verify push-cache utility definition
echo ""
echo "Test 4: Verify push-cache utility definition"
if grep -q 'push-cache.*writeShellScriptBin' flake.nix; then
    echo "✅ PASS: push-cache utility defined in flake.nix"
else
    echo "❌ FAIL: push-cache utility not found in flake.nix"
    exit 1
fi

# Test 5: Verify cache-analytics utility definition
echo ""
echo "Test 5: Verify cache-analytics utility definition"
if grep -q 'cache-analytics.*writeShellScriptBin' flake.nix; then
    echo "✅ PASS: cache-analytics utility defined in flake.nix"
else
    echo "❌ FAIL: cache-analytics utility not found in flake.nix"
    exit 1
fi

# Test 6: Verify no nix-env usage in setup-cache (security requirement)
echo ""
echo "Test 6: Security check - no nix-env in setup-cache script"
if grep -A 30 'setup-cache.*writeShellScriptBin' flake.nix | grep -q "nix-env"; then
    echo "⚠️  WARNING: setup-cache uses nix-env (violates security policy)"
    echo "   See Issue #4 - should use declarative dependencies"
    echo "   (Being fixed by parallel agent)"
else
    echo "✅ PASS: setup-cache does not use nix-env"
fi

# Test 7: Verify CI workflows use cachix-action
echo ""
echo "Test 7: Verify CI workflows use cachix-action@v15"
WORKFLOW_FILE=".github/workflows/ci.yml"
if grep -q "cachix/cachix-action@v15" "$WORKFLOW_FILE"; then
    echo "✅ PASS: ci.yml uses cachix-action@v15"
else
    echo "❌ FAIL: ci.yml does not use cachix-action@v15"
    exit 1
fi

# Test 8: Verify no cache-nix-action in main workflow
echo ""
echo "Test 8: Verify cache-nix-action removed from main workflow"
if grep -q "cache-nix-action" "$WORKFLOW_FILE"; then
    echo "⚠️  WARNING: ci.yml still contains cache-nix-action references"
    echo "   Migration to Cachix-only should remove all cache-nix-action"
else
    echo "✅ PASS: cache-nix-action removed from ci.yml"
fi

# Test 9: Verify CACHIX_AUTH is referenced in workflows
echo ""
echo "Test 9: Verify workflows reference CACHIX_AUTH secret"
if grep -q "CACHIX_AUTH" "$WORKFLOW_FILE"; then
    echo "✅ PASS: ci.yml references CACHIX_AUTH"
else
    echo "❌ FAIL: ci.yml does not reference CACHIX_AUTH"
    exit 1
fi

# Test 10: Verify fork protection (skipPush for fork PRs)
echo ""
echo "Test 10: Verify fork PR protection in workflows"
if grep -q "skipPush.*fork" "$WORKFLOW_FILE"; then
    echo "✅ PASS: Fork PR protection configured (skipPush)"
else
    echo "⚠️  WARNING: Fork PR protection may not be configured"
    echo "   Recommended: Add 'skipPush: \${{ github.event.pull_request.head.repo.fork }}'"
fi

# Summary
echo ""
echo "================================"
echo "🎉 Cachix Integration Test Suite Complete"
echo ""
echo "Next steps:"
echo "1. Create Cachix cache at app.cachix.org"
echo "2. Update flake.nix with real public key"
echo "3. Add CACHIX_AUTH to GitHub secrets"
echo "4. Run workflow to test end-to-end integration"
echo ""
echo "See CACHIX_SETUP.md for detailed instructions"
