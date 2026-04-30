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
    -h|--help)         sed -n '2,32p' "$0" 2>/dev/null || true; exit 0;;
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
  printf '\n%s┌─ sudo required ─────────────────────────────────%s\n' "$C_YELLOW$C_BOLD" "$C_RESET"
  printf '%s│%s component: %s\n' "$C_YELLOW" "$C_RESET" "$1"
  printf '%s│%s reason:    %s\n' "$C_YELLOW" "$C_RESET" "$2"
  printf '%s│%s you may be prompted for your password.\n' "$C_YELLOW" "$C_RESET"
  printf '%s└─────────────────────────────────────────────────%s\n\n' "$C_YELLOW" "$C_RESET"
  if [[ $ASSUME_YES -eq 0 && -t 0 ]]; then
    read -r -p "Continue? [Y/n] " ans
    case "$ans" in N|n|no|No) die "aborted by user";; esac
  fi
}

have() { command -v "$1" >/dev/null 2>&1; }

is_wsl() { [[ -r /proc/version ]] && grep -qiE 'microsoft|wsl' /proc/version; }

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
log "detected: $OS/$ARCH"

# ---------- banner ----------

cat <<EOF
${C_BOLD}Nanna Coder installer${C_RESET}
  registry:       $REGISTRY
  tag:            $TAG
  model:          $MODEL$(if [[ $SKIP_MODEL_PULL -eq 1 ]]; then echo " (skipped)"; fi)
  pod name:       $POD_NAME
  start pod:      $([[ $NO_START -eq 1 ]] && echo no || echo yes)

This installer may invoke sudo to install Podman. You will see a clear
notification before each privileged step.
EOF

if [[ $DRY_RUN -eq 1 ]]; then
  cat <<EOF

${C_YELLOW}${C_BOLD}--dry-run set: not executing any commands.${C_RESET}

Plan for $OS/$ARCH:
  1. Install Podman if missing
       Linux: sudo (apt-get|dnf|pacman|zypper) install -y podman
       macOS: brew install podman
  2. (macOS) podman machine init && podman machine start
  3. Verify: podman info
  4. Pull images$([[ $NO_PULL -eq 1 ]] && echo " — SKIPPED (--no-pull)")
       $HARNESS_IMAGE
       $OLLAMA_IMAGE
  5. Create pod: $POD_NAME (publishes :8080 and :11434)
  6. Run ollama-service from $OLLAMA_IMAGE
  7. Wait for ollama API at http://localhost:11434/api/tags
  8. Pull model$([[ $SKIP_MODEL_PULL -eq 1 ]] && echo " — SKIPPED (--skip-model-pull)"): podman exec ollama-service ollama pull $MODEL
EOF
  exit 0
fi

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
  if podman pod exists "$POD_NAME" 2>/dev/null; then
    warn "pod $POD_NAME already exists; stopping + removing for a clean start"
    podman pod stop "$POD_NAME" >/dev/null 2>&1 || true
    podman pod rm   "$POD_NAME" >/dev/null 2>&1 || true
  fi

  log "creating pod $POD_NAME (ports 8080, 11434)..."
  podman pod create --name "$POD_NAME" -p 8080:8080 -p 11434:11434

  log "starting ollama-service..."
  podman run -d --pod "$POD_NAME" --name ollama-service \
    -v ollama-data:/root/.ollama "$OLLAMA_IMAGE"

  # The harness binary is a CLI, not a long-running daemon, so we don't keep a
  # harness-service container running. Invoke it on-demand against the pod:
  #   podman run --rm --pod $POD_NAME -e OLLAMA_URL=http://localhost:11434 \
  #     $HARNESS_IMAGE /bin/harness <subcommand> ...
}

wait_for_ollama() {
  local i=0
  log "waiting for ollama API to come up..."
  until curl -fsS --max-time 2 http://localhost:11434/api/tags >/dev/null 2>&1; do
    i=$((i + 1))
    if [[ $i -gt 60 ]]; then
      die "ollama did not become ready within 120s. Check: podman logs ollama-service"
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
    $HARNESS_IMAGE /bin/harness models
EOF
