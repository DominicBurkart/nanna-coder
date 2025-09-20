#!/usr/bin/env bash
set -euo pipefail

echo "🚀 Setting up Nanna Coder Pod"
echo "============================="

# Source Nix environment
if [[ -f /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]]; then
    source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
fi

# Create pod
echo "📦 Creating nanna-coder pod..."
podman pod create --name nanna-coder-pod -p 8080:8080 -p 11434:11434

# Start ollama service
echo "🤖 Starting Ollama service..."
podman run -d --pod nanna-coder-pod --name ollama-service \
    -v ollama-data:/root/.ollama \
    nanna-coder-ollama:latest

# Wait for ollama to be ready
echo "⏳ Waiting for Ollama to start..."
sleep 10

# Start harness
echo "🔧 Starting harness service..."
podman run -d --pod nanna-coder-pod --name harness-service \
    -e OLLAMA_URL=http://localhost:11434 \
    nanna-coder-harness:latest

echo "✅ Pod setup completed!"
echo ""
echo "Services:"
echo "  Harness: http://localhost:8080"
echo "  Ollama:  http://localhost:11434"
echo ""
echo "Pod management:"
echo "  podman pod stop nanna-coder-pod"
echo "  podman pod start nanna-coder-pod"
echo "  podman pod rm nanna-coder-pod"