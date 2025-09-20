#!/usr/bin/env bash
set -euo pipefail

echo "🧪 Testing Nanna Coder Container Builds"
echo "======================================="

# Source Nix environment
if [[ -f /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]]; then
    source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
fi

# Build containers
echo "📦 Building harness container..."
nix build .#harnessImage

echo "📦 Building ollama container..."
nix build .#ollamaImage

echo "🐳 Loading images into Podman..."
podman load < result-harnessImage
podman load < result-ollamaImage

echo "📋 Listing container images..."
podman images | grep nanna-coder

echo "✅ Container builds completed successfully!"
echo ""
echo "Next steps:"
echo "  podman run --rm -p 8080:8080 nanna-coder-harness:latest"
echo "  podman run --rm -p 11434:11434 nanna-coder-ollama:latest"