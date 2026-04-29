#!/usr/bin/env bash
# install_smoke.sh - Lint + dry-run smoke test for scripts/install.sh.
#
# Verifies that scripts/install.sh:
#   1. Parses with `bash -n` (syntax check).
#   2. Passes shellcheck (if shellcheck is on PATH; warning otherwise).
#   3. Accepts the documented flag combinations under --dry-run without
#      touching the host system.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL="$SCRIPT_DIR/install.sh"
SMOKE_SELF="$SCRIPT_DIR/install_smoke.sh"

log() { printf '[smoke] %s\n' "$*"; }
fail() { printf '[smoke][FAIL] %s\n' "$*" >&2; exit 1; }

[ -f "$INSTALL" ] || fail "missing $INSTALL"

log "syntax-check: bash -n $INSTALL"
bash -n "$INSTALL"
bash -n "$SMOKE_SELF"

if command -v shellcheck >/dev/null 2>&1; then
    log "shellcheck $INSTALL $SMOKE_SELF"
    shellcheck "$INSTALL" "$SMOKE_SELF"
else
    log "shellcheck not on PATH; skipping (install shellcheck for full lint)"
fi

log "dry-run: --help"
bash "$INSTALL" --help >/dev/null

log "dry-run: --standalone --local-model gemma4:e4b"
bash "$INSTALL" --dry-run --standalone --local-model gemma4:e4b >/dev/null

log "dry-run: --standalone (default model)"
bash "$INSTALL" --dry-run --standalone >/dev/null

log "dry-run: --mcp --local-model gemma4:e4b"
bash "$INSTALL" --dry-run --mcp --local-model gemma4:e4b >/dev/null

log "dry-run: --mcp with custom image and runtime"
bash "$INSTALL" --dry-run --mcp \
    --container-runtime docker \
    --image example.invalid/nanna:test \
    --local-model qwen3:0.6b >/dev/null

log "dry-run: --local-model=foo (equals form)"
bash "$INSTALL" --dry-run --standalone --local-model=qwen3:0.6b >/dev/null

# Negative cases - script should exit non-zero.
log "negative: --mcp --standalone (mutually exclusive)"
if bash "$INSTALL" --dry-run --mcp --standalone >/dev/null 2>&1; then
    fail "expected error when combining --mcp and --standalone"
fi

log "negative: unknown flag"
if bash "$INSTALL" --dry-run --not-a-flag >/dev/null 2>&1; then
    fail "expected error for unknown flag"
fi

log "negative: --local-model without value"
if bash "$INSTALL" --dry-run --standalone --local-model >/dev/null 2>&1; then
    fail "expected error for --local-model without value"
fi

log "all smoke checks passed"
