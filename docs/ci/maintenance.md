# CI Maintenance

Recurring and one-off upkeep tasks for `.github/workflows/`. Every procedure here is grounded in a file or job that currently exists. When a task touches cache behavior, defer to the cache docs rather than duplicating: [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md), [../binary-cache-strategy.md](../binary-cache-strategy.md), [../cache-migration-guide.md](../cache-migration-guide.md), [../cachix-migration.md](../cachix-migration.md).

## The `all-checks` gate invariant

`ci.yml` contains a self-check that makes it impossible to forget wiring a new job into the merge gate:

```yaml
all_jobs=$(yq -r '.jobs | keys | .[]' .github/workflows/ci.yml | sort)
gate_needs=$(yq -r '.jobs["all-checks"].needs | .[]' .github/workflows/ci.yml | sort)
expected=$(echo "$all_jobs" | grep -vx 'all-checks')
missing=$(comm -23 <(echo "$expected") <(echo "$gate_needs") || true)
if [ -n "$missing" ]; then
  echo "::error::Jobs missing from 'all-checks'.needs:"
  echo "$missing"
  exit 1
fi
```

The `yq` expression enumerates jobs dynamically from the YAML. That means adding a new job requires **only** updating the `jobs.all-checks.needs` list; there is no hand-maintained allowlist to keep in sync.

When adding a new job:

1. Add the job definition.
2. Add the job name to `jobs.all-checks.needs` (alphabetically in the existing style).
3. Re-run the workflow on a throwaway branch; `all-checks` will tell you immediately if you forgot something.

## Workflow-coverage invariant

`scripts/check-ci-doc-coverage.sh` asserts that every `.github/workflows/*.y{,a}ml` file has a dedicated `## <filename>` heading in [architecture.md](architecture.md), or is explicitly excluded via `OMITTED: <filename> — <reason>` somewhere in that file. Once the planned `docs-check` job is wired into `ci.yml` (see [#254 follow-up](https://github.com/DominicBurkart/nanna-coder/pull/254)), this check will run automatically in CI. Until then, run it locally. Consequences:

- New workflow? Add a heading to [architecture.md](architecture.md) **and** describe triggers/jobs/matrix/secrets before merging.
- Rename? Update the heading.
- Retire a workflow? Either delete it from disk, or add an `OMITTED:` marker with justification.

## Link-coverage invariant

`scripts/check-docs-links.sh` validates relative links and `#anchor` fragments inside `docs/ci/*.md`. It does **not** check external HTTP URLs — that choice is documented in the script header and keeps CI free of flake on link-rot. Once the planned `docs-check` job is wired into `ci.yml`, this will also run automatically in CI. When moving a doc, update its inbound references, then run `bash scripts/check-docs-links.sh` locally.

## Secret rotation

Secrets used by the workflows (see [architecture.md](architecture.md) table):

| Secret | Used by | Rotation procedure |
|---|---|---|
| `CACHIX_AUTH` | `ci.yml` (every job that configures Cachix), `cache-warming.yml` (every job), `eval.yml` (`eval`) | Regenerate in Cachix dashboard, update the `CACHIX_AUTH` repo-level secret in repository Settings → Secrets and variables → Actions, verify by re-running a cache-warming workflow. See [../../CACHIX_SETUP.md](../../CACHIX_SETUP.md). |
| `CODECOV_TOKEN` | `ci.yml` (`test-matrix` security variant) | Regenerate in Codecov UI; update in repository secrets; re-run a security test to confirm upload. |
| `GITHUB_TOKEN` | `ci.yml` (container login, release upload), `eval.yml` (PR comments), `badges.yaml` (push) | Provided by GitHub Actions automatically; no rotation. Permissions are scoped per-job via `permissions:` blocks. |

## Upgrading pinned actions

Current pins (exact versions are in `.github/workflows/`):

- `actions/checkout@v4`
- `DeterminateSystems/nix-installer-action@main` — floating; watch for upstream changes
- `cachix/cachix-action@v15`
- `dtolnay/rust-toolchain@master` — floating by convention
- `taiki-e/install-action@v2`
- `codecov/codecov-action@v5`
- `docker/login-action@v3`
- `aquasecurity/trivy-action@master` — floating
- `github/codeql-action/upload-sarif@v3`
- `actions/upload-artifact@v4`
- `actions/upload-release-asset@v1` — deprecated; see [troubleshooting.md](troubleshooting.md)
- `peter-evans/create-or-update-comment@v4`

Upgrade procedure:

1. Bump one action at a time in a dedicated PR.
2. Confirm `all-checks` is green on a push branch before merging.
3. For floating pins (`@main`, `@master`), pin to a SHA if the action becomes flaky.

## Recurring tasks

| Cadence | Task | Owner |
|---|---|---|
| Weekly | Skim `cache-analytics` output from the most recent `cache-maintenance` run for regressions | Maintainer |
| Monthly | Verify `badges.yaml` still produces SVGs (img.shields.io upstream occasionally changes paths) | Maintainer |
| On `flake.lock` bump | Observe `cache-warming.yml` fires on path filter; if it does not, something has broken the trigger | Nix maintainer |
| On Rust toolchain bump | Re-run `warm-dependencies` manually with `force_rebuild=true` | Rust maintainer |

_Note: a "Every PR — Review `docs-check` output" task is planned but not yet active because the `docs-check` CI job has not yet been wired into `ci.yml` (see [#254 follow-up](https://github.com/DominicBurkart/nanna-coder/pull/254))._

## When things genuinely cannot be fixed here

- CVSS 4.0 tooling gap affecting `cargo audit` / `cargo deny` — tracked upstream; workaround is the inline skip in `ci.yml` with comments.
- `actions/upload-release-asset@v1` deprecation — scheduled for its own issue; do not bundle into docs work.

## Related documents

- [architecture.md](architecture.md) — the authoritative job inventory
- [troubleshooting.md](troubleshooting.md) — symptom-to-fix map
- [onboarding.md](onboarding.md) — pre-reading for anyone taking ownership
- [security.md](security.md) — secrets, permissions, scopes
- [performance.md](performance.md) — when to tune vs. when to upgrade
- [../CACHE_STRATEGY.md](../CACHE_STRATEGY.md) — cache operations
- [../binary-cache-strategy.md](../binary-cache-strategy.md) — cache architecture
- [../cache-migration-guide.md](../cache-migration-guide.md) — historical cache migrations
- [../cachix-migration.md](../cachix-migration.md) — Cachix-specific migration notes
- [../../CACHIX_SETUP.md](../../CACHIX_SETUP.md) — operator-facing Cachix setup
- [../../CONTRIBUTING.md](../../CONTRIBUTING.md) — contributor workflow
