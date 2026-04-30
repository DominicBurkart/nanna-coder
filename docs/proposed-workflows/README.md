# Proposed CI workflow updates (round 6)

Round 5 still failed three different ways:

| Lane    | Failed step           | Root cause |
|---------|-----------------------|---|
| Linux   | `Run installer`       | `image_loaded()` in install.sh tolerated the `localhost/` prefix for the *existence check* — but install.sh continued passing the bare ref to `podman run`, which podman 4+ rejects under enforcing short-name resolution. |
| macOS   | `Start colima`        | `--vm-type qemu` cross-arch emulation is unreliable on GHA arm64. |
| Windows | `Load images in WSL`  | Failure is opaque (no diagnostics). The earlier WSL setup did succeed. |

## Round 6 fixes

* **`scripts/install.sh`**: replaced `image_loaded()` with `resolve_image_ref()`
  that tries the bare ref, then `localhost/…`, then `docker.io/library/…`,
  then a `podman images` substring scan, and **rewrites `HARNESS_IMAGE` /
  `OLLAMA_IMAGE` in-place** to the resolved form. Subsequent
  `podman run "$OLLAMA_IMAGE"` etc. all use the canonical ref.
* **macOS**: switch from `--vm-type=qemu` to `--vm-type=vz --vz-rosetta`.
  macOS 13+ supports the Apple Virtualization framework with Rosetta 2,
  which lets a vz VM run x86_64 binaries on Apple Silicon at near-native
  speed. The colima start step also dumps `serial.log` + `ha.stderr.log`
  on failure for diagnostics.
* **Linux + Windows**: removed the workflow-side retag (now redundant —
  install.sh handles the resolution); added `set -x` and dumps
  (`podman info`, `dpkg -l | grep podman`, `uname -a`) so any future
  failure has context.

To activate:
```bash
mv docs/proposed-workflows/install-test.yml     .github/workflows/install-test.yml
mv docs/proposed-workflows/install-nightly.yml  .github/workflows/install-nightly.yml
git add .github/workflows/
git commit -m "ci(install): apply round-6 fixes"
git push
```

(Parked here because the Claude GitHub App lacks `workflows` permission.)
