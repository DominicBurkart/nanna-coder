# Nanna Coder

A coding agent for coding agents. Designed to let background agents delegate straightforward work to local models (or other providers).

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — system architecture and entity management
- [AGENTS.md](AGENTS.md) — instructions for agents building Nanna
- [TESTING.md](TESTING.md) — testing strategy and guidelines
- [CONTRIBUTING.md](CONTRIBUTING.md) — contributor workflow
- [CACHIX_SETUP.md](CACHIX_SETUP.md) — binary-cache setup (maintainers)

## Technologies

- [Ollama](https://ollama.ai/)
- [Nix](https://nixos.org/)
- [Podman](https://podman.io/)
- [Rust](https://www.rust-lang.org/)
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

The agent requires a running [Ollama](https://ollama.ai/) instance with a model installed:

```bash
# Install Ollama (see https://ollama.ai/download)
curl -fsSL https://ollama.ai/install.sh | sh

# Pull the default model
ollama pull qwen3:0.6b

# Start the Ollama server (must be running for agent commands and evals)
ollama serve
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

### Running Tests and Evals

See [TESTING.md](TESTING.md) for the full test topology and commands. Evals require Ollama and a pulled model (default `gemma4:e4b`, override via `NANNA_EVAL_MODEL`):

```bash
nix develop --command cargo nextest run \
  --workspace --features eval-runner \
  --run-ignored ignored-only \
  -E 'test(eval_runner)' \
  --test-threads=1
```

### Using Cachix (Optional but Recommended)

Run `nix run .#setup-cache` for read-only access to pre-built artifacts. See [CACHIX_SETUP.md](CACHIX_SETUP.md) for details and maintainer push setup.
