#!/usr/bin/env bash
# Nanna Coder one-line installer (Linux + macOS).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/DominicBurkart/nanna-coder/main/scripts/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/DominicBurkart/nanna-coder/main/scripts/install.sh | bash -s -- --skip-model-pull
#
# Flags:
#   --skip-model-pull   Don't pull the Gemma 4 model. Bring up the pod with a
#                       model-less Ollama; you can pull a model later. Used by CI.
#   --no-start          Do everything except start the pod.
#   --registry <url>    Override container registry base (default:
#                       ghcr.io/dominicburkart/nanna-coder; env: NANNA_REGISTRY).
#   --tag <tag>         Image tag to pull (default: latest; env: NANNA_TAG).
#   --model <name>      Ollama model to pull (default: gemma4:e4b; env: NANNA_MODEL).
#   --harness-image <ref>  Use this exact harness image ref (overrides --registry/--tag).
#   --ollama-image <ref>   Use this exact ollama image ref (overrides --registry/--tag).
#   --harness-port <n>  Host port to publish the harness on (default: 18080; env:
#                       NANNA_PORT). Was 8080 historically; 18080 avoids the very
#                       common frontend-dev collision on :8080. See #330.
#   --use-host-ollama   Skip the in-pod ollama container; reuse the Ollama already
#                       running on the host at http://localhost:11434. Auto-enabled
#                       when a working Ollama is detected on :11434 unless
#                       --no-use-host-ollama is passed. (env: NANNA_USE_HOST_OLLAMA=1)
#   --no-use-host-ollama  Force the installer to start its own ollama container even
#                       if one is already running on the host. Useful for CI or when
#                       you explicitly want pod-isolated state.
#   --no-pull           Don't `podman pull` the images. Use already-loaded local images
#                       (e.g. loaded via nix2container's copyToPodman). Used by CI.
#   --dry-run           Print what the installer would do for the detected OS and exit.
#                       Doesn't install anything. Used by CI to validate the script
#                       across platforms without depending on container runtimes.
#   --yes               Don't prompt; assume yes for sudo notices.
#   --no-claude-mcp     Don't register nanna-coder as a Claude Code MCP server even
#                       if a Claude Code config is detected.
#   --branch <name>     Accepted for parity with install.ps1's -Branch passthrough.
#                       Has no effect on install.sh itself; the Windows installer
#                       fetches install.sh from the chosen branch *before* invoking
#                       it, so by the time we run, the branch choice is already
#                       baked into our $0. We accept and ignore the value so the
#                       Windows path doesn't trip the unknown-flag guard.
#   -h, --help          Show this help.
#
# What this script does (in order):
#   1. Installs Podman if missing (apt / dnf / pacman / zypper / brew). REQUIRES SUDO on Linux.
#   2. On macOS, initializes and starts a Podman machine if needed.
#   3. Pulls prebuilt nanna-coder harness + ollama images from the registry.
#   4. Creates a pod and starts the harness + ollama containers.
#   5. Pulls the Gemma 4 model into the running Ollama container.
#
# All steps that need sudo print a clear notification first explaining why.
# Build-from-source is available via `nix build .#gemma-container` (see README).

set -euo pipefail

REGISTRY="${NANNA_REGISTRY:-ghcr.io/dominicburkart/nanna-coder}"
TAG="${NANNA_TAG:-latest}"
# Ollama 0.20.7+ required for gemma4:e4b model -- see #332. Image ships
# pkgs.ollama from flake.lock'd nixpkgs (via the dedicated nixpkgs-ollama
# input pinned in flake.nix; do not bump the main nixpkgs to fix this).
MODEL="${NANNA_MODEL:-gemma4:e4b}"
HARNESS_IMAGE_OVERRIDE=""
OLLAMA_IMAGE_OVERRIDE=""
SKIP_MODEL_PULL=0
NO_START=0
NO_PULL=0
DRY_RUN=0
ASSUME_YES=0
NO_CLAUDE_MCP=0
# Default harness host port. Was 8080 (collides with most frontend dev
# servers); 18080 is high enough to be unclaimed by typical local stacks.
# See #330. Override via NANNA_PORT or --harness-port.
HARNESS_PORT="${NANNA_PORT:-18080}"
# Tri-state for host-ollama: empty means "auto-detect at audit time".
# Setting to 1 forces reuse of host Ollama; setting to 0 forces in-pod
# Ollama. See #330.
USE_HOST_OLLAMA="${NANNA_USE_HOST_OLLAMA:-}"

# Validate a string parses as a TCP port. Centralised so --harness-port
# and NANNA_PORT both flow through it, and the error message is the same
# regardless of how the bad value was supplied. See #330 review thread.
validate_port() {
  local v="$1" src="$2"
  if [[ ! "$v" =~ ^[0-9]+$ ]] || (( v <= 0 || v >= 65536 )); then
    echo "invalid port from $src: '$v' (must be an integer in 1..65535)" >&2
    exit 64
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-model-pull)    SKIP_MODEL_PULL=1; shift;;
    --no-start)           NO_START=1; shift;;
    --no-pull)            NO_PULL=1; shift;;
    --dry-run)            DRY_RUN=1; shift;;
    --registry)           REGISTRY="$2"; shift 2;;
    --tag)                TAG="$2"; shift 2;;
    --model)              MODEL="$2"; shift 2;;
    --harness-image)      HARNESS_IMAGE_OVERRIDE="$2"; shift 2;;
    --ollama-image)       OLLAMA_IMAGE_OVERRIDE="$2"; shift 2;;
    --harness-port)       HARNESS_PORT="$2"; validate_port "$HARNESS_PORT" "--harness-port"; shift 2;;
    --use-host-ollama)    USE_HOST_OLLAMA=1; shift;;
    --no-use-host-ollama) USE_HOST_OLLAMA=0; shift;;
    --yes|-y)             ASSUME_YES=1; shift;;
    --no-claude-mcp)      NO_CLAUDE_MCP=1; shift;;
    --branch)             shift; if [[ $# -gt 0 ]]; then shift; fi;;  # see help text; ignored in-script
    -h|--help)            sed -n '2,49p' "$0" 2>/dev/null || true; exit 0;;
    *) echo "unknown flag: $1" >&2; exit 64;;
  esac
