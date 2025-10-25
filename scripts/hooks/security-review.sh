#!/usr/bin/env bash
# Security review hook - shared by Nix and cargo-husky
# Performs AI-powered security review of staged changes

set -e

if ! command -v claude >/dev/null 2>&1; then
  echo "⚠️  Claude CLI not available, skipping automated security review"
  exit 0
fi

echo "🔒 Running security review..."

git diff --cached | claude "You are a security engineer. Review the code being committed to determine if it can be committed/pushed. Does this commit leak any secrets, tokens, sensitive internals, or PII?

Provide your security analysis first, explaining any concerns you find.

Then end your response with ONLY ONE of these status lines:
STATUS: APPROVED
STATUS: BLOCKED" | tee /tmp/claude_review

# Parse structured output for explicit status markers
if grep -q "^STATUS: APPROVED" /tmp/claude_review; then
  echo "✅ Security review passed"
  exit 0
elif grep -q "^STATUS: BLOCKED" /tmp/claude_review; then
  echo "🚨 Security issues found above. Please fix before committing."
  exit 1
else
  echo "⚠️  No explicit status from security review, skipping"
  exit 0
fi
