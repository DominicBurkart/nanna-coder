use std::path::{Path, PathBuf};

const HELPERS: &[&str] = &["skip_or_panic", "ensure_runtime_or_skip"];

const IGNORE_PREFIXES: &[&str] = &["container_", "ollama_", "manual_"];

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
    ("harness/src/pod.rs", "InsideContainer skip"),
];

#[derive(Debug, PartialEq, Eq)]
struct Violation {
    line: usize,
    text: String,
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
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

const GUARD_WORDS: &[&str] = &["runtime", "podman", "docker", "ollama", "11434", "provider"];

const GUARD_SHAPES: &[&str] = &["if ", "else", "match ", "err(", "none"];

fn has_helper(lines: &[&str]) -> bool {
    lines
        .iter()
        .any(|l| !is_comment(l) && HELPERS.iter().any(|h| l.contains(h)))
}

fn has_early_return(lines: &[&str]) -> bool {
    lines.iter().any(|l| {
        !is_comment(l)
            && (l.contains("return;")
                || l.contains("return Ok(")
                || l.contains("return ()")
                || l.contains("return }")
                || l.contains("return,"))
    })
}

fn prose(line: &str) -> String {
    let mut out: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
    if let Some(pos) = line.find("//") {
        out.push(&line[pos..]);
    }
    out.join(" ").to_lowercase()
}

fn skip_wording_near_runtime(lines: &[&str], idx: usize) -> bool {
    let line = prose(lines[idx]);
    if !line.contains("skip") {
        return false;
    }
    let previous = if idx > 0 { lines[idx - 1] } else { "" };
    let context = format!("{previous} {line}").to_lowercase();
    RUNTIME_WORDS.iter().any(|w| context.contains(w))
}

fn guard_window<'a>(lines: &'a [&'a str], idx: usize) -> &'a [&'a str] {
    let cap = (idx + 6).min(lines.len());
    let mut end = idx + 1;
    while end < cap {
        let closes = lines[end].trim_start().starts_with('}');
        end += 1;
        if closes {
            break;
        }
    }
    &lines[idx..end]
}

fn silent_runtime_exit(lines: &[&str], idx: usize) -> bool {
    let lower = lines[idx].to_lowercase();
    let runtime_related = if lower.contains("is_available") {
        lower.contains('!')
    } else {
        GUARD_WORDS.iter().any(|w| lower.contains(w))
    };
    if !GUARD_SHAPES.iter().any(|s| lower.contains(s)) || !runtime_related {
        return false;
    }
    let window = guard_window(lines, idx);
    has_early_return(window) && !has_helper(window)
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
    for idx in first..lines.len() {
        let line = lines[idx];
        if is_comment(line) {
            continue;
        }
        let allowed = ALLOWED_SKIPS
            .iter()
            .any(|(f, m)| *f == rel_path && line.contains(m));
        if allowed {
            continue;
        }
        let near_helper = has_helper(&lines[idx.saturating_sub(2)..=idx]);
        let skip_without_helper = skip_wording_near_runtime(&lines, idx) && !near_helper;
        if skip_without_helper || silent_runtime_exit(&lines, idx) {
            out.push(Violation {
                line: idx + 1,
                text: line.trim().to_string(),
            });
        }
    }
    out
}

fn ignored_tests_outside_convention(source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if is_comment(line) || !line.trim_start().starts_with("#[ignore") {
            continue;
        }
        let name = lines[idx + 1..].iter().find_map(|l| {
            let t = l.trim_start();
            if t.starts_with("#[") || t.starts_with("//") {
                return None;
            }
            let t = t.strip_prefix("pub ").unwrap_or(t);
            let t = t.strip_prefix("async ").unwrap_or(t);
            t.strip_prefix("fn ")
                .map(|r| r.split('(').next().unwrap_or("").trim().to_string())
        });
        if let Some(name) = name {
            if !IGNORE_PREFIXES.iter().any(|p| name.starts_with(p)) {
                out.push(name);
            }
        }
    }
    out
}

fn job_section<'a>(workflow: &'a str, job: &str) -> Option<&'a str> {
    let header = format!("  {job}:");
    let mut offset = 0;
    let mut start = None;
    for line in workflow.split_inclusive('\n') {
        let top_level_key =
            line.starts_with("  ") && !line.starts_with("   ") && line.trim_end().ends_with(':');
        if let Some(s) = start {
            if top_level_key {
                return Some(&workflow[s..offset]);
            }
        } else if line.trim_end() == header {
            start = Some(offset);
        }
        offset += line.len();
    }
    start.map(|s| &workflow[s..])
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
fn flags_plain_skip_wording_near_runtime() {
    let src = "fn t() {\n    eprintln!(\"no docker, skip\");\n}\n";
    assert_eq!(violations("x/tests/a.rs", src, true).len(), 1);
}

#[test]
fn flags_skip_colon_no_docker() {
    let src = "fn t() {\n    println!(\"SKIP: no docker\");\n}\n";
    assert_eq!(violations("x/tests/a.rs", src, true).len(), 1);
}

#[test]
fn flags_skip_without_runtime_gate() {
    let src = "#[test]\nfn t() {\n    eprintln!(\"no runtime, skipping\");\n    return;\n}\n";
    let v = violations("x/tests/a.rs", src, true);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].line, 3);
}

