# CI Troubleshooting

Symptom-first index for failures you'll actually see. For workflow shape,
read [`architecture.md`](architecture.md) first.

## `all-checks` reports "Jobs missing from 'all-checks'.needs"

**Cause**: a new job was added to `ci.yml` without being added to
`jobs.all-checks.needs`.

**Fix**: edit `.github/workflows/ci.yml`, add the new job name under
`jobs.all-checks.needs`, push. The gate's verification step prints the
missing job names directly.

## `codecov-guard` rejects your PR

The guard rejects three classes of change to `codecov.yml`:

1. Lowering a numeric `target:` value.
2. Adding entries to `ignore:`.
3. Replacing a numeric `target:` with `auto` or removing it.

**Fix**: don't do those things. If you genuinely need to (e.g., a refactor
made code legitimately uncoverable), open an issue and get an admin
bypass. See `AGENTS.md` for the policy.

## `cargo nextest` hangs in `integration-container`

**Cause**: tests use `--test-threads=1` and one test is blocking on a
container that never becomes ready (typically Ollama on port 11434).

**Fix**:
- Locally: `podman pod logs nanna-pod` to see why Ollama didn't come up.
- In CI: the workflow probes `http://localhost:11434/api/tags` with a 5s
  timeout and skips `--run-ignored ignored-only` tests if Ollama isn't
  reachable. The fact that you're seeing a hang means the *non-ignored*
  test set is blocking. Look at the failing test name in the log.

## `tarpaulin` timeout (30 minutes)

**Cause**: a new test (or test set) made coverage collection exceed the
1800s timeout.

**Fix**: profile the slow test locally with
`cargo nextest run -- --test-threads=1 <test_name>`. If it's legitimately
slow, mark it `#[ignore]` and run it from a dedicated job; if it's a
loop/sleep, fix the test. Do not raise the timeout without justification.

## Cachix push fails on PRs from forks

**Cause**: forks can't access `secrets.CACHIX_AUTH`. The workflow already
sets `skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}`,
so pushes are skipped — pulls still work via the public cache URL.

**Symptom in log**: "skipping push, no auth token" (informational, not an
error).

## Container load fails with "Image not found"

**Cause**: `nix run .#harnessImage.copyToDockerDaemon` succeeded
according to its exit code but `docker image inspect` can't find the
tag. Usually a `nix2container` version mismatch.

**Fix**:
1. `docker info` — confirm the daemon is reachable.
2. `nix build .#harnessImage --print-out-paths` — confirm the build
   produced an image.
3. `docker images | grep nanna-coder` — see what tag actually landed.
4. Compare `nix/container-config.nix` `tag` field against what was
   loaded. If they differ, fix the config.

## Windows job fails with "cargo-nextest: not found"

**Cause**: `taiki-e/install-action` cache miss for the toolchain.

**Fix**: usually transient; re-run the failed job. If it persists, the
install action upstream may have changed — pin to a known-good version.

## `nix-installer-action` 403s on Determinate Systems' CDN

**Cause**: rate-limited or transient CDN outage.

**Fix**: re-run. If sustained, swap `@main` for a pinned tag
(`@v14`) — see commit `e003ed4` for the pattern used in
`ci-integration.yml`.

## codecov "no coverage report found"

**Cause**: tarpaulin job succeeded but didn't emit `lcov.info`, or
the upload step ran before the file existed.

**Fix**: confirm the `Run security checks` step actually invoked
tarpaulin (it gates on `matrix.test-type == 'security' && runner.os == 'Linux'`).
If tarpaulin ran and exited 0 but no file appeared, check the
`--output-dir` argument matches the codecov action's `files:` path.

## Eval suite produces inconsistent results across runs

**Cause**: model nondeterminism (default).

**Fix**: this is expected. The eval suite reports a scorecard, not a
pass/fail. Look at the failure-mode columns rather than the headline
number. To make a single run reproducible, set the model's seed and
temperature — see `crates/eval-runner/`.

## "Resource not accessible by integration" on GHA token

**Cause**: a workflow tried to write to a resource (issues, PRs,
packages) without the required `permissions:` block.

**Fix**: add the minimal scope at the job or workflow level. Example:
```yaml
permissions:
  contents: read
  pull-requests: write
```
Never grant `write-all`.

## When the workflow file itself looks broken

Run `yq -r '.jobs | keys' .github/workflows/ci.yml` locally. If yq errors
out, the YAML is invalid — usually an indentation slip after a multi-line
`run:` block.
