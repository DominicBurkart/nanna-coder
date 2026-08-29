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
#   --no-pull           Don't `podman pull` the images. Use already-loaded local images
#                       (e.g. loaded via nix2container's copyToPodman). Used by CI.
#   --dry-run           Print what the installer would do for the detected OS and exit.
#                       Doesn't install anything. Used by CI to validate the script
#                       across platforms without depending on container runtimes.
#   --yes               Don't prompt; assume yes for sudo notices.
#   --no-claude-mcp     Don't register nanna-coder as a Claude Code MCP server even
#                       if a Claude Code config is detected.
#   --force-wsl         On WSL2, proceed even if systemd is not detected as active.
#                       Default behavior on WSL2: probe for systemd; if missing,
#                       die with an actionable error pointing at /etc/wsl.conf.
#                       Use this flag if you have a non-systemd workaround
#                       (e.g. rootless podman with custom netns plumbing).
#   --branch <name>     No-op accepted for compatibility with scripts/install.ps1,
#                       which forwards its --Branch parameter to this script when
#                       delegating into WSL2. Recorded in plan output for parity.
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
MODEL="${NANNA_MODEL:-gemma4:e4b}"
HARNESS_IMAGE_OVERRIDE=""
OLLAMA_IMAGE_OVERRIDE=""
SKIP_MODEL_PULL=0
NO_START=0
NO_PULL=0
DRY_RUN=0
ASSUME_YES=0
NO_CLAUDE_MCP=0
FORCE_WSL=0
BRANCH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-model-pull) SKIP_MODEL_PULL=1; shift;;
    --no-start)        NO_START=1; shift;;
    --no-pull)         NO_PULL=1; shift;;
    --dry-run)         DRY_RUN=1; shift;;
    --registry)        REGISTRY="$2"; shift 2;;
    --tag)             TAG="$2"; shift 2;;
    --model)           MODEL="$2"; shift 2;;
    --harness-image)   HARNESS_IMAGE_OVERRIDE="$2"; shift 2;;
    --ollama-image)    OLLAMA_IMAGE_OVERRIDE="$2"; shift 2;;
    --yes|-y)          ASSUME_YES=1; shift;;
    --no-claude-mcp)   NO_CLAUDE_MCP=1; shift;;
    --force-wsl)       FORCE_WSL=1; shift;;
    --branch)          BRANCH="$2"; shift 2;;
    -h|--help)         sed -n '2,40p' "$0" 2>/dev/null || true; exit 0;;
    *) echo "unknown flag: $1" >&2; exit 64;;
  esac
done

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
  printf '\n%s┌─ sudo step ──────────────────────────────────%s\n' "$C_YELLOW$C_BOLD" "$C_RESET"
  printf '%s│%s component: %s\n' "$C_YELLOW" "$C_RESET" "$1"
  printf '%s│%s reason:    %s\n' "$C_YELLOW" "$C_RESET" "$2"
  printf '%s└───────────────────────────────────────────────%s\n\n' "$C_YELLOW" "$C_RESET"
}

have() { command -v "$1" >/dev/null 2>&1; }

is_wsl() { [[ -r /proc/version ]] && grep -qiE 'microsoft|wsl' /proc/version; }

# Probe whether systemd is the running PID-1 / active inside this WSL distro.
# WSL2 only routes /run/netns bind mounts through podman's pod infra container
# correctly when systemd is enabled (`[boot] systemd=true` in /etc/wsl.conf,
# applied after `wsl --shutdown`). Without systemd, `podman pod create` fails
# with "failed to bind mount ns at /run/netns/...: invalid argument".
#
# We accept any of:
#   - PID 1's comm is "systemd" (most reliable signal)
#   - `systemctl is-system-running` returns one of the live states
#   - /run/systemd/system exists (tmpfs marker created by systemd at boot)
is_wsl_systemd_active() {
  if [[ -r /proc/1/comm ]] && grep -qx 'systemd' /proc/1/comm 2>/dev/null; then
    return 0
  fi
  if have systemctl; then
    local state
    state="$(systemctl is-system-running 2>/dev/null || true)"
    case "$state" in
      running|degraded|starting|maintenance) return 0 ;;
    esac
  fi
  [[ -d /run/systemd/system ]]
}

# ---------- OS detection ----------

IS_WSL=no
case "$(uname -s)" in
  Linux*)
    OS=linux
    if is_wsl; then
      IS_WSL=yes
      if is_wsl_systemd_active; then
        warn "WSL2 detected with systemd active — proceeding (experimental)."
        warn "If \`podman pod create\` later fails with \"failed to bind mount ns\","
        warn "the kernel does not expose /run/netns; fall back to scripts/install.ps1."
      elif [[ $FORCE_WSL -eq 1 ]]; then
        warn "WSL2 detected without systemd; --force-wsl set, proceeding anyway."
        warn "Expect netns failures during pod creation if your distro lacks the"
        warn "/run/netns bind-mount path that podman pod infra containers need."
      else
        cat >&2 <<'EOF'
