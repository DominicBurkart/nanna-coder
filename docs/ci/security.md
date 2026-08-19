# CI Security

How secrets, permissions, and supply-chain integrity are handled in
this pipeline.

## Secrets

| Name | Used by | Scope |
|------|---------|-------|
| `CACHIX_AUTH` | `ci.yml`, `cache-warming.yml`, `eval.yml`, `install-*.yml` | Write to the `nanna-coder` Cachix cache. |
| `CODECOV_TOKEN` | `ci.yml` (security job) | Upload coverage to codecov.io. |
| `GITHUB_TOKEN` | every workflow | GitHub-provided per-run token. |

No other repository secrets should exist. Audit quarterly (see
[`maintenance.md`](maintenance.md)).

### Rotation

- `CACHIX_AUTH`: rotate quarterly via the Cachix dashboard. Generate a
  new write token, update the GitHub secret, revoke the old one.
- `CODECOV_TOKEN`: rotate when a maintainer leaves the project or on
  suspected compromise.
- `GITHUB_TOKEN`: managed by GitHub; you cannot rotate it manually.

### Fork PR handling

Fork PRs cannot access repository secrets. Affected workflows handle
this explicitly:

```yaml
skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}
```

This means fork PRs use Cachix read-only and don't push images to
`ghcr.io`. The `security-scan` job is gated on
`github.event_name != 'pull_request'` for the same reason.

## Permissions

The principle of least privilege applies at the **job** level. Every
job declares its own `permissions:` block; nothing inherits
`write-all`.

Current permissions in `ci.yml`:

| Job | Permissions |
|-----|-------------|
| `test-matrix` | default (read-only contents) |
| `build-matrix` | default |
| `build-containers` | `contents: read`, `packages: write` |
| `security-scan` | `contents: read`, `security-events: write` |
| `cache-maintenance` | default |

If you add a job that needs write access, declare the *minimum* scope
required and document why in the PR description.

## Supply Chain

### Pinned actions

Where possible, actions are pinned to a tag (`@v4`, `@v15`). A few use
`@main` (`DeterminateSystems/nix-installer-action`,
`aquasecurity/trivy-action`) — these are vendor-blessed mutable
references. Track upstream advisories for these.

The `ci-integration.yml` workflow demonstrates the better pattern:
`@v14` for the Nix installer (see commit `e003ed4`). Migrate other
workflows when their upstreams cut stable releases worth pinning.

### Dependency review

`cargo audit` and `cargo deny` are *currently disabled* with comments
in `ci.yml`:

```
# Skipping cargo audit due to CVSS 4.0 incompatibility
# Skipping cargo deny due to CVSS 4.0 incompatibility
```

This is a known gap. When the upstream tools support CVSS 4.0,
re-enable. Track via the relevant rustsec issues.

### Container scanning

`security-scan` runs Trivy against the harness image on every push to
`main` and on releases. Results upload to GitHub's Security tab as
SARIF. Critical findings should block release.

### Reproducible builds

Nix gives us content-addressed, reproducible builds end-to-end. This
means:

- A given commit always produces bit-identical artifacts.
- Cache poisoning is detectable: a mismatch between local and Cachix
  outputs for the same derivation is a hard signal.
- We can verify build provenance by re-running `nix build` on any
  artifact's source commit.

## Coverage Guard

`.github/workflows/codecov-guard.yml` and `AGENTS.md` enforce that
coverage cannot be silently relaxed. The guard rejects:

1. Lowered numeric `target:` values.
2. New `ignore:` entries (block- or flow-style).
3. Replacement of numeric `target:` with `auto`.
4. Edits to the guard itself, `.github/CODEOWNERS`, or other
   `.github/workflows/**` files that would weaken enforcement.

Admin bypass is the only path to override.

## Reporting Vulnerabilities

For security issues *in Nanna Coder itself*, use GitHub's private
vulnerability reporting (Security tab → "Report a vulnerability").
Do not file public issues for embargoed CVEs.

For supply-chain issues (compromised dependency, leaked secret, etc.),
contact the maintainers directly first; coordinate disclosure after
remediation.
