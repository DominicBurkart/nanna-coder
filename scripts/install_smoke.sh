#!/usr/bin/env bash
# Portable smoke test for scripts/install.sh.
#
# Exercises install.sh's static surface (syntax, --help, --dry-run, OS
# detection, WSL2 systemd-aware gate) without requiring podman, sudo, or
# network access. Designed to run on:
#   - Linux (bash 4+/5)
#   - macOS (system /bin/bash 3.2)
#   - WSL2 (any distro)
#
# CI uses this as the cheap, fast portable check. Full bring-up coverage
# (image load → install.sh → ollama API) lives in install-test.yml.
#
# Exit codes:
#   0  — all checks passed
#   1  — at least one check failed (details printed inline)
#   2  — invocation error (missing install.sh, etc.)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTALL_SH="$SCRIPT_DIR/install.sh"

if [[ ! -f "$INSTALL_SH" ]]; then
  printf 'install_smoke: %s not found\n' "$INSTALL_SH" >&2
  exit 2
fi

PASSED=0
FAILED=0
FAILURES=""

check() {
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then
    printf '  [OK]   %s\n' "$name"
    PASSED=$((PASSED + 1))
  else
    printf '  [FAIL] %s\n' "$name"
    FAILED=$((FAILED + 1))
    FAILURES="${FAILURES}${name}"$'\n'
  fi
}

check_grep() {
  local name="$1" pattern="$2"; shift 2
  local out
  if out="$("$@" 2>&1)" && printf '%s\n' "$out" | grep -qE "$pattern"; then
    printf '  [OK]   %s\n' "$name"
    PASSED=$((PASSED + 1))
  else
    printf '  [FAIL] %s (pattern: %s)\n' "$name" "$pattern"
    FAILED=$((FAILED + 1))
    FAILURES="${FAILURES}${name}"$'\n'
  fi
}

printf 'install_smoke: %s\n' "$INSTALL_SH"
printf '  bash:  %s\n' "$BASH_VERSION"
printf '  os:    %s\n' "$(uname -s)"
printf '\n'

# --- 1. syntax ---
# `bash -n` parses the script without executing. Catches accidental
# bashisms broken on macOS bash 3.2 only if the parser itself is 3.2;
# the CI matrix runs this on a real macOS runner so the actual 3.2
# parser exercises the file.
check "bash -n parse" bash -n "$INSTALL_SH"

# --- 2. --help exits 0 ---
# install.sh's --help slices the leading comment block via sed. Regression
# guard: if the slice range drifts, --help silently empties out.
check_grep "--help prints usage" "Nanna Coder one-line installer" \
  bash "$INSTALL_SH" --help

check_grep "--help mentions --skip-model-pull" "skip-model-pull" \
  bash "$INSTALL_SH" --help

# --- 3. unknown flag rejection ---
if bash "$INSTALL_SH" --no-such-flag-xyzzy >/dev/null 2>&1; then
  printf '  [FAIL] %s\n' "unknown flag should fail"
  FAILED=$((FAILED + 1))
  FAILURES="${FAILURES}unknown flag should fail"$'\n'
else
  printf '  [OK]   %s\n' "unknown flag rejected"
  PASSED=$((PASSED + 1))
fi

# --- 4. shellcheck if available (informational on macOS) ---
if command -v shellcheck >/dev/null 2>&1; then
  check "shellcheck -x install.sh" shellcheck -x "$INSTALL_SH"
else
  printf '  [SKIP] shellcheck not installed\n'
fi