done

# Validate the resolved port even if it came from NANNA_PORT (the
# --harness-port branch already validated; re-validating is cheap and
# closes the env-only path).
validate_port "$HARNESS_PORT" "NANNA_PORT/HARNESS_PORT"

HARNESS_IMAGE="${HARNESS_IMAGE_OVERRIDE:-$REGISTRY/harness:$TAG}"
OLLAMA_IMAGE="${OLLAMA_IMAGE_OVERRIDE:-$REGISTRY/ollama:$TAG}"
POD_NAME="nanna-coder-pod"

# ---------- helpers ----------

if [[ -t 1 ]]; then
  C_RESET=$'\033[0m'; C_BOLD=$'\033[1m'
  C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_RED=$'\033[31m'; C_BLUE=$'\033[34m'
else
  C_RESET=''; C_BOLD=''; C_GREEN=''; C_YELLOW=''; C_RED=''; C_BLUE=''
fi

log()  { printf '%s==>%s %s\n' "$C_BLUE$C_BOLD" "$C_RESET" "$*"; }
ok()   { printf '%s✓%s %s\n'  "$C_GREEN" "$C_RESET" "$*"; }
warn() { printf '%s!%s %s\n'  "$C_YELLOW" "$C_RESET" "$*" >&2; }
die()  { printf '%s✗%s %s\n'  "$C_RED" "$C_RESET" "$*" >&2; exit 1; }

notify_sudo() {
  # The wizard's plan + confirmation already covered the sudo step. Print
  # a short banner so the user knows this is the privileged moment, but
  # don't double-prompt; sudo itself will ask for the password.
  printf '\n%s┌─ sudo step ─────────────────────────────────────%s\n' "$C_YELLOW$C_BOLD" "$C_RESET"
  printf '%s│%s component: %s\n' "$C_YELLOW" "$C_RESET" "$1"
  printf '%s│%s reason:    %s\n' "$C_YELLOW" "$C_RESET" "$2"
  printf '%s└─────────────────────────────────────────────────%s\n\n' "$C_YELLOW" "$C_RESET"
}

have() { command -v "$1" >/dev/null 2>&1; }

is_wsl() { [[ -r /proc/version ]] && grep -qiE 'microsoft|wsl' /proc/version; }

# curl is a hard dependency: we use it for the host-ollama probe in
# audit_system, the readiness wait in wait_for_ollama, and the HTTP
# fallback in pull_model. If it's missing, the host-ollama auto-detect
# silently degrades to "in-pod mode", which then collides on :11434
# with the host Ollama the user actually has -- exactly the #330
# failure mode. Fail fast instead of degrading. See #330 review thread.
if ! have curl; then
  cat >&2 <<'EOF'
✗ curl is required but not installed.

  install.sh uses curl to:
    - probe the host Ollama at :11434 (auto-detection of #330's
      "ollama already running" case)
    - poll /api/tags during pod bring-up
    - fall back to /api/pull if no `ollama` CLI is on PATH
  Without curl, host-Ollama auto-detection silently misfires and you
  get the same :11434 conflict #330 was filed about.

  Install curl and re-run:
    Linux:  sudo apt-get install -y curl   (or dnf/pacman/zypper equivalent)
    macOS:  brew install curl              (or use the system curl)
EOF
  exit 1
fi

# ---------- OS detection ----------

case "$(uname -s)" in
  Linux*)
    if is_wsl; then
      cat >&2 <<'EOF'
✗ WSL2 (Windows Subsystem for Linux) is not a supported install target for
  scripts/install.sh.

  The bash installer drives podman directly inside the host kernel, but
  WSL2's kernel does not provide the network-namespace bind-mount path
  (/run/netns/) that podman pod creation requires. You will get errors
  such as:
    failed to bind mount ns at /run/netns/...: invalid argument

  Supported Windows install path: run scripts/install.ps1 from a Windows
  PowerShell session, which sets up Podman Desktop / WSL with a
  preconfigured environment. See README for details.
EOF
      die "WSL2 detected; not supported by install.sh"
    fi
    OS=linux ;;
  Darwin*) OS=macos ;;
  *) die "unsupported OS: $(uname -s). Linux and macOS only. (Windows: use scripts/install.ps1)" ;;
esac
ARCH="$(uname -m)"

# ---------- audit / plan / confirm ----------
#
# Before doing anything, inspect the system and tell the user exactly which
# steps will run, which will be skipped (already done), and where sudo will
# kick in. This is the wizard the user sees first; nothing destructive runs
# until they confirm. resolve_image_ref() is defined later but only called
# from audit_system() during execution — fine because audit runs after all
# functions are defined.

port_in_use_by() {
  local port="$1"
  if have ss; then
    ss -ltnp 2>/dev/null | awk -v p=":$port\$" '$4 ~ p' | head -3
  elif have lsof; then
    lsof -nP -iTCP:"$port" -sTCP:LISTEN 2>/dev/null | tail -n +2 | head -3
  elif have netstat; then
    netstat -an 2>/dev/null | awk -v p="\\.$port\$|:$port\$" '$4 ~ p && /LISTEN/' | head -3
  fi
}

detect_pkg_mgr() {
  if [[ "$OS" == macos ]]; then echo brew; return; fi
  for m in apt-get dnf pacman zypper; do have "$m" && { echo "$m"; return; }; done
  echo "(none — manual podman install required)"
}

claude_config_path() {
  # Prefer ~/.claude.json (the canonical user-scoped Claude Code config),
  # fall back to ~/.claude/settings.json. Both accept a top-level
  # `mcpServers` object.
  local p
  for p in "$HOME/.claude.json" "$HOME/.claude/settings.json"; do
    [[ -f "$p" ]] && { echo "$p"; return 0; }
  done
  return 1
}

