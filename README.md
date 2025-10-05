# Nanna Coder

A highly opinionated local coding assistant (WIP).

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
