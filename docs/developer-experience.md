# Developer Experience

Day-to-day developer commands and workflows. For test scope, see [../TESTING.md](../TESTING.md). For the cache strategy that speeds these commands up, see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md).

## Quick start

```bash
nix develop          # enter dev shell
dev-check            # format + lint + compile
container-dev        # start dev containers
dev-test watch       # TDD loop
```

## What `nix develop` sets up

### Pre-commit hooks

- `cargo fmt`
- `cargo clippy`
- `cargo nextest`
- `cargo audit`
- `cargo deny`
- `cargo tarpaulin`

### Aliases

| Group | Aliases |
|---|---|
| File navigation | `ll`, `la`, `l`, `..`, `...` |
| Cargo | `cb`, `ct`, `cc`, `cf`, `cn` |
| Git | `gs`, `ga`, `gc`, `gp`, `gl`, `gd` |
| Nix | `nb`, `nr`, `nd`, `nf` |
| Project | `dt`, `db`, `dc` |

### Tools (auto-installed)

- `cargo-watch`, `cargo-nextest`, `cargo-audit`, `cargo-deny`, `cargo-tarpaulin`, `cargo-expand`, `cargo-udeps`, `cargo-machete`, `cargo-outdated`.
- Container tools: Podman (preferred), Buildah, Skopeo.

## Core commands

| Command | Purpose |
|---|---|
| `dev-check` | format, clippy, compile check |
| `dev-build` | incremental build via `cargo-watch` |
| `dev-test` | full test suite + clippy + format + audit + deny |
| `dev-test unit` | unit tests only |
| `dev-test integration` | integration tests only |
| `dev-test watch` | continuous TDD |
| `dev-clean` | clean cargo + container artifacts (24h prune) |
| `dev-reset` | full env rebuild (updates flake inputs) |
| `container-dev` | start dev containers (compose-based) |
| `container-test` | run integration tests in containers |
| `container-stop` | stop all dev containers |
| `container-logs` | tail container logs |
| `cache-warm` | pre-warm common builds |

## Daily loop

```bash
nix develop
dev-check                # quick health check
container-dev            # only if needed
# ... edit ...
dev-check
dev-test unit
git add . && git commit  # pre-commit hooks run
```

## Testing patterns

```bash
# Unit (fast)
dev-test unit
dev-test watch           # TDD loop

# Integration
dev-test integration
container-test

# Coverage / benchmarks
cargo tarpaulin --skip-clean --ignore-tests
cargo bench
```

## Debugging

```bash
# Macro expansion
cargo expand
cargo expand <module>

# Dependency hygiene
cargo udeps
cargo machete
cargo outdated
```

## Container workflows

```bash
# Local container build + test
nix build .#qwen3-container
container-test
container-logs

# Multiple models
nix build .#llama3-container
nix build .#mistral-container
```

## Performance tips

- Run `cache-warm` before extended dev sessions; `setup-cache` if Cachix isn't configured yet (see [CACHE_STRATEGY.md](./CACHE_STRATEGY.md)).
- `dev-build` (cargo-watch) for fast incremental feedback.
- `cargo nextest run --test-threads=N` to tune parallelism.
- `cargo nextest run -p harness <pattern>` to run a focused subset.

## Troubleshooting

**Build failure**

```bash
cargo fmt --all
cargo clippy --workspace --fix
dev-clean && dev-build
```

**Container failure**

```bash
podman --version || docker --version
container-stop && container-dev
container-logs
```

**Cache miss**

```bash
nix run .#cache-analytics
nix run .#setup-cache
dev-reset
```

**Environment**

```bash
echo $RUST_TOOLCHAIN_PATH
echo $NIX_PATH
which cargo-watch
which cargo-nextest
```

**Container introspection**

```bash
podman ps -a
podman inspect <name>
podman exec -it <name> bash
```

## IDE integration

### VS Code

Recommended extensions: rust-analyzer, CodeLLDB, Better TOML, Nix IDE.

```json
{
  "rust-analyzer.server.path": "rust-analyzer",
  "rust-analyzer.cargo.features": "all",
  "rust-analyzer.checkOnSave.command": "clippy",
  "rust-analyzer.cargo.buildScripts.enable": true
}
```

### Neovim

Recommended plugins: nvim-lspconfig, rust-tools.nvim, nvim-cmp, telescope.nvim.

## Best practices

- Run `dev-check` before committing; pre-commit hooks enforce the gate anyway.
- Use `dev-test watch` for TDD.
- Keep deps current: `cargo outdated`, `cargo audit`.
- Clean containers regularly with `dev-clean`.
- Test with multiple model configurations when changing model code.
