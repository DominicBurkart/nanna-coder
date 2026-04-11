# AGENTS.md

Instructions for agents building nanna. See [ARCHITECTURE.md](ARCHITECTURE.md) for system design and control flow diagrams.

## Build & Test Commands

All dev tools are provided by the Nix flake devShell. See [CLAUDE.md](CLAUDE.md) for the full command reference.

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

## Key Source Locations

- **Harness entrypoint**: [`harness/src/main.rs`](harness/src/main.rs)
- **Agent loop**: [`harness/src/agent/`](harness/src/agent/)
- **Entity system**: [`harness/src/entities/`](harness/src/entities/)
- **Tool registry**: [`harness/src/tools.rs`](harness/src/tools.rs)
- **MCP server**: [`harness/src/mcp/`](harness/src/mcp/)

## Testing

See [TESTING.md](TESTING.md) for the full test strategy. Run all tests with:

```bash
./tests/run-all-tests.sh
```
