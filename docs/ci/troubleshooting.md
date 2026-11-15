# CI Troubleshooting

Triage guide for failures in each workflow defined under `.github/workflows/`. The job names below match [architecture.md](architecture.md) exactly; the symptoms and fixes are grounded in the actual YAML, not generic advice.

## Reading a failing CI run

1. Open the failing PR or branch in GitHub Actions.
2. Scroll to the `all-checks` job. If it reports `Jobs missing from 'all-checks'.needs`, see [maintenance.md](maintenance.md) — a new job was added without wiring it into the gate.
3. Otherwise the first failing job in the `needs:` graph is the one to investigate. The graph is in [architecture.md](architecture.md).

## ci.yml

### `test-matrix`

| Symptom | Likely cause | Fix |
|---|---|---|
| Windows job stuck at "Setup Rust toolchain" | Transient `rustup` flake | Re-run the job; the workflow uses `dtolnay/rust-toolchain@master` which occasionally hits rate limits. |
| Linux `integration-container` fails at "Pre-build test containers" with `copyToDockerDaemon` error | Docker daemon not healthy on the runner, or the `ollamaImage` failed to build | Check earlier build step output; re-run; if it repeats, the Nix container definition is broken — fix `nix/containers.nix`. |
| `security` test-type fails at `cargo tarpaulin --timeout 1800` | E2E tests legitimately exceeded 30 minutes under tarpaulin instrumentation | Split the slow test or increase the timeout in `ci.yml`. Do **not** silence with `\|\| true`. |
| `Upload coverage reports` fails with "fail_ci_if_error: true" | `CODECOV_TOKEN` missing or Codecov rejected `lcov.info` | Verify the `codecov` environment has the secret set; confirm tarpaulin actually produced `lcov.info`. |
| macOS/Windows units fail only on a specific crate | Platform-gated code lacks a `#[cfg(...)]` guard | Add the guard and re-run. Matrix is `fail-fast: false`, so one failed OS does not mask others. |

### `build-matrix`

| Symptom | Likely cause | Fix |
|---|---|---|
| `aarch64-linux` step falls through to `\|\| nix build .#nanna-coder` | Cross target not yet defined under `.#packages.aarch64-linux` in `flake.nix` | Either finish the cross target or accept the native fallback — but do not remove the fallback without replacing the target. |
| "No artifacts found after Nix build" | Nix `result` symlink was produced but the script's two fallback patterns did not match | Inspect the actual `result/` path in the job log; extend the `Prepare artifacts (Nix)` step. |
| macOS `aarch64-darwin` fails with "target not installed" | `rustup target add aarch64-apple-darwin` skipped | Ensure `matrix.cross == true` branch actually runs. |

### `build-containers`

| Symptom | Likely cause | Fix |
|---|---|---|
| `copyToDockerDaemon` fails with "Docker daemon status" message | Runner Docker service stopped | Re-run. If recurring, pin `ubuntu-latest` to a known-good image. |
| `docker push` fails with 403 | `GITHUB_TOKEN` lacks `packages: write` in the calling context | The job already declares `permissions: packages: write` — check that the workflow is not running from a restricted fork. Forks skip the push step anyway (`if: github.event_name != 'pull_request'`). |
| Image tag appears but not `:latest` | Tag race between concurrent pushes | Acceptable; the commit-SHA tag is authoritative. |

### `security-scan`

| Symptom | Likely cause | Fix |
|---|---|---|
| Runs on a PR and produces no SARIF | Expected — job is gated `if: github.event_name != 'pull_request'` | No action. |
| `upload-sarif` rejects the file | SARIF schema violation from Trivy upstream | Pin `aquasecurity/trivy-action` to a working SHA and file an upstream issue. |

### `cache-maintenance`

| Symptom | Likely cause | Fix |
|---|---|---|
| Job skipped on PR | Expected — gated `if: github.event_name == 'push' && github.ref == 'refs/heads/main'` | No action. |
| `cache-analytics` prints "Cache not configured" | `CACHIX_AUTH` missing or `cachix use` never ran | Verify secret; see [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md) and [../../CACHIX_SETUP.md](../../CACHIX_SETUP.md). |

### `release`

| Symptom | Likely cause | Fix |
|---|---|---|
| Never runs | Expected — gated `if: github.event_name == 'release'` | Publish a GitHub release to trigger. |
| `actions/upload-release-asset@v1` fails | This action is deprecated; it still works but fails on permission edge cases | Track migration to `softprops/action-gh-release`; keep scope narrow for #10. |

### `docs-check`

| Symptom | Likely cause | Fix |
|---|---|---|
| `check-docs-links.sh` reports a broken relative link | Doc was renamed or moved | Update the link target. The script only validates internal links; external URLs are intentionally not checked (see script header). |
| `check-ci-doc-coverage.sh` reports a workflow with no dedicated heading | A new `.github/workflows/*.y{,a}ml` file was added without a matching `##` heading in `architecture.md` | Add the heading, or add an `OMITTED: <filename> — <reason>` line to `architecture.md` if exclusion is justified. |

### `all-checks`

| Symptom | Likely cause | Fix |
|---|---|---|
| "Jobs missing from 'all-checks'.needs" | A new job was declared but not wired | Add the job to `jobs.all-checks.needs` in `ci.yml`. |
| "One or more CI jobs failed or were cancelled" | A dependency failed | Fix the upstream job; this step only aggregates. |

## cache-warming.yml

