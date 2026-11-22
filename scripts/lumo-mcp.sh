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

declare -a repo_mounts=()
declare -a wanted=()
if [[ -n "${NANNA_REPOS:-}" ]]; then
  IFS=',' read -r -a wanted <<< "$NANNA_REPOS"
fi
declare -A seen=()
repo_list="$("$BASH" "$SCRIPT_DIR/lumo-repos.sh" --list)" || exit 3
while IFS=$'\t' read -r name volume _slug; do
  if [[ ${#wanted[@]} -gt 0 ]]; then
    keep=0
    for w in "${wanted[@]}"; do
      if [[ "$w" == "$name" ]]; then keep=1; seen[$w]=1; fi
    done
    [[ $keep -eq 1 ]] || continue
  fi
  if ! podman volume exists "$volume" > /dev/null 2>&1; then
    printf 'repository volume %s is missing; refusing to start without it. Run: %s/lumo-repos.sh --only %s\n' "$volume" "$SCRIPT_DIR" "$name" >&2
    exit 3
  fi
  repo_mounts+=(-v "$volume:/repos/$name")
done <<< "$repo_list"
for w in "${wanted[@]}"; do
  if [[ -z "${seen[$w]:-}" ]]; then
    printf 'NANNA_REPOS names %s, which is not a repository in the lumo card\n' "$w" >&2
    exit 2
  fi
done

exec podman run --rm -i --pod "$POD" \
  -v "$CONFIG_VOLUME:/etc/nanna:ro" "${repo_mounts[@]}" -e NANNA_CONFIG_DIR=/etc/nanna \
  "$IMAGE" /bin/nanna mcp-serve --model "$MODEL"