# Parse the JSON body returned by GET /api/version into a bare version
# string. Falls back to a regex when jq is missing so the plan output
# never displays raw `{"version":"..."}` to the user. See #330 review.
parse_ollama_version() {
  local body="${1:-}"
  [[ -z "$body" ]] && { echo ""; return 0; }
  if have jq; then
    local v
    v="$(printf '%s' "$body" | jq -r '.version // empty' 2>/dev/null || true)"
    [[ -n "$v" ]] && { echo "$v"; return 0; }
  fi
  # Fallback: extract the first "version":"x.y.z" occurrence.
  local v
  v="$(printf '%s' "$body" | tr -d '\n\r' \
       | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
       | head -1)"
  [[ -n "$v" ]] && { echo "$v"; return 0; }
  echo ""
}

# Best-effort: detect whether port 11434 is held by *our own* pod's
# ollama-service container, vs. some other listener (the host Ollama
# the user is asking us to reuse). Returns 0 if our pod owns it.
#
# Without this check, auto-detect on a re-run would mistake the
# previous run's in-pod ollama-service for a new "host ollama" and
# silently flip `USE_HOST_OLLAMA=1` -- which would then tear the pod
# down and re-create it WITHOUT publishing :11434, breaking the next
# re-run when the in-pod ollama is no longer there to answer the probe.
# Steady-state behaviour of repeated invocations would not be
# idempotent. See #330 review thread on idempotency.
host_ollama_is_our_pod() {
  have podman || return 1
  podman pod exists "$POD_NAME" 2>/dev/null || return 1
  # `podman port <pod> 11434` prints the host:port mapping if the pod
  # publishes :11434, exits non-zero otherwise. That's a precise signal
  # that *this* pod is the listener.
  podman port "$POD_NAME" 11434 >/dev/null 2>&1
}

audit_system() {
  AUDIT_PODMAN=no
  AUDIT_PODMAN_VERSION=""
  AUDIT_MACHINE_RUNNING=no
  AUDIT_POD_EXISTS=no
  AUDIT_HARNESS_PORT_HOLDER=""
  AUDIT_PORT_11434_HOLDER=""
  AUDIT_HOST_OLLAMA_REACHABLE=no
  AUDIT_HOST_OLLAMA_VERSION=""
  AUDIT_HOST_OLLAMA_IS_OUR_POD=no
  AUDIT_HARNESS_LOADED=no
  AUDIT_OLLAMA_LOADED=no
  AUDIT_CLAUDE_CONFIG=""

  if have podman; then
    AUDIT_PODMAN=yes
    AUDIT_PODMAN_VERSION="$(podman --version 2>/dev/null | head -1)"
    if [[ "$OS" == macos ]] && [[ "${NANNA_SKIP_PODMAN_MACHINE:-0}" != "1" ]]; then
      podman machine list --format '{{.Running}}' 2>/dev/null | grep -qi true \
        && AUDIT_MACHINE_RUNNING=yes
    fi
    # podman info works only once the machine is up (mac) or the daemon is
    # responsive (linux). Skip pod/image probes silently if it isn't.
    if podman info >/dev/null 2>&1; then
      podman pod exists "$POD_NAME" 2>/dev/null && AUDIT_POD_EXISTS=yes
      if [[ $NO_PULL -eq 1 ]]; then
        # Self-contained: don't depend on resolve_image_ref (defined later).
        for cand in "$HARNESS_IMAGE" "localhost/$HARNESS_IMAGE" "docker.io/library/$HARNESS_IMAGE"; do
          podman image exists "$cand" 2>/dev/null && AUDIT_HARNESS_LOADED=yes && break
        done
        for cand in "$OLLAMA_IMAGE" "localhost/$OLLAMA_IMAGE" "docker.io/library/$OLLAMA_IMAGE"; do
          podman image exists "$cand" 2>/dev/null && AUDIT_OLLAMA_LOADED=yes && break
        done
      fi
    fi
  fi

  AUDIT_HARNESS_PORT_HOLDER="$(port_in_use_by "$HARNESS_PORT")"
  AUDIT_PORT_11434_HOLDER="$(port_in_use_by 11434)"

  # Probe host Ollama only when the user hasn't pinned the tri-state.
  # When --use-host-ollama or --no-use-host-ollama is passed explicitly,
  # the curl probe is just a cosmetic "we noticed Ollama is/isn't there"
  # plan-line; skipping it here removes a 2s delay on re-runs that pin
  # the choice. A *real* Ollama answers /api/version with a JSON blob;
  # a random process on :11434 won't, so this is a sharper signal than
  # just "port is open". See #330.
  if [[ -z "$USE_HOST_OLLAMA" ]]; then
    local body
    body="$(curl -fsS --max-time 2 http://localhost:11434/api/version 2>/dev/null || true)"
    if [[ -n "$body" ]]; then
      AUDIT_HOST_OLLAMA_REACHABLE=yes
      AUDIT_HOST_OLLAMA_VERSION="$body"
    fi
  fi

  # Re-entrancy guard: the probe can't tell our own pod's ollama-service
  # apart from a real host Ollama. If our pod owns :11434, the listener
  # is *us*, not the user's external Ollama, and auto-flipping to host
  # mode would tear the pod down. Treat this as "in-pod" regardless of
  # what /api/version said. See #330 idempotency thread.
  if [[ "$AUDIT_HOST_OLLAMA_REACHABLE" == yes ]] && host_ollama_is_our_pod; then
    AUDIT_HOST_OLLAMA_IS_OUR_POD=yes
    AUDIT_HOST_OLLAMA_REACHABLE=no
    AUDIT_HOST_OLLAMA_VERSION=""
  fi

  # Resolve the host-ollama tri-state. If the user didn't pin it, default
  # to "yes" iff a real Ollama answered AND it isn't our own pod.
  # Otherwise honour the explicit 0/1 from CLI/env.
  if [[ -z "$USE_HOST_OLLAMA" ]]; then
    if [[ "$AUDIT_HOST_OLLAMA_REACHABLE" == yes ]]; then
      USE_HOST_OLLAMA=1
    else
      USE_HOST_OLLAMA=0
    fi
  fi

  AUDIT_CLAUDE_CONFIG="$(claude_config_path 2>/dev/null || true)"
}

