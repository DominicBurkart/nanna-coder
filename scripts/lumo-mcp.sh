#!/usr/bin/env bash
set -euo pipefail

REGISTRY="${NANNA_REGISTRY:-ghcr.io/dominicburkart/nanna-coder}"
TAG="${NANNA_TAG:-latest}"
MODEL="${NANNA_MODEL:-gemma4:e4b}"
POD="${NANNA_POD:-nanna-coder-pod}"
IMAGE="${NANNA_HARNESS_IMAGE:-$REGISTRY/harness:$TAG}"
CONFIG_VOLUME="${NANNA_CONFIG_VOLUME:-nanna-lumo-config}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SELF="$SCRIPT_DIR/$(basename "${BASH_SOURCE[0]}")"

require_podman() {
  if ! command -v podman > /dev/null 2>&1; then
    printf 'podman not found on PATH; nanna runs only inside containers (see scripts/install.sh)\n' >&2
    exit 127
  fi
}

install_config() {
  require_podman
  local source="$SCRIPT_DIR/lumo-config"
  if [[ ! -d "$source/agents" ]]; then
    printf 'capability config not found at %s; run --install-config from the repository checkout\n' "$source" >&2
    exit 2
  fi
  podman volume rm -f "$CONFIG_VOLUME" > /dev/null
  podman volume create "$CONFIG_VOLUME" > /dev/null
  tar -C "$source" -cf - agents | podman volume import "$CONFIG_VOLUME" -
  printf 'installed the lumo capability config into podman volume %s\n' "$CONFIG_VOLUME"
}

case "${1:-}" in
  "")
    ;;
  --print)
    printf 'name: nanna-coder\ntransport: stdio\ncommand: %s\n' "$SELF"
    exit 0
    ;;
  --install-config)
    install_config
    exit 0
    ;;
  -h|--help)
    printf 'usage: %s [--print | --install-config]\n' "$0"
    exit 0
    ;;
  *)
    printf 'unknown argument: %s\n' "$1" >&2
    exit 2
    ;;
esac

require_podman

if ! podman volume exists "$CONFIG_VOLUME" > /dev/null 2>&1; then
  printf 'capability config volume %s is missing; refusing to start without it. Run: %s --install-config\n' "$CONFIG_VOLUME" "$SELF" >&2
  exit 3
fi

exec podman run --rm -i --pod "$POD" \
  -v "$CONFIG_VOLUME:/etc/nanna:ro" -e NANNA_CONFIG_DIR=/etc/nanna \
  "$IMAGE" /bin/nanna mcp-serve --model "$MODEL"
