# CLAUDE.md

All dev tools are provided by the Nix flake devShell. Prefix commands with
`nix develop --command` (or enter the shell with `nix develop`).

```bash
# Build
nix develop --command cargo build --workspace

# Test
nix develop --command cargo nextest run --workspace --all-features

# Lint
nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings

# Format check
nix develop --command cargo fmt --all -- --check

# Security
nix develop --command cargo deny check
```
