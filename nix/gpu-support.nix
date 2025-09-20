# GPU support configuration for containerized environments
{ lib, pkgs, system }:

let
  # NVIDIA GPU support
  nvidiaSupport = {
    # CUDA runtime libraries to include in containers
    cudaLibraries = with pkgs.cudaPackages; [
      cuda_runtime
      cuda_cudart
      libcublas
      libcufft
      libcurand
      libcusparse
      libcusolver
      libnpp
    ];

    # NVIDIA container runtime configuration
    containerRuntimeConfig = pkgs.writeTextFile {
      name = "nvidia-container-runtime.json";
      text = builtins.toJSON {
        runtimes = {
          nvidia = {
            path = "${pkgs.nvidia-container-runtime}/bin/nvidia-container-runtime";
            runtimeArgs = [];
          };
        };
      };
    };

    # Podman GPU configuration
    podmanGpuConfig = pkgs.writeTextFile {
      name = "containers.conf";
      text = ''
        [containers]
        # GPU support
        default_capabilities = [
          "CHOWN",
          "DAC_OVERRIDE",
          "FOWNER",
          "FSETID",
          "KILL",
          "NET_BIND_SERVICE",
          "SETFCAP",
          "SETGID",
          "SETPCAP",
          "SETUID",
          "SYS_CHROOT"
        ]

        [engine]
        # NVIDIA runtime
        runtime = "nvidia"

        # Device access
        [engine.runtimes.nvidia]
        path = "${pkgs.nvidia-container-runtime}/bin/nvidia-container-runtime"
      '';
    };

    # GPU-enabled container image with CUDA
    buildGpuImage = { name, contents ? [], config ? {} }:
      pkgs.dockerTools.buildLayeredImage {
        inherit name config;
        contents = contents ++ nvidiaSupport.cudaLibraries ++ [
          pkgs.nvidia-container-runtime
        ];

        # Ensure CUDA libraries are properly linked
        extraCommands = ''
          # Create symlinks for CUDA libraries
          mkdir -p usr/lib/x86_64-linux-gnu
          for lib in ${lib.concatStringsSep " " (map (pkg: "${pkg}/lib/*") nvidiaSupport.cudaLibraries)}; do
            if [ -f "$lib" ]; then
              ln -sf "$lib" usr/lib/x86_64-linux-gnu/$(basename "$lib")
            fi
          done

          # Set up CUDA paths
          mkdir -p usr/local/cuda/lib64
          for lib in ${lib.concatStringsSep " " (map (pkg: "${pkg}/lib/*") nvidiaSupport.cudaLibraries)}; do
            if [ -f "$lib" ]; then
              ln -sf "$lib" usr/local/cuda/lib64/$(basename "$lib")
            fi
          done
        '';
      };
  };

  # AMD ROCm support
  rocmSupport = {
    # ROCm libraries to include in containers
    rocmLibraries = with pkgs.rocmPackages; [
      rocm-runtime
      rocm-device-libs
      rocminfo
      rocm-smi
      hip
      rocsparse
      rocblas
      rocfft
      rocrand
      rocsolver
    ];

    # ROCm container runtime configuration
    containerRuntimeConfig = pkgs.writeTextFile {
      name = "rocm-container-runtime.json";
      text = builtins.toJSON {
        runtimes = {
          rocm = {
            path = "${pkgs.runc}/bin/runc";
            runtimeArgs = ["--device" "/dev/dri" "--device" "/dev/kfd"];
          };
        };
      };
    };

    # GPU-enabled container image with ROCm
    buildGpuImage = { name, contents ? [], config ? {} }:
      pkgs.dockerTools.buildLayeredImage {
        inherit name config;
        contents = contents ++ rocmSupport.rocmLibraries;

        extraCommands = ''
          # Create device nodes for AMD GPU access
          mkdir -p dev/dri dev

          # Set up ROCm library paths
          mkdir -p opt/rocm/lib
          for lib in ${lib.concatStringsSep " " (map (pkg: "${pkg}/lib/*") rocmSupport.rocmLibraries)}; do
            if [ -f "$lib" ]; then
              ln -sf "$lib" opt/rocm/lib/$(basename "$lib")
            fi
          done
        '';
      };
  };

  # Intel GPU support (for integrated graphics)
  intelGpuSupport = {
    # Intel GPU libraries
    intelLibraries = with pkgs; [
      intel-media-driver
      intel-compute-runtime
      mesa
      libva
      libva-utils
    ];

    # GPU-enabled container image with Intel GPU support
    buildGpuImage = { name, contents ? [], config ? {} }:
      pkgs.dockerTools.buildLayeredImage {
        inherit name config;
        contents = contents ++ intelGpuSupport.intelLibraries;

        extraCommands = ''
          # Create device nodes for Intel GPU access
          mkdir -p dev/dri

          # Set up Intel GPU library paths
          mkdir -p usr/lib/x86_64-linux-gnu/dri
          for lib in ${lib.concatStringsSep " " (map (pkg: "${pkg}/lib/dri/*") intelGpuSupport.intelLibraries)}; do
            if [ -f "$lib" ]; then
              ln -sf "$lib" usr/lib/x86_64-linux-gnu/dri/$(basename "$lib")
            fi
          done
        '';
      };
  };

  # GPU detection and configuration script
  gpuDetectionScript = pkgs.writeShellScriptBin "detect-gpu" ''
    #!/usr/bin/env bash
    set -e

    echo "🔍 Detecting available GPUs..."

    # Detect NVIDIA GPUs
    if command -v nvidia-smi &> /dev/null; then
      echo "✅ NVIDIA GPU detected:"
      nvidia-smi --query-gpu=name,memory.total --format=csv,noheader
      export GPU_TYPE="nvidia"
      export GPU_RUNTIME="nvidia"
      export CONTAINER_ARGS="--gpus all"
    # Detect AMD GPUs
    elif [ -d "/sys/class/drm" ] && ls /sys/class/drm/card*/device/vendor 2>/dev/null | xargs grep -l "0x1002" > /dev/null 2>&1; then
      echo "✅ AMD GPU detected:"
      if command -v rocm-smi &> /dev/null; then
        rocm-smi --showproductname
      else
        echo "  ROCm tools not available, basic AMD GPU detected"
      fi
      export GPU_TYPE="amd"
      export GPU_RUNTIME="runc"
      export CONTAINER_ARGS="--device /dev/dri --device /dev/kfd"
    # Detect Intel GPUs
    elif [ -d "/sys/class/drm" ] && ls /sys/class/drm/card*/device/vendor 2>/dev/null | xargs grep -l "0x8086" > /dev/null 2>&1; then
      echo "✅ Intel GPU detected:"
      ls /sys/class/drm/card*/device/device | head -1 | xargs cat
      export GPU_TYPE="intel"
      export GPU_RUNTIME="runc"
      export CONTAINER_ARGS="--device /dev/dri"
    else
      echo "⚠️  No GPU detected or GPU not supported"
      export GPU_TYPE="none"
      export GPU_RUNTIME="runc"
      export CONTAINER_ARGS=""
    fi

    echo ""
    echo "GPU Configuration:"
    echo "  Type: $GPU_TYPE"
    echo "  Runtime: $GPU_RUNTIME"
    echo "  Container Args: $CONTAINER_ARGS"
  '';

  # Podman GPU run script
  podmanGpuScript = pkgs.writeShellScriptBin "podman-gpu" ''
    #!/usr/bin/env bash
    set -e

    # Source GPU detection
    eval "$(${gpuDetectionScript}/bin/detect-gpu)"

    # Run podman with appropriate GPU configuration
    case "$GPU_TYPE" in
      nvidia)
        echo "🚀 Running with NVIDIA GPU support..."
        podman run --runtime nvidia --gpus all "$@"
        ;;
      amd)
        echo "🚀 Running with AMD GPU support..."
        podman run --device /dev/dri --device /dev/kfd "$@"
        ;;
      intel)
        echo "🚀 Running with Intel GPU support..."
        podman run --device /dev/dri "$@"
        ;;
      *)
        echo "🚀 Running without GPU acceleration..."
        podman run "$@"
        ;;
    esac
  '';

  # Docker Compose override for GPU support
  gpuComposeOverride = pkgs.writeTextFile {
    name = "docker-compose.gpu.yml";
    text = ''
      version: '3.8'

      services:
        ollama:
          deploy:
            resources:
              reservations:
                devices:
                  - driver: nvidia
                    count: all
                    capabilities: [gpu]
          # For AMD GPU support, uncomment below:
          # devices:
          #   - /dev/dri
          #   - /dev/kfd
          environment:
            - NVIDIA_VISIBLE_DEVICES=all
            - NVIDIA_DRIVER_CAPABILITIES=compute,utility

        harness:
          # Inherit GPU access from ollama if needed
          profiles:
            - gpu
    '';
  };

in {
  inherit nvidiaSupport rocmSupport intelGpuSupport;
  inherit gpuDetectionScript podmanGpuScript gpuComposeOverride;

  # Build GPU-enabled images based on detected hardware
  buildGpuImage = { name, contents ? [], config ? {}, gpuType ? "auto" }:
    if gpuType == "nvidia" || (gpuType == "auto" && builtins.pathExists "/dev/nvidia0")
    then nvidiaSupport.buildGpuImage { inherit name contents config; }
    else if gpuType == "amd" || (gpuType == "auto" && builtins.pathExists "/dev/kfd")
    then rocmSupport.buildGpuImage { inherit name contents config; }
    else if gpuType == "intel"
    then intelGpuSupport.buildGpuImage { inherit name contents config; }
    else pkgs.dockerTools.buildLayeredImage { inherit name contents config; };

  # Utility to check GPU support
  checkGpuSupport = system:
    lib.optionalAttrs (system == "x86_64-linux" || system == "aarch64-linux") {
      nvidia = builtins.pathExists "/dev/nvidia0";
      amd = builtins.pathExists "/dev/kfd";
      intel = builtins.pathExists "/dev/dri";
    };
}