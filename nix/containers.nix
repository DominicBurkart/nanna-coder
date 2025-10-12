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

  # Hash Behavior

  - **Real hash** (qwen3): Fully reproducible, cached by Nix store
  - **Placeholder hash** (llama3, mistral, gemma): Development mode

  # Development Mode (Placeholder Hashes)

  Placeholder hashes (sha256-0000...000) enable fast iteration:

  ```bash
  # Build is fast (no model download during nix build)
  nix build .#llama3-container

  # Container downloads model on first run
  docker run nanna-coder-ollama-llama3:latest
  # Model is downloaded inside container when needed
  ```

  Benefits:
  - Fast builds (no 4GB+ downloads)
  - Flexible for testing
  - On-demand model loading

  # Production Mode (Real Hashes)

  Convert to production for reproducible caching:

  ```bash
  # 1. Build and run container to download model
  nix build .#llama3-container
  docker run -it nanna-coder-ollama-llama3:latest

  # 2. Calculate actual content hash
  nix hash path /path/to/downloaded/model

  # 3. Update hash in modelRegistry (this file)
  # 4. Rebuild - now cached and reproducible
  ```

  # Example: Model Registry Entry

  ```nix
  "qwen3" = {
    name = "qwen3:0.6b";
    hash = "sha256-2EaXyBr1C+6wNyLzcWblzB52iV/2G26dSa5MFqpYJLc=";  # Real hash
    description = "Qwen3 0.6B - Fast and efficient model for testing";
    size = "560MB";
    homepage = "https://ollama.com/library/qwen3";
  };

  "llama3" = {
    name = "llama3:8b";
    hash = "sha256-0000000000000000000000000000000000000000000=";  # DEVELOPMENT MODE
    description = "Llama3 8B - High quality general purpose model";
    size = "4.7GB";
    homepage = "https://ollama.com/library/llama3";
  };
  ```

  # Usage

  ```nix
  # Access model metadata
  modelRegistry.qwen3.name
  => "qwen3:0.6b"

  modelRegistry.llama3.size
  => "4.7GB"
  ```

  # See Also

  - Hash detection logic: createModelDerivation function (below)
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
    "llama3" = {
      name = "llama3:8b";
      hash = "sha256-0000000000000000000000000000000000000000000="; # DEVELOPMENT MODE
      description = "Llama3 8B - High quality general purpose model";
      size = "4.7GB";
      homepage = "https://ollama.com/library/llama3";
    };
    "mistral" = {
      name = "mistral:7b";
      hash = "sha256-0000000000000000000000000000000000000000000="; # DEVELOPMENT MODE
      description = "Mistral 7B - Balanced performance model";
      size = "4.1GB";
      homepage = "https://ollama.com/library/mistral";
    };
    "gemma" = {
      name = "gemma:2b";
      hash = "sha256-0000000000000000000000000000000000000000000="; # DEVELOPMENT MODE
      description = "Gemma 2B - Lightweight model for development";
      size = "1.4GB";
      homepage = "https://ollama.com/library/gemma";
    };
  };

  # Function to create a model derivation with proper caching
  createModelDerivation = modelKey: modelInfo:
    # Use conditional logic to handle placeholder hashes
    if (lib.hasInfix "0000000000000000000000000000000000000000000" modelInfo.hash) then
      # For development/CI - create non-fixed derivation that downloads on demand
      pkgs.runCommand "${modelKey}-model" {
        nativeBuildInputs = with pkgs; [ ollama curl cacert ];
        # Development mode - no fixed hash
      } ''
        echo "🔄 Creating development model stub for ${modelInfo.name}..."
        mkdir -p $out/models
        echo "${modelInfo.name}" > $out/models/model.info
        echo "Development mode - model will be downloaded on first use" > $out/models/README
      ''
    else
      # Production mode with real hashes
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
    llama3-model = createModelDerivation "llama3" modelRegistry.llama3;
    mistral-model = createModelDerivation "mistral" modelRegistry.mistral;
    gemma-model = createModelDerivation "gemma" modelRegistry.gemma;
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

    llama3-container = nix2containerPkgs.nix2container.buildImage {
      name = containerConfig.images.models.llama3;
      tag = containerConfig.tags.default;
      fromImage = ollamaImage;
      copyToRoot = pkgs.buildEnv {
        name = "ollama-llama3-env";
        paths = [ pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils pkgs.curl models.llama3-model ];
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

    mistral-container = nix2containerPkgs.nix2container.buildImage {
      name = containerConfig.images.models.mistral;
      tag = containerConfig.tags.default;
      fromImage = ollamaImage;
      copyToRoot = pkgs.buildEnv {
        name = "ollama-mistral-env";
        paths = [ pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils pkgs.curl models.mistral-model ];
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

    gemma-container = nix2containerPkgs.nix2container.buildImage {
      name = containerConfig.images.models.gemma;
      tag = containerConfig.tags.default;
      fromImage = ollamaImage;
      copyToRoot = pkgs.buildEnv {
        name = "ollama-gemma-env";
        paths = [ pkgs.cacert pkgs.tzdata pkgs.bash pkgs.coreutils pkgs.curl models.gemma-model ];
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
