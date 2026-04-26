#!/usr/bin/env bash
# Capture the real sha256 for an Ollama model and rewrite its entry in
# nix/containers.nix. Designed for operators on a machine with:
#   - `nix` (>=2.4 flakes) available on PATH
#   - outbound network to the Ollama registry (registry.ollama.ai)
#   - `ollama` installed locally so the fixed-output builder can run
#
# Usage:
#   scripts/update-model-sha256.sh <modelKey>
#
# Where <modelKey> matches a key in `modelRegistry` in nix/containers.nix
# (currently: `qwen3` (already real-hashed) or `gemma`).
#
# The script is idempotent: it fails fast if the key is already holding a
# real (non-placeholder) hash, so reruns won't silently clobber a good value.
# Rerun with FORCE=1 to overwrite.
#
# Exit codes:
#   0 - hash captured, file updated, and build confirmed
#   1 - precondition failed (missing tool, bad arg, etc.)
#   2 - capture attempt failed (network, ollama error, nix build error)

set -euo pipefail

# ---- Colors --------------------------------------------------------------
if [[ -t 1 ]]; then
  RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
else
  RED=''; GREEN=''; YELLOW=''; BLUE=''; NC=''
fi

log()  { printf '%b[update-model-sha256]%b %s\n' "${BLUE}" "${NC}" "$*"; }
warn() { printf '%b[update-model-sha256]%b %s\n' "${YELLOW}" "${NC}" "$*" >&2; }
die()  { printf '%b[update-model-sha256]%b %s\n' "${RED}"   "${NC}" "$*" >&2; exit "${2:-1}"; }

