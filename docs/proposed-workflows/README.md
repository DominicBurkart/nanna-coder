# Proposed CI workflow updates (round 4)

Round 3 fixed the architecture (build once, ship via artifacts) but each
of the three bring-up lanes hit a different runtime issue. Round 4 fixes
all three.

To activate:
```bash
mv docs/proposed-workflows/install-test.yml     .github/workflows/install-test.yml
mv docs/proposed-workflows/install-nightly.yml  .github/workflows/install-nightly.yml
git add .github/workflows/
git commit -m "ci(install): apply round-4 fixes"
git push
```

(Parked here because the Claude GitHub App lacks `workflows` permission.)

## What broke in round 3

| Lane    | Failed step                                          | Cause |
|---------|------------------------------------------------------|---|
| Linux   | `Run installer`                                      | `podman load` of a docker-archive saved as `nanna-coder-harness:latest` stores it as `localhost/nanna-coder-harness:latest`. `install.sh`'s `podman image exists nanna-coder-harness:latest` is an exact-match check (per podman docs) and failed. |
| macOS   | `Start colima with podman runtime, x86_64 arch`      | colima default `--vm-type=vz` (Apple Virtualization) cannot run x86_64 on arm64. Cross-arch emulation requires `--vm-type=qemu`. |
| Windows | `Load images inside WSL`                             | Inside `wsl-bash`, `$GITHUB_WORKSPACE` is the *Windows* path (`D:\a\…`). `cd "$GITHUB_WORKSPACE"` fails in bash without translation. |

## Round 4 fixes

* **Linux + macOS + Windows**: after `podman load`, find the loaded image
  ref via `podman images --format` and `podman tag` it to the bare
  `nanna-coder-{harness,ollama}:latest` so the installer's exact-match
  existence check resolves.
* **macOS**: `colima start --vm-type qemu --arch x86_64 …`.
* **Windows**: `ws=$(wslpath -u "$GITHUB_WORKSPACE"); cd "$ws"` before any
  file work inside WSL.

`install-nightly.yml` gets the same retag normalization on its docker-bridge
load step.

`scripts/install.sh` is unchanged from round 3 (it already exposes
`NANNA_SKIP_PODMAN_MACHINE` for the colima lane).
