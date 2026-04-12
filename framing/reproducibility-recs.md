# Reproducibility Recommendations (Rust + Nix)

Key practices for reproducible Rust / multi-language builds with Nix:

- Use **Nix flakes** as the single source of truth for toolchains, dependencies, and build environments. Flakes pin exact versions and produce identical environments across developer machines and CI.
- Use **overlays** to pin core language versions (Rust, Node.js, etc.) and prevent implicit upgrades.
- Use **binary caches** (e.g., Cachix) to avoid rebuilding large dependencies from source on every machine.
- Build Rust dependencies separately from application source so the dependency layer is cached independently and rebuilt only when `Cargo.lock` changes.
- Use `rust-overlay` for fine-grained Rust toolchain management inside Nix (specific versions, nightly channels).
- For multi-language projects, define each language's build as an isolated, composable Nix derivation; compose them in a top-level flake.
- Use `buildRustPackage` (or `crane`/`naersk`) to integrate Cargo builds into Nix, ensuring dependencies are resolved through Nix rather than Cargo's network fetcher.
- Use hermetic container images built from Nix outputs (via `pkgs.dockerTools.buildLayeredImage`) so production artifacts match the Nix closure exactly.

## Containerized Multi-Service Setup (with Podman)

To run isolated containers (e.g., the Rust harness + Ollama) built entirely from Nix:

```nix
# flake.nix excerpt
packages.myRustApp = pkgs.rustPlatform.buildRustPackage {
  pname = "myRustApp";
  version = "0.1.0";
  src = ./.;
  cargoLock.lockFile = ./Cargo.lock;
};

packages.myRustAppImage = pkgs.dockerTools.buildLayeredImage {
  name = "myrustapp-container";
  contents = [ packages.myRustApp ];
  config.Cmd = [ "${packages.myRustApp}/bin/myRustApp" ];
};
```

```bash
nix build .#myRustAppImage
podman load < result
podman run --rm -it myrustapp-container
```

### Multi-Container Orchestration

**Podman pods** (containers share a network namespace):
```bash
podman pod create --name app-pod -p 8080:8080
podman create --pod app-pod --name rust-app  myrustapp-container
podman create --pod app-pod --name ollama    ollama-container
podman pod start app-pod
```

**Systemd + Quadlet** (production-style service management):
- Write `.container` unit files for each service.
- Express `Requires=` / `After=` dependencies between units.
- Manage with `systemctl --user start|stop|status`.

### GPU Access

Nix manages user-space GPU libraries (CUDA, ROCm, Mesa) inside the container image. Kernel drivers live on the host and are passed through at runtime:

```bash
# NVIDIA
podman run --gpus all myrustapp-container

# AMD / DRI devices
podman run --device /dev/dri myrustapp-container
```

macOS and Windows require a Linux VM layer (e.g., Podman's built-in VM); GPU passthrough through the VM is limited and platform-specific.

### Portability Summary

| Target | Build | Runtime |
|--------|-------|---------|
| x86_64-linux | Native Nix | Native Podman |
| aarch64-linux | Nix (native or cross) | Native Podman |
| aarch64-darwin (Apple Silicon) | Nix (native) | Podman VM (ARM64) |
| x86_64-darwin | Nix (native) | Podman VM (x86_64) |
| GPU (Linux) | Package user-space libs in image | `--gpus` / `--device` passthrough |
