# Reproducibility Recommendations for Rust / Multi-Language Apps with Nix

## Key Practices

- **Use Nix flakes** as the central declarative tool for managing dependencies, toolchains, and build environments. Flakes provide unified dependency declarations and fully reproducible builds with identical environments across developers and CI.
- **Pin exact versions** of core toolchains (Rust, Node.js, etc.) via overlays in your flake configuration to prevent version drift.
- **Use Cachix** (or another binary cache) to share precompiled artifacts across the team and CI, avoiding redundant recompilation of large Rust projects.
- **Separate dependency compilation from source builds**: build and cache dependencies once, then compile source incrementally.
- **Use `rust-overlay`** for advanced Rust toolchain management inside Nix (simplifies specifying Rust versions and integrating Cargo).
- **Produce OCI containers from Nix** using `buildImage` / `buildLayeredImage` to guarantee reproducible, hermetic runtime environments.
- **Compose derivations per language**: for multi-language pipelines (Rust, TypeScript, etc.), create isolated Nix derivations whose outputs are consumed by downstream builds.

## References

- [Managing Rust dependencies with Nix (Part I)](https://hadean.com/blog/managing-rust-dependencies-with-nix-part-i/)
- [Managing Rust dependencies with Nix (Part II)](https://hadean.com/blog/managing-rust-dependencies-with-nix-part-ii/)
- [How to package a Rust app with Nix](https://dev.to/misterio/how-to-package-a-rust-app-using-nix-3lh3)
- [rust-overlay](https://github.com/oxalica/rust-overlay)
- [Tweag: Rust + Nix](https://tweag.io/blog/2022-09-22-rust-nix/)
- [Managing multi-language dev environments with Nix flakes](https://sgolovin.live/managing-multi-language-development-environments-with-nix-flakes)
