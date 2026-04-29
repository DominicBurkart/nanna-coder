# Proposed CI workflows

These two files are GitHub Actions workflows that need to be moved to
`.github/workflows/` to take effect:

- `install-test.yml` — PR-gated matrix that exercises `scripts/install.sh`
  (and `scripts/install.ps1`) on Linux, macOS, and Windows-via-WSL2 with
  `--skip-model-pull`. Includes shellcheck and PSScriptAnalyzer lint jobs
  and a `install-test-gate` aggregator.
- `install-nightly.yml` — Daily + on-demand full E2E on Linux that pulls the
  real Gemma 4 model and runs a `harness models` smoke check.

## Why they're parked here

The Claude GitHub App that authored the install scripts does not have the
`workflows` permission, so it cannot push files under `.github/workflows/`.
A repository maintainer needs to move them into place:

```bash
git mv docs/proposed-workflows/install-test.yml     .github/workflows/
git mv docs/proposed-workflows/install-nightly.yml  .github/workflows/
git commit -m "ci(install): activate install-test + install-nightly workflows"
git push
```

## Notes on review

- `install-test.yml` deliberately stays out of `ci.yml`'s `all-checks` gate
  (which is enforced only over `ci.yml`'s own jobs); it has its own
  `install-test-gate` aggregator. If you want PR merge to require this
  workflow to pass, add it as a required status check in branch protection.
- The Linux + WSL lanes build the harness/ollama container images locally
  via `nix build .#harnessImage` / `.#ollamaImage` and load them into podman
  with `copyToPodman`, then run `install.sh --no-pull` against those local
  images. This avoids any dependency on ghcr package visibility for PR CI.
- The macOS lane only validates the macOS-specific code paths in
  `install.sh` (brew → podman, `podman machine init/start`, image existence
  check, public-registry pull). Full pod bring-up on macOS would require a
  Linux container builder we don't currently have set up; the Linux + WSL
  lanes cover that path.
- The nightly lane is the only place we actually pull Gemma 4 (multi-GB) to
  keep PR CI fast.