# --- 5. portability: no bash 4+ features in install.sh ---
# This is the load-bearing check for the macOS lane (issue #327).
# macOS ships /bin/bash 3.2 forever (Apple has frozen it on GPLv2).
# Any of these patterns will silently parse on bash 4/5 in dev but
# break for end users running scripts/install.sh under macOS's
# system bash:
#   - mapfile / readarray
#   - declare -A (associative arrays)
#   - [[ -v VAR ]]
#   - ${var^^} / ${var,,} / ${var^} / ${var,}  (case-conversion expansions)
#   - coproc
# Hard-fail if any of these slip in.
#
# The case-conversion patterns are matched literally on the parameter
# expansion form. We avoid false positives on `^^=` shell ops (none in
# practice) and on the `^` start-of-pattern in regexes by anchoring on
# the `${...}` opener.
BAD_PATTERNS='mapfile|readarray|declare -A|\[\[ -v |\$\{[A-Za-z_][A-Za-z0-9_]*(\[[^]]*\])?(,,|\^\^|,|\^)\}|^[[:space:]]*coproc[[:space:]]'
if grep -nE "$BAD_PATTERNS" "$INSTALL_SH" >/dev/null 2>&1; then
  printf '  [FAIL] install.sh uses bash 4+ features (mapfile/readarray/declare -A/[[ -v ]]/${var^^}/${var,,}/coproc)\n'
  grep -nE "$BAD_PATTERNS" "$INSTALL_SH" | sed 's/^/         /' >&2
  FAILED=$((FAILED + 1))
  FAILURES="${FAILURES}bash 4+ feature in install.sh"$'\n'
else
  printf '  [OK]   no bash 4+ features in install.sh (macOS bash 3.2 compatible)\n'
  PASSED=$((PASSED + 1))
fi

# --- 6. macOS-specific structural assertions ---
# install.sh's macOS branch invokes brew, podman machine init/start,
# and respects NANNA_SKIP_PODMAN_MACHINE. Keep these surfaces named
# (issue #327: macOS install path is the user-facing one we ship).
check_grep "macOS branch present"             "Darwin\\*\\) OS=macos"     grep -F "Darwin*) OS=macos" "$INSTALL_SH"
check_grep "install_podman_macos defined"     "install_podman_macos\\(\\)" grep -F "install_podman_macos()" "$INSTALL_SH"
check_grep "ensure_podman_machine_macos defined" "ensure_podman_machine_macos\\(\\)" grep -F "ensure_podman_machine_macos()" "$INSTALL_SH"
check_grep "NANNA_SKIP_PODMAN_MACHINE escape hatch present" "NANNA_SKIP_PODMAN_MACHINE" grep -F "NANNA_SKIP_PODMAN_MACHINE" "$INSTALL_SH"

# --- 7. WSL2 contract assertion ---
# install-test.yml's windows-bringup gate enforces that install.sh
# fails on WSL2 (is_wsl() detects /proc/version, prints the error
# block, exits non-zero). If is_wsl() is removed or renamed, the
# Windows lane silently flips from "expected fail" to "unexpected
# pass" and the gate's warning fires. Catch that drift here too.
check_grep "is_wsl() defined" "is_wsl\\(\\)" grep -F "is_wsl()" "$INSTALL_SH"

# --- 8. --dry-run exercises audit + plan code paths ---
# --help only slices a comment block. --dry-run forces the script
# through arg parsing -> OS detection -> audit_system -> print_plan
# without invoking podman or sudo. This is the deepest static-only
# code-path coverage we can get on a free hosted runner with no
# container runtime. Run with NANNA_SKIP_PODMAN_MACHINE=1 so the
# macOS branch doesn't probe `podman machine list` (which would be
# a no-op without podman, but keep the env explicit).
check_grep "--dry-run prints plan banner" "Nanna Coder installer" \
  env NANNA_SKIP_PODMAN_MACHINE=1 bash "$INSTALL_SH" --dry-run --yes

check_grep "--dry-run reports nothing executed" "nothing executed" \
  env NANNA_SKIP_PODMAN_MACHINE=1 bash "$INSTALL_SH" --dry-run --yes

printf '\n'
printf 'install_smoke: %d passed, %d failed\n' "$PASSED" "$FAILED"
if [[ $FAILED -gt 0 ]]; then
  printf '\nfailed checks:\n%s\n' "$FAILURES" >&2
  exit 1
fi
exit 0
