# Nanna Coder

A highly opinionated local coding assistant (WIP).

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) - System architecture and entity management
- [AGENTS.md](AGENTS.md) - Agent control loop and implementation details
- [TESTING.md](TESTING.md) - Testing strategy and guidelines

## Technologies
- [Ollama](https://ollama.ai/)
- [Nix](https://nixos.org/)
- [Podman](https://podman.io/)
- [Rust](https://rustlang.org)
- [Cachix](https://cachix.org/) - Binary cache for fast builds

## Quick Start

### Prerequisites
- Nix with flakes enabled
- (Optional) Cachix account for faster builds

### Setup

```bash
# Clone the repository
git clone https://github.com/DominicBurkart/nanna-coder.git
cd nanna-coder

# Enter development environment
nix develop

# Build the project
nix build
```

### Ollama Setup

Nanna Coder requires [Ollama](https://ollama.ai/download) for local LLM inference. After installing Ollama:

```bash
# Pull the default eval model
ollama pull qwen3:0.6b

# Start the Ollama server (must be running for agent commands and evals)
ollama serve
```

### Running Eval Tests

With Ollama running and `qwen3:0.6b` pulled:

```bash
cargo test --test eval_runner_tests -- --ignored
```

Or using the Nix container (no local Ollama install needed):

```bash
nix run .#qwen3-container.copyToDockerDaemon
docker run -d --name ollama-qwen3 -p 11434:11434 nanna-coder-ollama-qwen3:latest
until curl -sf http://localhost:11434/api/tags | grep -q qwen3; do sleep 2; done
cargo test --test eval_runner_tests -- --ignored
docker rm -f ollama-qwen3
```

### Using Cachix (Optional but Recommended)

Cachix provides a public binary cache for faster builds. No account needed to pull pre-built artifacts.

```bash
# Configure Cachix for faster builds (read-only access)
nix run .#setup-cache
```

See [CACHIX_SETUP.md](CACHIX_SETUP.md) for push access setup (maintainers only).