✗ WSL2 (Windows Subsystem for Linux) detected without systemd active.

  The bash installer drives podman directly, and WSL2's default (non-systemd)
  init does not provide the /run/netns bind-mount path that podman pod
  infra containers require. Pod creation will fail with:
    failed to bind mount ns at /run/netns/...: invalid argument

  Two supported paths forward:

  1. Enable systemd in this WSL distro (recommended):
       sudo tee /etc/wsl.conf >/dev/null <<'CONF'
       [boot]
       systemd=true
       CONF
     Then from a Windows PowerShell:  wsl --shutdown
     Reopen the WSL shell and re-run this installer. The check above will
     pass once `systemctl is-system-running` reports a live state.

  2. Use the Windows-native entry point: scripts/install.ps1 (run from a
     Windows PowerShell session). It bootstraps WSL + this script with
     systemd preconfigured. See README for details.

  Override (advanced): re-run with --force-wsl if you have a non-systemd
  workaround in place. Tracked in https://github.com/DominicBurkart/nanna-coder/issues/326.
EOF
        die "WSL2 without systemd; not supported by install.sh (see above for fix)"
      fi
    fi
    ;;
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

audit_system() {
  AUDIT_PODMAN=no
  AUDIT_PODMAN_VERSION=""
  AUDIT_MACHINE_RUNNING=no
  AUDIT_POD_EXISTS=no
  AUDIT_PORT_8080_HOLDER=""
  AUDIT_PORT_11434_HOLDER=""
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

  AUDIT_PORT_8080_HOLDER="$(port_in_use_by 8080)"
  AUDIT_PORT_11434_HOLDER="$(port_in_use_by 11434)"
  AUDIT_CLAUDE_CONFIG="$(claude_config_path 2>/dev/null || true)"
}

