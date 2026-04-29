# Proposed CI workflow updates (round 3)

These are **updated** versions of `install-test.yml` and `install-nightly.yml`
with full pod bring-up on Linux, macOS, and Windows. Parked here because the
Claude GitHub App lacks `workflows` permission. Activate with:

```bash
mv docs/proposed-workflows/install-test.yml     .github/workflows/install-test.yml
mv docs/proposed-workflows/install-nightly.yml  .github/workflows/install-nightly.yml
git add .github/workflows/
git commit -m "ci(install): apply round-3 fixes (full bring-up on all 3 OSes)"
git push
```

## Full bring-up on all 3 OSes

Architecture: **build once on Linux, ship via artifacts.** The container
images are built on `ubuntu-latest` with `nix build .#harnessImage` /
`.#ollamaImage`, exported to portable docker-archive tarballs, and uploaded
as a workflow artifact. Each per-OS bring-up job downloads the artifact and
loads it into its local podman.

| Job              | Runner          | Container runtime          | Notes |
|------------------|-----------------|----------------------------|---|
| `build-images`   | ubuntu-latest   | Docker (nix2container)     | Produces `harness.tar`, `ollama.tar` artifacts. |
| `linux-bringup`  | ubuntu-latest   | Native podman              | apt-installed podman, podman-load tarballs. |
| `macos-bringup`  | macos-latest    | colima w/ podman runtime, x86_64 emulation | `colima start --runtime podman --arch x86_64` provides a reliable Linux VM. The x86_64 emulation lets the same x86_64-linux containers we built on ubuntu run inside on the M1 mac. |
| `windows-bringup`| windows-latest  | Native podman inside WSL2 Ubuntu-22.04 | `Vampire/setup-wsl@v3` provisions Ubuntu 22.04 with podman + uidmap + slirp4netns from `additional-packages`. No nix in WSL — the artifacts are tarballs. |

`install.sh` learned `NANNA_SKIP_PODMAN_MACHINE=1` so the macOS lane can
neuter the script's built-in `podman machine` setup (since colima already
provides the VM).

## Why round 2 didn't ship

Round 2 downgraded macOS + Windows to `--dry-run` smoke tests. The user
explicitly required full bring-up on all three OSes in this PR, so round 3
puts that back. The remaining downgrades from round 1:

- macOS no longer uses the script's `ensure_podman_machine_macos` — colima
  is far more reliable on GHA arm64 runners. The script's path is still
  exercised on real macOS user machines (it's just gated behind the env
  flag for CI).
- Windows no longer installs Nix inside WSL — that step was the main
  failure in round 1. Building once on Linux and shipping the artifact
  to WSL is faster and more reliable.

## What round 3 keeps

- `shellcheck` lints `install.sh`.
- `ps1-lint` runs `PSScriptAnalyzer` on `install.ps1`.
- `ps1-parse` parses `install.ps1` with the PowerShell tokenizer (catches
  syntax errors without executing).
- `install-nightly.yml` runs full Gemma 4 pull + smoke daily on Linux.
