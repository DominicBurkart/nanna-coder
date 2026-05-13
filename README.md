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

### One-line install (Linux / macOS)

Bootstraps Podman, pulls the prebuilt harness + ollama containers, brings up the
Nanna pod, and pulls the Gemma 4 model. The script prints a clear notification
before each step that requires `sudo`.

```bash
curl -fsSL https://raw.githubusercontent.com/DominicBurkart/nanna-coder/main/scripts/install.sh | bash
```

Useful flags (pass after `bash -s --` for the curl form):

| Flag                | Purpose                                                         |
|---------------------|-----------------------------------------------------------------|
| `--skip-model-pull` | Bring up the pod without pulling the multi-GB Gemma 4 model.    |
| `--no-start`        | Install + pull images, but don't create the pod.                |
| `--model NAME`      | Pull a different Ollama model (default `gemma4:e4b`).           |
| `--registry URL`    | Use a different container registry.                             |
| `--yes`             | Skip the sudo confirmation prompts (for unattended installs).   |

After install:
```bash
podman pod ps                          # see the running pod
podman logs -f harness-service         # tail harness logs
curl http://localhost:11434/api/tags   # ollama API
```

### Build from source (developers)

Prerequisites: Nix with flakes enabled, optionally Cachix for faster builds.

```bash
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

From inside `nix develop`:

```bash
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
