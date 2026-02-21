# Nanna Coder

[![codecov](https://codecov.io/gh/DominicBurkart/nanna-coder/branch/main/graph/badge.svg)](https://codecov.io/gh/DominicBurkart/nanna-coder)
[![crates.io](https://img.shields.io/crates/v/model.svg)](https://crates.io/crates/model)
[![lines of code](https://raw.githubusercontent.com/DominicBurkart/nanna-coder/main/development_metadata/badges/lines_of_code.svg)](https://github.com/DominicBurkart/nanna-coder)
[![last commit](https://img.shields.io/github/last-commit/DominicBurkart/nanna-coder)](https://github.com/DominicBurkart/nanna-coder/commits/main)
[![contributors](https://raw.githubusercontent.com/DominicBurkart/nanna-coder/main/development_metadata/badges/contributors.svg)](https://github.com/DominicBurkart/nanna-coder/graphs/contributors)

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

### Using Cachix (Optional but Recommended)

Cachix provides a public binary cache for faster builds. No account needed to pull pre-built artifacts.

```bash
# Configure Cachix for faster builds (read-only access)
nix run .#setup-cache
```

See [CACHIX_SETUP.md](CACHIX_SETUP.md) for push access setup (maintainers only).
