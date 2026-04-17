#!/usr/bin/env bash
# Guard against silent relaxations of codecov.yml patch thresholds.
#
# Fails if, between $BASE_REF (default: origin/main) and HEAD, any `target:`
# value in codecov.yml DECREASES or the `ignore:` list GAINS entries. Target
# raises and comment-only edits always pass. If codecov.yml did not exist on
# BASE_REF (first-time add), any values are accepted.
#
# There is NO in-repo override. Repository admins who need to relax the
# threshold must do so via GitHub's force-merge / admin-override path, which
# is logged and auditable. This script must not provide any way for an
# automated agent (or a commit trailer) to bypass the regression guard.
#
# Usage (local):   BASE_REF=origin/main bash scripts/check-codecov-guard.sh
# Usage (CI):      invoked by .github/workflows/codecov-guard.yml
set -euo pipefail

BASE_REF="${BASE_REF:-origin/main}"
FILE="codecov.yml"

# Extract all integer `target:` percentages, smallest first.
min_target() {
  # $1 = file content on stdin; prints the lowest integer percent or nothing.
  grep -oE 'target:[[:space:]]*[0-9]+%' "$@" 2>/dev/null \
    | grep -oE '[0-9]+' \
    | sort -n \
    | head -1
}

# Extract the count of entries under the `ignore:` block.
ignore_count() {
  awk '/^ignore:/{flag=1; next} /^[^[:space:]-]/{flag=0} flag && /^[[:space:]]*-/{c++} END{print c+0}' "$@"
}

if ! git rev-parse --verify "$BASE_REF" >/dev/null 2>&1; then
  echo "guard: base ref '$BASE_REF' not resolvable; skipping check" >&2
  exit 0
fi

# If codecov.yml did not exist on base (first-time add), accept.
if ! git cat-file -e "$BASE_REF:$FILE" 2>/dev/null; then
  echo "guard: $FILE is new on this branch; no prior values to compare" >&2
  exit 0
fi

OLD_CONTENT="$(git show "$BASE_REF:$FILE")"
NEW_CONTENT="$(cat "$FILE" 2>/dev/null || true)"

# Quick equality check — no diff, no guard.
if [ "$OLD_CONTENT" = "$NEW_CONTENT" ]; then
  exit 0
fi

OLD_TARGET="$(printf '%s\n' "$OLD_CONTENT" | min_target /dev/stdin || true)"
NEW_TARGET="$(printf '%s\n' "$NEW_CONTENT" | min_target /dev/stdin || true)"
OLD_IGNORE="$(printf '%s\n' "$OLD_CONTENT" | ignore_count)"
NEW_IGNORE="$(printf '%s\n' "$NEW_CONTENT" | ignore_count)"

REGRESSION=0
REASONS=()

if [ -n "$OLD_TARGET" ] && [ -n "$NEW_TARGET" ] && [ "$NEW_TARGET" -lt "$OLD_TARGET" ]; then
  REGRESSION=1
  REASONS+=("patch target decreased: $OLD_TARGET% -> $NEW_TARGET%")
fi

if [ "$NEW_IGNORE" -gt "$OLD_IGNORE" ]; then
  REGRESSION=1
  REASONS+=("ignore: list grew from $OLD_IGNORE to $NEW_IGNORE entries")
fi

if [ "$REGRESSION" -eq 0 ]; then
  exit 0
fi

{
  echo "guard: codecov.yml regression detected and there is no in-repo override."
  for r in "${REASONS[@]}"; do echo "  - $r"; done
  echo "If this change is intentional, a repository admin must force-merge;"
  echo "no commit trailer or script flag can bypass this check."
} >&2
exit 1
