#!/usr/bin/env bash
# check-ci-doc-coverage.sh
#
# Asserts that every workflow file in .github/workflows/ is either:
#   (a) documented as a dedicated `## <filename>` heading in
#       docs/ci/architecture.md, OR
#   (b) explicitly excluded via a line of the form
#           OMITTED: <filename> — <reason>
#       anywhere in docs/ci/architecture.md.
#
# The check is stricter than "filename appears somewhere" on purpose:
# a simple substring match would silently pass if the filename was
# mentioned only in a link or table row. Requiring a dedicated ## heading
# forces authors to actually document the workflow.
#
# Usage: bash scripts/check-ci-doc-coverage.sh

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

arch_doc="docs/ci/architecture.md"
workflows_dir=".github/workflows"

if [ ! -f "$arch_doc" ]; then
    echo "error: $arch_doc is missing" >&2
    exit 1
fi

if [ ! -d "$workflows_dir" ]; then
    echo "error: $workflows_dir is missing" >&2
    exit 1
fi

shopt -s nullglob
workflows=("$workflows_dir"/*.yml "$workflows_dir"/*.yaml)
if [ ${#workflows[@]} -eq 0 ]; then
    echo "error: no workflow files found under $workflows_dir" >&2
    exit 1
fi

errors=0

for wf in "${workflows[@]}"; do
    fname="$(basename "$wf")"

    # Check for a dedicated `## <filename>` heading (exact match on the
    # heading text, allowing nothing else on the heading line besides
    # optional trailing whitespace).
    if grep -qE "^## ${fname//./\\.}[[:space:]]*$" "$arch_doc"; then
        continue
    fi

    # Check for an explicit OMITTED marker. Accept either em dash (U+2014)
    # or a plain ASCII hyphen so authors aren't blocked on typographic
    # details.
    if grep -qE "^OMITTED: ${fname//./\\.}[[:space:]]+(—|-)" "$arch_doc"; then
        echo "note: $fname is explicitly OMITTED in $arch_doc"
        continue
    fi

    echo "error: $fname has no dedicated '## $fname' heading in $arch_doc" >&2
    echo "       and no 'OMITTED: $fname — <reason>' marker." >&2
    errors=$((errors + 1))
done

if [ "$errors" -ne 0 ]; then
    echo ""
    echo "check-ci-doc-coverage.sh: $errors workflow file(s) undocumented" >&2
    exit 1
fi

echo "check-ci-doc-coverage.sh: OK (${#workflows[@]} workflow file(s) covered)"
