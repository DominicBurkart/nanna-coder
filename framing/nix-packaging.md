# Nix Packaging Notes

This file captures architectural notes on containerizing Rust applications with Nix. The key decisions and patterns are distilled in [framing/reproducibility-recs.md](reproducibility-recs.md).

## Core Pattern

Build both the Rust binary and its OCI container image entirely within Nix:

```nix
packages.myRustApp = pkgs.rustPlatform.buildRustPackage {
  pname = "myRustApp";
  version = "0.1.0";
  src = ./.;
  cargoLock.lockFile = ./Cargo.lock;
};

packages.myRustAppImage = pkgs.dockerTools.buildLayeredImage {
  name = "myrustapp-container";
  contents = [ packages.myRustApp ];
  config = {
    Cmd = [ "${packages.myRustApp}/bin/myRustApp" ];
    Env = [ "RUST_LOG=info" ];
  };
};
```

Build and run:
```bash
nix build .#myRustAppImage
podman load < result
podman run --rm -it myrustapp-container
```

## Key Architectural Constraints

- The Nix flake must declare the **complete closure** of every container: all services, libraries, and tools. Nothing is fetched at container start-up.
- **Cargo.lock must be committed** so `buildRustPackage` can produce a fixed-output derivation.
- External services (e.g., Ollama) are either packaged as separate Nix derivations and included in their own container image, or pulled from an upstream image and orchestrated via Podman pods / systemd Quadlet units.
- Cross-compilation requires explicit per-target configuration in the flake; use `pkgsCross.<target>` or `naersk`/`crane` with cross support.
- GPU kernel drivers are not managed by Nix — they come from the host via device passthrough (`--device`, `--gpus`). GPU user-space libraries (CUDA, ROCm) can be packaged in the Nix image.

## References

- [Hadean: Managing Rust dependencies with Nix](https://hadean.com/blog/managing-rust-dependencies-with-nix-part-i/)
- [dev.to: How to package a Rust app using Nix](https://dev.to/misterio/how-to-package-a-rust-app-using-nix-3lh3)
- [fasterthanli.me: Building a Rust service with Nix](https://fasterthanli.me/series/building-a-rust-service-with-nix)
- [NixOS Wiki: Podman](https://wiki.nixos.org/wiki/Podman)
- [nix2container](https://github.com/nlewo/nix2container)
