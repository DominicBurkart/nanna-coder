//! Criterion benchmarks for the cargo JSON parsers (issue #24).
//!
//! Measures throughput of [`parse_cargo_test_messages`] and
//! [`parse_clippy_messages`] against the small fixtures shipped under
//! `harness/tests/fixtures/`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use harness::entities::test::{parse_cargo_test_messages, parse_clippy_messages};

const CARGO_TEST_FIXTURE: &str = include_str!("../tests/fixtures/cargo_test_output.json");
const CLIPPY_FIXTURE: &str = include_str!("../tests/fixtures/clippy_output.json");

fn bench_parse_cargo_test(c: &mut Criterion) {
    c.bench_function("parse_cargo_test_messages", |b| {
        b.iter(|| {
            let results =
                parse_cargo_test_messages(black_box(CARGO_TEST_FIXTURE)).expect("fixture parses");
            black_box(results);
        });
    });
}

fn bench_parse_clippy(c: &mut Criterion) {
    c.bench_function("parse_clippy_messages", |b| {
        b.iter(|| {
            let results = parse_clippy_messages(black_box(CLIPPY_FIXTURE)).expect("fixture parses");
            black_box(results);
        });
    });
}

criterion_group!(benches, bench_parse_cargo_test, bench_parse_clippy);
criterion_main!(benches);
