//! Criterion benchmark for the AGENTS.md loader (issue #231).
//!
//! Measures end-to-end load + UTF-8 validation time on a realistic ~4 KiB
//! `AGENTS.md` file. The benchmark allocates a fresh tempdir per iteration
//! group so cold-cache page-fault costs are included and the numbers reflect
//! what a session start actually pays.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use harness::agent::agents_md::load;
use std::fs;
use tempfile::TempDir;

/// Produce a ~4 KiB AGENTS.md body with realistic markdown structure so
/// UTF-8 validation and allocation costs reflect production content rather
/// than a pathological all-ASCII blob.
fn realistic_agents_md() -> String {
    let header = "# AGENTS.md\n\n\
        This repository is configured for agent-assisted development. The\n\
        harness uses the guidance in this file to configure itself at session\n\
        start.\n\n\
        ## Build & test\n\n\
        - `nix develop --command cargo nextest run --workspace` — all tests.\n\
        - `nix develop --command cargo clippy --all-targets -- -D warnings`.\n\n\
        ## Conventions\n\n";
    let bullet = "- Prefer integration tests over unit tests when the seam is stable.\n";
    let mut s = String::with_capacity(4096);
    s.push_str(header);
    while s.len() < 4096 {
        s.push_str(bullet);
    }
    s
}

fn bench_load_agents_md(c: &mut Criterion) {
    let dir = TempDir::new().expect("tempdir");
    let body = realistic_agents_md();
    fs::write(dir.path().join("AGENTS.md"), &body).expect("write AGENTS.md");

    c.bench_function("agents_md::load_4kib", |b| {
        b.iter(|| {
            let doc = load(black_box(dir.path())).expect("load ok");
            black_box(doc);
        });
    });
}

criterion_group!(benches, bench_load_agents_md);
criterion_main!(benches);
