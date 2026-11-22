#!/usr/bin/env bash
set -euo pipefail

REGISTRY="${NANNA_REGISTRY:-ghcr.io/dominicburkart/nanna-coder}"
TAG="${NANNA_TAG:-latest}"
IMAGE="${NANNA_HARNESS_IMAGE:-$REGISTRY/harness:$TAG}"
CARGO_VOLUME="${NANNA_CARGO_VOLUME:-nanna-cargo-home}"
VOLUME_PREFIX="${NANNA_REPO_VOLUME_PREFIX:-nanna-repo-}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
  cat >&2 << 'EOF'
usage: lumo-cargo-vendor.sh [--volume VOLUME] [--image IMAGE] [REPO_NAME ...]

Fetches the crates.io and git dependencies of the named repositories into the
podman volume VOLUME (default nanna-cargo-home) so tasks can run cargo offline.
It is the only step that touches the network: one throwaway networked container
from the harness image, repository volumes mounted read-only, no host paths.
Repositories are copied inside the throwaway container so a missing Cargo.lock
can be resolved without writing to the repository volume.
Run it again after a dependency changes; the daemon mounts VOLUME read-only at
/cargo with CARGO_HOME=/cargo and CARGO_NET_OFFLINE=true. With no REPO_NAME every
repository in your identity catalog is vendored (see lumo-repos.sh --list).

VOLUME defaults to nanna-cargo-home, the volume lumo-mcp.sh mounts; set
NANNA_CARGO_VOLUME (read by both scripts) to use another name. The image defaults
to the harness image (NANNA_HARNESS_IMAGE). The daemon only mounts the volume
when it is created, so run lumo-mcp.sh --restart after vendoring.
EOF
}

require_podman() {
  if ! command -v podman > /dev/null 2>&1; then
    printf 'podman not found on PATH; nanna runs only inside containers (see scripts/install.sh)\n' >&2
    exit 127
  fi
}

declare -a names=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --volume) CARGO_VOLUME="${2:?--volume needs a value}"; shift 2 ;;
    --image) IMAGE="${2:?--image needs a value}"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    -*) usage; exit 2 ;;
    *) names+=("$1"); shift ;;
  esac
done

require_podman

if [[ ${#names[@]} -eq 0 ]]; then
  while IFS=$'\t' read -r name _volume _slug; do
    names+=("$name")
  done < <("$BASH" "$SCRIPT_DIR/lumo-repos.sh" --list)
fi
if [[ ${#names[@]} -eq 0 ]]; then
  printf 'no repositories to vendor\n' >&2
  exit 2
fi

declare -a mounts=()
for name in "${names[@]}"; do
  if [[ ! "$name" =~ ^[a-z0-9][a-z0-9._-]*$ ]]; then
    printf 'refusing repository name %s\n' "$name" >&2
    exit 2
  fi
  if ! podman volume exists "$VOLUME_PREFIX$name" > /dev/null 2>&1; then
    printf 'repository volume %s%s is missing; refusing. Run: %s/lumo-repos.sh --only %s\n' "$VOLUME_PREFIX" "$name" "$SCRIPT_DIR" "$name" >&2
    exit 3
  fi
  mounts+=(-v "$VOLUME_PREFIX$name:/repos/$name:ro")
done

if ! [[ "$CARGO_VOLUME" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]*$ ]]; then
  printf 'refusing volume name %s\n' "$CARGO_VOLUME" >&2
  exit 2
fi
podman volume exists "$CARGO_VOLUME" > /dev/null 2>&1 || podman volume create "$CARGO_VOLUME" > /dev/null

inner='
set -eu
mkdir -p /tmp/cargo-home /tmp/src
first=$1
sync=""
for n in "$@"; do
  cp -a "/repos/$n" "/tmp/src/$n"
  sync="$sync --sync /tmp/src/$n/Cargo.toml"
done
rm -rf /cargo/vendor /cargo/config.toml
cargo vendor --manifest-path "/tmp/src/$first/Cargo.toml" $sync /cargo/vendor > /tmp/vendor-config.toml
{ cat /tmp/vendor-config.toml; printf "\n[net]\noffline = true\n"; } > /cargo/config.toml
chmod -R a+rX /cargo
'

podman run --rm --entrypoint /bin/sh \
  -e CARGO_HOME=/tmp/cargo-home \
  -v "$CARGO_VOLUME:/cargo" "${mounts[@]}" \
  "$IMAGE" -c "$inner" vendor "${names[@]}"

printf 'vendored dependencies of %s into podman volume %s\nrun %s/lumo-mcp.sh --restart so the daemon mounts it\n' "${names[*]}" "$CARGO_VOLUME" "$SCRIPT_DIR"