# ---- Args ----------------------------------------------------------------
if [[ $# -ne 1 ]]; then
  die "usage: $0 <modelKey>   (e.g. gemma)"
fi
MODEL_KEY="$1"
FORCE="${FORCE:-0}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONTAINERS_NIX="${REPO_ROOT}/nix/containers.nix"
[[ -f "${CONTAINERS_NIX}" ]] || die "cannot find ${CONTAINERS_NIX}"

PLACEHOLDER='sha256-0000000000000000000000000000000000000000000='

# ---- Preconditions -------------------------------------------------------
command -v nix     >/dev/null 2>&1 || die "nix not found on PATH (needed to run the fixed-output derivation)"
command -v ollama  >/dev/null 2>&1 || die "ollama not found on PATH (the fixed-output builder shells out to it)"

# Resolve the model's full name (e.g. gemma4:e4b) from the registry so we
# can display it and so we can surface a clear error if the key doesn't
# exist.
MODEL_NAME="$(nix eval --raw \
  --impure \
  --expr "(import ${CONTAINERS_NIX} { \
    pkgs = (builtins.getFlake \"${REPO_ROOT}\").inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; \
    lib  = (builtins.getFlake \"${REPO_ROOT}\").inputs.nixpkgs.lib; \
    nix2containerPkgs = (builtins.getFlake \"${REPO_ROOT}\").inputs.nix2container.packages.\${builtins.currentSystem}; \
    harness = null; \
    rustToolchain = null; \
  }).modelRegistry.${MODEL_KEY}.name" 2>/dev/null)" || {
    # Fall back to a dumb grep if the flake eval above is too brittle for
    # the local nixpkgs/nix2container shape. This is best-effort - the user
    # can always read nix/containers.nix themselves.
    MODEL_NAME="$(awk -v key="\"${MODEL_KEY}\"" '
      $0 ~ key" =" {found=1}
      found && /name = /{match($0, /"[^"]*"/); print substr($0, RSTART+1, RLENGTH-2); exit}
    ' "${CONTAINERS_NIX}")"
  }

[[ -n "${MODEL_NAME}" ]] || die "could not resolve model name for key '${MODEL_KEY}' in ${CONTAINERS_NIX}"

# Current hash for this key
CURRENT_HASH="$(awk -v key="\"${MODEL_KEY}\"" '
  $0 ~ key" =" {found=1}
  found && /hash = /{match($0, /"sha256-[^"]*"/); print substr($0, RSTART+1, RLENGTH-2); exit}
' "${CONTAINERS_NIX}")"

if [[ -n "${CURRENT_HASH}" && "${CURRENT_HASH}" != "${PLACEHOLDER}" && "${FORCE}" != "1" ]]; then
  die "model '${MODEL_KEY}' already has a non-placeholder hash: ${CURRENT_HASH}
refusing to overwrite; rerun with FORCE=1 to replace it."
fi

log "model key : ${MODEL_KEY}"
log "model name: ${MODEL_NAME}"
log "old hash  : ${CURRENT_HASH:-<unset>}"

# ---- Capture -------------------------------------------------------------
# Strategy: temporarily swap the placeholder hash for `lib.fakeSha256`
# (43 A's, well-formed SRI but wrong). The placeholder routes
# `createModelDerivation` to the dev-stub branch (empty success, no
# capture possible); a non-zero-infix fake hash routes to the
# fixed-output path, which downloads the model and then fails the
# integrity check, printing the real `got: sha256-...` to stderr.
# Restore on exit so a Ctrl-C mid-capture doesn't leave a corrupted
# containers.nix.
BUILD_ATTR="${MODEL_KEY}-model"
FAKE_HASH='sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA='

cp "${CONTAINERS_NIX}" "${CONTAINERS_NIX}.bak"
trap 'mv -f "${CONTAINERS_NIX}.bak" "${CONTAINERS_NIX}" 2>/dev/null || true; rm -f "${BUILD_LOG:-}" 2>/dev/null || true' EXIT

tmp="$(mktemp)"
awk -v key="\"${MODEL_KEY}\"" -v new="${FAKE_HASH}" '
  $0 ~ key" =" {in_block=1}
  in_block && /hash = / {
    sub(/"sha256-[^"]*"/, "\"" new "\"")
    in_block=0
  }
  { print }
' "${CONTAINERS_NIX}" > "${tmp}"
mv "${tmp}" "${CONTAINERS_NIX}"
log "swapped placeholder for fakeSha256 to force fixed-output build"

log "running: nix build .#${BUILD_ATTR} (expected to fail with a hash mismatch)"

BUILD_LOG="$(mktemp)"

# Intentionally ignore the exit code: the build is supposed to fail. What
# we care about is the `got: sha256-...` line in the error output.
if nix build --no-link --print-build-logs \
    ".#${BUILD_ATTR}" 2> "${BUILD_LOG}" 1>&2; then
  die "nix build succeeded unexpectedly with fakeSha256; the registry returned a model whose hash matches all-A's, which is essentially impossible. Inspect ${BUILD_LOG}."
fi

# Grep for the line Nix prints on a hash mismatch. Format varies across
# versions; accept both `got:` and `got    ` styles and both `sha256-`
# (SRI) and raw-hex (64 hex chars) forms.
NEW_HASH="$(grep -Eo 'got:? +(sha256-[A-Za-z0-9+/=]+|[0-9a-f]{64})' "${BUILD_LOG}" \
  | head -n1 \
  | awk '{print $NF}')"

# If raw hex was captured, convert it to SRI form that Nix expects.
if [[ "${NEW_HASH}" =~ ^[0-9a-f]{64}$ ]]; then
  NEW_HASH="sha256-$(printf '%s' "${NEW_HASH}" | xxd -r -p | base64 -w0)="
  log "converted raw hex to SRI: ${NEW_HASH}"
fi

if [[ -z "${NEW_HASH}" ]]; then
  warn "could not find a 'got: sha256-...' line in the build log"
  warn "full build log follows:"
  cat "${BUILD_LOG}" >&2
  die "hash capture failed" 2
fi

log "captured hash: ${NEW_HASH}"

# ---- Rewrite -------------------------------------------------------------
# At this point: containers.nix has fakeSha256 written in (we swapped it
# above to force the fixed-output build), and ${CONTAINERS_NIX}.bak holds
# the original (with the placeholder). Apply the captured real hash on
# top of the original — keep .bak untouched so the trap can roll back if
# validation fails.
tmp="$(mktemp)"
awk -v key="\"${MODEL_KEY}\"" -v new="${NEW_HASH}" '
  $0 ~ key" =" {in_block=1}
  in_block && /hash = / {
    sub(/"sha256-[^"]*"/, "\"" new "\"")
    in_block=0
  }
  { print }
' "${CONTAINERS_NIX}.bak" > "${tmp}"
mv "${tmp}" "${CONTAINERS_NIX}"

log "${GREEN}updated${NC} ${CONTAINERS_NIX}"
log "diff:"
git -C "${REPO_ROOT}" --no-pager diff -- "${CONTAINERS_NIX}" || true

# ---- Post-capture validation ---------------------------------------------
log "running post-capture validation: nix build .#${BUILD_ATTR}"
VALIDATION_LOG="$(mktemp)"
if ! nix build --no-link --print-build-logs \
    ".#${BUILD_ATTR}" 2> "${VALIDATION_LOG}" 1>&2; then
  warn "post-capture build FAILED — the captured hash was not accepted by Nix"
  warn "validation build log follows:"
  cat "${VALIDATION_LOG}" >&2
  rm -f "${VALIDATION_LOG}"
  die "hash validation failed; containers.nix will be restored from .bak on exit" 2
fi
rm -f "${VALIDATION_LOG}" "${CONTAINERS_NIX}.bak"
trap - EXIT

log "${GREEN}validation passed${NC} — nix build accepted the new hash"
log "${GREEN}done.${NC} Commit with:  git add nix/containers.nix && git commit -m 'nix: capture ${MODEL_KEY} sha256 (closes #240)'"