print_plan() {
  printf '\n%sNanna Coder installer%s\n' "$C_BOLD" "$C_RESET"
  printf '  detected:       %s/%s\n' "$OS" "$ARCH"
  printf '  registry:       %s\n' "$REGISTRY"
  printf '  tag:            %s\n' "$TAG"
  printf '  model:          %s%s\n' "$MODEL" \
    "$([[ $SKIP_MODEL_PULL -eq 1 ]] && echo ' (skipped)')"
  printf '  pod name:       %s\n' "$POD_NAME"
  if [[ "$HARNESS_PORT" == "18080" ]]; then
    printf '  harness port:   %s (default; override with --harness-port / NANNA_PORT)\n' "$HARNESS_PORT"
  else
    printf '  harness port:   %s (overridden from default 18080)\n' "$HARNESS_PORT"
  fi
  if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
    printf '  ollama:         host (reusing existing Ollama on :11434)\n'
  else
    printf '  ollama:         in-pod container\n'
  fi

  printf '\n%sCurrent state:%s\n' "$C_BOLD" "$C_RESET"
  if [[ "$AUDIT_PODMAN" == yes ]]; then
    printf '  %s✓%s %s\n' "$C_GREEN" "$C_RESET" "$AUDIT_PODMAN_VERSION"
  else
    printf '  %s✗%s podman: not installed\n' "$C_RED" "$C_RESET"
  fi
  if [[ "$OS" == macos && "${NANNA_SKIP_PODMAN_MACHINE:-0}" != "1" ]]; then
    if [[ "$AUDIT_MACHINE_RUNNING" == yes ]]; then
      printf '  %s✓%s podman machine: running\n' "$C_GREEN" "$C_RESET"
    elif [[ "$AUDIT_PODMAN" == yes ]]; then
      printf '  %s✗%s podman machine: not running\n' "$C_RED" "$C_RESET"
    fi
  fi
  if [[ "$AUDIT_POD_EXISTS" == yes ]]; then
    printf '  %s!%s existing pod %s detected (will be replaced for a clean start)\n' \
      "$C_YELLOW" "$C_RESET" "$POD_NAME"
  fi
  if [[ "$AUDIT_HOST_OLLAMA_IS_OUR_POD" == yes ]]; then
    printf '  %si%s :11434 is held by the existing %s (in-pod ollama-service); not treating it as a host Ollama\n' \
      "$C_BLUE" "$C_RESET" "$POD_NAME"
  fi
  if [[ "$AUDIT_HOST_OLLAMA_REACHABLE" == yes ]]; then
    local _v
    _v="$(parse_ollama_version "$AUDIT_HOST_OLLAMA_VERSION")"
    [[ -z "$_v" ]] && _v="(unparseable)"
    if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
      printf '  %s✓%s host Ollama detected on :11434 (%s) — re-using it (skipping in-pod ollama)\n' \
        "$C_GREEN" "$C_RESET" "$_v"
    else
      printf '  %si%s host Ollama detected on :11434 (%s) — ignored (--no-use-host-ollama)\n' \
        "$C_BLUE" "$C_RESET" "$_v"
    fi
  fi
  if [[ -n "$AUDIT_HARNESS_PORT_HOLDER" && "$AUDIT_POD_EXISTS" != yes ]]; then
    printf '  %s!%s port %s in use by another process:\n' "$C_YELLOW" "$C_RESET" "$HARNESS_PORT"
    printf '%s\n' "$AUDIT_HARNESS_PORT_HOLDER" | sed 's/^/      /'
  fi
  # Only flag :11434 conflict when we plan to bind it ourselves. In
  # host-ollama mode, the listener on :11434 *is* the thing we're going
  # to use, so it's not a conflict.
  if [[ "$USE_HOST_OLLAMA" != 1 \
        && -n "$AUDIT_PORT_11434_HOLDER" \
        && "$AUDIT_POD_EXISTS" != yes ]]; then
    printf '  %s!%s port 11434 in use by another process:\n' "$C_YELLOW" "$C_RESET"
    printf '%s\n' "$AUDIT_PORT_11434_HOLDER" | sed 's/^/      /'
  fi
  if [[ $NO_PULL -eq 1 ]]; then
    if [[ "$AUDIT_HARNESS_LOADED" == yes ]]; then
      printf '  %s✓%s harness image already loaded locally\n' "$C_GREEN" "$C_RESET"
    fi
    if [[ "$AUDIT_OLLAMA_LOADED" == yes ]]; then
      printf '  %s✓%s ollama image already loaded locally\n' "$C_GREEN" "$C_RESET"
    fi
  fi
  if [[ -n "$AUDIT_CLAUDE_CONFIG" ]]; then
    if [[ $NO_CLAUDE_MCP -eq 1 ]]; then
      printf '  %si%s Claude Code config at %s (skipping auto-MCP registration: --no-claude-mcp)\n' \
        "$C_BLUE" "$C_RESET" "$AUDIT_CLAUDE_CONFIG"
    elif ! have jq; then
      printf '  %s!%s Claude Code config at %s — jq not installed, MCP auto-registration will be skipped\n' \
        "$C_YELLOW" "$C_RESET" "$AUDIT_CLAUDE_CONFIG"
    else
      printf '  %s✓%s Claude Code config at %s (will register nanna-coder as MCP server)\n' \
        "$C_GREEN" "$C_RESET" "$AUDIT_CLAUDE_CONFIG"
    fi
  fi

  printf '\n%sPlan:%s\n' "$C_BOLD" "$C_RESET"
  local n=1 sudo_needed=0
  if [[ "$AUDIT_PODMAN" != yes ]]; then
    local pm; pm="$(detect_pkg_mgr)"
    if [[ "$OS" == linux ]]; then
      printf '  %d. Install podman via %s   %s(REQUIRES SUDO)%s\n' \
        "$n" "$pm" "$C_YELLOW$C_BOLD" "$C_RESET"
      sudo_needed=1
    else
      printf '  %d. Install podman via %s\n' "$n" "$pm"
    fi
    n=$((n+1))
  else
    printf '  %s·%s podman already installed — skipping install\n' "$C_BLUE" "$C_RESET"
  fi
  if [[ "$OS" == macos && "${NANNA_SKIP_PODMAN_MACHINE:-0}" != "1" ]]; then
    if [[ "$AUDIT_MACHINE_RUNNING" != yes ]]; then
      printf '  %d. Init/start podman machine\n' "$n"; n=$((n+1))
    else
      printf '  %s·%s podman machine already running — skipping\n' "$C_BLUE" "$C_RESET"
    fi
  fi
  if [[ $NO_PULL -eq 1 ]]; then
    if [[ "$AUDIT_HARNESS_LOADED" == yes && "$AUDIT_OLLAMA_LOADED" == yes ]]; then
      printf '  %s·%s images already loaded (--no-pull) — skipping pull\n' "$C_BLUE" "$C_RESET"
    else
      printf '  %d. Resolve already-loaded images (--no-pull set)\n' "$n"; n=$((n+1))
    fi
  else
    printf '  %d. Pull harness + ollama images from %s\n' "$n" "$REGISTRY"; n=$((n+1))
  fi
  if [[ $NO_START -eq 0 ]]; then
    local pub_desc
    if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
      pub_desc=":${HARNESS_PORT} (ollama: re-using host)"
    else
      pub_desc=":${HARNESS_PORT} and :11434"
    fi
    if [[ "$AUDIT_POD_EXISTS" == yes ]]; then
      printf '  %d. Stop and remove existing %s, recreate publishing %s\n' \
        "$n" "$POD_NAME" "$pub_desc"
    else
      printf '  %d. Create pod %s publishing %s\n' "$n" "$POD_NAME" "$pub_desc"
    fi
    n=$((n+1))
    if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
      printf '  %d. Skip in-pod ollama-service; use host Ollama at http://localhost:11434\n' "$n"; n=$((n+1))
      printf '  %d. Verify host ollama API at http://localhost:11434/api/tags\n' "$n"; n=$((n+1))
    else
      printf '  %d. Run ollama-service inside the pod\n' "$n"; n=$((n+1))
      printf '  %d. Wait for ollama API at http://localhost:11434/api/tags (120s timeout)\n' "$n"; n=$((n+1))
    fi
    if [[ $SKIP_MODEL_PULL -eq 0 ]]; then
      if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
        printf '  %d. Pull model %s via host ollama (multi-GB, the slow step)\n' "$n" "$MODEL"; n=$((n+1))
      else
        printf '  %d. Pull model %s (multi-GB, the slow step)\n' "$n" "$MODEL"; n=$((n+1))
      fi
    fi
    if [[ -n "$AUDIT_CLAUDE_CONFIG" && $NO_CLAUDE_MCP -eq 0 ]] && have jq; then
      printf '  %d. Register nanna-coder as MCP server in %s (idempotent; backs up the file first)\n' \
        "$n" "$AUDIT_CLAUDE_CONFIG"; n=$((n+1))
    fi
  else
    printf '  %s·%s --no-start: skipping pod bring-up\n' "$C_BLUE" "$C_RESET"
  fi

  if [[ $sudo_needed -eq 1 ]]; then
    printf '\n  %sNote:%s the install step needs sudo. The shell will prompt for your password before the privileged command runs.\n' \
      "$C_YELLOW$C_BOLD" "$C_RESET"
  fi

  printf '\n%sIdempotency:%s re-running this script is safe — it skips any step\n' \
    "$C_BOLD" "$C_RESET"
  printf '  already complete and replaces an existing %s for a clean restart.\n' "$POD_NAME"
}

