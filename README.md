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
git clone https://github.com/DominicBurkart/nanna-coder.git
cd nanna-coder
nix develop
nix build
```

### LLM Setup (Ollama)

The agent requires a running [Ollama](https://ollama.ai/) instance with a model installed:

```bash
# Install Ollama (see https://ollama.ai/download)
curl -fsSL https://ollama.ai/install.sh | sh

# Pull the default model and start the server
ollama pull qwen3:0.6b
ollama serve
```

### Running the Agent

```bash
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

### Running Eval Tests

With Ollama running and a model pulled (default `gemma4:e4b`, override via `NANNA_EVAL_MODEL`):

```bash
nix develop --command cargo nextest run \
  --workspace --features eval-runner \
  --run-ignored ignored-only \
  -E 'test(eval_runner)' \
  --test-threads=1
```

### Cachix (Optional)

Cachix provides a public binary cache for faster builds. No account is needed for read-only access:

```bash
nix run .#setup-cache
```

See [CACHIX_SETUP.md](CACHIX_SETUP.md) for push-access setup (maintainers only).
