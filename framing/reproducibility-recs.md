# Recommendations for reproducible Rust / multi-language applications using Nix

Compiled framing notes; not authoritative project documentation.

- Use Nix flakes as the central declarative tool for managing dependencies, toolchains, and build environments consistently across Rust, TypeScript, or other languages. Flakes allow unified dependency declarations and fully reproducible builds with identical environments for all developers and CI systems.
- In your flake configuration, use overlays to pin exact versions of core languages and tools such as Rust, Node.js, and yarn. This prevents version conflicts and ensures everyone uses the same toolchains.
- Leverage binary caches like Cachix to greatly speed up builds by reusing precompiled artifacts across your team or CI. This is essential for large Rust projects or multi-language builds.
- Structure your Rust builds to separate dependency compilation from source code builds. Build and cache dependencies once, then build your source faster on subsequent builds. This also benefits continuous integration workflows.
- Programmatically coordinate between Rust and other languages' build steps (e.g., compiling Rust to WebAssembly and then consuming it in TypeScript). Nix flakes allow you to automate these cross-language workflows in a single reproducible pipeline.
- Use tools like `rust-overlay` for advanced Rust toolchain management inside Nix, which simplifies specifying Rust versions and integrating Cargo dependencies into Nix builds.
- Adopt Nix flakes incrementally if you are new, starting with the critical Rust build pipelines, then expand as your team grows more comfortable.
- Develop deployment and packaging pipelines using Nix's hermetic build system to ensure reproducible Rust builds on any platform without being affected by system-level dependencies.
- For multi-stage builds involving multiple languages (Rust, TypeScript, Kotlin, etc.), create isolated, composable Nix derivations or flakes that produce artifacts consumable by downstream builds, ensuring each step is reproducible and versioned.

These practices together deliver reliable, reproducible builds and development environments for complex multi-language applications centered on Rust. They reduce build failures, improve developer onboarding, and enable consistent CI and deployment.

## Sources

- https://sgolovin.live/managing-multi-language-development-environments-with-nix-flakes
- https://filtra.io/rust/interviews/flox-mar-24
- https://www.reddit.com/r/rust/comments/a27fhg/looking_for_documentation_on_deploying_rust/
- https://tweag.io/blog/2022-09-22-rust-nix/
- https://community.fly.io/t/running-reproducible-rust-a-fly-and-nix-love-story/3781
- https://discourse.nixos.org/t/using-nix-for-a-multi-stage-build-with-multiple-languages-involved/26807
- https://discourse.nixos.org/t/introducing-brioche-a-new-nix-like-package-manager-written-in-rust/46447
- https://www.youtube.com/watch?v=Ss1IXtYnpsg
- https://dennybritz.com/posts/adopting-nix/
- https://github.com/nix-community/awesome-nix
