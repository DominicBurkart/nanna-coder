# Container image definitions using nix2container
# This module contains:
# - Base container images (harnessImage, ollamaImage)
# - Model registry and metadata
# - Multi-model containers with pre-cached models
# - Model derivation creation logic

{ pkgs
, lib
, nix2containerPkgs
, containerConfig
, harness
}:

let
  # Container image for the harness CLI
  harnessImage = nix2containerPkgs.nix2container.buildImage {
    name = containerConfig.images.harness;
    tag = containerConfig.tags.default;

    copyToRoot = pkgs.buildEnv {
      name = "harness-env";
      paths = [
        harness
        pkgs.cacert  # For HTTPS requests
        pkgs.tzdata  # Timezone data
        pkgs.bash    # Shell for debugging
        pkgs.coreutils # Basic utilities
      ];
      pathsToLink = [ "/bin" "/etc" "/share" ];
    };

    config = {
      Cmd = [ "${harness}/bin/harness" ];
      Env = [
        "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        "RUST_LOG=info"
        "PATH=/bin"
      ];
      WorkingDir = "/app";
      ExposedPorts = {
        "8080/tcp" = {};
      };
    };

    # Reproducible layer strategy
    maxLayers = containerConfig.runtime.maxLayers;
  };

  # Ollama service container using nix2container
  ollamaImage = nix2containerPkgs.nix2container.buildImage {
    name = containerConfig.images.ollama;
    tag = containerConfig.tags.default;

    copyToRoot = pkgs.buildEnv {
      name = "ollama-env";
      paths = [
        pkgs.ollama
        pkgs.cacert
        pkgs.tzdata
        pkgs.bash
        pkgs.coreutils
      ];
      pathsToLink = [ "/bin" "/etc" "/share" ];
    };

    config = {
      Cmd = [ "${pkgs.ollama}/bin/ollama" "serve" ];
      Env = [
        "OLLAMA_HOST=0.0.0.0"
        "OLLAMA_PORT=11434"
        "PATH=/bin"
      ];
      WorkingDir = "/app";
      ExposedPorts = {
        "11434/tcp" = {};
      };
      Volumes = {
        "/root/.ollama" = {};
      };
    };

    # Reproducible layer strategy
    maxLayers = containerConfig.runtime.maxLayers;
  };

  /** Model registry with content hashes

  Metadata for all supported AI models with content-addressable caching.

  # Usage

  ```nix
  # Access model metadata
  modelRegistry.qwen3.name
  => "qwen3:0.6b"

  modelRegistry.llama3.size
  => "4.7GB"
  ```

  # Model Hashes

  Each model requires a content hash for reproducible builds. To calculate a hash:

  ```bash
  # 1. Pull model with ollama
  ollama pull llama3:8b

  # 2. Find model file location
  # Typically in ~/.ollama/models/blobs/

  # 3. Calculate nix hash
  nix hash path /path/to/model/blob
  ```

  # See Also

  - Container loading: nix/container-config.nix
  */
  modelRegistry = {
    "qwen3" = {
      name = "qwen3:0.6b";
      hash = "sha256-2EaXyBr1C+6wNyLzcWblzB52iV/2G26dSa5MFqpYJLc=";
      description = "Qwen3 0.6B - Fast and efficient model for testing";
      size = "560MB";
      homepage = "https://ollama.com/library/qwen3";
    };
  };

  # Function to create a model derivation with proper caching
  createModelDerivation = modelKey: modelInfo:
    pkgs.runCommand "${modelKey}-model" {
        # Fixed-output derivation for reproducible caching
        outputHash = modelInfo.hash;
        outputHashAlgo = "sha256";
        outputHashMode = "recursive";
        nativeBuildInputs = with pkgs; [ ollama curl cacert ];
        # Add meta information for documentation
        meta = with lib; {
          description = "${modelInfo.description} (cached for testing)";
          longDescription = ''
            Pre-downloaded ${modelInfo.name} model for reproducible testing.
            This derivation downloads the model once and caches it by content hash.
            Size: ${modelInfo.size}
          '';
          homepage = modelInfo.homepage;
          platforms = platforms.linux;
        };
      } ''
      echo "🔄 Setting up ${modelInfo.name} model download (reproducible)..."

      # Create output directory structure
      mkdir -p $out/models

      # Set up environment for ollama
      export OLLAMA_MODELS=$out/models
      export HOME=$(mktemp -d)
      export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt

      # Start ollama server in isolated environment
      echo "🚀 Starting temporary Ollama server..."
      ollama serve > ollama.log 2>&1 &
      OLLAMA_PID=$!

      # Function to cleanup on exit
      cleanup() {
        echo "🧹 Cleaning up Ollama server..."
        kill $OLLAMA_PID 2>/dev/null || true
        wait $OLLAMA_PID 2>/dev/null || true
      }
      trap cleanup EXIT

      # Wait for ollama to be ready
      echo "⏳ Waiting for Ollama server..."
      for i in {1..30}; do
        if curl -s http://localhost:11434/api/tags >/dev/null 2>&1; then
          echo "✅ Ollama server ready"
          break
        fi
        sleep 2
        if [ $i -eq 30 ]; then
          echo "❌ Ollama server failed to start"
          cat ollama.log
          exit 1
        fi
      done

      # Download the model
      echo "📥 Downloading ${modelInfo.name} model (${modelInfo.size} - will be cached by hash)..."
      if ! ollama pull ${modelInfo.name}; then
        echo "❌ Failed to download ${modelInfo.name}"
        cat ollama.log
        exit 1
      fi

      # Verify download
      if ! ollama list | grep -q "${modelInfo.name}"; then
        echo "❌ Model verification failed"
        ollama list
        exit 1
      fi

      # Stop ollama (cleanup will handle this too)
      cleanup

      echo "✅ ${modelInfo.name} model cached at $out/models"
      echo "📊 Model cache contents:"
      find $out/models -type f -exec ls -lh {} \; | head -5
    '';

  # Multi-model cache system - reproducible model derivations
  models = {
    qwen3-model = createModelDerivation "qwen3" modelRegistry.qwen3;
  };

  # Multi-model containers with pre-cached models
  containers = {
    qwen3-container = nix2containerPkgs.nix2container.buildImage {
      name = containerConfig.images.models.qwen3;
      tag = containerConfig.tags.default;
      fromImage = ollamaImage;
      copyToRoot = pkgs.buildEnv {
        name = "ollama-qwen3-env";
        paths = [ pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils pkgs.curl models.qwen3-model ];
        pathsToLink = [ "/bin" "/etc" "/share" "/models" ];
      };
      config = {
        Cmd = [ "${pkgs.ollama}/bin/ollama" "serve" ];
        Env = [ "OLLAMA_HOST=0.0.0.0" "OLLAMA_PORT=11434" "OLLAMA_MODELS=/models" "PATH=/bin" ];
        WorkingDir = "/app";
        ExposedPorts = { "11434/tcp" = {}; };
        Volumes = { "/root/.ollama" = {}; };
      };
      created = containerConfig.runtime.buildTimestamp;
      maxLayers = containerConfig.runtime.maxLayers;
    };
  };

in
{
  inherit harnessImage ollamaImage;
  inherit modelRegistry models containers;
}
