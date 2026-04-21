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
# (e.g. `gemma`, `llama3`, `mistral`).
#
# The script is idempotent: it fails fast if the key is already holding a
# real (non-placeholder) hash, so reruns won't silently clobber a good value.
# Rerun with FORCE=1 to overwrite.
#
# Exit codes:
#   0 - hash captured and file updated
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
  die "usage: $0 <modelKey>   (e.g. gemma, llama3, mistral)"
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
# Strategy: ask Nix to build the fixed-output derivation with a fake (valid
# but wrong) hash. The build will run, download the model, then fail when
# it checks the hash - and the error message contains the real `got:` hash.
#
# We use `lib.fakeSha256` indirectly by passing the placeholder value (which
# is a well-formed sha256 that just happens to be wrong) and scraping the
# diagnostic out of stderr.
BUILD_ATTR="models.${MODEL_KEY}-model"
log "running: nix build .#${BUILD_ATTR} (expected to fail with a hash mismatch)"

BUILD_LOG="$(mktemp)"
trap 'rm -f "${BUILD_LOG}"' EXIT

# Intentionally ignore the exit code: the build is supposed to fail. What
# we care about is the `got: sha256-...` line in the error output.
if nix build --no-link --print-build-logs \
    ".#${BUILD_ATTR}" 2> "${BUILD_LOG}" 1>&2; then
  # Surprising: build succeeded. That means the current hash is already
  # correct or the derivation is content-addressed differently than we
  # expect. Bail so we don't clobber something good.
  die "nix build succeeded unexpectedly; nothing to capture. Is the hash already correct?"
fi

# Grep for the line Nix prints on a hash mismatch. Format varies across
# versions; accept both `got:` and `got    ` styles and both `sha256-`
# (SRI) and raw-hex forms.
NEW_HASH="$(grep -Eo 'got:? +sha256-[A-Za-z0-9+/=]+' "${BUILD_LOG}" \
  | head -n1 \
  | awk '{print $NF}')"

if [[ -z "${NEW_HASH}" ]]; then
  warn "could not find a 'got: sha256-...' line in the build log"
  warn "full build log follows:"
  cat "${BUILD_LOG}" >&2
  die "hash capture failed" 2
fi

log "captured hash: ${NEW_HASH}"

# ---- Rewrite -------------------------------------------------------------
# In-place replace only the hash line inside the matching block. We scope
# the replacement with a small awk state machine so we don't accidentally
# rewrite a placeholder belonging to another model.
tmp="$(mktemp)"
awk -v key="\"${MODEL_KEY}\"" -v new="${NEW_HASH}" '
  $0 ~ key" =" {in_block=1}
  in_block && /hash = / {
    sub(/"sha256-[^"]*"/, "\"" new "\"")
    in_block=0
  }
  { print }
' "${CONTAINERS_NIX}" > "${tmp}"

mv "${tmp}" "${CONTAINERS_NIX}"

log "${GREEN}updated${NC} ${CONTAINERS_NIX}"
log "diff:"
git -C "${REPO_ROOT}" --no-pager diff -- "${CONTAINERS_NIX}" || true

log "${GREEN}done.${NC} Verify with:  nix build .#${BUILD_ATTR}"
