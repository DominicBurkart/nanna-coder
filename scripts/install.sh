#!/usr/bin/env bash
# install.sh - One-line installer for nanna-coder.
#
# Two install modes (one is REQUIRED; the script will not infer):
#   --mcp          Install nanna in a container and register it as an MCP
#                  tool with the Claude Code CLI.
#   --standalone   Install the nanna `harness` binary into the user's cargo
#                  bin path for direct use as a CLI.
#
# Common flags:
#   --local-model <name>      Ollama model tag to use (default: gemma4:e4b).
#                             Any model accepted by `ollama pull` works.
#   --dry-run                 Print actions without executing system changes.
#                             Used by scripts/install_smoke.sh.
#   --container-runtime       `podman` or `docker` (default: auto-detect,
#                             podman preferred to match nanna's tooling).
#   --image                   Container image reference for the harness when
#                             --mcp is used (default:
#                             ghcr.io/dominicburkart/nanna-coder-harness:latest).
#   --container-network       Container network mode for --mcp (default:
#                             bridge). Use 'host' only with
#                             --unsafe-host-network (see below).
#   --unsafe-host-network     Opt-in to --network=host for the registered
#                             MCP container. Exposes loopback / host LAN
#                             services to the harness; only enable when
#                             targeting a localhost Ollama with no
#                             alternative endpoint.
#   --repo-url                Git URL for `cargo install` in --standalone
#                             mode (default:
#                             https://github.com/DominicBurkart/nanna-coder).
#   --branch                  Git branch passed to cargo install (default:
#                             main). Tracks a moving ref; prefer --rev.
#   --rev                     Git commit / tag passed to cargo install. If
#                             set, takes precedence over --branch and gives
#                             a reproducible install.
#   -h, --help                Show this help and exit.
#
# Exit codes:
#   0  success
#   1  usage / argument error
#   2  missing required dependency
#   3  install step failed

set -euo pipefail

# ---------- defaults ----------
MODE=""                                          # mcp | standalone (REQUIRED)
LOCAL_MODEL="gemma4:e4b"
DRY_RUN="false"
CONTAINER_RUNTIME=""                             # auto-detected
IMAGE_REF="ghcr.io/dominicburkart/nanna-coder-harness:latest"
CONTAINER_NETWORK="bridge"                       # safe default
UNSAFE_HOST_NETWORK="false"                      # explicit opt-in for host net
REPO_URL="https://github.com/DominicBurkart/nanna-coder"
BRANCH="main"
REV=""

# ---------- logging ----------
log()  { printf '[install] %s\n' "$*" >&2; }
warn() { printf '[install][warn] %s\n' "$*" >&2; }
err()  { printf '[install][error] %s\n' "$*" >&2; }

