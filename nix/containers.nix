# Container image definitions using nix2container
# This module contains:
# - Base container images (harnessImage, ollamaImage)
# - Model registry and metadata
# - Multi-model containers with pre-cached models
# - Model derivation creation logic

{ pkgs
, lib
, nix2containerPkgs
, harness
}:

let
  # Container image for the harness CLI
  harnessImage = nix2containerPkgs.nix2container.buildImage {
    name = "nanna-coder-harness";
    tag = "latest";

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
    maxLayers = 100;
  };

  # Ollama service container using nix2container
  ollamaImage = nix2containerPkgs.nix2container.buildImage {
    name = "nanna-coder-ollama";
    tag = "latest";

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
    maxLayers = 100;
  };

  # vLLM container wrapper scripts
  # Since vLLM uses the upstream Docker image, we provide wrapper scripts
  # instead of building custom Nix containers
  vllmImage = { model ? "XiaomiMiMo/MiMo-V2-Flash", extraArgs ? [] }:
    pkgs.writeShellApplication {
      name = "run-vllm-${builtins.replaceStrings ["/"] ["-"] model}";
      runtimeInputs = with pkgs; [ docker podman ];
      text = ''
        # Default model: ${model}
        MODEL="''${1:-${model}}"
        
        echo "🚀 Starting vLLM server with model: $MODEL"
        echo "📦 Using vllm/vllm-openai:latest"
        echo "🌐 API will be available on http://localhost:8000"
        echo ""
        
        # Check if using docker or podman
        if command -v docker &> /dev/null; then
          CONTAINER_CMD=docker
        elif command -v podman &> /dev/null; then
          CONTAINER_CMD=podman
        else
          echo "❌ Error: Neither docker nor podman found"
          exit 1
        fi
        
        # Run vLLM container
        $CONTAINER_CMD run -d \
          --name nanna-coder-vllm \
          -p 8000:8000 \
          -v "$HOME/.cache/huggingface:/root/.cache/huggingface" \
          vllm/vllm-openai:latest \
          --model "$MODEL" \
          --host 0.0.0.0 \
          --port 8000 \
          --trust-remote-code \
          ${lib.concatStringsSep " " extraArgs}
        
        echo "✅ vLLM container started"
        echo "📊 Monitor logs with: $CONTAINER_CMD logs -f nanna-coder-vllm"
        echo "🔍 Check health: curl http://localhost:8000/health"
        echo "📚 List models: curl http://localhost:8000/v1/models"
      '';
    };

  # Model registry with metadata for all supported models
  # Updated to use HuggingFace models for vLLM
  modelRegistry = {
    "mimo-v2-flash" = {
      name = "XiaomiMiMo/MiMo-V2-Flash";
      description = "MiMo V2 Flash - Fast reasoning model with custom architecture";
      size = "~2GB";
      homepage = "https://huggingface.co/XiaomiMiMo/MiMo-V2-Flash";
      requiresTrustRemoteCode = true;
    };
    "qwen3-coder-30b" = {
      name = "Qwen/Qwen3-Coder-30B-A3B-Instruct";
      description = "Qwen3 Coder 30B - Advanced coding model with instruction tuning";
      size = "~30GB";
      homepage = "https://huggingface.co/Qwen/Qwen3-Coder-30B-A3B-Instruct";
      requiresTrustRemoteCode = false;
    };
    # Legacy Ollama models (kept for backward compatibility during migration)
    "qwen3" = {
      name = "qwen3:0.6b";
      hash = "sha256-2EaXyBr1C+6wNyLzcWblzB52iV/2G26dSa5MFqpYJLc=";
      description = "Qwen3 0.6B - Fast and efficient model for testing (Ollama)";
      size = "560MB";
      homepage = "https://ollama.com/library/qwen3";
    };
    "llama3" = {
      name = "llama3:8b";
      hash = "sha256-0000000000000000000000000000000000000000000="; # Placeholder
      description = "Llama3 8B - High quality general purpose model (Ollama)";
      size = "4.7GB";
      homepage = "https://ollama.com/library/llama3";
    };
    "mistral" = {
      name = "mistral:7b";
      hash = "sha256-0000000000000000000000000000000000000000000="; # Placeholder
      description = "Mistral 7B - Balanced performance model (Ollama)";
      size = "4.1GB";
      homepage = "https://ollama.com/library/mistral";
    };
    "gemma" = {
      name = "gemma:2b";
      hash = "sha256-0000000000000000000000000000000000000000000="; # Placeholder
      description = "Gemma 2B - Lightweight model for development (Ollama)";
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
      name = "nanna-coder-ollama-qwen3";
      tag = "latest";
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
      created = "2025-09-20T00:00:00Z";
      maxLayers = 100;
    };

    llama3-container = nix2containerPkgs.nix2container.buildImage {
      name = "nanna-coder-ollama-llama3";
      tag = "latest";
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
      created = "2025-09-20T00:00:00Z";
      maxLayers = 100;
    };

    mistral-container = nix2containerPkgs.nix2container.buildImage {
      name = "nanna-coder-ollama-mistral";
      tag = "latest";
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
      created = "2025-09-20T00:00:00Z";
      maxLayers = 100;
    };

    gemma-container = nix2containerPkgs.nix2container.buildImage {
      name = "nanna-coder-ollama-gemma";
      tag = "latest";
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
      created = "2025-09-20T00:00:00Z";
      maxLayers = 100;
    };
  };

in
{
  inherit harnessImage ollamaImage vllmImage;
  inherit modelRegistry models containers;
}
