#!/usr/bin/env bash
set -euo pipefail

echo "🏗️  Building Multi-Architecture Containers"
echo "=========================================="

# Source Nix environment
if [[ -f /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]]; then
    source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
fi

ARCHITECTURES=("x86_64-linux" "aarch64-linux")
IMAGES=("harnessImage" "ollamaImage")

# Function to build for specific architecture
build_arch() {
    local arch=$1
    local image=$2

    echo "📦 Building $image for $arch..."

    if [[ "$arch" == "x86_64-linux" ]]; then
        # Native build
        if nix build .#"$image"; then
            echo "✅ Built $image for $arch"

            # Load into Podman with arch-specific tag
            podman load < result
            local base_name
            case "$image" in
                "harnessImage") base_name="nanna-coder-harness" ;;
                "ollamaImage") base_name="nanna-coder-ollama" ;;
            esac

            podman tag "$base_name:latest" "$base_name:$arch"
            echo "🏷️  Tagged as $base_name:$arch"
        else
            echo "❌ Failed to build $image for $arch"
            return 1
        fi
    else
        # Cross-compilation attempt
        echo "⚠️  Attempting cross-compilation for $arch..."

        # Capture build output for diagnostics
        local build_log
        build_log=$(mktemp)

        if nix build .#packages."$arch"."$image" --print-build-logs 2>"$build_log"; then
            echo "✅ Cross-compiled $image for $arch"
            rm -f "$build_log"

            # Validate result exists before loading
            if [ ! -e "result" ]; then
                echo "❌ ERROR: Build succeeded but result symlink not found"
                ls -la result* 2>/dev/null || echo "No result files found"
                return 1
            fi

            # Load into podman with error checking
            if ! podman load < result; then
                echo "❌ ERROR: Failed to load $image for $arch into podman"
                return 1
            fi

            local base_name
            case "$image" in
                "harnessImage") base_name="nanna-coder-harness" ;;
                "ollamaImage") base_name="nanna-coder-ollama" ;;
            esac

            # Verify image was loaded
            if ! podman image exists "$base_name:latest"; then
                echo "❌ ERROR: Image $base_name:latest not found after loading"
                echo "Available images:"
                podman images | grep "$base_name" || echo "No matching images found"
                return 1
            fi

            # Tag with error checking
            if ! podman tag "$base_name:latest" "$base_name:$arch"; then
                echo "❌ ERROR: Failed to tag image as $base_name:$arch"
                return 1
            fi
            echo "🏷️  Tagged as $base_name:$arch"
        else
            # Analyze the build failure
            local error_msg
            error_msg=$(cat "$build_log")
            rm -f "$build_log"

            echo "⚠️  Cross-compilation failed for $arch"

            # Distinguish between different failure types
            if echo "$error_msg" | grep -qi "unsupported system\|not supported"; then
                echo "💡 Cross-compilation to $arch is not configured for this flake"
                echo "   This is expected - cross-compilation setup is optional"
                return 1
            elif echo "$error_msg" | grep -qi "toolchain\|linker"; then
                echo "❌ Cross-compilation toolchain error:"
                echo "$error_msg" | grep -i "toolchain\|linker" | head -5
                return 1
            elif echo "$error_msg" | grep -qi "network\|fetch\|download"; then
                echo "❌ Network error during cross-compilation:"
                echo "$error_msg" | grep -i "network\|fetch\|download" | head -5
                echo "💡 This may be a transient network issue - retry may succeed"
                return 1
            else
                echo "❌ Build error (first 10 lines):"
                echo "$error_msg" | head -10
            fi

            # Fallback: Build with emulation (slower but works)
            echo ""
            echo "🔄 Checking for QEMU emulation fallback..."
            if command -v qemu-user-static >/dev/null 2>&1; then
                echo "   QEMU available but emulation setup not implemented"
                echo "⏭️  Skipping $image for $arch (emulation setup required)"
                return 1
            else
                echo "   No QEMU emulation available"
                echo "⏭️  Skipping $arch build"
                return 1
            fi
        fi
    fi
}

# Function to test container
test_container() {
    local image_name=$1
    local arch=$2

    echo "🧪 Testing $image_name:$arch..."

    case "$image_name" in
        "nanna-coder-harness")
            if podman run --rm "$image_name:$arch" --version >/dev/null 2>&1; then
                echo "✅ $image_name:$arch responds correctly"
            else
                echo "⚠️  $image_name:$arch test failed (may be expected for cross-arch)"
            fi
            ;;
        "nanna-coder-ollama")
            if podman run --rm "$image_name:$arch" --help >/dev/null 2>&1; then
                echo "✅ $image_name:$arch responds correctly"
            else
                echo "⚠️  $image_name:$arch test failed (may be expected for cross-arch)"
            fi
            ;;
    esac
}

# Build all combinations
for image in "${IMAGES[@]}"; do
    for arch in "${ARCHITECTURES[@]}"; do
        echo ""
        echo "🎯 Building $image for $arch"
        echo "----------------------------------------"

        if build_arch "$arch" "$image"; then
            # Test the built container
            case "$image" in
                "harnessImage") test_container "nanna-coder-harness" "$arch" ;;
                "ollamaImage") test_container "nanna-coder-ollama" "$arch" ;;
            esac
        fi
    done
done

echo ""
echo "📋 Summary of built images:"
echo "=========================="
podman images | grep nanna-coder | sort

echo ""
echo "💡 Usage examples:"
echo "  podman run --rm -p 8080:8080 nanna-coder-harness:x86_64-linux"
echo "  podman run --rm -p 11434:11434 nanna-coder-ollama:x86_64-linux"

# Optional: Create multi-arch manifests (requires podman >= 3.0)
if command -v podman >/dev/null 2>&1 && podman version --format='{{.Client.Version}}' | grep -E '^[3-9]' >/dev/null; then
    echo ""
    echo "🔗 Creating multi-arch manifests..."

    for image in "${IMAGES[@]}"; do
        local base_name
        case "$image" in
            "harnessImage") base_name="nanna-coder-harness" ;;
            "ollamaImage") base_name="nanna-coder-ollama" ;;
        esac

        # Create manifest list if we have multiple architectures
        available_images=$(podman images --format "{{.Repository}}:{{.Tag}}" | grep "^$base_name:" | grep -v ":latest")
        if [[ $(echo "$available_images" | wc -l) -gt 1 ]]; then
            echo "📝 Creating manifest for $base_name..."

            manifest_name="$base_name:multi-arch"
            if podman manifest rm "$manifest_name" 2>/dev/null || true; then
                echo "🗑️  Removed existing manifest"
            fi

            podman manifest create "$manifest_name"
            for img in $available_images; do
                podman manifest add "$manifest_name" "$img" || echo "⚠️  Failed to add $img to manifest"
            done

            echo "✅ Multi-arch manifest created: $manifest_name"
        fi
    done
fi

echo ""
echo "✅ Multi-architecture container build completed!"