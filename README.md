# Nanna Coder

A coding agent for coding agents. Designed to let background agents delegate straightforward work to local models (or other providers).

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — system architecture and entity management
- [AGENTS.md](AGENTS.md) — instructions for agents building Nanna
- [TESTING.md](TESTING.md) — testing strategy and guidelines
- [CONTRIBUTING.md](CONTRIBUTING.md) — contributor workflow
- [CACHIX_SETUP.md](CACHIX_SETUP.md) — binary-cache setup (maintainers)

## Technologies

- [Ollama](https://ollama.ai/) — local LLM runtime
- [Nix](https://nixos.org/) — reproducible builds and dev shell
- [Podman](https://podman.io/) — rootless container runtime
- [Rust](https://www.rust-lang.org/) — harness implementation language
- [Cachix](https://cachix.org/) — binary cache for fast builds

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

The agent requires a running [Ollama](https://ollama.ai/) instance with a model installed. See the [Ollama install guide](https://ollama.ai/download) for platform-specific instructions.

```bash
# Install Ollama (Linux/macOS)
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
