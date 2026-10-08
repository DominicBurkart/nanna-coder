#!/usr/bin/env bash
set -euo pipefail

workflow="${1:-.github/workflows/ci.yml}"
failures=0

fail() {
  echo "::error::$1"
  failures=$((failures + 1))
}

section() {
  awk -v header="  $1:" '
    $0 == header { on = 1; next }
    on && /^  [^ ].*:[[:space:]]*$/ { exit }
    on { print }
  ' "$workflow"
}

required="$(section container-runtime-required)"
gate="$(section all-checks)"

[ -n "$required" ] || fail "job container-runtime-required is missing"
[ -n "$gate" ] || fail "job all-checks is missing"

has() {
  printf '%s\n' "$1" | grep -Fq -- "$2"
}

expect() {
  has "$2" "$3" || fail "$1 must contain: $3"
}

forbid() {
  ! has "$2" "$3" || fail "$1 must not contain: $3"
}

expect required "$required" 'NANNA_REQUIRE_RUNTIME: "1"'
expect required "$required" 'container_'
expect required "$required" "bash -c 'PATH=\"\$1:\$PATH\""
forbid required "$required" 'env PATH='
expect required "$required" 'command -v podman skopeo'
expect required "$required" 'unknown transport'
forbid required "$required" '11434'
if printf '%s\n' "$required" | grep -qi 'ollama'; then
  fail "required must not mention ollama"
fi

for pattern in '|| true' 'continue-on-error' 'no-capture' '2>/dev/null' ; do
  forbid required "$required" "$pattern"
done
expect required "$required" 'if: always()'
expect required "$required" 'permissions:'
forbid required "$required" 'environment:'
forbid gate "$gate" '- container-ollama'

expect gate "$gate" '- container-runtime-required'

if [ "$failures" -ne 0 ]; then
  exit 1
fi
echo "OK: runtime job structure"
