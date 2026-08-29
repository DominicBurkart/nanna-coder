# CI infrastructure integration tests (issue #8)

The workflow is implemented at
[`.github/workflows/ci-integration.yml`](../../.github/workflows/ci-integration.yml).
This document explains what each job exists to assert; the YAML itself is
the source of truth for how it does so.

## Why a separate workflow file

`ci.yml` has an `all-checks` gate that explicitly enumerates every job it
depends on. Adding CI-infrastructure self-tests to `ci.yml` would make
every unrelated product PR pay their cost and would couple their failure
modes to the product gate. A dedicated workflow keeps the signal separate
and cheap.

## Triggers

- `pull_request` with `paths:` filter for `.github/workflows/**`,
  `.github/actions/**`, `flake.nix`, `flake.lock`, `nix/**`, `scripts/**`,
  and the workflow file itself, so jobs only run when CI itself is being
  touched.
- `schedule: cron: "0 6 * * 1"` — weekly Monday 06:00 UTC, to catch
  upstream-action and runner-image drift.
- `workflow_dispatch` for manual debugging.

Concurrency: `group: ci-integration-${{ github.ref }}` with
`cancel-in-progress: true`.

## Jobs

All jobs run on `ubuntu-latest` with `timeout-minutes: 10`.

### `container-loading` — smoke

Exercises the exact container-load code path used by `ci.yml`'s
`integration-container` job: `nix build .#harnessImage`,
`copyToDockerDaemon`, `docker image inspect`, then a `--help` smoke run.

### `empty-cache` — cold-start fallback

Deliberately does **not** configure Cachix, forcing Nix to fall back to
the public `cache.nixos.org` / source builds. Records elapsed wall-clock
to `$GITHUB_STEP_SUMMARY` for cold-vs-warm comparison.

Purpose: regression-catch if we ever silently make Cachix a hard
dependency. Without this test, a misconfigured Cachix token would look
like "CI is slow" rather than "CI cannot build from cold".

### `expected-failure` — negative test

Invokes `nix build '.#__ci_integration_does_not_exist__'` under
`continue-on-error: true` and asserts both that the step outcome is
`failure` and that the log references the bogus attribute. If a CI
refactor ever masks errors (for example with a misplaced `|| true`)
this job fails loudly.

## Explicit non-goals

- Cross-platform matrix (macOS / Windows). Covered piecewise by the
  existing `ci.yml` `build-matrix`.
- Chaos engineering beyond the three cases above.
- Performance regression thresholds — tracked by issue #9.
- Local reproducibility harness — tracked as a follow-up.
