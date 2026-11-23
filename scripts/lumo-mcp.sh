#!/usr/bin/env bash
set -euo pipefail

REGISTRY="${NANNA_REGISTRY:-ghcr.io/dominicburkart/nanna-coder}"
TAG="${NANNA_TAG:-latest}"
MODEL="${NANNA_MODEL:-gemma4:e4b}"
POD="${NANNA_POD:-nanna-coder-pod}"
IMAGE="${NANNA_HARNESS_IMAGE:-$REGISTRY/harness:$TAG}"
CONFIG_VOLUME="${NANNA_CONFIG_VOLUME:-nanna-lumo-config}"
STATE_VOLUME="${NANNA_STATE_VOLUME:-nanna-state}"
CARGO_VOLUME="${NANNA_CARGO_VOLUME:-nanna-cargo-home}"
STATE_PATH=/state
CONFIG_PATH=/etc/nanna
CARGO_PATH=/cargo
SANDBOX_HIDE="/repos:$CONFIG_PATH:$STATE_PATH"
DAEMON="${NANNA_DAEMON_NAME:-nanna-daemon}"
SOCKET="${NANNA_DAEMON_SOCKET:-/run/nanna/mcp.sock}"
WAIT_SECS="${NANNA_DAEMON_WAIT_SECS:-30}"
AUDIT_TIMEOUT="${NANNA_AUDIT_TIMEOUT_SECS:-300}"
CLIENT_TAG="lumo"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SELF="$SCRIPT_DIR/$(basename "${BASH_SOURCE[0]}")"

say() { printf '%s\n' "$*" >&2; }

require_podman() {
  if ! command -v podman > /dev/null 2>&1; then
    say 'podman not found on PATH; nanna runs only inside containers (see scripts/install.sh)'
    exit 127
  fi
}

catalog_source() {
  if [[ -n "${NANNA_CATALOG_SRC:-}" ]]; then
    printf '%s\n' "$NANNA_CATALOG_SRC"
  elif [[ -n "${XDG_CONFIG_HOME:-}" ]]; then
    printf '%s\n' "$XDG_CONFIG_HOME/nanna/agents"
  else
    printf '%s\n' "${HOME:-/nonexistent}/.config/nanna/agents"
  fi
}

install_catalog() {
  require_podman
  local source stage identities
  source="$(catalog_source)"
  if [[ ! -f "$source/auditor.toml" ]]; then
    say "the catalog at $source has no auditor.toml; refusing to install it. Set NANNA_CATALOG_SRC to the directory holding your identity cards (auditor.toml, rust-implementer.toml, prompts/, ...)"
    exit 2
  fi
  if podman container exists "$DAEMON" > /dev/null 2>&1; then
    say "the container $DAEMON exists and mounts the config volume $CONFIG_VOLUME; replacing the volume would remove it with its running tasks. Run: $SELF --stop, then --install-catalog again"
    exit 2
  fi
  stage="$(mktemp -d)"
  trap "rm -rf '$stage'" EXIT
  mkdir "$stage/agents"
  cp -RL "$source/." "$stage/agents/"
  identities="$(cd "$stage/agents" && for card in *.toml; do [[ -e "$card" ]] && printf '%s\n' "${card%.toml}"; done | paste -sd, -)"
  podman volume rm -f "$CONFIG_VOLUME" > /dev/null
  podman volume create "$CONFIG_VOLUME" > /dev/null
  tar -C "$stage" -cf - agents | podman volume import "$CONFIG_VOLUME" -
  printf 'installed the identity catalog from %s into podman volume %s\nidentities: %s\n' "$source" "$CONFIG_VOLUME" "${identities//,/, }"
}

declare -a repo_mounts=()
declare -a repo_names=()

resolve_mounts() {
  if ! podman volume exists "$CONFIG_VOLUME" > /dev/null 2>&1; then
    say "identity catalog volume $CONFIG_VOLUME is missing; refusing to start without it. Run: $SELF --install-catalog"
    exit 3
  fi
  local -a wanted=()
  if [[ -n "${NANNA_REPOS:-}" ]]; then
    IFS=',' read -r -a wanted <<< "$NANNA_REPOS"
  fi
  local -A seen=()
  local repo_list name volume _slug keep w
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
      say "repository volume $volume is missing; refusing to start without it. Run: $SCRIPT_DIR/lumo-repos.sh --only $name"
      exit 3
    fi
    repo_mounts+=(-v "$volume:/repos/$name")
    repo_names+=("$name")
  done <<< "$repo_list"
  for w in "${wanted[@]}"; do
    if [[ -z "${seen[$w]:-}" ]]; then
      say "NANNA_REPOS names $w, which is not a repository in the identity catalog"
      exit 2
    fi
  done
}

