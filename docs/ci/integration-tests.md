# CI infrastructure integration tests (issue #8)

This document is the minimum-viable seed for issue #8. It describes the
three jobs that should live in `.github/workflows/ci-integration.yml`
and exactly what each one asserts, so that a reviewer with
workflow-write scope can drop the YAML in with confidence.

## Why a separate workflow file

`ci.yml` has an `all-checks` gate that explicitly enumerates every job
it depends on. Adding CI-infrastructure self-tests directly to `ci.yml`
would make every unrelated product PR pay their cost and would also
couple their failure modes to the product gate. A dedicated workflow
(`ci-integration.yml`) keeps the signal separate and cheap.

## Triggers

- `pull_request` with `paths:` filter for `.github/workflows/**`,
  `.github/actions/**`, `flake.nix`, `flake.lock`, `nix/**`,
  `scripts/**`, and the workflow file itself. This means the jobs only
  run when CI itself is being touched.
- `schedule: cron: "0 6 * * 1"` — weekly Monday 06:00 UTC, to catch
  upstream-action and runner-image drift.
- `workflow_dispatch` for manual debugging.

Concurrency: `group: ci-integration-${{ github.ref }}` with
`cancel-in-progress: true`.

## Jobs

All jobs run on `ubuntu-latest` with `timeout-minutes: 10`.

### 1. `container-loading` — smoke

Exercises the exact container-load code path used by `ci.yml`'s
`integration-container` job.

- Checkout, install Nix, configure Cachix in pull-only mode.
- `nix build .#harnessImage --print-build-logs`.
- `nix run .#harnessImage.copyToDockerDaemon`.
- `docker image inspect nanna-coder-harness:latest` must succeed.
- `docker run --rm nanna-coder-harness:latest --help` must exit 0.

### 2. `empty-cache` — cold-start fallback

Deliberately does **not** configure Cachix, forcing Nix to fall back to
the public `cache.nixos.org` / source builds.

- `nix build .#harness --option substituters https://cache.nixos.org`
  must succeed.
- The elapsed wall-clock is recorded to `$GITHUB_STEP_SUMMARY` for a
  cold-vs-warm comparison.

Purpose: regression-catch if we ever silently make Cachix a hard
dependency. Without this test, a misconfigured cachix token would look
like “CI is slow” rather than “CI cannot build from cold”.

### 3. `expected-failure` — negative test

Asserts that our failure reporting actually surfaces failures.

- Invoke `nix build '.#__ci_integration_does_not_exist__' --no-link`
  under `continue-on-error: true`.
- The next step asserts `steps.brokenrun.outcome == 'failure'` and
  that the log references `__ci_integration_does_not_exist__`. If a
  CI refactor ever masks errors (for example with a misplaced
  `|| true`) this job fails loudly.

## Explicit non-goals for this PR

- Cross-platform matrix (macOS / Windows). Covered piecewise by the
  existing `ci.yml` `build-matrix`.
- Chaos engineering beyond the three cases above.
- Performance regression thresholds — tracked by issue #9.
- Local reproducibility harness — tracked as a follow-up comment.

## Reviewer action

Drop the following file at `.github/workflows/ci-integration.yml`. The
content is the implementation of the plan above; a reviewer with
workflow-write scope can apply it verbatim or extract it from the PR
description.

```yaml
name: CI Infrastructure Integration

on:
  pull_request:
    paths:
      - ".github/workflows/**"
      - ".github/actions/**"
      - "flake.nix"
      - "flake.lock"
      - "nix/**"
      - "scripts/**"
      - ".github/workflows/ci-integration.yml"
  schedule:
    - cron: "0 6 * * 1"
  workflow_dispatch:

concurrency:
  group: ci-integration-${{ github.ref }}
  cancel-in-progress: true

jobs:
  container-loading:
    name: container-loading (smoke)
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v4
      - uses: DeterminateSystems/nix-installer-action@v14
      - uses: cachix/cachix-action@v15
        with:
          name: nanna-coder
          skipPush: "true"
      - run: nix build .#harnessImage --print-build-logs
      - id: image
        name: Derive image reference from nix
        shell: bash
        run: |
          set -euo pipefail
          name=$(nix eval --raw .#harnessImage.imageName)
          tag=$(nix eval --raw .#harnessImage.imageTag)
          echo "ref=${name}:${tag}" >> "$GITHUB_OUTPUT"
      - run: nix run .#harnessImage.copyToDockerDaemon
      - name: Verify image is present
        shell: bash
        run: |
          set -euo pipefail
          docker image inspect '${{ steps.image.outputs.ref }}' >/dev/null
      - name: Smoke-run the image
        run: docker run --rm '${{ steps.image.outputs.ref }}' --help | head -n 5

  empty-cache:
    name: empty-cache (cold-start fallback)
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v4
      - uses: DeterminateSystems/nix-installer-action@v14
      - id: cold
        run: |
          set -euo pipefail
          start=$(date -u +%s)
          nix build .#harness --print-build-logs --option substituters https://cache.nixos.org
          end=$(date -u +%s)
          echo "duration_seconds=$((end - start))" >> "$GITHUB_OUTPUT"
      - run: |
          {
            echo "## Cold-cache build"
            echo
            echo "| metric | value |"
            echo "|--------|-------|"
            echo "| duration (s) | ${{ steps.cold.outputs.duration_seconds }} |"
            echo "| substituters | cache.nixos.org only (cachix deliberately skipped) |"
          } >> "$GITHUB_STEP_SUMMARY"

  expected-failure:
    name: expected-failure (negative test)
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v4
      - uses: DeterminateSystems/nix-installer-action@v14
      - id: brokenrun
        continue-on-error: true
        shell: bash
        run: |
          set -o pipefail
          nix build '.#__ci_integration_does_not_exist__' --no-link 2>&1 | tee /tmp/broken.log
      - run: |
          set -euo pipefail
          if [ '${{ steps.brokenrun.outcome }}' != 'failure' ]; then
            echo "::error::Expected failure, got '${{ steps.brokenrun.outcome }}'"
            exit 1
          fi
          grep -q '__ci_integration_does_not_exist__' /tmp/broken.log
```
