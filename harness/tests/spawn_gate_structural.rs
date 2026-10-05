use std::fs;
use std::path::{Path, PathBuf};

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

fn production_code(path: &Path) -> String {
    let source = fs::read_to_string(path).unwrap();
    let cut = ["#[cfg(test)]\nmod tests", "#[cfg(test)]\npub(crate) mod "]
        .iter()
        .filter_map(|marker| source.find(marker))
        .min()
        .unwrap_or(source.len());
    source[..cut].to_string()
}

fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (relative, production_code(&path))
        })
        .collect()
}

const DISPATCH_CALLS: &[(&str, &[&str])] = &[
    (".submit_task(", &["task.rs", "backlog.rs", "main.rs"]),
    (".submit_with_identity(", &["task.rs"]),
    (".submit_spawn(", &["task.rs", "mcp/handlers.rs"]),
    (".submit(", &["task.rs"]),
    ("dispatcher.enqueue(", &["task.rs", "scheduler/dispatch.rs"]),
    (
        ".enqueue(",
        &["task.rs", "backlog.rs", "scheduler/dispatch.rs"],
    ),
];

#[test]
fn every_spawn_dispatch_call_site_is_a_known_gated_path() {
    let mut offenders = Vec::new();
    for (file, code) in sources() {
        for (call, allowed) in DISPATCH_CALLS {
            if code.contains(call) && !allowed.contains(&file.as_str()) {
                offenders.push(format!("{file} calls `{call}`"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "new spawn call sites must go through SpawnGate and be added to the allowlist here: {offenders:?}"
    );
}

fn code_of(sources: &[(String, String)], file: &str) -> String {
    sources
        .iter()
        .find(|(name, _)| name == file)
        .map(|(_, code)| code.clone())
        .unwrap_or_else(|| panic!("{file} not found"))
}

#[test]
fn the_mcp_assign_path_checks_the_gate_before_submitting() {
    let code = code_of(&sources(), "mcp/handlers.rs");
    let check = code
        .find("spawn_gate\n        .check(")
        .expect("gate check");
    let submit = code.find(".submit_spawn(").expect("submit_spawn");
    assert!(check < submit);
}

#[test]
fn backlog_sync_checks_the_gate_before_every_enqueue() {
    let code = code_of(&sources(), "backlog.rs");
    let start = code.find("pub async fn backlog_sync(").unwrap();
    let body = &code[start..];
    let check = body.find("gate.check(").expect("gate check");
    let enqueue = body.find("sink.enqueue(").expect("enqueue");
    assert!(check < enqueue);
    assert_eq!(body.matches("sink.enqueue(").count(), 1);
    assert!(body.contains("sink.enqueue(task, &proof)"));
}

#[test]
fn raw_task_manager_submission_is_only_reachable_with_an_allowed_proof_in_production_paths() {
    let all = sources();
    let backlog = code_of(&all, "backlog.rs");
    assert!(backlog.contains("async fn enqueue(&self, task: QueuedTask, proof: &Allowed)"));
    assert!(!backlog.contains("async fn enqueue(&self, task: QueuedTask)"));
    let handlers = code_of(&all, "mcp/handlers.rs");
    assert!(handlers.contains("allowed,"));
}
