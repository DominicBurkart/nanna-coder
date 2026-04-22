# CI Architecture

This document describes the CI surface that ships in `.github/workflows/`. It is the canonical, per-workflow reference for Nanna Coder's pipelines and enumerates every workflow file checked in to the repository.

The scripts in `scripts/check-ci-doc-coverage.sh` assert that every workflow filename appears as a dedicated `##` heading below (or is explicitly excluded via an `OMITTED:` marker). Any new workflow **must** be given a heading here before it can merge.

For a narrative, higher-level view of the pipeline that predates this tree, see [../ci-cd-pipeline.md](../ci-cd-pipeline.md). For binary-cache details, see [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md), [../binary-cache-strategy.md](../binary-cache-strategy.md), [../cache-migration-guide.md](../cache-migration-guide.md), and [../cachix-migration.md](../cachix-migration.md).

## Workflow inventory

| File | Trigger(s) | Jobs | Matrix dimensions | Secrets |
|---|---|---|---|---|
| `ci.yml` | `push` (main, develop), `pull_request` (main), `release` | `test-matrix`, `build-matrix`, `build-containers`, `security-scan`, `cache-maintenance`, `release`, `all-checks` _(`docs-check` pending — see [#254 follow-up](https://github.com/DominicBurkart/nanna-coder/pull/254))_ | OS x test-type; target x runner; arch x image | `CACHIX_AUTH` (repo-level secret), `CODECOV_TOKEN`, `GITHUB_TOKEN` |
| `cache-warming.yml` | `push` (main, path-filtered), `workflow_dispatch` | `warm-dependencies`, `warm-containers`, `warm-cross-platform` (currently disabled via `if: false`), `summary` | `image` (harness, ollama); `target` x `runner` (disabled) | `CACHIX_AUTH` |
| `eval.yml` | `workflow_dispatch` | `eval` | none | `CACHIX_AUTH`, `GITHUB_TOKEN` |
| `badges.yaml` | `push` (main) | `update-badges` | none | `GITHUB_TOKEN` |

`CACHIX_AUTH` and `CODECOV_TOKEN` are repo-level secrets (not scoped to any GitHub environment). `GITHUB_TOKEN` is provided automatically by GitHub Actions. The `test-matrix` job declares `environment: codecov` solely to satisfy Codecov's OIDC requirements; only `CODECOV_TOKEN` is consumed there. `CACHIX_AUTH` is required for any job that pushes to Cachix; Cachix pulls are unauthenticated. See [../../CACHIX_SETUP.md](../../CACHIX_SETUP.md).

## ci.yml

The primary pipeline. Triggers on every push to `main`/`develop`, every PR targeting `main`, and every published release. See the live file at [.github/workflows/ci.yml](../../.github/workflows/ci.yml).

### Jobs

- **`test-matrix`** — runs on `ubuntu-latest`, `macos-latest`, `windows-latest` across test types `unit`, `lint`, `security`, `integration`, `integration-container`. Linux uses `nix develop --command ...`; macOS and Windows fall back to `dtolnay/rust-toolchain@master` with cargo-installed tools. The `security` variant runs `cargo tarpaulin` and uploads coverage to Codecov (see [../../codecov.yml](../../codecov.yml)). `cargo audit` and `cargo deny` are intentionally skipped with inline comments citing CVSS 4.0 incompatibility.
- **`build-matrix`** — runs on `needs: test-matrix`. Cross-builds `nanna-coder` for `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`. Linux uses `nix build`; macOS uses `cargo build --release`. Outputs are collected under `artifacts/` and uploaded via `actions/upload-artifact@v4`.
- **`build-containers`** — runs on `needs: test-matrix`. Builds `harnessImage` and `ollamaImage` for `x86_64` with `nix2container`'s `copyToDockerDaemon`, tags with the commit SHA and `latest`, and pushes to `ghcr.io/<repo lowercase>/<image>` on non-PR events. Requires `packages: write` permission and logs in with `GITHUB_TOKEN`.
- **`security-scan`** — runs on `needs: build-containers`, only on non-PR events. Runs `aquasecurity/trivy-action` against the pushed harness image and uploads SARIF via `github/codeql-action/upload-sarif@v3`. Requires `security-events: write`.
- **`cache-maintenance`** — runs on `needs: [test-matrix, build-matrix, build-containers]`, gated to `push` events on `refs/heads/main`. Invokes `nix run .#cache-analytics` (defined in [../../nix/cache.nix](../../nix/cache.nix)) and appends the report to `$GITHUB_STEP_SUMMARY`.
- **`release`** — runs on `needs: [test-matrix, build-matrix, build-containers]`, gated to `release` events. Rebuilds for each target and attaches `harness-<target>` to the GitHub release via `actions/upload-release-asset@v1`.
- **`docs-check`** _(pending — not yet in `ci.yml`; see [#254 follow-up](https://github.com/DominicBurkart/nanna-coder/pull/254))_ — will run `scripts/check-docs-links.sh` and `scripts/check-ci-doc-coverage.sh` on `ubuntu-latest` so this doc tree cannot silently drift from the workflows it describes. The wiring commit was blocked on `workflows` permission scope and has not yet landed.
- **`all-checks`** — the gate job. `if: always()`, `needs:` every other job. Contains a `yq`-backed self-check that asserts every declared job appears in its own `needs:` list (see [maintenance.md](maintenance.md) for the exact logic), then fails on any dependency `failure`/`cancelled`.

### Job graph

```mermaid
graph TD
    test-matrix --> build-matrix
    test-matrix --> build-containers
    build-containers --> security-scan
    test-matrix --> cache-maintenance
    build-matrix --> cache-maintenance
    build-containers --> cache-maintenance
    test-matrix --> release
    build-matrix --> release
    build-containers --> release
    test-matrix --> all-checks
    build-matrix --> all-checks
    build-containers --> all-checks
    security-scan --> all-checks
    cache-maintenance --> all-checks
    release --> all-checks
```

_Note: a `docs-check --> all-checks` edge is planned but not yet wired (see [#254 follow-up](https://github.com/DominicBurkart/nanna-coder/pull/254))._

## cache-warming.yml

Pre-populates the Cachix binary cache on every push to `main` that touches `flake.lock`, any `Cargo.toml`, or the workflow itself. Also available manually via `workflow_dispatch` with a `force_rebuild` boolean. See [.github/workflows/cache-warming.yml](../../.github/workflows/cache-warming.yml).

### Jobs

- **`warm-dependencies`** — builds the Rust toolchain and cargo dependencies (`nix develop --command cargo fetch --locked`, then `cargo build --workspace --all-features --release`). Emits a `deps-key` output of the form `cachix-v1-deps-<flake hash>-<cargo hash>`. The hash algorithm is the first 16 chars of a `sha256sum` of each lockfile.
- **`warm-containers`** — parallel matrix over `image: [harness, ollama]`. Runs `nix build .#harnessImage` / `nix build .#ollamaImage` with `--no-link` so the outputs land only in the Nix store (and from there, Cachix).
- **`warm-cross-platform`** — currently `if: false`. Preserved for when cross-compilation stabilizes. Would target `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`.
- **`summary`** — `if: always()`, depends on the first two jobs, writes a markdown table to `$GITHUB_STEP_SUMMARY`.

## eval.yml

Manually triggered eval suite. `workflow_dispatch` only — it is never a PR gate. Inputs: `pr_number` (optional, posts a comment), `model` (choice: `qwen3:0.6b` or `llama3.1:8b`), `case_filter` (optional nextest filter appended to `test(eval)`). See [.github/workflows/eval.yml](../../.github/workflows/eval.yml).

### Jobs

- **`eval`** — single job. Installs Nix + Cachix, builds the workspace with `--release`, installs Ollama via upstream installer, launches `ollama serve` in the background, waits up to 30s for readiness, pulls the selected model, runs `cargo nextest run ... -E "test(eval) [& <filter>]"`, captures output to `eval-results.md`, uploads it as `eval-results` artifact, and optionally posts it via `peter-evans/create-or-update-comment@v4` when `pr_number` is set.

## badges.yaml

Read-only badge refresh on every push to `main`. See [.github/workflows/badges.yaml](../../.github/workflows/badges.yaml).

### Jobs

- **`update-badges`** — computes two integers: lines of Rust code (via `git ls-files -z '*.rs' | xargs -0 cat | wc -l`) and distinct non-test contributors (via `git log --all --format="%ae"`). Fetches SVGs from `img.shields.io` into `development_metadata/badges/` and commits them as `github-actions[bot]`. `git push || true` is used deliberately so a failed push does not fail the workflow.

## Cross-references to existing docs

- Pipeline narrative predating this tree: [../ci-cd-pipeline.md](../ci-cd-pipeline.md)
- Cache strategy (operational): [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md)
- Cache strategy (high-level): [../binary-cache-strategy.md](../binary-cache-strategy.md)
- Historical migration notes: [../cache-migration-guide.md](../cache-migration-guide.md), [../cachix-migration.md](../cachix-migration.md)
- Developer workflow and shortcuts: [../developer-experience.md](../developer-experience.md)
- Cachix credentials and push setup: [../../CACHIX_SETUP.md](../../CACHIX_SETUP.md)
- System architecture: [../../ARCHITECTURE.md](../../ARCHITECTURE.md)
- Testing philosophy and gates: [../../TESTING.md](../../TESTING.md)
- Contributor workflow: [../../CONTRIBUTING.md](../../CONTRIBUTING.md)
- Agent operating instructions: [../../AGENTS.md](../../AGENTS.md)

## Related documents

- [troubleshooting.md](troubleshooting.md) — failure triage for each job above
- [maintenance.md](maintenance.md) — upgrade, rotation, and gate-ownership procedures
- [onboarding.md](onboarding.md) — reading order for a new contributor
- [performance.md](performance.md) — where time is spent and why
- [security.md](security.md) — secrets, permissions, and supply-chain posture
