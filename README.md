# Nanna Coder

A highly opinionated local coding assistant (WIP).

## Project Status

```mermaid
gantt
    title Nanna Coder Development
    dateFormat YYYY-MM-DD
    axisFormat %b %d

    section Foundation
    Initial implementation           :done, 2025-09-20, 1d
    Nix flake & containers           :done, 2025-09-20, 2d
    CI pipeline                      :done, 2025-09-21, 3d
    Dependency auditing (cargo deny)  :done, 2025-10-04, 1d

    section Infrastructure
    Agent architecture               :done, 2025-10-04, 2d
    Cachix binary cache              :done, 2025-10-05, 2d
    Security hardening               :done, 2025-10-05, 1d
    Entity management system         :done, 2025-10-09, 1d
    CI formatting hooks              :done, 2025-10-25, 1d

    section Core Agent
    Git entities                     :done, 2025-12-31, 1d
    Container redesign (nix2container):done, 2026-01-01, 1d
    Error handling overhaul          :done, 2026-01-21, 1d
    Ollama chat API                  :done, 2026-02-18, 1d
    Agent MVP control loop           :done, 2026-02-25, 1d
    MVP tools & entities             :done, 2026-02-28, 1d
    LLM intelligence (phase 2)       :done, 2026-02-28, 1d
    E2E agent integration            :done, 2026-03-01, 1d

    section MCP & Evals
    MCP server infrastructure        :done, 2026-03-03, 1d
    Context entities                 :done, 2026-03-08, 8d
    Dev containers                   :done, 2026-03-15, 1d
    Shared model pool                :done, 2026-03-21, 1d
    Task lifecycle & dispatch        :done, 2026-03-21, 1d
    Repo onboarding                  :done, 2026-03-22, 1d
    Dev container implementation     :done, 2026-03-22, 1d
    GitHub PR tool                   :done, 2026-03-22, 1d
    Eval runner                      :done, 2026-03-22, 1d
    LLMs wired into agent loop       :done, 2026-03-24, 2d
    100% patch coverage CI           :done, 2026-03-25, 1d
    Code cleanup & test coverage     :done, 2026-03-28, 1d

    section Planned
    Expose Nanna via MCP             :active, mcp, after 2026-03-28, 1d
    Agentic eval suite               :eval, after mcp, 1d
    SWE-bench adapter                :after eval, 1d
    Workspace isolation              :after mcp, 1d
    AST & filesystem entities        :1d
    Testing & analysis entities      :1d
    Environment entities             :1d
    Sandbox telemetry entities       :1d
    Migrate to vLLM                  :1d
    Observability consolidation      :1d
    CI workflow consolidation        :1d
```

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

### Using Cachix (Optional but Recommended)

Cachix provides a public binary cache for faster builds. No account needed to pull pre-built artifacts.

```bash
# Configure Cachix for faster builds (read-only access)
nix run .#setup-cache
```

See [CACHIX_SETUP.md](CACHIX_SETUP.md) for push access setup (maintainers only).
