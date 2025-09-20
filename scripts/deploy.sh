#!/usr/bin/env bash
# Deployment script for Nanna Coder project
set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEPLOYMENT_TYPE="${DEPLOYMENT_TYPE:-local}"
GPU_SUPPORT="${GPU_SUPPORT:-auto}"
OLLAMA_MODEL="${OLLAMA_MODEL:-llama3.1:8b}"
COMPOSE_FILE="${PROJECT_ROOT}/docker-compose.yml"

# Default values
ENVIRONMENT="${ENVIRONMENT:-development}"
REGISTRY="${REGISTRY:-localhost:5000}"
NAMESPACE="${NAMESPACE:-nanna-coder}"

# Help function
show_help() {
    cat << EOF
Nanna Coder Deployment Script

Usage: $0 [OPTIONS] [COMMAND]

COMMANDS:
    start           Start the application stack
    stop            Stop the application stack
    restart         Restart the application stack
    status          Show status of running services
    logs            Show logs from services
    shell           Open shell in running container
    update          Update and restart services
    cleanup         Clean up stopped containers and images
    help            Show this help

DEPLOYMENT TYPES:
    local           Local development (default)
    pod             Podman pod deployment
    compose         Docker/Podman compose deployment
    kube            Kubernetes deployment

OPTIONS:
    -t, --type TYPE         Deployment type (local, pod, compose, kube)
    -e, --env ENV           Environment (development, production)
    -g, --gpu TYPE          GPU support (nvidia, amd, intel, none, auto)
    -m, --model MODEL       Ollama model to use
    --registry URL          Container registry URL
    --namespace NS          Kubernetes namespace
    -f, --follow            Follow logs (for logs command)
    -v, --verbose           Verbose output
    -h, --help              Show this help

EXAMPLES:
    $0 start                        # Start local development stack
    $0 start --type pod --gpu nvidia   # Start with Podman pod and NVIDIA GPU
    $0 logs --follow               # Follow application logs
    $0 shell harness              # Open shell in harness container

ENVIRONMENT VARIABLES:
    DEPLOYMENT_TYPE     Type of deployment
    GPU_SUPPORT         GPU support type
    OLLAMA_MODEL        Default Ollama model
    ENVIRONMENT         Application environment
    REGISTRY            Container registry URL
    NAMESPACE           Kubernetes namespace
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

# Check dependencies
check_dependencies() {
    local missing_deps=()

    case $DEPLOYMENT_TYPE in
        local|pod|compose)
            if ! command -v podman &> /dev/null; then
                missing_deps+=("podman")
            fi
            ;;
        kube)
            if ! command -v kubectl &> /dev/null; then
                missing_deps+=("kubectl")
            fi
            ;;
    esac

    if [ ${#missing_deps[@]} -ne 0 ]; then
        log_error "Missing dependencies: ${missing_deps[*]}"
        log_info "Please install the missing dependencies and try again"
        exit 1
    fi
}

# Detect GPU support
detect_gpu() {
    if [ "$GPU_SUPPORT" = "auto" ]; then
        log_info "Auto-detecting GPU support..."

        if command -v nvidia-smi &> /dev/null && nvidia-smi > /dev/null 2>&1; then
            GPU_SUPPORT="nvidia"
            log_success "NVIDIA GPU detected"
        elif [ -d "/sys/class/drm" ] && ls /sys/class/drm/card*/device/vendor 2>/dev/null | xargs grep -l "0x1002" > /dev/null 2>&1; then
            GPU_SUPPORT="amd"
            log_success "AMD GPU detected"
        elif [ -d "/sys/class/drm" ] && ls /sys/class/drm/card*/device/vendor 2>/dev/null | xargs grep -l "0x8086" > /dev/null 2>&1; then
            GPU_SUPPORT="intel"
            log_success "Intel GPU detected"
        else
            GPU_SUPPORT="none"
            log_info "No GPU detected, using CPU-only mode"
        fi
    fi

    export GPU_SUPPORT
}

# Generate container runtime arguments based on GPU support
get_gpu_args() {
    case $GPU_SUPPORT in
        nvidia)
            echo "--runtime nvidia --gpus all"
            ;;
        amd)
            echo "--device /dev/dri --device /dev/kfd"
            ;;
        intel)
            echo "--device /dev/dri"
            ;;
        *)
            echo ""
            ;;
    esac
}

