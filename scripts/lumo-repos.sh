#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CARD="${NANNA_LUMO_CARD:-$SCRIPT_DIR/lumo-config/agents/lumo.toml}"
VOLUME_PREFIX="${NANNA_REPO_VOLUME_PREFIX:-nanna-repo-}"
GIT_IMAGE="${NANNA_REPOS_IMAGE:-docker.io/library/rust:1-bookworm}"
SELF="$SCRIPT_DIR/$(basename "${BASH_SOURCE[0]}")"

require_podman() {
  if ! command -v podman > /dev/null 2>&1; then
    printf 'podman not found on PATH; repositories are kept in podman volumes (see scripts/install.sh)\n' >&2
    exit 127
  fi
}

card_slugs() {
  if [[ ! -r "$CARD" ]]; then
    printf 'lumo card not found at %s\n' "$CARD" >&2
    exit 2
  fi
  local line
  line="$(grep -m1 '^repos = \[' "$CARD" || true)"
  if [[ -z "$line" ]]; then
    printf 'no scope.repos entry in %s\n' "$CARD" >&2
    exit 2
  fi
  grep -o '"[^"]*"' <<< "$line" | tr -d '"'
}

declare -a NAMES=() VOLUMES=() SLUGS=()

load_repos() {
  local slug name slugs
  slugs="$(card_slugs)" || exit 2
  while IFS= read -r slug; do
    if [[ ! "$slug" =~ ^[A-Za-z0-9.-]+/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]]; then
      printf 'refusing malformed repository slug in the card: %s\n' "$slug" >&2
      exit 2
    fi
    name="${slug##*/}"
    name="${name,,}"
    NAMES+=("$name")
    VOLUMES+=("$VOLUME_PREFIX$name")
    SLUGS+=("$slug")
  done <<< "$slugs"
}

list_repos() {
  local i
  for i in "${!NAMES[@]}"; do
    printf '%s\t%s\t%s\n' "${NAMES[$i]}" "${VOLUMES[$i]}" "${SLUGS[$i]}"
  done
}

fail_private() {
  printf 'could not reach https://%s anonymously. If the repository is private, credentials are not supported by this script: none are accepted and none are embedded. Skipping it.\n' "$1" >&2
}

populate() {
  local volume="$1" slug="$2" url="https://$2"
  if podman volume exists "$volume" > /dev/null 2>&1; then
    if ! podman run --rm -e GIT_TERMINAL_PROMPT=0 -v "$volume:/repo" --entrypoint sh "$GIT_IMAGE" -c \
      'git -C /repo remote set-url origin "$1" && git -C /repo fetch --prune origin && git -C /repo worktree prune && git -C /repo merge --ff-only @{upstream}' sh "$url"; then
      fail_private "$slug"
      printf 'the volume %s was kept; remove it with: podman volume rm %s\n' "$volume" "$volume" >&2
      return 1
    fi
    printf 'refreshed %s from %s\n' "$volume" "$url"
  else
    podman volume create "$volume" > /dev/null
    if ! podman run --rm -e GIT_TERMINAL_PROMPT=0 -v "$volume:/repo" --entrypoint git "$GIT_IMAGE" clone --quiet "$url" /repo; then
      podman volume rm -f "$volume" > /dev/null
      fail_private "$slug"
      return 1
    fi
    printf 'cloned %s into %s\n' "$url" "$volume"
  fi
}

ONLY=""
LIST=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --list) LIST=1 ;;
    --only)
      if [[ $# -lt 2 ]]; then
        printf '%s needs a repository name\n' "$1" >&2
        exit 2
      fi
      ONLY="$2"
      shift
      ;;
    -h|--help)
      printf 'usage: %s [--list] [--only <name>]\n' "$0"
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      exit 2
      ;;
  esac
  shift
done

if [[ $LIST -eq 0 ]]; then
  require_podman
fi

load_repos

if [[ $LIST -eq 1 ]]; then
  list_repos
  exit 0
fi

selected=()
for i in "${!NAMES[@]}"; do
  if [[ -z "$ONLY" || "${NAMES[$i]}" == "$ONLY" ]]; then
    selected+=("$i")
  fi
done
if [[ ${#selected[@]} -eq 0 ]]; then
  printf 'no repository named %s in the lumo card; run: %s --list\n' "$ONLY" "$SELF" >&2
  exit 2
fi

status=0
for i in "${selected[@]}"; do
  populate "${VOLUMES[$i]}" "${SLUGS[$i]}" || status=4
done
exit $status
