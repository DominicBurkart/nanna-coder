#!/usr/bin/env bash
# Build script for Nanna Coder project
set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${PROJECT_ROOT}/target"
CONTAINER_DIR="${PROJECT_ROOT}/.containers"

# Default values
ARCHITECTURE="${ARCHITECTURE:-$(uname -m)}"
PLATFORM="${PLATFORM:-linux}"
GPU_SUPPORT="${GPU_SUPPORT:-auto}"
PUSH_IMAGES="${PUSH_IMAGES:-false}"
REGISTRY="${REGISTRY:-localhost:5000}"

# Help function
show_help() {
    cat << EOF
Nanna Coder Build Script

Usage: $0 [OPTIONS] [COMMAND]

COMMANDS:
    all             Build everything (workspace + containers)
    workspace       Build Rust workspace only
    containers      Build container images only
    cross           Build for multiple architectures
    gpu             Build with GPU support
    clean           Clean build artifacts
    help            Show this help

OPTIONS:
    -a, --arch ARCH         Target architecture (x86_64, aarch64)
    -p, --platform PLATFORM Target platform (linux, darwin)
    -g, --gpu TYPE          GPU support (nvidia, amd, intel, none, auto)
    --push                  Push images to registry
    --registry URL          Container registry URL
    -v, --verbose           Verbose output
    -h, --help              Show this help

EXAMPLES:
    $0 all                          # Build everything
    $0 containers --gpu nvidia      # Build with NVIDIA GPU support
    $0 cross                        # Cross-compile for all architectures
    $0 all --push --registry my.registry.com

ENVIRONMENT VARIABLES:
    ARCHITECTURE    Target architecture
    PLATFORM        Target platform
    GPU_SUPPORT     GPU support type
    PUSH_IMAGES     Push images to registry (true/false)
    REGISTRY        Container registry URL
    NIX_PROFILE     Nix profile to use
EOF
}

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if Nix is available
check_nix() {
    if ! command -v nix &> /dev/null; then
        log_error "Nix is not installed or not in PATH"
        log_info "Please install Nix: https://nixos.org/download.html"
        exit 1
    fi

    if ! nix --version | grep -q "nix (Nix) 2."; then
        log_warning "Nix version might be too old, consider updating"
    fi
}

# Check if flakes are enabled
check_flakes() {
    if ! nix eval --expr "builtins.currentSystem" &> /dev/null; then
        log_error "Nix flakes are not enabled"
        log_info "Enable flakes by adding to ~/.config/nix/nix.conf:"
        log_info "experimental-features = nix-command flakes"
        exit 1
    fi
}

# Clean build artifacts
clean_build() {
    log_info "Cleaning build artifacts..."

    # Clean Rust artifacts
    if [ -d "$BUILD_DIR" ]; then
        rm -rf "$BUILD_DIR"
        log_success "Cleaned Rust build artifacts"
    fi

    # Clean Nix results
    find "$PROJECT_ROOT" -name "result*" -type l -delete
    log_success "Cleaned Nix result symlinks"

    # Clean container artifacts
    if [ -d "$CONTAINER_DIR" ]; then
        rm -rf "$CONTAINER_DIR"
        log_success "Cleaned container artifacts"
    fi
}

# Build Rust workspace
build_workspace() {
    log_info "Building Rust workspace..."

    cd "$PROJECT_ROOT"

    if command -v nix &> /dev/null; then
        # Use Nix build
        nix build .#nanna-coder
        log_success "Built workspace with Nix"
    else
        # Fallback to cargo
        log_warning "Nix not available, using cargo fallback"
        cargo build --workspace --release
        log_success "Built workspace with Cargo"
    fi
}

# Build container images
build_containers() {
    local gpu_flag=""

    log_info "Building container images..."

    cd "$PROJECT_ROOT"

    # Build harness container
    log_info "Building harness container..."
    nix build .#harnessImage
    cp result harness-image.tar.gz
    log_success "Built harness container"

    # Build Ollama container (if available)
    if nix eval .#ollamaImage &> /dev/null; then
        log_info "Building Ollama container..."
        nix build .#ollamaImage
        cp result ollama-image.tar.gz
        log_success "Built Ollama container"
    else
        log_warning "Ollama container not available in this configuration"
    fi

    # GPU-enabled containers
    if [ "$GPU_SUPPORT" != "none" ]; then
        log_info "Building GPU-enabled containers..."

        # Check if GPU support is available
        if nix eval .#packages.x86_64-linux.harnessImageGpu &> /dev/null; then
            nix build .#packages.x86_64-linux.harnessImageGpu
            cp result harness-gpu-image.tar.gz
            log_success "Built GPU-enabled harness container"
        else
            log_warning "GPU-enabled containers not available"
        fi
    fi
}

