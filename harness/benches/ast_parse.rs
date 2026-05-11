//! Criterion benchmark for Rust AST parsing (issue #23).
//!
//! Measures end-to-end parse time on a synthetic Rust source file large
//! enough to exercise `syn`'s item-walking loop, plus a minimal file to
//! establish the per-invocation overhead floor.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use harness::entities::ast::rust::parse_rust_source;

/// Produce a moderate-sized Rust source string with a mix of items so the
/// benchmark measures realistic parsing work (not just lexer startup).
fn realistic_rust_source() -> String {
    let mut s = String::with_capacity(8192);
    s.push_str("use std::collections::HashMap;\nuse std::fmt::Display;\n\n");
    for i in 0..32 {
        s.push_str(&format!(
            "pub struct Item{i} {{ pub id: u64, pub name: String }}\n\
             impl Item{i} {{\n\
             \x20   pub fn new(id: u64) -> Self {{ Self {{ id, name: String::new() }} }}\n\
             \x20   pub async fn load(id: u64) -> Self {{ Self::new(id) }}\n\
             }}\n\n",
            i = i
        ));
    }
    s
}

fn bench_parse_realistic(c: &mut Criterion) {
    let source = realistic_rust_source();
    c.bench_function("ast_parse/realistic", |b| {
        b.iter(|| {
            let summary = parse_rust_source(black_box(&source)).expect("parse");
            black_box(summary);
        });
    });
}

fn bench_parse_minimal(c: &mut Criterion) {
    let source = "fn main() {}\n";
    c.bench_function("ast_parse/minimal", |b| {
        b.iter(|| {
            let summary = parse_rust_source(black_box(source)).expect("parse");
            black_box(summary);
        });
    });
}

criterion_group!(benches, bench_parse_realistic, bench_parse_minimal);
criterion_main!(benches);
