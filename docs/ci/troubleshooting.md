# CI Troubleshooting

Symptom → cause → fix, for every category of failure we've actually seen in
this repository's CI. Keep entries factual and grounded in the workflow YAML
under [`.github/workflows/`](../../.github/workflows/). Add a new entry
whenever you spend more than 30 minutes debugging something in CI.

## How to read a failed CI run

1. Open the run summary on GitHub. The `all-checks` gate lists every job it
   depends on with pass/fail/cancelled state.
2. If `all-checks` failed with `::error::Jobs missing from 'all-checks'.needs`,
   the issue is not a test failure — see
   [`all-checks` gate audit failure](#all-checks-gate-audit-failure).
3. Otherwise, open the earliest failing dependency. Because
   `fail-fast: false` is set on every matrix, later failures are usually
   consequences of the earliest.

## Errors by category

### `all-checks` gate audit failure

**Symptom:** `all-checks` fails immediately with something like:

```text
::error::Jobs missing from 'all-checks'.needs:
new-job-name
Add them to .github/workflows/ci.yml under jobs.all-checks.needs
```

**Cause:** A new job was added to `.github/workflows/ci.yml` without being
listed under `jobs.all-checks.needs`. The audit is intentional — it prevents
silent bypass of the gate.

**Fix:** Add the job to `jobs.all-checks.needs` in the same PR that
introduced the job.

### Codecov guard rejects a `codecov.yml` change

**Symptom:** `codecov-guard / guard` fails on a PR that touches
`codecov.yml`, `.github/workflows/codecov-guard.yml`, or `.github/CODEOWNERS`.

**Cause:** One of the guarded invariants was violated. See
[`AGENTS.md`](../../AGENTS.md) for the enumerated list; the common ones are:

- `patch.default.target` was lowered.
- A numeric target was replaced with `auto`, or removed.
- An entry was added to `ignore:`.
- `strict_yaml_branch` was dropped.
- `codecov.yml` didn't exist on the base ref (first-time adds require admin).

**Fix:** Revert the guarded change. If the change is intentional and
justified, an admin merge is the only path.

### `patch` coverage below 100%

**Symptom:** Codecov `patch` check fails with lines uncovered.

**Cause:** New/modified lines were not exercised by tests. Common gotchas:

- A `#[cfg(feature = "…")]` module isn't compiled without `--all-features`,
  so its tests never ran. Check that the `security` matrix cell used
  `--workspace --all-features`.
- An `#[ignore]`d test wasn't run. `#[ignore]` marks the test as opt-in;
  those lines legitimately show as uncovered.
- A test exists but only runs against a live model (Ollama on `:11434`),
  which isn't reachable in the standard matrix cell. See
  [Integration tests skipped](#integration-tests-skipped-because-ollama-is-not-reachable).

**Fix:** Add or unskip a test that hits the new lines. Do not add to
`codecov.yml` `ignore:` — the guard rejects it.

### Integration tests skipped because Ollama is not reachable

**Symptom:** `test-matrix (integration-container)` logs:

```text
Ollama not reachable on port 11434, skipping model integration tests
```

**Cause:** The `--run-ignored ignored-only` pass only runs when
`http://localhost:11434/api/tags` returns 200 within 5s. If the pre-built
`nanna-coder-ollama:latest` container isn't running, or the model isn't
pulled inside it, the `curl --max-time 5` check fails and those tests are
skipped. This is not a failure — it's expected in the standard matrix. Live
model tests run in [`install-nightly.yml`](../../.github/workflows/install-nightly.yml)
and in [`eval.yml`](../../.github/workflows/eval.yml).

**Fix (if you actually wanted live model tests):** Trigger `install-nightly`
or `eval` manually via `workflow_dispatch`.

### `Failed to load test container` / `Container nanna-coder-ollama:latest not found`

**Symptom:**

```text
❌ Failed to load test container
💡 Check Docker daemon status: docker info
```

or

```text
❌ Container nanna-coder-ollama:latest not found
```

**Cause:** `nix run .#ollamaImage.copyToDockerDaemon` failed to load the
built image into the Docker daemon. Usually one of: Docker not running on the
runner (rare on GitHub-hosted), disk pressure, or a nix2container revision
mismatch.

**Fix:** Re-run the job. If it reproduces, run locally:

```bash
nix build .#ollamaImage --print-build-logs
nix run .#ollamaImage.copyToDockerDaemon
docker image inspect nanna-coder-ollama:latest
```

If `copyToDockerDaemon` itself fails locally, check `flake.nix` for the
nix2container revision and match against a green `main` build.

### Cachix push failing on a fork PR

**Symptom:** `cachix/cachix-action` in a PR from a fork logs a push error.

**Cause:** Forks don't have access to `secrets.CACHIX_AUTH`. The workflow
already sets `skipPush: ${{ github.event_name == 'pull_request' &&
github.event.pull_request.head.repo.fork }}` in `ci.yml` and
`build-containers`, so pushes are suppressed. If you're seeing a push error
anyway, someone likely added a new job that installs Cachix without wiring
the `skipPush` condition.

**Fix:** Copy the `skipPush` expression from an existing Cachix step in the
same workflow.

### `Expected binary not found at $BINARY_PATH`

**Symptom:** In `build-matrix` on macOS:

```text
❌ ERROR: Expected binary not found at target/release/nanna
```

**Cause:** The binary was renamed `harness` → `nanna` in `Cargo.toml`
`[[bin]]`. If someone reverts that rename, or reintroduces a `harness` bin,
the `cp` step in the `Prepare artifacts (Cargo)` block will fail. The release
artifact filename still says `harness-<target>`, but the source binary is
`nanna`.

**Fix:** Do not rename the binary back. If a real binary rename is needed,
update `build-matrix` artifact prep, the `release` job, and this note in
lockstep.

### Trivy scan uploads no SARIF / `security-scan` fails

**Symptom:** `security-scan / Security Scan` fails or reports empty results.

**Cause:** `security-scan` runs against `${{ env.REGISTRY }}/…/harness:latest`,
which is only pushed on non-PR events. On PRs the job is gated out
(`if: github.event_name != 'pull_request'`). If the job runs on a `push` and
the tag hasn't been pushed yet (e.g., `build-containers` was skipped),
Trivy has nothing to scan.

**Fix:** Confirm `build-containers` succeeded and actually pushed. If a fork
merged into main without container push permissions, expect Trivy to fail
until the next successful push from a maintainer branch.

### tarpaulin timeout

**Symptom:** `test-matrix (security)` fails with tarpaulin killed after
1800s.

**Cause:** New long-running integration test, or slow model interaction
under instrumentation. `--timeout 1800` is already 30 min.

**Fix:** Profile the slow tests locally under tarpaulin — do not immediately
bump the timeout. If a test genuinely needs to run under coverage and cannot
be shortened, split it or mark it `#[ignore]` and add a companion assertion
elsewhere.

### `flake.nix` / `flake.lock` change breaks a downstream job

**Symptom:** After a flake update, `test-matrix` or `build-containers` fails
on Linux while macOS/Windows are green.

**Cause:** Only Linux uses Nix; macOS/Windows use `dtolnay/rust-toolchain`.
A Linux-only failure after a flake change is usually the flake, not the code.

**Fix:** Reproduce locally:

```bash
nix flake check
nix develop --command cargo nextest run --workspace --lib --all-features
```

If it fails, roll back the offending flake input or pin it.

### Actions runner "unable to install nix" / `nix-installer-action` failure

**Symptom:** `DeterminateSystems/nix-installer-action@main` fails during
setup.

**Cause:** Usually a transient GitHub Actions runner networking issue. We
track `@main`, so upstream regressions can also bite us.

**Fix:** Re-run. If it reproduces across multiple runs, pin
`nix-installer-action` to a known-good SHA and open an issue.

### Nightly install E2E hangs on model pull

**Symptom:** [`install-nightly.yml`](../../.github/workflows/install-nightly.yml)
`linux-full-e2e` job hangs at the model-pull step.

**Cause:** Multi-GB Gemma download from Ollama registry. There are 3 retries
with 600s each and increasing backoff (`10s`, `20s`).

**Fix:** If pulls keep failing, the underlying issue is either Ollama
registry availability or runner disk pressure. Consider switching the
nightly to a smaller model, or splitting the pull into its own job with its
own retry policy.

## Escalation

If you cannot resolve a CI failure in a reasonable time:

1. Open a GitHub issue with the run URL, the failing job name, and the
   relevant log excerpt.
2. Tag `@DominicBurkart`.
3. If the failure blocks a merge and is CI-environmental (not a code
   defect), do not disable the failing check. Escalate per
   [`AGENTS.md`](../../AGENTS.md).

## See also

- [`architecture.md`](architecture.md) — how the pipeline is put together.
- [`maintenance.md`](maintenance.md) — routine maintenance that prevents
  the failures listed here.
- [`performance.md`](performance.md) — when the fix is "make it faster".
