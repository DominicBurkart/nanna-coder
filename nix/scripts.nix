# Build scripts and development utilities
# This module contains:
# - Container build and management scripts
# - Development workflow utilities (dev-build, dev-test, dev-check, etc.)
# - Cache warming and cleanup utilities

{ pkgs
, lib
, podConfig
, modelRegistry
, binaryCacheConfig
, rustToolchain
, cacheConfig
}:

let
  # Build scripts for common operations
  buildScripts = {
    build-all = pkgs.writeShellScriptBin "build-all" ''
      echo "🔨 Building all containers..."
      nix build .#harnessImage
      nix build .#ollamaImage
      echo "✅ All containers built successfully!"
    '';

    load-images = pkgs.writeShellScriptBin "load-images" ''
      echo "📦 Loading container images into podman..."
      podman load < result-harness
      podman load < result-ollama
      echo "✅ Images loaded successfully!"
    '';

    start-pod = pkgs.writeShellScriptBin "start-pod" ''
      echo "🚀 Starting nanna-coder pod..."
      podman play kube ${podConfig}
      echo "✅ Pod started successfully!"
      echo "🌐 Harness available at: http://localhost:8080"
      echo "🤖 Ollama API available at: http://localhost:11434"
    '';

    stop-pod = pkgs.writeShellScriptBin "stop-pod" ''
      echo "🛑 Stopping nanna-coder pod..."

      # Check if podman is available
      if ! command -v podman &> /dev/null; then
        echo "❌ ERROR: podman command not found"
        echo "💡 Please install podman to manage pods"
        exit 1
      fi

      # Check if pod exists
      if ! podman pod exists nanna-coder-pod 2>/dev/null; then
        echo "ℹ️  Pod 'nanna-coder-pod' does not exist (already removed)"
        exit 0
      fi

      # Get pod status
      local status
      status=$(podman pod inspect nanna-coder-pod --format '{{.State}}' 2>/dev/null || echo "unknown")

      # Stop pod if running
      if [[ "$status" == "Running" ]] || [[ "$status" == "Degraded" ]]; then
        echo "Stopping pod 'nanna-coder-pod'..."
        if ! podman pod stop nanna-coder-pod 2>&1; then
          echo "❌ ERROR: Failed to stop pod 'nanna-coder-pod'"
          exit 1
        fi
        echo "✅ Pod stopped"
      else
        echo "ℹ️  Pod already stopped (status: $status)"
      fi

      # Remove pod
      echo "Removing pod 'nanna-coder-pod'..."
      if ! podman pod rm nanna-coder-pod 2>&1; then
        echo "❌ ERROR: Failed to remove pod 'nanna-coder-pod'"
        exit 1
      fi

      echo "✅ Pod stopped and removed successfully!"
    '';
  };

  # Cache management utilities
  cacheUtils = {
    # Script to check cache size and manage cleanup
    cache-info = pkgs.writeShellScriptBin "cache-info" ''
      echo "🗂️  Nanna Coder Model Cache Information"
      echo "======================================"

      CACHE_DIR="/nix/store"

      echo "📊 Cache Statistics:"
      echo "  Total models cached: $(find $CACHE_DIR -name "*-model" | wc -l)"
      echo "  Total cache size: $(du -sh $CACHE_DIR | cut -f1)"
      echo "  Available space: $(df -h $CACHE_DIR | tail -1 | awk '{print $4}')"

      echo ""
      echo "🏷️  Available Models:"
      ${lib.concatMapStringsSep "\n" (item: ''
        echo "  - ${item.value.name} (${item.value.size}) - ${item.value.description}"
      '') (lib.attrsToList modelRegistry)}

      echo ""
      echo "💡 Usage:"
      echo "  nix build .#qwen3-model     # Cache qwen3 model"
      echo "  nix build .#llama3-model    # Cache llama3 model"
      echo "  nix build .#mistral-model   # Cache mistral model"
      echo "  nix build .#gemma-model     # Cache gemma model"
      echo ""
      echo "  nix build .#ollama-qwen3    # Pre-built container with qwen3"
      echo "  nix build .#ollama-llama3   # Pre-built container with llama3"
    '';

    # Script to clean up old cached models
    cache-cleanup = pkgs.writeShellScriptBin "cache-cleanup" ''
      echo "🧹 Cleaning up model cache..."
      echo "Max size: ${toString cacheConfig.maxTotalSize}"
      echo "Eviction policy: ${cacheConfig.evictionPolicy}"

      # This would implement actual cleanup logic
      # For now, it's informational
      echo "⚠️  Cleanup logic not yet implemented"
      echo "Use 'nix-collect-garbage' for manual cleanup"
    '';
  };

  # Development experience optimization utilities
  devUtils = {
    # Fast incremental development build
    dev-build = pkgs.writeShellScriptBin "dev-build" ''
      echo "🚀 Starting fast incremental build..."

      # Use cargo-watch for incremental compilation
      if command -v cargo-watch &> /dev/null; then
        echo "📦 Using cargo-watch for incremental builds"
        cargo watch -x "build --workspace"
      else
        echo "📦 Running standard incremental build"
        cargo build --workspace
      fi

      echo "✅ Build complete!"
    '';

    # Comprehensive test runner with watch mode
    dev-test = pkgs.writeShellScriptBin "dev-test" ''
      echo "🧪 Starting comprehensive test suite..."

      # Run different test types based on arguments
      case "''${1:-all}" in
        "unit")
          echo "Running unit tests..."
          cargo nextest run --workspace --lib
          ;;
        "integration")
          echo "Running integration tests..."
          cargo nextest run --workspace --test '*'
          ;;
        "watch")
          echo "Starting test watch mode..."
          if command -v cargo-watch &> /dev/null; then
            cargo watch -x "nextest run --workspace"
          else
            echo "⚠️  cargo-watch not available, running tests once"
            cargo nextest run --workspace
          fi
          ;;
        "all"|*)
          echo "Running all tests..."
          cargo nextest run --workspace

          echo "🔍 Running clippy checks..."
          cargo clippy --workspace --all-targets -- -D warnings

          echo "📝 Checking formatting..."
          cargo fmt --all -- --check

          echo "🔒 Running security audit..."
          cargo audit

          echo "📋 Checking licenses..."
          cargo deny check
          ;;
      esac

      echo "✅ Test suite complete!"
    '';

    # Quick syntax and format check
    dev-check = pkgs.writeShellScriptBin "dev-check" ''
      echo "🔍 Running quick development checks..."

      echo "📝 Checking Rust formatting..."
      if ! cargo fmt --all -- --check; then
        echo "💡 Run 'cargo fmt' to fix formatting issues"
        exit 1
      fi

      echo "🔍 Running clippy (fast mode)..."
      if ! cargo clippy --workspace --all-targets -- -D warnings; then
        echo "💡 Fix clippy warnings before committing"
        exit 1
      fi

      echo "🏗️  Checking compilation..."
      if ! cargo check --workspace; then
        echo "💡 Fix compilation errors"
        exit 1
      fi

      echo "✅ All checks passed!"
    '';

    # Clean development artifacts
    dev-clean = pkgs.writeShellScriptBin "dev-clean" ''
      echo "🧹 Cleaning development artifacts..."

      echo "📦 Cleaning Cargo artifacts..."
      cargo clean

      echo "🗑️  Removing target directory..."
      rm -rf target/

      echo "🐳 Cleaning container images..."
      if command -v podman &> /dev/null; then
        podman system prune -f --filter until=24h
      elif command -v docker &> /dev/null; then
        docker system prune -f --filter until=24h
      fi

      echo "♻️  Cleaning Nix store (optional)..."
      nix store gc --max-age 7d

      echo "✅ Cleanup complete!"
    '';

    # Reset development environment
    dev-reset = pkgs.writeShellScriptBin "dev-reset" ''
      echo "🔄 Resetting development environment..."

      echo "🧹 Running cleanup..."
      dev-clean

      echo "🔧 Updating flake inputs..."
      nix flake update

      echo "📥 Rebuilding development shell..."
      nix develop --refresh

      echo "🎯 Warming cache with common builds..."
      nix build .#nanna-coder --no-link

      echo "✅ Development environment reset complete!"
    '';

    # Start development containers
    container-dev = pkgs.writeShellScriptBin "container-dev" ''
      echo "🐳 Starting development containers..."

      # Use docker-compose for orchestration
      if [ -f "docker-compose.yml" ] || [ -f "docker-compose.yaml" ]; then
        echo "📋 Using docker-compose configuration"
        if command -v podman-compose &> /dev/null; then
          podman-compose up -d
        elif command -v docker-compose &> /dev/null; then
          docker-compose up -d
        else
          echo "⚠️  No compose tool available"
          exit 1
        fi
      else
        echo "🚀 Starting individual containers..."

        # Start Ollama container
        echo "🤖 Starting Ollama container..."
        nix run .#start-pod
      fi

      echo "✅ Development containers started!"
      echo "💡 Use 'container-logs' to view logs"
    '';

    # Run containerized tests
    container-test = pkgs.writeShellScriptBin "container-test" ''
      echo "🧪 Running containerized tests..."

      echo "🐳 Starting test containers..."
      nix build .#ollamaImage --no-link

      # Load and start test container using nix2container's copyToDockerDaemon
      echo "📦 Loading test container..."
      if command -v podman &> /dev/null; then
        # Use nix2container's built-in copyToDockerDaemon method
        nix run .#ollamaImage.copyToDockerDaemon
        podman run -d --name nanna-test-ollama -p 11434:11434 nanna-coder-ollama:latest
      else
        echo "⚠️  Podman not available, skipping container tests"
        exit 1
      fi

      echo "⏳ Waiting for container to be ready..."
      sleep 10

      echo "🧪 Running integration tests..."
      cargo test --workspace --test '*' -- --test-threads=1

      echo "🧹 Cleaning up test containers..."
      podman stop nanna-test-ollama
      podman rm nanna-test-ollama

      echo "✅ Containerized tests complete!"
    '';

    # Stop all development containers
    container-stop = pkgs.writeShellScriptBin "container-stop" ''
      echo "🛑 Stopping development containers..."

      local stop_errors=0

      if command -v podman &> /dev/null; then
        echo "🐳 Checking podman containers..."

        # Get list of running containers
        local running_containers
        running_containers=$(podman ps -q 2>/dev/null)

        if [ -n "$running_containers" ]; then
          echo "Stopping $(echo "$running_containers" | wc -l) running container(s)..."
          if ! echo "$running_containers" | xargs -r podman stop 2>&1; then
            echo "⚠️  Some containers failed to stop"
            ((stop_errors++))
          else
            echo "✅ Stopped podman containers"
          fi
        else
          echo "ℹ️  No running podman containers"
        fi

        # Try to stop pod
        echo "Checking for nanna-coder pod..."
        if podman pod exists nanna-coder-pod 2>/dev/null; then
          if ! nix run .#stop-pod; then
            echo "⚠️  Failed to stop nanna-coder pod"
            ((stop_errors++))
          fi
        else
          echo "ℹ️  No nanna-coder pod running"
        fi
      else
        echo "ℹ️  Podman not available"
      fi

      if command -v docker &> /dev/null; then
        echo "🐳 Checking docker containers..."

        # Get list of running containers
        local running_containers
        running_containers=$(docker ps -q 2>/dev/null)

        if [ -n "$running_containers" ]; then
          echo "Stopping $(echo "$running_containers" | wc -l) running container(s)..."
          if ! echo "$running_containers" | xargs -r docker stop 2>&1; then
            echo "⚠️  Some containers failed to stop"
            ((stop_errors++))
          else
            echo "✅ Stopped docker containers"
          fi
        else
          echo "ℹ️  No running docker containers"
        fi
      else
        echo "ℹ️  Docker not available"
      fi

      if [ $stop_errors -eq 0 ]; then
        echo "✅ All containers stopped successfully!"
        exit 0
      else
        echo "⚠️  Completed with $stop_errors error(s)"
        exit 1
      fi
    '';

    # View container logs
    container-logs = pkgs.writeShellScriptBin "container-logs" ''
      echo "📋 Viewing container logs..."

      if command -v podman &> /dev/null; then
        echo "🐳 Podman containers:"
        podman ps --format "{{.Names}}" | while read container; do
          if [ -n "$container" ]; then
            echo "--- Logs for $container ---"
            podman logs --tail 20 "$container"
            echo ""
          fi
        done
      fi

      echo "💡 Use 'podman logs -f <container>' for live logs"
    '';

    # Warm cache with frequently used builds
    cache-warm = pkgs.writeShellScriptBin "cache-warm" ''
      echo "🔥 Warming development cache..."

      echo "📦 Building core packages..."
      nix build .#nanna-coder --no-link --print-build-logs

      echo "🐳 Building container images..."
      nix build .#harnessImage --no-link --print-build-logs &
      nix build .#ollamaImage --no-link --print-build-logs &

      echo "⏳ Waiting for background builds..."
      wait

      echo "📊 Cache statistics:"
      nix run .#cache-analytics

      echo "✅ Cache warming complete!"
    '';
  };

in
{
  inherit buildScripts cacheUtils devUtils;
}
