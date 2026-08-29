# CI Onboarding

Read this on your first day as a nanna-coder CI maintainer. Assumes you
already know Rust and GitHub Actions at a working level.

## In one paragraph

nanna-coder ships a Rust workspace (harness + model + framing + image-builder
+ eval-runner) as prebuilt binaries and container images. CI is Nix-first on
Linux and native on macOS/Windows. The primary pipeline
([`ci.yml`](../../.github/workflows/ci.yml)) is a fan-out matrix of tests,
lints, coverage, cross-target builds, and container publishes, gated by a
single `all-checks` job. Coverage is enforced at 100% on patch by codecov
with a policy guard ([`codecov-guard.yml`](../../.github/workflows/codecov-guard.yml))
that rejects silent relaxations. Cache is centralized in Cachix
(`nanna-coder`), warmed on `main` pushes. The full architecture is in
[`architecture.md`](architecture.md).

## First hour

1. Read [`ARCHITECTURE.md`](../../ARCHITECTURE.md) — the runtime the CI is
   built to ship.
2. Read [`AGENTS.md`](../../AGENTS.md) — the invariants you must not touch.
3. Read [`architecture.md`](architecture.md) — this doc's counterpart for the
   pipeline itself.
4. Skim every file in [`.github/workflows/`](../../.github/workflows/). You
   don't need to memorize them; know their names and one-line purposes (see
   the inventory table in `architecture.md`).
5. Skim [`TESTING.md`](../../TESTING.md).

## First day: reproduce a CI run locally

You will be more effective if you can reproduce failing CI jobs on your own
machine.

### Prerequisites

- Nix installed (multi-user; use
  [`DeterminateSystems/nix-installer`](https://github.com/DeterminateSystems/nix-installer)
  to match CI exactly).
- Docker daemon running (for container tests).
- `direnv` optional but recommended — the repo ships `.envrc`.

### Enter the dev shell

```bash
cd nanna-coder
nix develop
```

This gives you the exact Rust toolchain, `cargo-nextest`, `cargo-tarpaulin`,
`cargo-audit`, `cargo-deny`, and the flake apps CI uses.

### Reproduce each test-matrix cell

| CI cell | Local command |
|---------|---------------|
| `unit` | `cargo nextest run --workspace --lib --all-features` |
| `lint` | `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo fmt --all -- --check && cargo doc --no-deps --workspace` |
| `security` (coverage) | `cargo tarpaulin --workspace --all-features --skip-clean --out Lcov --output-dir . --timeout 1800` |
| `integration` | `cargo nextest run --workspace --test '*' --all-features` |
| `integration-container` | Prebuild the ollama image (below), then run the same as `integration` |

Prebuild the ollama image before running container tests:

```bash
nix build .#ollamaImage --print-build-logs
nix run .#ollamaImage.copyToDockerDaemon
docker image inspect nanna-coder-ollama:latest
```

### Reproduce a build-matrix cell

Linux native:

```bash
nix build .#nanna-coder
./result/bin/nanna --help
```

Linux cross to aarch64:

```bash
nix build .#packages.aarch64-linux.nanna-coder
```

macOS native:

```bash
cargo build --release
./target/release/nanna --help
```

macOS cross to aarch64:

```bash
rustup target add aarch64-apple-darwin
cargo build --release --target aarch64-apple-darwin
```

### Reproduce a container build

```bash
nix build .#harnessImage --print-build-logs
nix run .#harnessImage.copyToDockerDaemon
docker image inspect nanna-coder-harness:latest
docker run --rm nanna-coder-harness:latest --help
```

## First week: know the invariants

Memorize these — they are enforced automatically:

- **`all-checks` covers every non-gate job in `ci.yml`.** New job → add to
  `all-checks.needs`.
- **`codecov.yml` `patch.default.target: 100%` is a floor, not a target.**
  Guarded by `codecov-guard.yml`.
- **`codecov.yml` `ignore:` cannot grow.** Same guard.
- **No `--no-verify` commits.** Pre-commit hook is comprehensive; fix real
  issues, create new commits.
- **Release artifact filenames stay `harness-<target>`.** Even though the
  binary is `nanna` internally.
- **Fork PRs must not require secrets.** `skipPush` on Cachix, Codecov token
  auto-ignored on forks.

## First month: know the failure modes

Read [`troubleshooting.md`](troubleshooting.md) end-to-end. It's short
and every entry is drawn from a real incident.

## Where things live

| Concern | Location |
|---------|----------|
| Workflows | [`.github/workflows/`](../../.github/workflows/) |
| Codecov config | [`codecov.yml`](../../codecov.yml) |
| Cargo deny / audit config | [`deny.toml`](../../deny.toml) |
| Markdown lint config | [`.rumdl.toml`](../../.rumdl.toml) |
| Tarpaulin config | [`tarpaulin.toml`](../../tarpaulin.toml) |
| Nix flake | [`flake.nix`](../../flake.nix), [`flake.lock`](../../flake.lock) |
| Nix container config | [`nix/containers.nix`](../../nix/), [`nix/container-config.nix`](../../nix/) |
| Pre-commit hooks | [`.cargo-husky/hooks/`](../../.cargo-husky/hooks/) |
| CI CLI helpers | [`scripts/`](../../scripts/), `nix run .#<app>` |
| CI docs (this dir) | [`docs/ci/`](.) |
| Broader CI notes | [`docs/ci-cd-pipeline.md`](../ci-cd-pipeline.md), [`docs/CACHE_STRATEGY.md`](../CACHE_STRATEGY.md) |

## Who to ask

- Repo owner: `@DominicBurkart`.
- Cachix / cache issues: check
  [`docs/cachix-migration.md`](../cachix-migration.md) and
  [`CACHIX_SETUP.md`](../../CACHIX_SETUP.md) first.
- Codecov guard policy: see [`AGENTS.md`](../../AGENTS.md) — non-negotiable
  without admin sign-off.

## Your first PR

A safe onboarding change:

1. Find one factual detail in this document or `architecture.md` that has
   drifted from the current state of the workflows.
2. Fix it in a docs-only PR.
3. Cite the workflow file + line number in the PR description.

Reviewer will confirm the fix matches reality, merge, and welcome you.

## See also

- [`architecture.md`](architecture.md)
- [`troubleshooting.md`](troubleshooting.md)
- [`maintenance.md`](maintenance.md)
- [`performance.md`](performance.md)
- [`security.md`](security.md)