# Local deployment (direct container execution)
deploy_local() {
    log_info "Starting local deployment..."

    local gpu_args
    gpu_args=$(get_gpu_args)

    # Start Ollama service
    log_info "Starting Ollama service..."
    podman run -d \
        --name nanna-coder-ollama \
        --publish 11434:11434 \
        --volume ollama_data:/root/.ollama \
        $gpu_args \
        --env OLLAMA_HOST=0.0.0.0 \
        nanna-coder-ollama:latest

    # Wait for Ollama to be ready
    log_info "Waiting for Ollama to be ready..."
    for i in {1..30}; do
        if curl -s http://localhost:11434/api/tags > /dev/null 2>&1; then
            log_success "Ollama is ready"
            break
        fi
        if [ $i -eq 30 ]; then
            log_error "Ollama failed to start within timeout"
            exit 1
        fi
        sleep 2
    done

    # Pull default model if not exists
    log_info "Ensuring model $OLLAMA_MODEL is available..."
    if ! curl -s http://localhost:11434/api/tags | jq -r '.models[].name' | grep -q "^${OLLAMA_MODEL}$"; then
        log_info "Pulling model $OLLAMA_MODEL..."
        curl -X POST http://localhost:11434/api/pull -d "{\"name\":\"$OLLAMA_MODEL\"}"
    fi

    # Start harness service
    log_info "Starting harness service..."
    podman run -d \
        --name nanna-coder-harness \
        --publish 8080:8080 \
        --env OLLAMA_URL=http://localhost:11434 \
        --env RUST_LOG=info \
        --network host \
        nanna-coder-harness:latest \
        harness chat --model "$OLLAMA_MODEL" --tools

    log_success "Local deployment started successfully!"
    log_info "Services available at:"
    echo "  • Harness API: http://localhost:8080"
    echo "  • Ollama API: http://localhost:11434"
}

# Pod deployment (Podman pod)
deploy_pod() {
    log_info "Starting pod deployment..."

    local gpu_args
    gpu_args=$(get_gpu_args)

    # Create pod
    log_info "Creating nanna-coder pod..."
    podman pod create \
        --name nanna-coder-pod \
        --publish 8080:8080 \
        --publish 11434:11434

    # Start Ollama in pod
    log_info "Starting Ollama service in pod..."
    podman run -d \
        --pod nanna-coder-pod \
        --name nanna-coder-ollama \
        --volume ollama_data:/root/.ollama \
        $gpu_args \
        --env OLLAMA_HOST=0.0.0.0 \
        nanna-coder-ollama:latest

    # Wait for Ollama
    log_info "Waiting for Ollama to be ready..."
    for i in {1..30}; do
        if curl -s http://localhost:11434/api/tags > /dev/null 2>&1; then
            log_success "Ollama is ready"
            break
        fi
        if [ $i -eq 30 ]; then
            log_error "Ollama failed to start within timeout"
            exit 1
        fi
        sleep 2
    done

    # Start harness in pod
    log_info "Starting harness service in pod..."
    podman run -d \
        --pod nanna-coder-pod \
        --name nanna-coder-harness \
        --env OLLAMA_URL=http://localhost:11434 \
        --env RUST_LOG=info \
        nanna-coder-harness:latest \
        harness chat --model "$OLLAMA_MODEL" --tools

    log_success "Pod deployment started successfully!"
    log_info "Pod status:"
    podman pod ps
}

