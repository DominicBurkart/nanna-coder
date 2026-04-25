# CI Security

Secrets, permissions, and supply-chain posture of the workflows in `.github/workflows/`. Everything here reflects the current YAML.

## Secrets

| Secret | Referenced in | Scope |
|---|---|---|
| `CACHIX_AUTH` | `ci.yml` (`test-matrix`, `build-matrix`, `build-containers`, `cache-maintenance`, `release`), `cache-warming.yml` (all jobs), `eval.yml` (`eval`) | Repository-level secret. Cachix push authentication; pulls are unauthenticated. Not scoped to any GitHub environment — every consuming job pulls it directly from repo secrets. |
| `CODECOV_TOKEN` | `ci.yml` (`test-matrix` `security` variant) | Uploading `lcov.info`. Scoped to the `codecov` GitHub environment, which is referenced only by `test-matrix` via `environment: codecov`. |
| `GITHUB_TOKEN` | `ci.yml` (`build-containers` registry login, `release` asset upload), `eval.yml` (PR comment), `badges.yaml` (commit push) | Per-run token issued by GitHub Actions. Permissions are constrained via per-job `permissions:` blocks. |

`CACHIX_AUTH` is a **repository-level** secret (no `environment:` block in any consuming job). `CODECOV_TOKEN` is the **only** environment-scoped secret here — it lives in the `codecov` environment, which `test-matrix` opts into via `environment: codecov`. See [../../CACHIX_SETUP.md](../../CACHIX_SETUP.md) for how to provision the Cachix side and [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md) for the keys it authenticates against.

## Fork safety

`ci.yml`'s Cachix step sets:

```yaml
skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}
```

This means a fork-based PR can **read** from the cache but cannot push to it, and `CACHIX_AUTH` is not exposed to fork runners. The same pattern is used in `build-containers`.

Additionally, `build-containers`'s push step is gated on `if: github.event_name != 'pull_request'`, so container images are never pushed for PRs from any source.

## Permissions

`ci.yml` declares permissions per job, not globally:

- `test-matrix`: default (read). Declares `environment: codecov` so Codecov secrets are only injected where needed.
- `build-matrix`: default.
- `build-containers`: `contents: read`, `packages: write`. Required to push to `ghcr.io`.
- `security-scan`: `contents: read`, `security-events: write`. Required to upload SARIF via CodeQL.
- `cache-maintenance`: default.
- `release`: default (uses the release event's upload URL, authenticated via `GITHUB_TOKEN`).
- `all-checks`: default.
- `docs-check`: default.

`eval.yml`'s `eval` job declares `contents: read`, `pull-requests: write` — needed only when `pr_number` is supplied.

`badges.yaml`'s `update-badges` job declares `contents: write` — needed to commit badge SVGs back to `main`.

No workflow grants blanket `write-all`.

## Supply-chain scanning

- **Trivy** (`security-scan`) scans the pushed harness image and uploads SARIF. Only runs on non-PR events, so fork PRs cannot exfiltrate via this path.
- **Codecov** (`test-matrix` security variant) uploads coverage. `fail_ci_if_error: true` means a failed upload blocks the gate.
- **`cargo audit` and `cargo deny`** are **currently skipped** in `test-matrix`'s security variant with an inline comment citing CVSS 4.0 tooling incompatibility. See [troubleshooting.md](troubleshooting.md) and the inline comment in `ci.yml`. `deny.toml` still exists and is valid; the skip is purely about the runner tooling.

## Pinned actions (risk surface)

See [maintenance.md](maintenance.md) for the list. Actions pinned to `@main` / `@master` (`DeterminateSystems/nix-installer-action`, `dtolnay/rust-toolchain`, `aquasecurity/trivy-action`) are a supply-chain risk surface and should be SHA-pinned if any of them ever ships a breaking or hostile change.

## Secret rotation

See [maintenance.md](maintenance.md). The short version:

- `CACHIX_AUTH`: regenerate in Cachix, update the repo-level secret under repository Settings → Secrets and variables → Actions (no environment), re-run cache warming.
- `CODECOV_TOKEN`: regenerate in Codecov, update the secret inside the `codecov` GitHub environment under repository Settings → Environments → codecov, re-run any `security` matrix job.
- `GITHUB_TOKEN`: managed by GitHub Actions; nothing to rotate.

## What we do not do (deliberately)

- **No `write-all` permissions.** Every job is least-privilege.
- **No Cachix push from fork PRs.** Enforced by `skipPush:` and by `github.event_name != 'pull_request'` on container push steps.
- **No external URL fetching in the docs link checker.** `scripts/check-docs-links.sh` only validates relative paths and anchors; external URLs would introduce flakiness and potential SSRF-shaped exposure in CI (see script header). This is called out here because it is a security-relevant design choice, not an oversight.
- **No `--no-verify` commits via CI.** `AGENTS.md` forbids skipping hooks; the docs-check job cannot bypass anything.

## Related documents

- [architecture.md](architecture.md) — job-by-job permissions summary
- [troubleshooting.md](troubleshooting.md) — secret-adjacent failure modes
- [maintenance.md](maintenance.md) — rotation procedures
- [../../CACHIX_SETUP.md](../../CACHIX_SETUP.md) — Cachix credentials
- [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md) — what the credentials authenticate against
- [../ci-cd-pipeline.md](../ci-cd-pipeline.md) — earlier narrative on supply-chain posture