#[test]
fn flags_silent_which_is_err_return() {
    let src = "fn t() {\n    if which::which(\"podman\").is_err() {\n        return;\n    }\n}\n";
    assert_eq!(violations("x/tests/a.rs", src, true).len(), 1);
}

#[test]
fn flags_silent_let_else_return() {
    let src =
        "fn t() {\n    let Ok(p) = which::which(\"podman\") else {\n        return;\n    };\n}\n";
    assert_eq!(violations("x/tests/a.rs", src, true).len(), 1);
}

#[test]
fn flags_one_line_negated_return() {
    let src = "fn t() {\n    if !runtime.is_available() { return; }\n}\n";
    assert_eq!(violations("x/tests/a.rs", src, true).len(), 1);
}

#[test]
fn flags_early_return_on_unavailable_runtime_without_message() {
    let src = "#[test]\nfn t() {\n    if !rt.is_available() {\n        return;\n    }\n}\n";
    assert_eq!(violations("x/tests/a.rs", src, true).len(), 1);
}

#[test]
fn flags_silent_match_err_return_for_provider() {
    let src = "fn t() {\n    let p = match OllamaProvider::new() {\n        Ok(p) => p,\n        Err(_) => return,\n    };\n}\n";
    let v = violations("x/tests/a.rs", src, true);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].line, 2);
}

#[test]
fn accepts_helper_gate() {
    let src = "#[test]\nfn t() {\n    if !ensure_runtime_or_skip(&rt, \"t\") {\n        return;\n    }\n}\n";
    assert!(violations("x/tests/a.rs", src, true).is_empty());
}

#[test]
fn accepts_skip_or_panic_before_return() {
    let src = "fn t() {\n    if which::which(\"podman\").is_err() {\n        skip_or_panic(\"podman not on PATH\");\n        return;\n    }\n}\n";
    assert!(violations("x/tests/a.rs", src, true).is_empty());
}

#[test]
fn accepts_multiline_helper_call_with_skip_wording() {
    let src = "fn t() {\n    if r.is_err() {\n        skip_or_panic(\n            \"ollama skipping reason\",\n        );\n        return;\n    }\n}\n";
    assert!(violations("x/tests/a.rs", src, true).is_empty());
}

#[test]
fn mentioning_a_honour_word_does_not_exempt_the_function() {
    let src = "fn t() {\n    let _ = \"NANNA_REQUIRE_RUNTIME skip_or_panic\";\n    if !rt.is_available() {\n        eprintln!(\"skipping\");\n        return;\n    }\n}\n";
    assert!(!violations("x/tests/a.rs", src, true).is_empty());
}

#[test]
fn honouring_one_fn_does_not_excuse_the_next() {
    let src = "fn a() {\n    skip_or_panic(\"x\");\n}\nfn b() {\n    eprintln!(\"ollama down, skipping\");\n}\n";
    assert_eq!(violations("x/tests/a.rs", src, true).len(), 1);
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
fn ignored_tests_need_a_convention_prefix() {
    let src = "#[tokio::test]\n#[ignore]\nasync fn test_thing() {}\n#[ignore = \"x\"]\n#[test]\nfn container_ok() {}\n#[ignore]\nfn ollama_ok() {}\n#[ignore]\nfn manual_ok() {}\n";
    assert_eq!(ignored_tests_outside_convention(src), vec!["test_thing"]);
}

#[test]
fn ignore_inside_comments_is_not_counted() {
    let src = "// #[ignore]\nfn test_thing() {}\n";
    assert!(ignored_tests_outside_convention(src).is_empty());
}

#[test]
fn job_section_isolates_one_job() {
    let wf = "jobs:\n  a:\n    name: A\n    steps: []\n  b:\n    name: B\n";
    let a = job_section(wf, "a").unwrap();
    assert!(a.contains("name: A") && !a.contains("name: B"));
    assert!(job_section(wf, "b").unwrap().contains("name: B"));
    assert!(job_section(wf, "c").is_none());
}

#[test]
fn every_ignored_test_follows_a_prefix_convention() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for dir in ["harness/src", "harness/tests", "model/src", "model/tests"] {
        rust_files(&root.join(dir), &mut files);
    }
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
        let source = std::fs::read_to_string(&file).unwrap();
        for name in ignored_tests_outside_convention(&source) {
            report.push(format!("{rel}: {name}"));
        }
    }
    assert!(
        report.is_empty(),
        "#[ignore]d tests must be named with one of {IGNORE_PREFIXES:?} so a CI job selects them:\n{}",
        report.join("\n")
    );
}

#[test]
fn ci_selects_ignored_tests_by_prefix_convention() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let wf = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap();
    let required = job_section(&wf, "container-runtime-required").expect("required job");
    let ollama = job_section(&wf, "container-ollama").expect("ollama job");
    assert!(required.contains("NANNA_REQUIRE_RUNTIME: \"1\""));
    assert!(required.contains("container_"));
    assert!(!required.contains("11434") && !required.to_lowercase().contains("ollama"));
    assert!(!required.contains("continue-on-error"));
    assert!(ollama.contains("ollama_") && ollama.contains("11434"));
    assert!(ollama.contains("continue-on-error: true"));
    assert!(required.contains("if: always()") && ollama.contains("if: always()"));
    for job in [required, ollama] {
        assert!(job.contains("permissions:") && !job.contains("environment:"));
    }
    let gate = job_section(&wf, "all-checks").expect("gate job");
    assert!(gate.contains("- container-runtime-required") && gate.contains("- container-ollama"));
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
