use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const QUERY_FILE: &str = "hub/query_flow.rs";
const SEAM_FILE: &str = "hub/librarian.rs";
const TASK_FILE: &str = "task/librarian.rs";
const HUB_FILES: [&str; 2] = [QUERY_FILE, SEAM_FILE];
const NO_SPAWN: [&str; 15] = [
    "TaskManager",
    "TaskRunner",
    "QueueStore",
    "Dispatcher",
    "TaskWorkspace",
    "AgentLoop",
    "Command",
    "process",
    "spawn",
    "submit_librarian",
    "LaunchTarget",
    "PiExecutor",
    "from_proof",
    "register_librarian",
    "File",
];
const LOG_MACROS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];
const AGENT_TEXT: [&str; 7] = ["query", "req", "why", "item", "reply", "raw", "text"];

fn normalized(source: &str) -> String {
    source.replace("\r\n", "\n")
}

fn test_module_start(text: &str) -> Option<usize> {
    let marker = "\n#[cfg(test)]\nmod ";
    let mut from = 0;
    while let Some(found) = text[from..].find(marker) {
        let at = from + found;
        let rest = &text[at + marker.len()..];
        let line = rest.lines().next().unwrap_or("");
        if line.trim_end().ends_with('{') {
            return Some(at);
        }
        from = at + marker.len();
    }
    None
}

fn production(source: &str) -> String {
    let text = normalized(source);
    let cut = test_module_start(&text).unwrap_or(text.len());
    let kept: Vec<&str> = text[..cut]
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| !line.trim_start().starts_with("#[cfg(test)]"))
        .collect();
    kept.join("\n")
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn tree() -> BTreeMap<String, String> {
    let src = src_dir();
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut all = BTreeMap::new();
    for path in files {
        let rel = path.strip_prefix(&src).unwrap();
        let rel = rel.to_string_lossy().replace('\\', "/");
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        if !stem.ends_with("_tests") {
            all.insert(rel, production(&fs::read_to_string(&path).unwrap()));
        }
    }
    assert!(all.len() > 50);
    all
}

fn words(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .collect()
}

fn uses(tree: &BTreeMap<String, String>, word: &str) -> BTreeMap<String, usize> {
    let mut found = BTreeMap::new();
    for (file, text) in tree {
        let count = words(text).iter().filter(|w| **w == word).count();
        if count > 0 {
            found.insert(file.clone(), count);
        }
    }
    found
}

fn files_using(tree: &BTreeMap<String, String>, word: &str) -> Vec<String> {
    uses(tree, word).into_keys().collect()
}

#[test]
fn query_has_no_production_caller_because_only_its_own_file_names_the_request_type() {
    let tree = tree();
    assert_eq!(files_using(&tree, "QueryRequest"), [QUERY_FILE]);
    assert_eq!(files_using(&tree, "QueryAnswer"), [QUERY_FILE]);
}

#[test]
fn the_librarian_seam_is_implemented_once_and_only_by_the_task_layer() {
    let tree = tree();
    let impls: Vec<&String> = tree
        .iter()
        .filter(|(_, text)| text.contains("impl Librarian for"))
        .map(|(file, _)| file)
        .collect();
    assert_eq!(impls, [TASK_FILE]);
}

#[test]
fn the_hub_installs_a_librarian_from_nowhere_in_production_yet() {
    let tree = tree();
    assert_eq!(files_using(&tree, "with_librarian"), [QUERY_FILE]);
    assert_eq!(files_using(&tree, "librarian_gate"), [SEAM_FILE]);
    assert_eq!(files_using(&tree, "TaskLibrarian"), [TASK_FILE]);
}

#[test]
fn the_hub_side_of_the_seam_cannot_spawn_queue_run_a_process_or_open_a_file() {
    let tree = tree();
    for file in HUB_FILES {
        let found: Vec<&str> = words(&tree[file]);
        for banned in NO_SPAWN {
            assert!(!found.contains(&banned), "{file} names {banned}");
        }
    }
}

#[test]
fn the_librarian_registry_is_built_only_by_the_task_layers_seam_implementation() {
    let tree = tree();
    let found = uses(&tree, "register_librarian");
    let expected = BTreeMap::from([("hub/index.rs".to_string(), 1), (TASK_FILE.to_string(), 2)]);
    assert_eq!(found, expected);
}

#[test]
fn the_librarian_run_is_built_only_by_the_query_flow() {
    let tree = tree();
    let built: Vec<&String> = tree
        .iter()
        .filter(|(_, text)| text.contains("LibrarianRun::new("))
        .map(|(file, _)| file)
        .collect();
    assert_eq!(built, [QUERY_FILE]);
}

fn log_calls(text: &str) -> Vec<String> {
    let mut calls = Vec::new();
    for macro_name in LOG_MACROS {
        let marker = format!("tracing::{macro_name}!(");
        for part in text.split(&marker).skip(1) {
            let end = part.find(");").unwrap_or(part.len());
            calls.push(part[..end].to_string());
        }
    }
    calls
}

#[test]
fn the_query_flow_logs_codes_and_failure_kinds_never_agent_text() {
    let tree = tree();
    for file in HUB_FILES {
        for call in log_calls(&tree[file]) {
            for word in words(&call) {
                assert!(!AGENT_TEXT.contains(&word), "{file} logs {word}: {call}");
            }
        }
    }
    assert!(!log_calls(&tree[QUERY_FILE]).is_empty());
}

#[test]
fn the_scanner_handles_crlf_comments_test_modules_and_log_calls() {
    let source = "fn a() { x }\r\n// QueryRequest\r\n#[cfg(test)]\r\nmod declared;\r\n#[cfg(test)]\r\nmod t {\r\n QueryRequest }\r\n";
    let kept = production(source);
    assert!(kept.contains("fn a()"));
    assert!(!kept.contains("QueryRequest"));
    assert!(!kept.contains('\r'));
    let calls = log_calls("tracing::warn!(?codes, \"m\"); tracing::info!(query, \"m\");");
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().any(|call| call.contains("query")));
    assert_eq!(words("a.b(c_d)"), ["a", "b", "c_d"]);
}
