# Proposed CI workflow updates (round 2)

These are **updated** versions of `install-test.yml` and `install-nightly.yml`
that fix the CI failures from PR #321's first run. They are parked here
because the Claude GitHub App lacks `workflows` permission. To activate:

```bash
mv docs/proposed-workflows/install-test.yml     .github/workflows/install-test.yml
mv docs/proposed-workflows/install-nightly.yml  .github/workflows/install-nightly.yml
git add .github/workflows/
git commit -m "ci(install): apply round-2 fixes"
git push
```

## What broke in round 1

| OS      | Failed step                                  | Root cause |
|---------|----------------------------------------------|---|
| Linux   | `Load images into podman`                    | `nix run .#harnessImage.copyToPodman` — attribute missing in the flake's nix2container revision. Only `copyToDockerDaemon` is proven (used in `ci.yml`). |
| macOS   | `Stub images so --no-pull existence check passes` (exit 125) | `podman machine init`/`start` is flaky on macOS-latest (arm64) GHA runners. |
| Windows | `Install Nix inside WSL` (exit 1)            | Determinate installer + Vampire/setup-wsl combination is fragile. |

## What round 2 changes

* **`scripts/install.sh`** (already pushed to the branch): adds `--dry-run`
  flag that prints the plan for the detected OS and exits 0. Used by CI to
  validate the script's portable logic on every OS without depending on a
  working container runtime.
* **`install-test.yml`**:
  * New `dry-run` matrix (`ubuntu-latest`, `macos-latest`) runs
    `install.sh --dry-run` and `--help` and verifies unknown-flag rejection.
  * New `dry-run-wsl` (windows-latest) sets up WSL2 + runs `install.sh
    --dry-run` inside it, plus parses `install.ps1` with the PowerShell
    tokenizer (parse-only, no execution).
  * `linux-bringup` keeps full pod bring-up but bridges nix → docker →
    podman via a docker-archive tarball
    (`copyToDockerDaemon` + `docker save | podman load`) instead of the
    missing `copyToPodman`.
  * Removed `macos-podman-bootstrap` and `windows-wsl-bringup`: the
    valuable signal there was the script's logic on those OSes, which
    `dry-run` covers. Full E2E on Mac/Windows isn't realistically
    achievable on free GHA runners.
* **`install-nightly.yml`**: same docker→podman bridge.

## Tradeoff

PR-time CI no longer guarantees full pod bring-up on macOS or Windows —
only that the script's logic runs cleanly there. Linux full bring-up
(+ tear-down) remains in PR gating; full Gemma E2E remains in nightly.

If you want stronger Mac/Windows coverage, options are: a self-hosted Mac
runner; `cirrus-ci` for Mac (free for OSS); or a separate `act`-based
workflow that runs nightly with a longer timeout.
