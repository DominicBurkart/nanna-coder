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

# Optional: Configure Cachix for faster builds
nix run .#setup-cache

# Enter development environment
nix develop

# Build the project
nix build
```

### Using Cachix (Optional but Recommended)

Cachix provides unlimited binary cache storage for faster builds. See [CACHIX_SETUP.md](CACHIX_SETUP.md) for setup instructions.

**Benefits:**
- ✅ Unlimited storage (vs GitHub's 10GB limit)
- ✅ Persistent cache across all CI runs
- ✅ Shared cache between CI and local development
- ✅ Dramatically faster builds (download vs rebuild)
