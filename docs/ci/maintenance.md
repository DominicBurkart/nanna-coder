# CI Maintenance

Routine care and feeding of the CI/CD system. Each entry has a cadence, an
owner-of-last-resort, and a concrete command.

## Cadences

### Weekly

- **Check `ci-metrics.yml` artifacts on recent `main` runs** (once the
  workflow proposed in [`proposed-ci-metrics-workflow.md`](proposed-ci-metrics-workflow.md)
  is installed). Per-job wall times are attached as `ci-metrics-<run_id>`
  artifacts (90d retention). A job that has drifted over its target from
  [`performance.md`](performance.md#wall-time-budget) is a signal to
  investigate.
- **Review `ci-integration.yml` cron run** (scheduled `0 6 * * 1`, Mondays
  06:00 UTC). This is the canary for CI infra drift: nix2container revisions,
  container loading, cold-cache path. Failures here rarely fail PRs but
  predict PR failures within days.
- **Skim `install-nightly.yml` runs** for the previous week. Persistent
  model-pull failures indicate registry issues or disk pressure that will
  eventually hit dev flows.
- **Check Cachix dashboard** for hit rate and storage. See
  [`docs/binary-cache-strategy.md`](../binary-cache-strategy.md) for baseline
  numbers.

### Monthly

- **Actionlint / yamllint the workflow tree.** These aren't currently in the
  CI pipeline (only in the local pre-commit hook), so a monthly manual pass
  catches drift:

  ```bash
  actionlint .github/workflows/*.yml
  yamllint .github/workflows/*.yml .github/workflows/*.yaml
  ```

- **Review pinned action versions.** Most actions are pinned by major
  (`actions/checkout@v4`, `codecov/codecov-action@v5`,
  `cachix/cachix-action@v15`). A few track `@main`
  (`DeterminateSystems/nix-installer-action@main`, `dtolnay/rust-toolchain@master`,
  `aquasecurity/trivy-action@master`) — audit for known regressions.
- **Look for stale `TODO` / `# Temporarily ignored` comments in
  `.github/workflows/`, `codecov.yml`, and `deny.toml`.** Two currently live
  ones to track:
  - `ci.yml` `security` step disables `cargo audit` / `cargo deny` for CVSS
    4.0 compatibility.
  - `deny.toml` ignores `RUSTSEC-2025-0134` (rustls-pemfile unmaintained,
    tracked in issue #40).

### Quarterly

- **`flake.lock` refresh.** Run:

  ```bash
  nix flake update
  nix flake check
  nix develop --command cargo nextest run --workspace --lib --all-features
  ```

  Land as a standalone PR with the diff of `flake.lock` and any downstream
  fixes required by the update.
- **Rust toolchain review.** No `rust-toolchain.toml` at the workspace root,
  so the toolchain is what Nix provides on Linux and whatever
  `dtolnay/rust-toolchain@master` resolves to on macOS/Windows on the day.
  Confirm parity by running a `--version` comparison in a scratch job.
- **`cargo audit` / `cargo deny` re-enablement check.** These are disabled
  in `ci.yml` for CVSS 4.0 reasons; recheck the upstream advisories tooling
  quarterly and re-enable when compatible.

### Ad-hoc (on release)

- Confirm `release` matrix produced all four `harness-<target>` assets on
  the GitHub release page.
- Confirm `build-containers` pushed both `harness:<sha>` and `harness:latest`
  (same for `ollama`) to `ghcr.io/${OWNER,,}/`.
- Confirm Trivy report was uploaded to code-scanning.

## Task recipes

### Add a new job to `ci.yml`

1. Add the job under `jobs:`.
2. Add the job name to `jobs.all-checks.needs`.
3. Push. If step 2 is missed, `all-checks` will fail with a helpful
   `::error::Jobs missing from 'all-checks'.needs:` message — fix by
   completing step 2.
4. Update the workflow inventory in
   [`docs/ci/architecture.md`](architecture.md).

### Remove a job from `ci.yml`

1. Delete the job block.
2. Remove the entry from `jobs.all-checks.needs`.
3. Update `docs/ci/architecture.md`.
4. Search for the job name across the repo (`docs/`, `README.md`) for
   dangling references.

### Add a new target to `build-matrix`

1. Add the `(target, runner, cross)` triple to `strategy.matrix.include`.
2. Confirm nix flake supports `.#packages.<target>.nanna-coder` for Linux
   targets; add to `flake.nix` if not.
3. Add a matching entry in the `release` matrix so release assets stay in
   sync.
4. Update the `build-matrix` table in
   [`docs/ci/architecture.md`](architecture.md).

### Bump `tarpaulin --timeout`

Do this only after profiling. Timeout is currently `1800` (30 min). Steps:

1. Reproduce locally: `cargo tarpaulin --workspace --all-features
   --skip-clean --out Lcov --output-dir . --timeout 1800`.
2. Identify the slow test(s) from tarpaulin's log.
3. Prefer splitting or shortening the slow test to raising the ceiling.
4. If a raise is unavoidable, document the reason in a comment right above
   the `--timeout` flag in `ci.yml`.

### Rotate `CACHIX_AUTH` / `CODECOV_TOKEN`

Both are repo secrets. To rotate:

1. Generate a new token upstream (Cachix or Codecov).
2. Update the GitHub repo secret (`Settings → Secrets → Actions`).
3. Trigger a `workflow_dispatch` of `cache-warming` (Cachix) or manually
   re-run a `security` matrix cell (Codecov) to confirm the new token works.
4. Revoke the old token upstream.

Fork PRs correctly do not need these secrets: Cachix pushes are suppressed
via `skipPush`, and Codecov `security` cell runs but `token:` is ignored on
forked PRs by codecov-action.

### Update pinned action versions

Search-and-replace in `.github/workflows/`, e.g.:

```bash
grep -rn 'codecov/codecov-action@v5' .github/workflows/
```

Bump, run `actionlint` locally, push, watch the run. Roll back on failure.

### Regenerate badges

`badges.yaml` runs automatically on every `main` push. If the badges look
stale, trigger it manually via `workflow_dispatch` in the Actions UI.

## What NOT to do

Per [`AGENTS.md`](../../AGENTS.md):

- Do not lower `target:` in `codecov.yml`.
- Do not add to `codecov.yml` `ignore:`.
- Do not remove or replace the numeric target with `auto`.
- Do not edit `codecov-guard.yml`, `CODEOWNERS`, or `.github/workflows/**`
  to bypass the guard.
- Do not use `--no-verify` on commits. If the pre-commit hook fails, fix
  the underlying issue and create a new commit.

## See also

- [`architecture.md`](architecture.md) — what you are maintaining.
- [`troubleshooting.md`](troubleshooting.md) — when maintenance surfaces
  a real problem.
- [`performance.md`](performance.md) — cache and wall-time hygiene.
- [`security.md`](security.md) — secrets, permissions, trust boundary.