daemon_exists() { podman container exists "$DAEMON" > /dev/null 2>&1; }

daemon_running() {
  [[ "$(podman inspect --format '{{.State.Running}}' "$DAEMON" 2> /dev/null || true)" == "true" ]]
}

socket_ready() { podman exec "$DAEMON" test -S "$SOCKET" > /dev/null 2>&1; }

create_daemon() {
  local -a retention=() cargo=()
  if podman volume exists "$CARGO_VOLUME" > /dev/null 2>&1; then
    cargo=(-v "$CARGO_VOLUME:$CARGO_PATH:ro" -e "CARGO_HOME=$CARGO_PATH" -e CARGO_NET_OFFLINE=true -e "NANNA_SANDBOX_HIDE=$SANDBOX_HIDE")
  else
    say "the cargo-home volume $CARGO_VOLUME does not exist; starting without it, so the cargo tools stay unavailable. Run $SCRIPT_DIR/lumo-cargo-vendor.sh, then $SELF --restart"
  fi
  if [[ -n "${NANNA_RESULT_RETENTION_DAYS:-}" ]]; then
    retention=(-e "NANNA_RESULT_RETENTION_DAYS=$NANNA_RESULT_RETENTION_DAYS")
  fi
  podman run -d --name "$DAEMON" --pod "$POD" --restart=no \
    -v "$STATE_VOLUME:$STATE_PATH" \
    -v "$CONFIG_VOLUME:$CONFIG_PATH:ro" "${repo_mounts[@]}" "${cargo[@]}" \
    -e NANNA_CONFIG_DIR="$CONFIG_PATH" \
    -e NANNA_QUEUE_PATH="$STATE_PATH/queue.jsonl" \
    -e NANNA_STATE_DIR="$STATE_PATH" \
    -e "NANNA_AUDIT_TIMEOUT_SECS=$AUDIT_TIMEOUT" "${retention[@]}" \
    "$IMAGE" /bin/nanna mcp-serve --listen "$SOCKET" --model "$MODEL" > /dev/null
}

prune_worktrees() {
  local name
  for name in "${repo_names[@]}"; do
    podman exec "$DAEMON" git -C "/repos/$name" worktree prune > /dev/null 2>&1 \
      || say "could not prune stale worktree records in /repos/$name; continuing"
  done
}

wait_for_socket() {
  local waited=0 limit=$((WAIT_SECS * 2))
  while ((waited < limit)); do
    if ! daemon_running; then
      say "the container $DAEMON stopped while starting; last log lines:"
      podman logs --tail 20 "$DAEMON" >&2 2> /dev/null || true
      exit 4
    fi
    if socket_ready; then
      return 0
    fi
    sleep 0.5
    waited=$((waited + 1))
  done
  say "the daemon $DAEMON did not open $SOCKET within ${WAIT_SECS}s; last log lines:"
  podman logs --tail 20 "$DAEMON" >&2 2> /dev/null || true
  exit 4
}

ensure_daemon() {
  local started=0
  resolve_mounts
  if daemon_running; then
    wait_for_socket
    return 0
  fi
  if daemon_exists; then
    say "starting the stopped container $DAEMON"
    podman start "$DAEMON" > /dev/null
    started=1
  else
    say "creating the container $DAEMON from $IMAGE"
    if create_daemon; then
      started=1
    elif daemon_exists; then
      say "another launcher created $DAEMON first; using it"
    else
      say "podman could not create $DAEMON"
      exit 4
    fi
  fi
  wait_for_socket
  if [[ $started -eq 1 ]]; then
    prune_worktrees
  fi
}

stop_daemon() {
  if daemon_exists; then
    podman stop "$DAEMON" > /dev/null 2>&1 || true
    podman rm "$DAEMON" > /dev/null
    say "stopped and removed $DAEMON; the state volume $STATE_VOLUME and its results were kept. A task that was running is reported as failed (DaemonRestarted) on the next start."
  else
    say "$DAEMON does not exist"
  fi
}

task_counts() {
  local reply
  reply="$(printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tasks/list"}' \
    | podman exec -i "$DAEMON" /bin/nanna mcp-attach --client status 2> /dev/null)" || return 1
  local counts
  counts="$(grep -o '"status":"[a-z_]*"' <<< "$reply" | sort | uniq -c | awk '{gsub(/"status":"|"/, "", $2); printf "%s=%s ", $2, $1}' || true)"
  printf '%s\n' "${counts:-none}"
}