preflight_port_check() {
  # If our pod already exists, tear it down FIRST so its containers
  # release any ports they hold. Then probe ports — anything still
  # listening is a real conflict from a process outside our pod.
  # Previously start_pod() did the cleanup, but that meant the audit's
  # port probe ran while our stale pod's containers were still bound,
  # masking conflicts from other processes (e.g. a host ollama on
  # :11434).
  if [[ "$AUDIT_POD_EXISTS" == yes ]]; then
    log "removing existing $POD_NAME so its ports are released before the conflict check..."
    podman pod stop "$POD_NAME" >/dev/null 2>&1 || true
    podman pod rm   "$POD_NAME" >/dev/null 2>&1 || true
    AUDIT_POD_EXISTS=no
    # Give the kernel a moment to release the listening sockets after
    # pasta exits. Without this, the re-probe can race and miss a still-
    # transitioning port.
    sleep 1
    AUDIT_HARNESS_PORT_HOLDER="$(port_in_use_by "$HARNESS_PORT")"
    AUDIT_PORT_11434_HOLDER="$(port_in_use_by 11434)"
  fi

  local conflict=0 holder
  # Always check the harness port. Only check 11434 when we plan to
  # publish it ourselves; in host-ollama mode the holder of :11434 is
  # the Ollama we're going to talk to (#330).
  local specs=("$HARNESS_PORT:AUDIT_HARNESS_PORT_HOLDER")
  if [[ "$USE_HOST_OLLAMA" != 1 ]]; then
    specs+=("11434:AUDIT_PORT_11434_HOLDER")
  fi
  for spec in "${specs[@]}"; do
    local port="${spec%%:*}" var="${spec##*:}"
    holder="${!var}"
    [[ -z "$holder" ]] && continue
    warn "port $port is held by another process:"
    printf '%s\n' "$holder" | sed 's/^/    /' >&2
    conflict=1
  done
  [[ $conflict -eq 0 ]] && return 0
  cat >&2 <<EOF

${C_RED}${C_BOLD}✗ port conflict on ${HARNESS_PORT} and/or 11434${C_RESET}

Free the port(s) and re-run. Common culprits:
  - host ollama on 11434:
      pass --use-host-ollama to re-use it (or auto-detect on next run), or:
      Linux:  sudo systemctl stop ollama
      macOS:  pkill ollama   (or brew services stop ollama)
  - another pod publishing the same ports:
      podman ps -a | grep -E ':${HARNESS_PORT}|:11434'
  - an orphaned process holding the harness port:
      Linux:  ss -ltnp | grep -E ':${HARNESS_PORT}|:11434'
      macOS:  lsof -iTCP:${HARNESS_PORT} -sTCP:LISTEN
EOF
  exit 1
}

