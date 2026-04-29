#!/usr/bin/env bash
# install.sh - One-line installer for nanna-coder.
#
# Two install modes:
#   --mcp          Install nanna in a container and register it as an MCP
#                  tool with the Claude Code CLI.
#   --standalone   Install the nanna `harness` binary into the user's cargo
#                  bin path for direct use as a CLI.
#
# Common flags:
#   --local-model <name>   Ollama model tag to use (default: gemma4:e4b).
#                          Any model accepted by `ollama pull` works.
#   --dry-run              Print actions without executing system changes.
#                          Used by scripts/install_smoke.sh.
#   --container-runtime    `podman` or `docker` (default: auto-detect,
#                          podman preferred to match nanna's tooling).
#   --image                Container image reference for the harness when
#                          --mcp is used (default:
#                          ghcr.io/dominicburkart/nanna-coder-harness:latest).
#   --repo-url             Git URL for `cargo install` in --standalone mode
#                          (default: https://github.com/DominicBurkart/nanna-coder).
#   --branch               Git branch passed to cargo install (default: main).
#   -h, --help             Show this help and exit.
#
# Exit codes:
#   0  success
#   1  usage / argument error
#   2  missing required dependency
#   3  install step failed

set -euo pipefail

# ---------- defaults ----------
MODE=""                                          # mcp | standalone
LOCAL_MODEL="gemma4:e4b"
DRY_RUN="false"
CONTAINER_RUNTIME=""                             # auto-detected
IMAGE_REF="ghcr.io/dominicburkart/nanna-coder-harness:latest"
REPO_URL="https://github.com/DominicBurkart/nanna-coder"
BRANCH="main"

# ---------- logging ----------
log()  { printf '[install] %s\n' "$*" >&2; }
warn() { printf '[install][warn] %s\n' "$*" >&2; }
err()  { printf '[install][error] %s\n' "$*" >&2; }

# ---------- helpers ----------
usage() {
    sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
}

run() {
    # Echo the command. Execute unless --dry-run.
    log "+ $*"
    if [ "$DRY_RUN" = "true" ]; then
        return 0
    fi
    "$@"
}

require_cmd() {
    local cmd="$1"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        err "required command not found: $cmd"
        return 2
    fi
}

detect_runtime() {
    if [ -n "$CONTAINER_RUNTIME" ]; then
        printf '%s' "$CONTAINER_RUNTIME"
        return 0
    fi
    if command -v podman >/dev/null 2>&1; then
        printf 'podman'
    elif command -v docker >/dev/null 2>&1; then
        printf 'docker'
    else
        printf ''
    fi
}

# ---------- argument parsing ----------
parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --mcp)
                if [ -n "$MODE" ] && [ "$MODE" != "mcp" ]; then
                    err "cannot combine --mcp and --standalone"
                    return 1
                fi
                MODE="mcp"
                shift
                ;;
            --standalone)
                if [ -n "$MODE" ] && [ "$MODE" != "standalone" ]; then
                    err "cannot combine --mcp and --standalone"
                    return 1
                fi
                MODE="standalone"
                shift
                ;;
            --local-model)
                [ $# -ge 2 ] || { err "--local-model requires a value"; return 1; }
                LOCAL_MODEL="$2"
                shift 2
                ;;
            --local-model=*)
                LOCAL_MODEL="${1#*=}"
                shift
                ;;
            --container-runtime)
                [ $# -ge 2 ] || { err "--container-runtime requires a value"; return 1; }
                CONTAINER_RUNTIME="$2"
                shift 2
                ;;
            --container-runtime=*)
                CONTAINER_RUNTIME="${1#*=}"
                shift
                ;;
            --image)
                [ $# -ge 2 ] || { err "--image requires a value"; return 1; }
                IMAGE_REF="$2"
                shift 2
                ;;
            --image=*)
                IMAGE_REF="${1#*=}"
                shift
                ;;
            --repo-url)
                [ $# -ge 2 ] || { err "--repo-url requires a value"; return 1; }
                REPO_URL="$2"
                shift 2
                ;;
            --repo-url=*)
                REPO_URL="${1#*=}"
                shift
                ;;
            --branch)
                [ $# -ge 2 ] || { err "--branch requires a value"; return 1; }
                BRANCH="$2"
                shift 2
                ;;
            --branch=*)
                BRANCH="${1#*=}"
                shift
                ;;
            --dry-run)
                DRY_RUN="true"
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                err "unknown argument: $1"
                usage >&2
                return 1
                ;;
        esac
    done

    # Default mode is standalone if unspecified.
    if [ -z "$MODE" ]; then
        MODE="standalone"
    fi
}