cargo_status() {
  local present=absent started=""
  if podman volume exists "$CARGO_VOLUME" > /dev/null 2>&1; then present=present; fi
  if daemon_running; then
    if podman inspect --format '{{range .Mounts}}{{.Name}}:{{.Destination}} {{end}}' "$DAEMON" 2> /dev/null | grep -qF " $CARGO_VOLUME:$CARGO_PATH " ; then
      started="; daemon started with it"
    else
      started="; daemon started WITHOUT it"
    fi
  fi
  if [[ "$present" == absent ]]; then
    printf 'cargo-home: absent (%s); cargo tools are unavailable; run %s/lumo-cargo-vendor.sh, then --restart%s\n' "$CARGO_VOLUME" "$SCRIPT_DIR" "$started"
  elif [[ "$started" == "; daemon started WITHOUT it" ]]; then
    printf 'cargo-home: present (%s)%s; run %s --restart to mount it\n' "$CARGO_VOLUME" "$started" "$SELF"
  else
    printf 'cargo-home: present (%s)%s\n' "$CARGO_VOLUME" "$started"
  fi
}

status_daemon() {
  cargo_status
  if ! daemon_exists; then
    printf 'daemon: absent (%s)\n' "$DAEMON"
    exit 1
  fi
  if ! daemon_running; then
    printf 'daemon: stopped (%s); the next launch restarts it\n' "$DAEMON"
    exit 1
  fi
  printf 'daemon: running (%s)\n' "$DAEMON"
  local have want
  have="$(podman inspect --format '{{.Image}}' "$DAEMON" 2> /dev/null || true)"
  want="$(podman image inspect --format '{{.Id}}' "$IMAGE" 2> /dev/null || true)"
  if [[ -z "$want" ]]; then
    printf 'image: %s (current %s not found locally)\n' "$have" "$IMAGE"
  elif [[ "$have" == "$want" ]]; then
    printf 'image: %s (current)\n' "$have"
  else
    printf 'image: STALE: running %s, %s is now %s; run %s --restart to replace it (tasks running now will be reported as failed)\n' "$have" "$IMAGE" "$want" "$SELF"
  fi
  if socket_ready; then
    printf 'socket: present (%s)\n' "$SOCKET"
    printf 'tasks: %s\n' "$(task_counts || printf 'unavailable')"
  else
    printf 'socket: absent (%s)\n' "$SOCKET"
  fi
}

usage() {
  cat << EOF
usage: $0 [--start | --stop | --restart | --status | --install-catalog | --print | --help]

With no argument: make sure the $DAEMON container is running, then run the
connection bridge (lumo-bridge.py), which relays stdin/stdout to it through
'nanna mcp-attach' and reconnects when the daemon restarts or the host sleeps.
This is the command Lumo Desktop runs.

  --start            ensure the daemon is running and its socket is open
  --stop             stop and remove the daemon; the $STATE_VOLUME volume is kept
  --restart          stop, remove and start the daemon (replaces a stale image)
  --status           running state, image freshness, socket, task counts and
                     whether the cargo-home volume exists and is mounted
  --install-catalog  copy your identity catalog into the $CONFIG_VOLUME volume
                     (from \$NANNA_CATALOG_SRC, else \$XDG_CONFIG_HOME/nanna/agents,
                     else ~/.config/nanna/agents; it must hold auditor.toml)
  --print            show the name and command to enter in Lumo

Cargo tools need the volume $CARGO_VOLUME (override with \$NANNA_CARGO_VOLUME),
filled by scripts/lumo-cargo-vendor.sh. The launcher mounts it read-only at
$CARGO_PATH when it exists, never creates it, and starts the daemon without it
otherwise. After vendoring, run --restart so the daemon mounts it.
EOF
}

case "${1:-}" in
  "")
    mode=attach
    ;;
  --print)
    printf 'name: nanna-coder\ntransport: stdio\ncommand: %s\n' "$SELF"
    exit 0
    ;;
  --install-catalog)
    install_catalog
    exit 0
    ;;
  --start) mode=start ;;
  --stop) mode=stop ;;
  --restart) mode=restart ;;
  --status) mode=status ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    say "unknown argument: $1"
    exit 2
    ;;
esac

require_podman

case "$mode" in
  stop)
    stop_daemon
    exit 0
    ;;
  status)
    status_daemon
    exit 0
    ;;
  restart)
    stop_daemon
    ensure_daemon
    exit 0
    ;;
  start)
    ensure_daemon
    exit 0
    ;;
esac

ensure_daemon
if ! command -v python3 > /dev/null 2>&1; then
  say 'python3 not found on PATH; the connection bridge needs it'
  exit 127
fi
exec python3 "$SCRIPT_DIR/lumo-bridge.py" --daemon "$DAEMON" --client "$CLIENT_TAG" --launcher "$SELF"
