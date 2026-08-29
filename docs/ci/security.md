# CI Security

The trust boundary of nanna-coder's CI, the secrets it holds, the scans it
runs, and the guarantees it does and does not provide.

## Trust model

- **Trusted:** Commits from `main`, from maintainer branches on
  `DominicBurkart/nanna-coder`, from tagged releases.
- **Untrusted:** Pull requests from forks. These run in the standard
  `pull_request` event, which withholds repository secrets from workflows
  (GitHub's default), and additional in-repo guards suppress side effects.

## Secrets

Held as GitHub Actions repository secrets. Never committed.

| Secret | Used by | Rotate on |
|--------|---------|-----------|
| `CACHIX_AUTH` | `cachix/cachix-action@v15` in every Linux job that installs Nix | Compromise; annual hygiene |
| `CODECOV_TOKEN` | `codecov/codecov-action@v5` in `test-matrix (security)` | Compromise; annual hygiene |
| `GITHUB_TOKEN` | Auto-provisioned per-run by GitHub | N/A (per-run token) |

Rotation procedure: see
[`maintenance.md`](maintenance.md#rotate-cachix_auth--codecov_token).

## Fork-safe workflow patterns

Fork PRs cannot access `secrets.*`. Beyond that, each workflow that pushes
side effects gates them explicitly:

- **`cachix-action` push:** Every invocation sets
  `skipPush: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.fork }}`.
  This means fork PRs can pull from the cache but do not push, so a fork PR
  cannot poison the cache.
- **Container registry push:** `build-containers` gates its push step on
  `if: github.event_name != 'pull_request'`. Fork PRs still build the image
  (so the build is exercised) but do not push, so a fork PR cannot push
  arbitrary bytes to `ghcr.io/DominicBurkart/nanna-coder/*`.
- **Trivy scan upload:** `security-scan` runs only on non-PR events. It
  needs a published image tag to scan.
- **Release upload:** `release` runs only on `release: types: [published]`,
  which requires maintainer action.

## Permissions

Workflows set explicit `permissions:` blocks where they need more than
read-only. Current escalations:

| Workflow / job | Permission | Why |
|----------------|------------|-----|
| `build-containers` | `contents: read, packages: write` | Push to GHCR. |
| `security-scan` | `contents: read, security-events: write` | Upload SARIF to code-scanning. |
| `eval` | `contents: read, pull-requests: write` | Optionally post results comment on a PR. |
| `codecov-guard` | `contents: read` (implicit only-read) | Read-only. |
| `badges` | `contents: write` | Commit regenerated SVGs to `main`. |

If a new job needs more than default read permissions, add an explicit
`permissions:` block, do not rely on the workflow default.

## What gets scanned

- **Trivy vulnerability scan** ([`ci.yml`](../../.github/workflows/ci.yml)
  `security-scan` job) — scans the pushed `harness:latest` image for
  vulnerable dependencies. SARIF is uploaded to GitHub code-scanning.
- **Cargo dep advisories** — `cargo audit` and `cargo deny` are currently
  **disabled** in `ci.yml`'s `security` step due to a CVSS 4.0
  incompatibility. They still run locally via `.cargo-husky/hooks/pre-commit`
  when the binaries are installed. Re-enable when upstream tooling supports
  CVSS 4.0.
- **License / dep bans** — `deny.toml` allowlist is enforced locally via
  pre-commit; ignored advisories are tracked (currently
  `RUSTSEC-2025-0134` → issue #40).
- **Pre-commit secret scan** — `.cargo-husky/hooks/pre-commit` optionally
  runs a Claude-based security review on the staged diff if the `claude`
  CLI is present. See `security review` block in the hook.

## What does not get scanned (and how we cope)

- **PR fork code before it runs on `main`.** Standard GitHub `pull_request`
  event with fork-safe patterns (above) is the mitigation. Reviewers should
  read fork PRs for workflow modifications before merging.
- **Untrusted network egress from test jobs.** Assumed benign because tests
  only reach `registry-1.docker.io` (via nix2container push), `cache.nixos.org`,
  `cachix.org`, `crates.io`, and GitHub. If a new dependency reaches
  elsewhere, treat it as a review flag.
- **`nix-installer-action@main` upstream.** Tracks `main` intentionally,
  since patched versions are usually rolled out via `main`. If upstream is
  ever compromised, pin to a known-good SHA and open an issue.

## Codecov guard (integrity of coverage policy)

[`codecov-guard.yml`](../../.github/workflows/codecov-guard.yml) protects
against silent weakening of the coverage floor:

- Base ref of the guard is `origin/${{ github.base_ref }}` on PRs, `HEAD~1`
  on push to `main`.
- Fails closed: unresolvable base ref → error.
- Rejects: numeric-target regression, target → `auto` swap, target removal,
  `ignore:` growth, `strict_yaml_branch` removal, first-time `codecov.yml`
  addition (admin merge only).
- Cannot be bypassed by editing `codecov-guard.yml` itself — the guard is
  triggered on paths that include its own file.

Corresponding agent policy: [`AGENTS.md`](../../AGENTS.md).

## Reporting a vulnerability

If you find a vulnerability in the CI configuration (secret exposure,
privilege escalation, sandbox escape via workflow injection, etc.):

1. Do not open a public issue.
2. Contact the repo owner (`@DominicBurkart`) directly.
3. Do not push a proof-of-concept to any branch of this repo.

## Practical guidance for contributors

- Use secrets only through `${{ secrets.NAME }}`. Never `echo` them.
- Avoid `run: |` blocks that pipe untrusted input through `bash -c` or
  `eval`. GitHub Actions workflow injection via user-controlled fields
  (PR titles, issue bodies, branch names) is a real class of vulnerability.
- Never add a workflow trigger of `pull_request_target` without extremely
  careful review — it grants secrets to fork PRs.
- Prefer pinning third-party actions by SHA when the trust cost of `@main`
  or `@vN` is unacceptable.

## See also

- [`architecture.md`](architecture.md) — the pipeline structure being secured.
- [`maintenance.md`](maintenance.md) — secret rotation.
- [`troubleshooting.md`](troubleshooting.md) — fork-PR-specific failure
  modes (cache push, container push, Codecov token).
- [`AGENTS.md`](../../AGENTS.md) — non-negotiable coverage-policy invariants.