# ---------- install: mcp (containerised) ----------
install_mcp() {
    log "mode: mcp (containerised, model=$LOCAL_MODEL)"

    local runtime
    runtime="$(detect_runtime)"
    if [ -z "$runtime" ]; then
        err "no container runtime found; install podman (preferred) or docker"
        return 2
    fi
    log "container runtime: $runtime"

    # Pull the harness container image.
    run "$runtime" pull "$IMAGE_REF"

    # Pull the requested Ollama model into the user's local Ollama install.
    if command -v ollama >/dev/null 2>&1; then
        run ollama pull "$LOCAL_MODEL"
    else
        warn "ollama CLI not found locally; skipping 'ollama pull $LOCAL_MODEL'."
        warn "the harness container will need an Ollama endpoint reachable at runtime."
    fi

    # Register with Claude Code CLI if available; otherwise emit JSON the user
    # can paste into ~/.claude/mcp_servers.json.
    local cmd_json
    cmd_json=$(cat <<JSON
{
  "mcpServers": {
    "nanna": {
      "command": "$runtime",
      "args": [
        "run", "--rm", "-i",
        "--network=host",
        "$IMAGE_REF",
        "mcp-serve",
        "--model", "$LOCAL_MODEL"
      ]
    }
  }
}
JSON
)

    if command -v claude >/dev/null 2>&1; then
        log "registering nanna with Claude Code CLI"
        # `claude mcp add` accepts: name -- command [args...]
        run claude mcp add nanna -- \
            "$runtime" run --rm -i --network=host \
            "$IMAGE_REF" mcp-serve --model "$LOCAL_MODEL"
    else
        warn "claude CLI not found; printing MCP config to stdout."
        warn "add the following to ~/.claude/mcp_servers.json (or your"
        warn "Claude Code MCP config) under the appropriate key:"
        if [ "$DRY_RUN" = "true" ]; then
            log "(dry-run) would print MCP JSON config"
        else
            printf '%s\n' "$cmd_json"
        fi
    fi

    log "mcp install complete"
}

# ---------- install: standalone ----------
install_standalone() {
    log "mode: standalone (cargo install, model=$LOCAL_MODEL)"

    require_cmd cargo || return 2

    # Pull the requested Ollama model so the binary works out of the box.
    if command -v ollama >/dev/null 2>&1; then
        run ollama pull "$LOCAL_MODEL"
    else
        warn "ollama CLI not found; install from https://ollama.ai/download"
        warn "and run: ollama pull $LOCAL_MODEL"
    fi

    # Install the harness binary from the configured git ref.
    run cargo install \
        --git "$REPO_URL" \
        --branch "$BRANCH" \
        --bin harness \
        --locked \
        harness

    cat <<MSG >&2
[install] standalone install complete.
[install] try:
[install]   harness agent --prompt "hello" --model $LOCAL_MODEL --tools
MSG
}

main() {
    parse_args "$@"
    case "$MODE" in
        mcp)        install_mcp ;;
        standalone) install_standalone ;;
        *)
            err "internal error: unknown mode '$MODE'"
            return 1
            ;;
    esac
}

main "$@"
