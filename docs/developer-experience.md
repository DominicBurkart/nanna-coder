# Developer Experience Guide

Quick reference for the dev-shell utilities. The shell prints the same list at
startup (see `nix/dev-shell.nix:75`); this doc is for reading without entering
the shell.

## Quick start

```bash
nix develop          # enter shell
dev-check            # format + clippy + compile check
container-dev        # start dev containers
dev-test watch       # tests in watch mode
```

## Auto-installed tooling

The Nix dev-shell provides:

- **Rust workflow**: `cargo-watch`, `cargo-nextest`, `cargo-audit`,
  `cargo-deny`, `cargo-tarpaulin`, `cargo-expand`, `cargo-udeps`,
  `cargo-machete`, `cargo-outdated`.
- **Containers**: Podman (preferred), Buildah, Skopeo.
- **Pre-commit hook**: cargo fmt, clippy, nextest, audit, deny, tarpaulin
  (configured via `.cargo-husky`).

## `dev-*` commands (defined in `nix/scripts.nix`)

| Command | Purpose |
|---------|---------|
| `dev-check` | fmt check, clippy, compile — fast pre-commit validation |
| `dev-build` | Incremental build via `cargo-watch` |
| `dev-test [unit\|integration\|watch]` | Test runner; bare = full suite + lint/audit/deny |
| `dev-clean` | Clean cargo target, prune containers (>24 h old) |
| `dev-reset` | Full reset: clean, update flake, rebuild dev shell, warm caches |
| `container-dev` | Start dev containers (podman or docker compose) |
| `container-test` | Load test containers + run integration tests |
| `container-stop` | Stop all dev containers |
| `container-logs` | Tail container logs |
| `cache-warm` | Pre-warm common builds (parallel) |

Aliases set in the shell: `dt` → `dev-test`, `db` → `dev-build`,
`dc` → `dev-check`, plus standard cargo (`cb`/`ct`/`cc`/`cf`/`cn`),
git (`gs`/`ga`/`gc`/`gp`/`gl`/`gd`), and nix (`nb`/`nr`/`nd`/`nf`)
shortcuts. See `nix/dev-shell.nix:185` for the full list.

## Daily workflow

```bash
nix develop && dev-check        # session start
container-dev                   # if integration work needed
# … edit …
dev-check && dev-test unit      # tight loop
dev-test                        # full pre-commit validation
git commit                      # husky hook re-runs the checks
```

## Cache management

See [CACHE_STRATEGY.md](./CACHE_STRATEGY.md) for the full strategy. The
in-shell entry points are:

```bash
nix run .#setup-cache       # one-time Cachix substituter setup
cache-warm                  # pre-build common derivations
nix run .#cache-analytics   # store size + bottleneck report
```

## Common debugging

| Symptom | Try |
|---------|-----|
| Build fails after dependency change | `dev-clean && dev-build` |
| Container runtime not found | `podman --version`, then `container-stop && container-dev` |
| Cache misses | `nix run .#cache-analytics`, then `nix run .#setup-cache` |
| Macro confusion | `cargo expand [module]` |
| Stale dependencies | `cargo udeps`, `cargo machete`, `cargo outdated` |

## IDE setup

Recommended LSP: `rust-analyzer` with `checkOnSave.command = "clippy"` and
`cargo.features = "all"`. The dev-shell exposes the toolchain at
`$RUST_TOOLCHAIN_PATH`; point your editor's rust-analyzer binary there if it
doesn't pick up the flake automatically.
