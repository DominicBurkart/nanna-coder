# Nanna Coder

A coding agent for coding agents. Designed for background agents to defer straightforward work to local models or by other model providers.

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) - System architecture and entity management
- [AGENTS.md](AGENTS.md) - Agent control flow and state machine
- [TESTING.md](TESTING.md) - Testing strategy and guidelines
- [CONTRIBUTING.md](CONTRIBUTING.md) - Contribution workflow
- [CACHIX_SETUP.md](CACHIX_SETUP.md) - Cachix maintainer/contributor setup
- [docs/cache-strategy.md](docs/cache-strategy.md) - Binary cache (Cachix) strategy and CI integration
- [docs/developer-experience.md](docs/developer-experience.md) - Dev-shell utilities and workflows
- [docs/agent-evaluation-patterns.md](docs/agent-evaluation-patterns.md) - Agent evaluation framework

## Technologies
- [Ollama](https://ollama.ai/)
- [Nix](https://nixos.org/)
- [Podman](https://podman.io/)
- [Rust](https://rust-lang.org)
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

### LLM Setup (Ollama)

The agent requires a running [Ollama](https://ollama.ai/) instance with a model installed:

```bash
# Install Ollama (see https://ollama.ai/download)
curl -fsSL https://ollama.ai/install.sh | sh

# Pull the default model
ollama pull qwen3:0.6b

# Verify Ollama is running
nix develop --command cargo run --bin harness -- health
```

### Running the Agent

```bash
# Enter development environment
nix develop

# Run the agent with tools enabled (recommended)
cargo run --bin harness -- agent --prompt "Your task description" --tools

# Run with a specific model and verbose output
cargo run --bin harness -- agent --prompt "Your task" --model qwen3:0.6b --tools --verbose
```

### Using as an MCP Server (Claude Code)

```bash
nix develop --command cargo build --release --bin harness && claude mcp add nanna -- "$(pwd)/target/release/harness" mcp-serve --model gemma4:e4b
```

### Using Cachix (Optional but Recommended)

Cachix provides a public binary cache for faster builds. No account needed to pull pre-built artifacts.

```bash
# Configure Cachix for faster builds (read-only access)
nix run .#setup-cache
```

See [CACHIX_SETUP.md](CACHIX_SETUP.md) for push access setup (maintainers only).