confirm_plan() {
  if [[ $ASSUME_YES -eq 1 ]]; then
    log "--yes set: proceeding without prompt"
    return 0
  fi
  if [[ ! -t 0 ]]; then
    log "non-interactive (no tty): proceeding"
    return 0
  fi
  printf '\n'
  read -r -p "Proceed with the plan above? [Y/n] " ans
  case "$ans" in N|n|no|No) die "aborted by user" ;; esac
}

register_claude_mcp() {
  [[ -z "$AUDIT_CLAUDE_CONFIG" ]] && return 0
  if [[ $NO_CLAUDE_MCP -eq 1 ]]; then
    log "skipping Claude MCP registration (--no-claude-mcp)"
    return 0
  fi
  if ! have jq; then
    warn "jq not installed; skipping Claude Code MCP auto-registration."
    warn "To enable later: install jq, then re-run this script."
    return 0
  fi

  # Build the desired MCP server entry. Stdio transport: Claude Code
  # spawns `podman run` with -i so the harness's mcp-serve can talk JSON
  # over stdin/stdout. --rm cleans up the per-invocation container.
  # --pod attaches to nanna-coder-pod's network namespace so the harness
  # reaches ollama-service at http://localhost:11434. In host-Ollama
  # mode the pod has no ollama-service; we tell the harness to talk to
  # the host's loopback via host.containers.internal (#330).
  local entry ollama_url
  if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
    ollama_url="http://host.containers.internal:11434"
  else
    ollama_url="http://localhost:11434"
  fi
  entry=$(jq -n \
    --arg pod "$POD_NAME" \
    --arg img "$HARNESS_IMAGE" \
    --arg model "$MODEL" \
    --arg ollama "$ollama_url" \
    '{
      command: "podman",
      args: ["run","--rm","-i","--pod",$pod,"-e","OLLAMA_URL="+$ollama,$img,"/bin/harness","mcp-serve","--model",$model]
    }')

  # Idempotent: if an entry with the same content already exists, skip.
  local existing
  existing=$(jq -er '.mcpServers["nanna-coder"] // empty' "$AUDIT_CLAUDE_CONFIG" 2>/dev/null || true)
  if [[ -n "$existing" ]] \
     && jq -en --argjson a "$existing" --argjson b "$entry" '$a == $b' >/dev/null 2>&1; then
    ok "Claude MCP entry for nanna-coder already up-to-date — skipping write"
    return 0
  fi

  # Backup before mutating user state.
  local backup ts
  ts=$(date +%Y%m%dT%H%M%S)
  backup="${AUDIT_CLAUDE_CONFIG}.nanna-coder.bak.${ts}"
  cp "$AUDIT_CLAUDE_CONFIG" "$backup"
  log "backed up $AUDIT_CLAUDE_CONFIG -> $backup"

  # Merge: ensure .mcpServers exists, then set our entry. jq's
  # `(.x // {})` idiom handles both "key missing" and "key is null".
  local tmp; tmp="$(mktemp)"
  if ! jq --argjson entry "$entry" \
        '.mcpServers = (.mcpServers // {}) | .mcpServers["nanna-coder"] = $entry' \
        "$AUDIT_CLAUDE_CONFIG" > "$tmp"; then
    rm -f "$tmp"
    warn "jq failed to update $AUDIT_CLAUDE_CONFIG; original left untouched (backup at $backup)."
    return 1
  fi
  mv "$tmp" "$AUDIT_CLAUDE_CONFIG"
  ok "registered nanna-coder as MCP server in $AUDIT_CLAUDE_CONFIG"
  log "  command:  podman run --rm -i --pod $POD_NAME $HARNESS_IMAGE /bin/harness mcp-serve --model $MODEL"
}

audit_system
print_plan

if [[ $DRY_RUN -eq 1 ]]; then
  printf '\n%s--dry-run set: nothing executed.%s\n' "$C_YELLOW$C_BOLD" "$C_RESET"
  exit 0
fi

confirm_plan
preflight_port_check

# ---------- 1. podman ----------

install_podman_linux() {
  if have apt-get; then
    notify_sudo "Podman" "Installing podman via apt-get. Podman runs the model + harness containers."
    sudo apt-get update -y
    sudo apt-get install -y podman
  elif have dnf; then
    notify_sudo "Podman" "Installing podman via dnf."
    sudo dnf install -y podman
  elif have pacman; then
    notify_sudo "Podman" "Installing podman via pacman."
    sudo pacman -Sy --noconfirm podman
  elif have zypper; then
    notify_sudo "Podman" "Installing podman via zypper."
    sudo zypper install -y podman
  else
    die "no supported package manager found (apt-get/dnf/pacman/zypper). Install podman manually then re-run."
  fi
}

install_podman_macos() {
  if ! have brew; then
    die "Homebrew not found. Install from https://brew.sh and re-run, or install podman manually."
  fi
  log "installing podman via brew (no sudo needed)..."
  brew install podman
}