# Compose deployment
deploy_compose() {
    log_info "Starting compose deployment..."

    cd "$PROJECT_ROOT"

    # Generate compose file with GPU support if needed
    local compose_files=("-f" "$COMPOSE_FILE")

    if [ "$GPU_SUPPORT" != "none" ] && [ -f "nix/gpu-compose-override.yml" ]; then
        compose_files+=("-f" "nix/gpu-compose-override.yml")
        log_info "Using GPU-enabled compose configuration"
    fi

    # Start services
    podman-compose "${compose_files[@]}" up -d

    log_success "Compose deployment started successfully!"
    log_info "Service status:"
    podman-compose "${compose_files[@]}" ps
}

# Kubernetes deployment
deploy_kube() {
    log_info "Starting Kubernetes deployment..."

    cd "$PROJECT_ROOT"

    # Apply Kubernetes manifests
    if [ -d "k8s" ]; then
        kubectl apply -f k8s/ --namespace="$NAMESPACE"
        log_success "Kubernetes resources applied"

        # Wait for pods to be ready
        log_info "Waiting for pods to be ready..."
        kubectl wait --for=condition=ready pod -l app=nanna-coder --namespace="$NAMESPACE" --timeout=300s

        log_success "Kubernetes deployment completed!"
        log_info "Pod status:"
        kubectl get pods --namespace="$NAMESPACE"
    else
        log_error "Kubernetes manifests not found in k8s/ directory"
        exit 1
    fi
}

# Start deployment
start_deployment() {
    log_info "Starting deployment (type: $DEPLOYMENT_TYPE)..."

    check_dependencies
    detect_gpu

    case $DEPLOYMENT_TYPE in
        local)
            deploy_local
            ;;
        pod)
            deploy_pod
            ;;
        compose)
            deploy_compose
            ;;
        kube)
            deploy_kube
            ;;
        *)
            log_error "Unknown deployment type: $DEPLOYMENT_TYPE"
            exit 1
            ;;
    esac

    log_success "Deployment started successfully!"
    show_status
}

# Stop deployment
stop_deployment() {
    log_info "Stopping deployment (type: $DEPLOYMENT_TYPE)..."

    case $DEPLOYMENT_TYPE in
        local)
            podman stop nanna-coder-harness nanna-coder-ollama || true
            podman rm nanna-coder-harness nanna-coder-ollama || true
            ;;
        pod)
            podman pod stop nanna-coder-pod || true
            podman pod rm nanna-coder-pod || true
            ;;
        compose)
            cd "$PROJECT_ROOT"
            podman-compose down
            ;;
        kube)
            kubectl delete -f k8s/ --namespace="$NAMESPACE" || true
            ;;
    esac

    log_success "Deployment stopped successfully!"
}

# Show status
show_status() {
    log_info "Deployment status (type: $DEPLOYMENT_TYPE):"

    case $DEPLOYMENT_TYPE in
        local)
            echo "Containers:"
            podman ps --filter name=nanna-coder --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"
            ;;
        pod)
            echo "Pods:"
            podman pod ps --filter name=nanna-coder
            echo
            echo "Containers:"
            podman ps --filter pod=nanna-coder-pod --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"
            ;;
        compose)
            cd "$PROJECT_ROOT"
            podman-compose ps
            ;;
        kube)
            kubectl get pods,svc --namespace="$NAMESPACE"
            ;;
    esac

    # Check service health
    echo
    log_info "Service health:"
    if curl -s http://localhost:11434/api/tags > /dev/null 2>&1; then
        echo "  ✅ Ollama API (http://localhost:11434)"
    else
        echo "  ❌ Ollama API (http://localhost:11434)"
    fi

    if curl -s http://localhost:8080/health > /dev/null 2>&1; then
        echo "  ✅ Harness API (http://localhost:8080)"
    else
        echo "  ❌ Harness API (http://localhost:8080)"
    fi
}

