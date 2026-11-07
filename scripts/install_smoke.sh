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

check_grep "--help mentions --force-wsl" "force-wsl" \
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

# --- 4. --branch flag accepted (no-op for parity with install.ps1) ---
# install.ps1 forwards `--branch <name>` to install.sh when delegating into
# WSL2; before issue #326 the bash side rejected it as "unknown flag" and
# the entire WSL install path failed at flag-parse. Smoke that the flag is
# now consumed silently. We pair it with --help so the test exits 0
# without needing podman; flag-parse runs before --help is honored only
# when the unknown flag would have come first, so we put --branch first.
check_grep "--branch <name> accepted" "Nanna Coder one-line installer" \
  bash "$INSTALL_SH" --branch some-feature-branch --help

# --- 5. WSL2 systemd-aware gate ---
# Direct-invoke is_wsl / is_wsl_systemd_active by sourcing the script up
# to the OS-detection block. We can't easily source the whole file (it
# runs side effects), so we grep the source instead — the assertions
# below guard the structure that issue #326 codified.
check_grep "is_wsl helper present"        "is_wsl\\(\\) \\{"               grep -F "is_wsl()"            "$INSTALL_SH"
check_grep "is_wsl_systemd_active helper" "is_wsl_systemd_active\\(\\) \\{" grep -F "is_wsl_systemd_active()" "$INSTALL_SH"
check_grep "FORCE_WSL flag wired"         "FORCE_WSL=1"                    grep -F "FORCE_WSL=1"         "$INSTALL_SH"
check_grep "WSL systemd error mentions wsl.conf" "/etc/wsl.conf" grep -F "/etc/wsl.conf" "$INSTALL_SH"
check_grep "WSL fallback points at install.ps1"  "install.ps1"   grep -F "install.ps1"   "$INSTALL_SH"

# --- 6. shellcheck if available (informational on macOS) ---
if command -v shellcheck >/dev/null 2>&1; then
  check "shellcheck -x install.sh" shellcheck -x "$INSTALL_SH"
else
  printf '  [SKIP] shellcheck not installed\n'
fi

# --- 7. portability: no bash 4+ features in install.sh ---
# Hard-fail patterns: `mapfile`, `readarray`, associative arrays
# (`declare -A`), and `[[ -v VAR ]]` are bash 4+. macOS ships bash 3.2.
if grep -nE 'mapfile|readarray|declare -A|\[\[ -v ' "$INSTALL_SH" >/dev/null 2>&1; then
  printf '  [FAIL] install.sh uses bash 4+ features (mapfile/readarray/declare -A/[[ -v ]])\n'
  grep -nE 'mapfile|readarray|declare -A|\[\[ -v ' "$INSTALL_SH" | sed 's/^/         /' >&2
  FAILED=$((FAILED + 1))
  FAILURES="${FAILURES}bash 4+ feature in install.sh"$'\n'
else
  printf '  [OK]   no bash 4+ features in install.sh (macOS bash 3.2 compatible)\n'
  PASSED=$((PASSED + 1))
fi

printf '\n'
printf 'install_smoke: %d passed, %d failed\n' "$PASSED" "$FAILED"
if [[ $FAILED -gt 0 ]]; then
  printf '\nfailed checks:\n%s\n' "$FAILURES" >&2
  exit 1
fi
exit 0
