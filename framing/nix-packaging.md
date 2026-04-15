# Nix Packaging: Containerized Rust Apps with Podman

This document summarizes the architectural approach for building and distributing containerized Rust applications (including external services like Ollama) using Nix, with complete runtime isolation.

## Core Approach

- Build the Rust binary with `rustPlatform.buildRustPackage`, resolving all Cargo dependencies through Nix for reproducible builds.
- Build a minimal OCI container image with Nix (`buildImage` / `buildLayeredImage`) containing the Rust binary and all runtime dependencies — no system packages, no external downloads at runtime.
- Package external services (e.g., Ollama) as separate Nix-built containers; communicate over defined network interfaces.
- Orchestrate multiple containers with **Podman pods** or **systemd + Quadlet**.

## Example Nix Flake

```nix
{
  description = "Rust app with Nix container";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
    in {
      packages.${system}.myRustApp = pkgs.rustPlatform.buildRustPackage {
        pname = "myRustApp";
        version = "0.1.0";
        src = ./.;
        cargoLock = ./Cargo.lock;
      };

      packages.${system}.myRustAppImage = pkgs.buildImage {
        name = "myrustapp-container";
        contents = [ self.packages.${system}.myRustApp ];
        config = {
          Cmd = [ "${self.packages.${system}.myRustApp}/bin/myRustApp" ];
          Env = [ "RUST_LOG=info" ];
        };
      };
    };
}
```

Build and run:

```bash
nix build .#myRustApp
nix build .#myRustAppImage
podman load < result
podman run --rm -it myrustapp-container
```

## Multi-Container Orchestration

### Podman Pods

Containers in a pod share a network namespace and communicate over `localhost`:

```bash
podman pod create --name app-pod -p 8080:8080
podman create --pod app-pod --name rust-app myrustapp-container
podman create --pod app-pod --name ollama-service ollama-container
podman pod start app-pod
```

### Systemd + Quadlet

Quadlet creates systemd services from Podman container definitions.

`ollama-service.container`:
```ini
[Unit]
Description=Ollama Service Container

[Container]
Image=ollama-container
PublishPort=11434:11434
Restart=always

[Install]
WantedBy=default.target
```

`rust-app.container`:
```ini
[Unit]
Description=Rust Application Container
Requires=ollama-service.service
After=ollama-service.service

[Container]
Image=myrustapp-container
PublishPort=8080:8080
Restart=always
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
```

```bash
systemctl --user enable rust-app.service ollama-service.service
systemctl --user start rust-app.service ollama-service.service
```

## Architectural Constraints

- The Nix flake must declare the **complete closure** of dependencies for each container image.
- Cross-compilation is required per target architecture; use QEMU emulation as a fallback for unsupported native builds.
- GPU drivers and kernel modules cannot be packaged inside Nix container images — expose GPU devices at runtime via Podman device passthrough (`--device /dev/dri`, `--gpus all`).
- GPU user-space libraries (CUDA, ROCm, Mesa) can be packaged inside the container image for a consistent runtime.
- Podman pods and Quadlet require explicit network, volume, and lifecycle configuration.

## Portability

| Target | Support |
|---|---|
| x86_64 Linux | Native |
| aarch64 Linux / Apple Silicon | Native (build on host or cross-compile) |
| macOS / Windows | Linux containers via Podman VM |
| NVIDIA/AMD GPU | Linux hosts with device passthrough; limited on macOS/Windows |

## References

- [buildRustPackage docs](https://nixos.org/manual/nixpkgs/stable/#rust)
- [nix2container](https://github.com/nlewo/nix2container)
- [Podman on NixOS](https://wiki.nixos.org/wiki/Podman)
- [Quadlet guide](https://blog.stackademic.com/awesome-container-orchestration-with-quadlet-podman-for-the-win-e4bce5dd217f)
- [Building a Rust service with Nix](https://fasterthanli.me/series/building-a-rust-service-with-nix)