print_plan() {
  printf '\n%sNanna Coder installer%s\n' "$C_BOLD" "$C_RESET"
  printf '  detected:       %s/%s%s\n' "$OS" "$ARCH" \
    "$([[ "$IS_WSL" == yes ]] && echo ' (WSL2)')"
  printf '  registry:       %s\n' "$REGISTRY"
  printf '  tag:            %s\n' "$TAG"
  if [[ -n "$BRANCH" ]]; then
    printf '  branch:         %s (informational; passed in by install.ps1)\n' "$BRANCH"
  fi
  printf '  model:          %s%s\n' "$MODEL" \
    "$([[ $SKIP_MODEL_PULL -eq 1 ]] && echo ' (skipped)')"
  printf '  pod name:       %s\n' "$POD_NAME"

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
  if [[ -n "$AUDIT_PORT_8080_HOLDER" && "$AUDIT_POD_EXISTS" != yes ]]; then
    printf '  %s!%s port 8080 in use by another process:\n' "$C_YELLOW" "$C_RESET"
    printf '%s\n' "$AUDIT_PORT_8080_HOLDER" | sed 's/^/      /'
  fi
  if [[ -n "$AUDIT_PORT_11434_HOLDER" && "$AUDIT_POD_EXISTS" != yes ]]; then
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
    if [[ "$AUDIT_POD_EXISTS" == yes ]]; then
      printf '  %d. Stop and remove existing %s, recreate publishing :8080 and :11434\n' \
        "$n" "$POD_NAME"
    else
      printf '  %d. Create pod %s publishing :8080 and :11434\n' "$n" "$POD_NAME"
    fi
    n=$((n+1))
    printf '  %d. Run ollama-service inside the pod\n' "$n"; n=$((n+1))
    printf '  %d. Wait for ollama API at http://localhost:11434/api/tags (120s timeout)\n' "$n"; n=$((n+1))
    if [[ $SKIP_MODEL_PULL -eq 0 ]]; then
      printf '  %d. Pull model %s (multi-GB, the slow step)\n' "$n" "$MODEL"; n=$((n+1))
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
    AUDIT_PORT_8080_HOLDER="$(port_in_use_by 8080)"
    AUDIT_PORT_11434_HOLDER="$(port_in_use_by 11434)"
  fi

  local conflict=0 holder
  for spec in "8080:AUDIT_PORT_8080_HOLDER" "11434:AUDIT_PORT_11434_HOLDER"; do
    local port="${spec%%:*}" var="${spec##*:}"
    holder="${!var}"
    [[ -z "$holder" ]] && continue
    warn "port $port is held by another process:"
    printf '%s\n' "$holder" | sed 's/^/    /' >&2
    conflict=1
  done
  [[ $conflict -eq 0 ]] && return 0
  cat >&2 <<EOF

${C_RED}${C_BOLD}✗ port conflict on 8080 and/or 11434${C_RESET}

Free the port(s) and re-run. Common culprits:
  - host ollama on 11434:
      Linux:  sudo systemctl stop ollama
      macOS:  pkill ollama   (or brew services stop ollama)
  - another pod publishing the same ports:
      podman ps -a | grep -E '8080|11434'
  - an orphaned process holding the port:
      Linux:  ss -ltnp | grep -E ':8080|:11434'
      macOS:  lsof -iTCP:11434 -sTCP:LISTEN
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
  # reaches ollama-service at http://localhost:11434.
  local entry
  entry=$(jq -n \
    --arg pod "$POD_NAME" \
    --arg img "$HARNESS_IMAGE" \
    --arg model "$MODEL" \
    '{
      command: "podman",
      args: ["run","--rm","-i","--pod",$pod,$img,"/bin/nanna","mcp-serve","--model",$model]
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
  log "  command:  podman run --rm -i --pod $POD_NAME $HARNESS_IMAGE /bin/nanna mcp-serve --model $MODEL"
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
  warn "  $OLLAMA_IMAGE"
  if HARNESS_RESOLVED=$(resolve_image_ref "$HARNESS_IMAGE"); then
    [[ "$HARNESS_RESOLVED" == "$HARNESS_IMAGE" ]] || \
      log "harness image resolved: $HARNESS_IMAGE -> $HARNESS_RESOLVED"
    HARNESS_IMAGE="$HARNESS_RESOLVED"
  else
    podman images >&2 || true
    die "harness image $HARNESS_IMAGE not loaded locally and --no-pull was set."
  fi
  if OLLAMA_RESOLVED=$(resolve_image_ref "$OLLAMA_IMAGE"); then
    [[ "$OLLAMA_RESOLVED" == "$OLLAMA_IMAGE" ]] || \
      log "ollama image resolved: $OLLAMA_IMAGE -> $OLLAMA_RESOLVED"
    OLLAMA_IMAGE="$OLLAMA_RESOLVED"
  else
    podman images >&2 || true
    die "ollama image $OLLAMA_IMAGE not loaded locally and --no-pull was set."
  fi
else
  log "pulling $HARNESS_IMAGE"
  podman pull "$HARNESS_IMAGE" \
    || die "failed to pull $HARNESS_IMAGE. The image may be private; run \`podman login ghcr.io\` and re-run, or override with --registry."

  log "pulling $OLLAMA_IMAGE"
  podman pull "$OLLAMA_IMAGE" || die "failed to pull $OLLAMA_IMAGE."

  ok "images pulled"
fi

# ---------- 3. start pod ----------

start_pod() {
  # preflight_port_check() handles teardown of any existing pod before
  # we get here, so this function can assume a clean slate.
  log "creating pod $POD_NAME (ports 8080, 11434)..."
  podman pod create --name "$POD_NAME" -p 8080:8080 -p 11434:11434

  log "starting ollama-service..."
  podman run -d --pod "$POD_NAME" --name ollama-service \
    -v ollama-data:/root/.ollama "$OLLAMA_IMAGE"

  # The harness binary is a CLI, not a long-running daemon, so we don't keep a
  # harness-service container running. Invoke it on-demand against the pod:
  #   podman run --rm --pod $POD_NAME -e OLLAMA_URL=http://localhost:11434 \
  #     $HARNESS_IMAGE /bin/nanna <subcommand> ...
}

dump_ollama_logs() {
  printf '%s┌─ podman logs ollama-service (last 50 lines) ─────%s\n' "$C_YELLOW$C_BOLD" "$C_RESET" >&2
  podman logs --tail 50 ollama-service 2>&1 | sed 's/^/    /' >&2 || true
  printf '%s└──────────────────────────────────────────────────%s\n' "$C_YELLOW" "$C_RESET" >&2
}

wait_for_ollama() {
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
    warn "to pull later: podman exec ollama-service ollama pull $MODEL"
  else
    pull_model
  fi
  register_claude_mcp
fi

# ---------- done ----------

cat <<EOF

${C_GREEN}${C_BOLD}✓ Nanna Coder installed${C_RESET}

  pod:          $POD_NAME
  harness:      http://localhost:8080
  ollama:       http://localhost:11434
  model:        $([[ $SKIP_MODEL_PULL -eq 1 ]] && echo "(none — pull manually)" || echo "$MODEL")

Common commands:
  podman pod ps
  podman logs -f ollama-service
  podman pod stop $POD_NAME
  podman pod start $POD_NAME

Run the harness CLI against the running pod (one-shot):
  podman run --rm --pod $POD_NAME \\
    -e OLLAMA_URL=http://localhost:11434 \\
    $HARNESS_IMAGE /bin/nanna models
EOF
