# Proposed CI workflow updates (round 5)

Round 4's bring-up lanes still failed, three different ways:

| Lane    | Failed step                                | Real root cause |
|---------|--------------------------------------------|---|
| Linux   | `Run installer` (exit 1)                   | `podman tag bare-name` normalizes back to `localhost/bare-name`, so the round-4 retag step was a no-op. `install.sh`'s exact-match `podman image exists nanna-coder-harness:latest` still failed. |
| macOS   | `Start colima with podman runtime, x86_64` | qemu wasn't pulled by `brew install colima`. `colima --vm-type qemu` requires `qemu-system-x86_64` to be on PATH. |
| Windows | `Set up WSL2 (Ubuntu 22.04)`               | GHA cache service was returning 400 ("Our services aren't available right now"); `Vampire/setup-wsl@v3` failed on cache restore. |

## Round 5 fixes

* **`scripts/install.sh`** (already pushed): added `image_loaded()` that
  tolerates `localhost/`, `docker.io/library/` prefixes and falls back to
  a `podman images` substring scan. Removes the brittleness from the
  workflow-side retag entirely.
* **macOS**: `brew install … qemu` explicitly; assert `qemu-system-x86_64`
  on PATH; pass `--verbose` to `colima start` so any future failure shows
  a real reason.
* **Windows**: `Vampire/setup-wsl@v3` invoked with `use-cache: 'false'`
  to dodge the GHA cache outage. Slower but doesn't depend on a flaky
  external service.

To activate:
```bash
mv docs/proposed-workflows/install-test.yml     .github/workflows/install-test.yml
mv docs/proposed-workflows/install-nightly.yml  .github/workflows/install-nightly.yml
git add .github/workflows/
git commit -m "ci(install): apply round-5 fixes"
git push
```