# ---------- helpers ----------
# Static heredoc — does not depend on $0, so it works under
# `curl ... | bash -s -- --help` where $0 is `bash`.
usage() {
    cat <<'USAGE'
install.sh - One-line installer for nanna-coder.

Two install modes (one is REQUIRED; the script will not infer):
  --mcp          Install nanna in a container and register it as an MCP
                 tool with the Claude Code CLI.
  --standalone   Install the nanna `harness` binary into the user's cargo
                 bin path for direct use as a CLI.

Common flags:
  --local-model <name>      Ollama model tag (default: gemma4:e4b).
  --dry-run                 Print actions without executing changes.
  --container-runtime       podman | docker (default: auto-detect).
  --image                   Container image ref for --mcp.
  --container-network       Container network mode for --mcp (default:
                            bridge). Use 'host' only with
                            --unsafe-host-network.
  --unsafe-host-network     Opt-in to --network=host (loopback / host LAN
                            visibility for the harness container).
  --repo-url                Git URL for cargo install in --standalone.
  --branch                  Git branch (moving ref; default: main).
  --rev                     Git commit / tag (reproducible; preferred).
  -h, --help                Show this help and exit.

Exit codes:
  0 success | 1 usage error | 2 missing dependency | 3 install failed
USAGE
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

# Build the MCP JSON config safely. Uses jq with --arg when available so all
# user-controlled values ($runtime, $IMAGE_REF, $LOCAL_MODEL, $network) are
# JSON-escaped. Falls back to a pure-bash escape if jq is missing.
build_mcp_json() {
    local runtime="$1" image="$2" model="$3" network="$4"
    if command -v jq >/dev/null 2>&1; then
        jq -n \
            --arg runtime "$runtime" \
            --arg image   "$image" \
            --arg model   "$model" \
            --arg network "--network=$network" \
            '{
                mcpServers: {
                    nanna: {
                        command: $runtime,
                        args: [
                            "run", "--rm", "-i",
                            $network,
                            $image,
                            "mcp-serve",
                            "--model", $model
                        ]
                    }
                }
            }'
    else
        # Pure-bash JSON-escape: backslash, double-quote, control chars.
        _json_escape() {
            local s="$1" out="" i ch
            for (( i=0; i<${#s}; i++ )); do
                ch="${s:i:1}"
                case "$ch" in
                    $'\\') out+=$'\\\\' ;;
                    $'"')  out+=$'\\"' ;;
                    $'\n') out+=$'\\n' ;;
                    $'\r') out+=$'\\r' ;;
                    $'\t') out+=$'\\t' ;;
                    *)     out+="$ch" ;;
                esac
            done
            printf '%s' "$out"
        }
        local er ei em en
        er="$(_json_escape "$runtime")"
        ei="$(_json_escape "$image")"
        em="$(_json_escape "$model")"
        en="$(_json_escape "--network=$network")"
        cat <<JSON
{
  "mcpServers": {
    "nanna": {
      "command": "$er",
      "args": [
        "run", "--rm", "-i",
        "$en",
        "$ei",
        "mcp-serve",
        "--model", "$em"
      ]
    }
  }
}
JSON
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
            --container-network)
                [ $# -ge 2 ] || { err "--container-network requires a value"; return 1; }
                CONTAINER_NETWORK="$2"
                shift 2
                ;;
            --container-network=*)
                CONTAINER_NETWORK="${1#*=}"
                shift
                ;;
            --unsafe-host-network)
                UNSAFE_HOST_NETWORK="true"
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
            --rev)
                [ $# -ge 2 ] || { err "--rev requires a value"; return 1; }
                REV="$2"
                shift 2
                ;;
            --rev=*)
                REV="${1#*=}"
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

    # An explicit mode is REQUIRED. Refuse to silently fall through to
    # standalone (which would invoke `cargo install` without consent).
    if [ -z "$MODE" ]; then
        err "an install mode is required: pass --mcp or --standalone"
        usage >&2
        return 1
    fi

    # If the user asked for a host network, set CONTAINER_NETWORK=host
    # explicitly so a single knob carries through to install_mcp.
    if [ "$UNSAFE_HOST_NETWORK" = "true" ]; then
        CONTAINER_NETWORK="host"
    fi
    # If the user picked CONTAINER_NETWORK=host without --unsafe-host-network,
    # require the explicit opt-in.
    if [ "$CONTAINER_NETWORK" = "host" ] && [ "$UNSAFE_HOST_NETWORK" != "true" ]; then
        err "--container-network=host requires --unsafe-host-network (host networking exposes loopback / LAN to the container)"
        return 1
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
    log "container network: $CONTAINER_NETWORK"
    if [ "$CONTAINER_NETWORK" = "host" ]; then
        warn "using --network=host: harness can reach loopback services and the host LAN."
        warn "this is enabled because --unsafe-host-network was passed."
    fi

    # Pull the harness container image.
    run "$runtime" pull "$IMAGE_REF"

    # Pull the requested Ollama model into the user's local Ollama install.
    if command -v ollama >/dev/null 2>&1; then
        run ollama pull "$LOCAL_MODEL"
    else
        warn "ollama CLI not found locally; skipping 'ollama pull $LOCAL_MODEL'."
        warn "the harness container will need an Ollama endpoint reachable at runtime."
    fi

    # Build a safely-escaped JSON config (used for paste fallback below).
    local cmd_json
    cmd_json="$(build_mcp_json "$runtime" "$IMAGE_REF" "$LOCAL_MODEL" "$CONTAINER_NETWORK")"

    if command -v claude >/dev/null 2>&1; then
        log "registering nanna with Claude Code CLI (idempotent: remove-then-add)"
        # Idempotency: `claude mcp add` errors if the name already exists, so
        # remove any prior registration first. The remove is best-effort —
        # ignore failure when nothing is registered.
        if [ "$DRY_RUN" = "true" ]; then
            log "+ claude mcp remove nanna (best-effort)"
            log "+ claude mcp add nanna -- $runtime run --rm -i --network=$CONTAINER_NETWORK $IMAGE_REF mcp-serve --model $LOCAL_MODEL"
        else
            claude mcp remove nanna >/dev/null 2>&1 || true
            claude mcp add nanna -- \
                "$runtime" run --rm -i "--network=$CONTAINER_NETWORK" \
                "$IMAGE_REF" mcp-serve --model "$LOCAL_MODEL"
        fi
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

    # Install the harness binary. Prefer a pinned --rev for reproducibility;
    # fall back to --branch with a loud warning + commit-hash echo so the
    # user sees exactly what they got.
    if [ -n "$REV" ]; then
        log "pinning install to git rev: $REV"
        run cargo install \
            --git "$REPO_URL" \
            --rev "$REV" \
            --bin harness \
            --locked \
            --force \
            harness
    else
        warn "installing from --branch $BRANCH (a moving ref). For a"
        warn "reproducible install pass --rev <sha>. Resolving current $BRANCH HEAD..."
        local resolved=""
        if command -v git >/dev/null 2>&1; then
            resolved="$(git ls-remote "$REPO_URL" "refs/heads/$BRANCH" 2>/dev/null | awk '{print $1}' | head -n1 || true)"
        fi
        if [ -n "$resolved" ]; then
            warn "current $BRANCH HEAD = $resolved"
            warn "to reproduce this install: re-run with --rev $resolved"
        else
            warn "could not resolve $BRANCH HEAD (git missing or remote unreachable)"
        fi
        run cargo install \
            --git "$REPO_URL" \
            --branch "$BRANCH" \
            --bin harness \
            --locked \
            --force \
            harness
    fi

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
