use std::path::{Path, PathBuf};

const HONOURS: &[&str] = &[
    "NANNA_REQUIRE_RUNTIME",
    "skip_or_panic",
    "ensure_runtime_or_skip",
];

const ALLOWED_SKIPS: &[(&str, &str)] = &[
    (
        "model/tests/ollama_chat_integration.rs",
        "answered directly",
    ),
    (
        "harness/tests/swebench_fixture_generation.rs",
        "no vendored dataset",
    ),
    (
        "harness/tests/swebench_fixture_generation.rs",
        "no fixture at",
    ),
    ("harness/src/pod.rs", "in-container env"),
];

#[derive(Debug, PartialEq, Eq)]
struct Violation {
    line: usize,
    text: String,
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

fn is_fn_start(line: &str) -> bool {
    let t = line.trim_start();
    let t = t.strip_prefix("pub ").unwrap_or(t);
    let t = t.strip_prefix("async ").unwrap_or(t);
    t.starts_with("fn ")
}

const RUNTIME_WORDS: &[&str] = &[
    "runtime",
    "container",
    "ollama",
    "podman",
    "docker",
    "provider",
    "llm",
    "model",
];

fn skips_early(line: &str, previous: &str) -> bool {
    if !(line.to_lowercase().contains("skipping") || line.contains("SKIPPED")) {
        return false;
    }
    let context = format!("{previous} {line}").to_lowercase();
    RUNTIME_WORDS.iter().any(|w| context.contains(w))
}

fn checks_runtime_and_returns(body: &[&str]) -> bool {
    body.iter()
        .any(|l| !is_comment(l) && l.contains("is_available()"))
        && body.iter().any(|l| l.trim() == "return;")
}

fn violations(rel_path: &str, source: &str, is_test_file: bool) -> Vec<Violation> {
    let lines: Vec<&str> = source.lines().collect();
    let first = if is_test_file {
        0
    } else {
        match lines.iter().position(|l| l.trim() == "#[cfg(test)]") {
            Some(i) => i,
            None => return Vec::new(),
        }
    };
    let mut out = Vec::new();
    let mut idx = first;
    while idx < lines.len() {
        if !is_fn_start(lines[idx]) {
            idx += 1;
            continue;
        }
        let start = idx;
        let mut end = idx + 1;
        while end < lines.len() && !is_fn_start(lines[end]) {
            end += 1;
        }
        let body = &lines[start..end];
        let honoured = body
            .iter()
            .any(|l| !is_comment(l) && HONOURS.iter().any(|h| l.contains(h)));
        if !honoured {
            for (offset, line) in body.iter().enumerate() {
                if is_comment(line) {
                    continue;
                }
                let allowed = ALLOWED_SKIPS
                    .iter()
                    .any(|(f, m)| *f == rel_path && line.contains(m));
                let previous = if offset > 0 { body[offset - 1] } else { "" };
                if skips_early(line, previous) && !allowed {
                    out.push(Violation {
                        line: start + offset + 1,
                        text: line.trim().to_string(),
                    });
                }
            }
            if is_test_file && checks_runtime_and_returns(body) {
                out.push(Violation {
                    line: start + 1,
                    text: lines[start].trim().to_string(),
                });
            }
        }
        idx = end;
    }
    out.sort_by_key(|v| v.line);
    out.dedup();
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn flags_skip_without_runtime_gate() {
    let src = "#[test]\nfn t() {\n    eprintln!(\"no runtime, skipping\");\n    return;\n}\n";
    let v = violations("x/tests/a.rs", src, true);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].line, 3);
}

#[test]
fn flags_early_return_on_unavailable_runtime_without_message() {
    let src = "#[test]\nfn t() {\n    if !rt.is_available() {\n        return;\n    }\n}\n";
    assert_eq!(violations("x/tests/a.rs", src, true).len(), 1);
}

#[test]
fn accepts_helper_gate() {
    let src = "#[test]\nfn t() {\n    if !ensure_runtime_or_skip(&rt, \"t\") {\n        return;\n    }\n}\n";
    assert!(violations("x/tests/a.rs", src, true).is_empty());
}

#[test]
fn accepts_explicit_env_gate() {
    let src = "fn t() {\n    if std::env::var(\"NANNA_REQUIRE_RUNTIME\").is_ok() { panic!(); }\n    eprintln!(\"skipping\");\n}\n";
    assert!(violations("x/tests/a.rs", src, true).is_empty());
}

#[test]
fn ignores_skips_unrelated_to_runtimes() {
    let src = "fn t() {\n    eprintln!(\"Skipping: git not available\");\n}\n";
    assert!(violations("x/tests/a.rs", src, true).is_empty());
}

#[test]
fn ignores_comments_and_non_test_source() {
    let src = "/// skipping notifications\nfn f() {\n    // skipping\n}\n";
    assert!(violations("x/src/a.rs", src, false).is_empty());
    assert!(violations("x/tests/a.rs", src, true).is_empty());
}

#[test]
fn only_scans_after_cfg_test_in_src() {
    let src = "fn f() {\n    warn!(\"ollama skipping\");\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        eprintln!(\"ollama skipping\");\n    }\n}\n";
    let v = violations("x/src/a.rs", src, false);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].line, 8);
}

#[test]
fn honouring_one_fn_does_not_excuse_the_next() {
    let src = "fn a() {\n    skip_or_panic(\"x\");\n}\nfn b() {\n    eprintln!(\"ollama down, skipping\");\n}\n";
    assert_eq!(violations("x/tests/a.rs", src, true).len(), 1);
}

#[test]
fn workspace_runtime_dependent_tests_honour_require_runtime() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for dir in ["harness/src", "harness/tests", "model/src", "model/tests"] {
        rust_files(&root.join(dir), &mut files);
    }
    files.sort();
    assert!(
        files.len() > 10,
        "scan found too few files: {}",
        files.len()
    );
    let mut report = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        if rel == "harness/tests/runtime_skip_lint.rs" {
            continue;
        }
        let is_test_file = rel.contains("/tests/");
        let source = std::fs::read_to_string(&file).unwrap();
        for v in violations(&rel, &source, is_test_file) {
            report.push(format!("{rel}:{}: {}", v.line, v.text));
        }
    }
    assert!(
        report.is_empty(),
        "runtime-dependent tests must call harness::container::ensure_runtime_or_skip or skip_or_panic (honouring NANNA_REQUIRE_RUNTIME) instead of skipping silently:\n{}",
        report.join("\n")
    );
}