# Cross-compilation build
build_cross() {
    local architectures=("x86_64-linux" "aarch64-linux")

    log_info "Starting cross-compilation build..."

    for arch in "${architectures[@]}"; do
        log_info "Building for $arch..."

        # Check if the architecture is supported
        if nix eval .#packages.${arch}.nanna-coder &> /dev/null; then
            nix build .#packages.${arch}.nanna-coder
            cp result "nanna-coder-${arch}"
            log_success "Built for $arch"

            # Build containers for this architecture
            if nix eval .#packages.${arch}.harnessImage &> /dev/null; then
                nix build .#packages.${arch}.harnessImage
                cp result "harness-image-${arch}.tar.gz"
                log_success "Built container for $arch"
            fi
        else
            log_warning "Architecture $arch not supported or configured"
        fi
    done
}

# Load and optionally push container images
load_images() {
    log_info "Loading container images..."

    mkdir -p "$CONTAINER_DIR"

    # Load harness image
    if [ -f "harness-image.tar.gz" ]; then
        podman load < harness-image.tar.gz
        log_success "Loaded harness image"

        if [ "$PUSH_IMAGES" = "true" ]; then
            podman tag nanna-coder-harness:latest "$REGISTRY/nanna-coder-harness:latest"
            podman push "$REGISTRY/nanna-coder-harness:latest"
            log_success "Pushed harness image to $REGISTRY"
        fi
    fi

    # Load Ollama image
    if [ -f "ollama-image.tar.gz" ]; then
        podman load < ollama-image.tar.gz
        log_success "Loaded Ollama image"

        if [ "$PUSH_IMAGES" = "true" ]; then
            podman tag nanna-coder-ollama:latest "$REGISTRY/nanna-coder-ollama:latest"
            podman push "$REGISTRY/nanna-coder-ollama:latest"
            log_success "Pushed Ollama image to $REGISTRY"
        fi
    fi

    # Load GPU images
    if [ -f "harness-gpu-image.tar.gz" ]; then
        podman load < harness-gpu-image.tar.gz
        log_success "Loaded GPU-enabled harness image"

        if [ "$PUSH_IMAGES" = "true" ]; then
            podman tag nanna-coder-harness-gpu:latest "$REGISTRY/nanna-coder-harness-gpu:latest"
            podman push "$REGISTRY/nanna-coder-harness-gpu:latest"
            log_success "Pushed GPU-enabled harness image to $REGISTRY"
        fi
    fi
}

# Run tests
run_tests() {
    log_info "Running tests..."

    cd "$PROJECT_ROOT"

    if command -v nix &> /dev/null; then
        # Use Nix checks
        nix flake check
        log_success "All Nix checks passed"
    else
        # Fallback to cargo
        cargo test --workspace
        log_success "All Cargo tests passed"
    fi
}

# Main build function
build_all() {
    log_info "Starting full build process..."

    check_nix
    check_flakes

    # Run tests first
    run_tests

    # Build workspace
    build_workspace

    # Build containers
    build_containers

    # Load images
    load_images

    log_success "Build completed successfully!"

    # Show summary
    echo
    log_info "Build Summary:"
    echo "  ✅ Rust workspace built"
    echo "  ✅ Container images built"

    if [ "$PUSH_IMAGES" = "true" ]; then
        echo "  ✅ Images pushed to registry"
    fi

    echo
    log_info "Next steps:"
    echo "  • Run containers: $PROJECT_ROOT/scripts/deploy.sh"
    echo "  • Start development: nix develop"
    echo "  • Run tests: cargo test --workspace"
}

# Parse command line arguments
parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            -a|--arch)
                ARCHITECTURE="$2"
                shift 2
                ;;
            -p|--platform)
                PLATFORM="$2"
                shift 2
                ;;
            -g|--gpu)
                GPU_SUPPORT="$2"
                shift 2
                ;;
            --push)
                PUSH_IMAGES="true"
                shift
                ;;
            --registry)
                REGISTRY="$2"
                shift 2
                ;;
            -v|--verbose)
                set -x
                shift
                ;;
            -h|--help)
                show_help
                exit 0
                ;;
            all)
                build_all
                exit 0
                ;;
            workspace)
                build_workspace
                exit 0
                ;;
            containers)
                build_containers
                load_images
                exit 0
                ;;
            cross)
                build_cross
                exit 0
                ;;
            gpu)
                GPU_SUPPORT="${GPU_SUPPORT:-nvidia}"
                build_containers
                load_images
                exit 0
                ;;
            clean)
                clean_build
                exit 0
                ;;
            help)
                show_help
                exit 0
                ;;
            *)
                log_error "Unknown option: $1"
                show_help
                exit 1
                ;;
        esac
    done
}

# If no arguments provided, run full build
if [ $# -eq 0 ]; then
    build_all
    exit 0
fi

# Parse arguments
parse_args "$@"