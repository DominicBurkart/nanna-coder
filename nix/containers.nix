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
, rustToolchain
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

  # Development container image for nanna self-development
  devContainerImage = nix2containerPkgs.nix2container.buildImage {
    name = "nanna-coder-dev";
    tag = "latest";

    copyToRoot = pkgs.buildEnv {
      name = "dev-env";
      paths = [
        harness
        rustToolchain
        pkgs.cargo-nextest
        pkgs.cargo-audit
        pkgs.cargo-deny
        pkgs.cargo-tarpaulin
        pkgs.bash
        pkgs.coreutils
        pkgs.git
        pkgs.cacert
        pkgs.pkg-config
        pkgs.openssl
        pkgs.stdenv.cc
        pkgs.libssh2
        pkgs.zlib
      ];
      pathsToLink = [ "/bin" "/etc" "/share" "/lib" ];
    };

    config = {
      Cmd = [ "${pkgs.bash}/bin/sleep" "infinity" ];
      Env = [
        "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        "PATH=/bin"
        "CARGO_HOME=/tmp/cargo"
        "PKG_CONFIG_PATH=/lib/pkgconfig"
        "RUST_LOG=info"
      ];
      WorkingDir = "/workspace";
    };

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
        "HOME=/root"
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
    "gemma" = {
      name = "gemma4:e4b";
      hash = "sha256-KkYtMpk6i++WJ9pqnzsOQ55qRA3zR57PxIewn8K0OeM=";
      description = "Gemma 4 E4B - Larger Gemma 4 variant for CI evaluation (Ollama)";
      size = "~4GB";
      homepage = "https://ollama.com/library/gemma4";
    };
  };

  assertRealModelHash = modelKey: modelInfo:
    if (lib.hasInfix "0000000000000000000000000000000000000000000" modelInfo.hash) then
      throw ''
        Model `${modelInfo.name}` (key=${modelKey}) is using the all-zeros
        placeholder sha256 in nix/containers.nix. Production model paths
        require a real, content-addressed hash so the weights are baked
        into the image and pushed to the binary cache.

        Capture the real hash by running, on a machine with network +
        Ollama installed:

            scripts/update-model-sha256.sh ${modelKey}

        See nix/README.md ("Capturing a model sha256") and issue #240.
      ''
    else
      modelInfo;

  createStrictModelDerivation = modelKey: modelInfo:
    createModelDerivation modelKey (assertRealModelHash modelKey modelInfo);

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
    gemma-model = createModelDerivation "gemma" modelRegistry.gemma;
  };

  strictModels = {
    qwen3-model-strict = createStrictModelDerivation "qwen3" modelRegistry.qwen3;
    gemma-model-strict = createStrictModelDerivation "gemma" modelRegistry.gemma;
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
  inherit harnessImage ollamaImage vllmImage devContainerImage;
  inherit modelRegistry models strictModels containers;
}