# Show logs
show_logs() {
    local service="${1:-}"
    local follow_flag=""

    if [ "$FOLLOW_LOGS" = "true" ]; then
        follow_flag="--follow"
    fi

    case $DEPLOYMENT_TYPE in
        local)
            if [ -n "$service" ]; then
                podman logs $follow_flag "nanna-coder-$service"
            else
                podman logs $follow_flag nanna-coder-harness &
                podman logs $follow_flag nanna-coder-ollama &
                wait
            fi
            ;;
        pod)
            if [ -n "$service" ]; then
                podman logs $follow_flag "nanna-coder-$service"
            else
                podman pod logs $follow_flag nanna-coder-pod
            fi
            ;;
        compose)
            cd "$PROJECT_ROOT"
            if [ -n "$service" ]; then
                podman-compose logs $follow_flag "$service"
            else
                podman-compose logs $follow_flag
            fi
            ;;
        kube)
            if [ -n "$service" ]; then
                kubectl logs -l app="$service" --namespace="$NAMESPACE" $follow_flag
            else
                kubectl logs -l app=nanna-coder --namespace="$NAMESPACE" $follow_flag
            fi
            ;;
    esac
}

# Open shell in container
open_shell() {
    local service="${1:-harness}"

    log_info "Opening shell in $service container..."

    case $DEPLOYMENT_TYPE in
        local|pod)
            podman exec -it "nanna-coder-$service" /bin/bash
            ;;
        compose)
            cd "$PROJECT_ROOT"
            podman-compose exec "$service" /bin/bash
            ;;
        kube)
            local pod
            pod=$(kubectl get pods -l app="$service" --namespace="$NAMESPACE" -o jsonpath='{.items[0].metadata.name}')
            kubectl exec -it "$pod" --namespace="$NAMESPACE" -- /bin/bash
            ;;
    esac
}

# Update deployment
update_deployment() {
    log_info "Updating deployment..."

    # Rebuild images
    "$PROJECT_ROOT/scripts/build.sh" containers

    # Restart deployment
    stop_deployment
    start_deployment

    log_success "Deployment updated successfully!"
}

# Cleanup
cleanup_deployment() {
    log_info "Cleaning up deployment artifacts..."

    case $DEPLOYMENT_TYPE in
        local|pod|compose)
            # Remove stopped containers
            podman container prune -f

            # Remove dangling images
            podman image prune -f

            # Remove unused volumes
            podman volume prune -f
            ;;
        kube)
            # Clean up completed jobs and failed pods
            kubectl delete jobs --field-selector status.successful=1 --namespace="$NAMESPACE" || true
            kubectl delete pods --field-selector status.phase=Failed --namespace="$NAMESPACE" || true
            ;;
    esac

    log_success "Cleanup completed!"
}

# Parse command line arguments
FOLLOW_LOGS="false"

parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            -t|--type)
                DEPLOYMENT_TYPE="$2"
                shift 2
                ;;
            -e|--env)
                ENVIRONMENT="$2"
                shift 2
                ;;
            -g|--gpu)
                GPU_SUPPORT="$2"
                shift 2
                ;;
            -m|--model)
                OLLAMA_MODEL="$2"
                shift 2
                ;;
            --registry)
                REGISTRY="$2"
                shift 2
                ;;
            --namespace)
                NAMESPACE="$2"
                shift 2
                ;;
            -f|--follow)
                FOLLOW_LOGS="true"
                shift
                ;;
            -v|--verbose)
                set -x
                shift
                ;;
            -h|--help)
                show_help
                exit 0
                ;;
            start)
                start_deployment
                exit 0
                ;;
            stop)
                stop_deployment
                exit 0
                ;;
            restart)
                stop_deployment
                start_deployment
                exit 0
                ;;
            status)
                show_status
                exit 0
                ;;
            logs)
                shift
                show_logs "$@"
                exit 0
                ;;
            shell)
                shift
                open_shell "$@"
                exit 0
                ;;
            update)
                update_deployment
                exit 0
                ;;
            cleanup)
                cleanup_deployment
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

# If no arguments provided, start deployment
if [ $# -eq 0 ]; then
    start_deployment
    exit 0
fi

# Parse arguments
parse_args "$@"