ensure_podman_machine_macos() {
  if ! podman machine list --format '{{.Name}}' 2>/dev/null | grep -q .; then
    log "initializing podman machine (this can take a few minutes)..."
    podman machine init
  fi
  if ! podman machine list --format '{{.Running}}' 2>/dev/null | grep -qi true; then
    log "starting podman machine..."
    podman machine start
  fi
}

if have podman; then
  ok "podman already installed: $(podman --version)"
else
  if [[ "$OS" == linux ]]; then install_podman_linux; else install_podman_macos; fi
  have podman || die "podman install reported success but \`podman\` not on PATH"
  ok "podman installed: $(podman --version)"
fi

if [[ "$OS" == macos ]]; then
  if [[ "${NANNA_SKIP_PODMAN_MACHINE:-0}" == "1" ]]; then
    warn "NANNA_SKIP_PODMAN_MACHINE=1: not touching podman machine (caller manages the VM, e.g. colima)."
  else
    ensure_podman_machine_macos
  fi
fi

# Sanity: podman is responsive.
if ! podman info >/dev/null 2>&1; then
  die "podman is installed but not responsive. Try: podman machine start (macOS) or starting the podman service (Linux)."
fi

# ---------- 2. pull images ----------

# `podman image exists <ref>` is an exact-match check, but `podman load` of a
# docker-archive often stores short-name images with a `localhost/` prefix and
# `podman run`/`podman tag` may reject bare refs under enforcing short-name
# resolution. Resolve a user-supplied ref to whatever podman actually has it
# stored as, so downstream `podman run` uses the canonical ref.
resolve_image_ref() {
  local ref="$1" candidate
  for candidate in "$ref" "localhost/$ref" "docker.io/library/$ref"; do
    if podman image exists "$candidate" 2>/dev/null; then
      echo "$candidate"
      return 0
    fi
  done
  candidate=$(podman images --format '{{.Repository}}:{{.Tag}}' \
    | grep -E "(^|/)${ref//\//\\/}\$" | head -1)
  if [[ -n "$candidate" ]]; then
    echo "$candidate"
    return 0
  fi
  return 1
}

if [[ $NO_PULL -eq 1 ]]; then
  warn "--no-pull set: skipping podman pull. Expecting images already loaded:"
  warn "  $HARNESS_IMAGE"
  if [[ "$USE_HOST_OLLAMA" != 1 ]]; then
    warn "  $OLLAMA_IMAGE"
  fi
  if HARNESS_RESOLVED=$(resolve_image_ref "$HARNESS_IMAGE"); then
    [[ "$HARNESS_RESOLVED" == "$HARNESS_IMAGE" ]] || \
      log "harness image resolved: $HARNESS_IMAGE -> $HARNESS_RESOLVED"
    HARNESS_IMAGE="$HARNESS_RESOLVED"
  else
    podman images >&2 || true
    die "harness image $HARNESS_IMAGE not loaded locally and --no-pull was set."
  fi
  if [[ "$USE_HOST_OLLAMA" != 1 ]]; then
    if OLLAMA_RESOLVED=$(resolve_image_ref "$OLLAMA_IMAGE"); then
      [[ "$OLLAMA_RESOLVED" == "$OLLAMA_IMAGE" ]] || \
        log "ollama image resolved: $OLLAMA_IMAGE -> $OLLAMA_RESOLVED"
      OLLAMA_IMAGE="$OLLAMA_RESOLVED"
    else
      podman images >&2 || true
      die "ollama image $OLLAMA_IMAGE not loaded locally and --no-pull was set."
    fi
  fi
else
  log "pulling $HARNESS_IMAGE"
  podman pull "$HARNESS_IMAGE" \
    || die "failed to pull $HARNESS_IMAGE. The image may be private; run \`podman login ghcr.io\` and re-run, or override with --registry."

  if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
    log "host Ollama mode: skipping pull of $OLLAMA_IMAGE"
  else
    log "pulling $OLLAMA_IMAGE"
    podman pull "$OLLAMA_IMAGE" || die "failed to pull $OLLAMA_IMAGE."
  fi

  ok "images pulled"
fi

# ---------- 3. start pod ----------

start_pod() {
  # preflight_port_check() handles teardown of any existing pod before
  # we get here, so this function can assume a clean slate.
  if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
    # Host-Ollama mode: only publish the harness port; host already
    # owns :11434 and we'll talk to it via host.containers.internal
    # from inside the pod's netns. See #330.
    log "creating pod $POD_NAME (port ${HARNESS_PORT}; ollama: host)..."
    podman pod create --name "$POD_NAME" -p "${HARNESS_PORT}:8080"
    ok "pod $POD_NAME up; using host Ollama at http://localhost:11434 (no in-pod ollama-service)"
  else
    log "creating pod $POD_NAME (ports ${HARNESS_PORT}, 11434)..."
    podman pod create --name "$POD_NAME" -p "${HARNESS_PORT}:8080" -p 11434:11434

    log "starting ollama-service..."
    podman run -d --pod "$POD_NAME" --name ollama-service \
      -v ollama-data:/root/.ollama "$OLLAMA_IMAGE"
  fi

  # The harness binary is a CLI, not a long-running daemon, so we don't keep a
  # harness-service container running. Invoke it on-demand against the pod:
  #   podman run --rm --pod $POD_NAME -e OLLAMA_URL=http://localhost:11434 \
  #     $HARNESS_IMAGE /bin/harness <subcommand> ...
}

dump_ollama_logs() {
  printf '%s┌─ podman logs ollama-service (last 50 lines) ─────%s\n' "$C_YELLOW$C_BOLD" "$C_RESET" >&2
  podman logs --tail 50 ollama-service 2>&1 | sed 's/^/    /' >&2 || true
  printf '%s└──────────────────────────────────────────────────%s\n' "$C_YELLOW" "$C_RESET" >&2
}

