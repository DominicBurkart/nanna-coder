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
ollama="$(section container-ollama)"
gate="$(section all-checks)"

[ -n "$required" ] || fail "job container-runtime-required is missing"
[ -n "$ollama" ] || fail "job container-ollama is missing"
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
forbid required "$required" '11434'
if printf '%s\n' "$required" | grep -qi 'ollama'; then
  fail "required must not mention ollama"
fi
forbid required "$required" 'continue-on-error'

expect ollama "$ollama" 'ollama_'
expect ollama "$ollama" '11434'
expect ollama "$ollama" 'continue-on-error: true'

for pair in "required:$required" "ollama:$ollama"; do
  name="${pair%%:*}"
  body="${pair#*:}"
  expect "$name" "$body" 'if: always()'
  expect "$name" "$body" 'permissions:'
  forbid "$name" "$body" 'environment:'
done

expect gate "$gate" '- container-runtime-required'
expect gate "$gate" '- container-ollama'

if [ "$failures" -ne 0 ]; then
  exit 1
fi
echo "OK: runtime job structure"