| Symptom | Likely cause | Fix |
|---|---|---|
| `warm-dependencies` never runs on a push to `main` | Path filter — only `flake.lock`, `Cargo.*`, or the workflow file trigger it | Use `workflow_dispatch` with `force_rebuild=true` if you need to warm manually. |
| `warm-containers` succeeds but PR CI still rebuilds everything | Cache key mismatch between warming run and PR run | Compare `deps-key` output against the PR's Nix-derived hashes. See [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md). |
| `warm-cross-platform` never runs | Expected — `if: false` (disabled in-tree). See [architecture.md](architecture.md). | No action until cross-compilation is re-enabled. |

## ci-integration.yml

Background on these jobs (intent, exit criteria, source-of-truth YAML) lives in [integration-tests.md](integration-tests.md). The table below is symptom-to-fix only.

| Symptom | Likely cause | Fix |
|---|---|---|
| `container-loading` fails at "Verify image is present" | `nix run .#harnessImage.copyToDockerDaemon` did not actually load the image, or the derived image ref does not match what was loaded | Compare the `nix eval --raw .#harnessImage.imageName`/`.imageTag` outputs with `docker images` in the runner log. |
| `empty-cache` exceeds the 10-minute timeout | Cold builds got slower upstream, or the Nix substituter list expanded silently | Time the build locally with `--option substituters https://cache.nixos.org`; if it really takes >10 min, raise the timeout — do not silently add Cachix back. |
| `expected-failure` reports `Expected failure, got 'success'` | Someone accidentally defined a flake attribute that matches `__ci_integration_does_not_exist__`, or the assertion logic was changed | Pick a new attribute name guaranteed not to exist, or restore the original assertion. |

## codecov-guard.yml

| Symptom | Likely cause | Fix |
|---|---|---|
| `guard` fails with "base ref '...' not resolvable" | Branch was created without enough history, or the base ref string is malformed | Re-run after pushing/fetching the base branch. The check is fail-closed by design. |
| `guard` fails with "patch target decreased" / "ignore: list grew" / "strict_yaml_branch changed" | A `codecov.yml` change is genuinely relaxing coverage policy | Either revert the change, or — if intentional — a repository admin must use GitHub's "merge without waiting for requirements" admin merge. No script flag bypasses this. |
| `guard` fails with "did not exist on base" | First-time addition of `codecov.yml` is being attempted on a non-admin merge | Land the initial `codecov.yml` via admin merge, or rebase on a base where it already exists. |

## eval.yml

| Symptom | Likely cause | Fix |
|---|---|---|
| "Ollama not ready" after 30 seconds | Ollama installer regressed, or the runner has no internet | Re-run; if persistent, bump the readiness loop. |
| Comment never appears on the PR | `pr_number` input was empty | Re-dispatch with the correct PR number. |
| Eval tests green but `outcome=failure` posted | `nextest` returned non-zero for a non-test reason (e.g. build failure) | Inspect `eval-output.txt` uploaded as artifact. |

## install-nightly.yml

| Symptom | Likely cause | Fix |
|---|---|---|
| `linux-full-e2e` hits the 90-minute timeout | Cold cache + multi-GB model pull genuinely exceeded the budget, or Ollama upstream regressed | Re-run; if persistent, dispatch with `model=` set to a smaller model to isolate the cause. |
| `docker save` / `podman load` retag step fails with "could not find loaded image" | nix2container changed the loaded image name | Inspect `podman images` output in the run log and update the retag pattern. |
| Verify-model step fails with `grep -q 'gemma4'` mismatch | `inputs.model` was set to a non-gemma4 model but the assertion is hard-coded | Update the assertion in the workflow or supply a gemma4 model on dispatch. |

## install-test.yml

| Symptom | Likely cause | Fix |
|---|---|---|
| `shellcheck` fails on `scripts/install.sh` | Real shell-script regression | Fix the script; do not add `# shellcheck disable=` without a comment justifying it. |
| `ps1-lint` reports `Error`-severity findings | PSScriptAnalyzer caught a real issue | Fix in `scripts/install.ps1`. `Warning` severity does not fail; `Error` does. |
| `ps1-parse` fails | `install.ps1` has a syntax error | Fix the parse error; the parser is the same one PowerShell uses at runtime. |
| `linux-bringup` / `macos-bringup` / `windows-bringup` fails | Platform-specific install regression | Reproduce locally per the platform; the bringup script is the canonical install path. |
| `install-test-gate` fails after one bringup job failed | Aggregator job — see the failed upstream | Fix the upstream bringup job. |

## badges.yaml

| Symptom | Likely cause | Fix |
|---|---|---|
| No badge update commit on `main` | `git diff --cached --quiet` — nothing changed | No action; the workflow only commits diffs. |
| Push fails silently | `git push \|\| true` is deliberate — the workflow never fails on push errors | Accept, or rework if this ever becomes load-bearing. |

## Known-unfixable / out of scope

- `cargo audit` and `cargo deny` are skipped in `test-matrix` due to CVSS 4.0 tooling incompatibility. See the inline comment in `ci.yml` around the `security` test-type. Unblocking this is tracked separately.
- `warm-cross-platform` is `if: false`. Do not enable without verifying the cross targets first.

## Related documents

- [architecture.md](architecture.md) — job-by-job reference
- [maintenance.md](maintenance.md) — recurring upkeep that prevents most failures here
- [performance.md](performance.md) — when a job is slow rather than failing
- [security.md](security.md) — when a failure involves secrets or permissions
- [integration-tests.md](integration-tests.md) — `ci-integration.yml` design rationale
- [../ci-cd-pipeline.md](../ci-cd-pipeline.md) — higher-level pipeline narrative
- [../../TESTING.md](../../TESTING.md) — test philosophy driving the gates