wait_for_ollama() {
  if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
    # Host Ollama already passed audit_system's /api/version probe; just
    # re-confirm /api/tags (the endpoint pull_model implicitly relies on)
    # so we fail fast if the host process died between audit and start.
    if curl -fsS --max-time 5 http://localhost:11434/api/tags >/dev/null 2>&1; then
      ok "host ollama API is reachable (skipping container readiness wait)"
      return 0
    fi
    die "host Ollama at :11434 was reachable during audit but no longer responds. Restart it and re-run, or pass --no-use-host-ollama to start the pod's own ollama."
  fi

  local i=0 status
  log "waiting for ollama API to come up..."
  until curl -fsS --max-time 2 http://localhost:11434/api/tags >/dev/null 2>&1; do
    # Detect crashloop early: if the container has already exited,
    # waiting another 120s won't help. Surface the logs immediately.
    status=$(podman inspect ollama-service --format '{{.State.Status}}' 2>/dev/null || echo "missing")
    if [[ "$status" == "exited" || "$status" == "missing" ]]; then
      dump_ollama_logs
      die "ollama-service container exited (status=$status). Container did not stay running long enough to serve. Common cause: image config bug — e.g. ollama panics with 'panic: \$HOME is not defined' if HOME is missing from the container Env (fixed in PR #321; if you pulled from ghcr.io you may have an older image — try \`podman pull --policy=always\` or rebuild locally + use --no-pull)."
    fi
    i=$((i + 1))
    if [[ $i -gt 60 ]]; then
      dump_ollama_logs
      die "ollama did not become ready within 120s (container is $status). See logs above."
    fi
    sleep 2
  done
  ok "ollama API is up"
}

pull_model() {
  if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
    # Host mode: shell out to whichever `ollama` binary the user has,
    # falling back to the HTTP /api/pull endpoint if the CLI isn't on
    # PATH (common when host Ollama was installed via a non-PATH-y
    # method like the macOS .app or systemd-only). See #330.
    if have ollama; then
      log "pulling model $MODEL via host ollama CLI (this is the multi-GB step)..."
      ollama pull "$MODEL"
    else
      # Stream progress via stream:true + line-buffered jq (or grep
      # fallback). The previous implementation passed stream:false +
      # --max-time 86400, which buffered the entire multi-GB response
      # and emitted zero output until the pull finished -- visually
      # indistinguishable from a hang, so users would kill the
      # installer at the 30-min mark and report it as broken. See #330
      # review thread.
      log "pulling model $MODEL via host ollama HTTP API (no \`ollama\` binary on PATH; this is the multi-GB step)..."
      warn "no \`ollama\` CLI found; using HTTP fallback. Progress lines below come from /api/pull."
      # No --max-time: a multi-GB pull on a slow link can legitimately
      # exceed any fixed bound; killing the connection after N seconds
      # truncates the model and corrupts ollama's blob cache. Failure
      # comes from the server side via the connection closing, which
      # curl + the pipefail-protected pipeline below surface correctly.
      # `set -o pipefail` is already enabled at the top of the script
      # (set -euo pipefail), so the pipeline's exit status is the
      # rightmost non-zero status -- which is what we want here.
      if have jq; then
        if ! curl -fsS -N -X POST http://localhost:11434/api/pull \
              -H 'Content-Type: application/json' \
              -d "{\"name\":\"${MODEL}\",\"stream\":true}" \
            | jq -r '[.status, (.completed // empty | tostring), (.total // empty | tostring)] | map(select(. != "")) | join(" ")'; then
          die "ollama HTTP pull of $MODEL failed."
        fi
      else
        if ! curl -fsS -N -X POST http://localhost:11434/api/pull \
              -H 'Content-Type: application/json' \
              -d "{\"name\":\"${MODEL}\",\"stream\":true}" \
            | grep --line-buffered -o '"status":"[^"]*"'; then
          die "ollama HTTP pull of $MODEL failed."
        fi
      fi
    fi
    ok "model $MODEL ready (via host ollama)"
    return 0
  fi
  log "pulling model $MODEL into the running ollama container (this is the multi-GB step)..."
  podman exec ollama-service ollama pull "$MODEL"
  ok "model $MODEL ready"
}

if [[ $NO_START -eq 1 ]]; then
  warn "--no-start set: skipping pod bring-up and model pull"
else
  start_pod
  wait_for_ollama
  if [[ $SKIP_MODEL_PULL -eq 1 ]]; then
    warn "--skip-model-pull set: NOT pulling $MODEL. Chat/agent will fail until you pull a model."
    if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
      warn "to pull later: ollama pull $MODEL   (host Ollama)"
    else
      warn "to pull later: podman exec ollama-service ollama pull $MODEL"
    fi
  else
    pull_model
  fi
  register_claude_mcp
fi

# ---------- done ----------

if [[ "$USE_HOST_OLLAMA" == 1 ]]; then
  OLLAMA_URL_FOR_HARNESS="http://host.containers.internal:11434"
  OLLAMA_DESC="http://localhost:11434 (host process; in-pod harness reaches it via host.containers.internal)"
else
  OLLAMA_URL_FOR_HARNESS="http://localhost:11434"
  OLLAMA_DESC="http://localhost:11434"
fi

cat <<EOF

${C_GREEN}${C_BOLD}✓ Nanna Coder installed${C_RESET}

  pod:          $POD_NAME
  harness:      http://localhost:${HARNESS_PORT}
  ollama:       ${OLLAMA_DESC}
  model:        $([[ $SKIP_MODEL_PULL -eq 1 ]] && echo "(none — pull manually)" || echo "$MODEL")

Common commands:
  podman pod ps
  $([[ "$USE_HOST_OLLAMA" == 1 ]] && echo "# host ollama: use \`journalctl -u ollama\` (Linux) or your normal ollama logs path" || echo "podman logs -f ollama-service")
  podman pod stop $POD_NAME
  podman pod start $POD_NAME

Run the harness CLI against the running pod (one-shot):
  podman run --rm --pod $POD_NAME \\
    -e OLLAMA_URL=${OLLAMA_URL_FOR_HARNESS} \\
    $HARNESS_IMAGE /bin/harness models
EOF
