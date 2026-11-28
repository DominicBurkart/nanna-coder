nanna-coder is a Rust cargo workspace (harness, image-builder, model, framing, evals, nanna-effects) built inside the Nix devShell.

Check commands, run from the repository root:

- cargo nextest run --workspace --lib --all-features
- cargo nextest run --workspace --test '*' --all-features
- cargo clippy --workspace --all-targets --all-features -- -D warnings
- cargo fmt --all -- --check
- cargo deny check

Write tests before implementation. Do not lower coverage targets. Read ARCHITECTURE.md and TESTING.md before changing harness behaviour.
