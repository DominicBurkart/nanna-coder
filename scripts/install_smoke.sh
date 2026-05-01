#!/usr/bin/env bash
# install_smoke.sh - Lint + dry-run smoke test for scripts/install.sh.
#
# Verifies that scripts/install.sh:
#   1. Parses with `bash -n` (syntax check).
#   2. Passes shellcheck (REQUIRED — installer is the highest-risk shell
#      in the repo; do not soft-skip).
#   3. Accepts the documented flag combinations under --dry-run without
#      touching the host system.
#   4. Rejects invalid combinations (mutually-exclusive modes, missing
#      mode, unknown flag, missing flag value, host-network without the
#      explicit --unsafe-host-network opt-in).

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

# The static-analysis tool is REQUIRED here, not soft-skipped: the
# installer is the highest-risk shell in the repo and we want quoting
# bugs caught on every PR, not "only on the maintainer's laptop".
if ! command -v shellcheck >/dev/null 2>&1; then
    fail "shellcheck is required for installer smoke; install it (apt-get install shellcheck / brew install shellcheck / nix-shell -p shellcheck) and re-run"
fi
log "shellcheck $INSTALL $SMOKE_SELF"
shellcheck "$INSTALL" "$SMOKE_SELF"

log "dry-run: --help"
bash "$INSTALL" --help >/dev/null

log "dry-run: --standalone --local-model gemma4:e4b"
bash "$INSTALL" --dry-run --standalone --local-model gemma4:e4b >/dev/null

log "dry-run: --standalone (default model)"
bash "$INSTALL" --dry-run --standalone >/dev/null

log "dry-run: --mcp --local-model gemma4:e4b (default bridge network)"
bash "$INSTALL" --dry-run --mcp --local-model gemma4:e4b >/dev/null

log "dry-run: --mcp with custom image and runtime"
bash "$INSTALL" --dry-run --mcp \
    --container-runtime docker \
    --image example.invalid/nanna:test \
    --local-model qwen3:0.6b >/dev/null

log "dry-run: --local-model=foo (equals form)"
bash "$INSTALL" --dry-run --standalone --local-model=qwen3:0.6b >/dev/null

log "dry-run: --mcp --unsafe-host-network (explicit host net opt-in)"
bash "$INSTALL" --dry-run --mcp --unsafe-host-network \
    --container-runtime docker \
    --image example.invalid/nanna:test >/dev/null

log "dry-run: --standalone --rev <fake-sha> (pinned install)"
bash "$INSTALL" --dry-run --standalone \
    --rev 0000000000000000000000000000000000000000 >/dev/null

# Negative cases - script should exit non-zero.
log "negative: --mcp --standalone (mutually exclusive)"
if bash "$INSTALL" --dry-run --mcp --standalone >/dev/null 2>&1; then
    fail "expected error when combining --mcp and --standalone"
fi

log "negative: no mode (must require --mcp or --standalone)"
if bash "$INSTALL" --dry-run >/dev/null 2>&1; then
    fail "expected error when no mode is specified"
fi

log "negative: unknown flag"
if bash "$INSTALL" --dry-run --standalone --not-a-flag >/dev/null 2>&1; then
    fail "expected error for unknown flag"
fi

log "negative: --local-model without value"
if bash "$INSTALL" --dry-run --standalone --local-model >/dev/null 2>&1; then
    fail "expected error for --local-model without value"
fi

log "negative: --container-network=host without --unsafe-host-network"
if bash "$INSTALL" --dry-run --mcp --container-network host >/dev/null 2>&1; then
    fail "expected error when host network selected without --unsafe-host-network"
fi

log "all smoke checks passed"
