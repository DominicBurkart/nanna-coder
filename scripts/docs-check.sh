#!/usr/bin/env bash
# docs-check.sh
#
# Local entry point that runs the docs/ci/* invariants on a developer
# machine, without requiring a CI workflow with `workflows`-scope
# permission to be merged first.
#
# This wraps:
#   - scripts/check-docs-links.sh       (internal-link / anchor validity)
#   - scripts/check-ci-doc-coverage.sh  (every .github/workflows/*.y{a,}ml
#                                       has a dedicated ## heading in
#                                       docs/ci/architecture.md)
#
# Both checks are read-only and have no network dependency.
#
# Usage: bash scripts/docs-check.sh

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

echo "==> scripts/check-docs-links.sh"
bash scripts/check-docs-links.sh

echo ""
echo "==> scripts/check-ci-doc-coverage.sh"
bash scripts/check-ci-doc-coverage.sh

echo ""
echo "docs-check.sh: OK (both checks passed)"
