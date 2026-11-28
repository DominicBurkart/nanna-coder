use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN_IN_LIBRARIAN_LAUNCH: [&str; 15] = [
    "AgentLoop",
    "TaskWorkspace",
    "check_repo",
    "Command",
    "std::process",
    "tokio::process",
    "run_tool_loop",
    "ModelProvider",
    ".chat(",
    "default_provider",
    "providers",
    "model_facing_layers",
    "action_subject_for",
    "origin_slug",
    "action_repo_key",
];

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
        .collect();
    kept.join("\n")
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

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn is_test_file(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    name.ends_with("_tests.rs") || name == "tests.rs" || relative.contains("/tests/")
}

fn sources() -> Vec<(String, String)> {
    let root = src_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut out = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !is_test_file(&relative) {
            let text = production(&fs::read_to_string(&file).unwrap());
            out.push((relative, text));
        }
    }
    out.sort();
    out
}

fn occurrences(needle: &str) -> Vec<(String, usize)> {
    sources()
        .into_iter()
        .map(|(name, text)| (name, text.matches(needle).count()))
        .filter(|(_, count)| *count > 0)
        .collect()
}

fn files_containing(needle: &str) -> Vec<String> {
    sources()
        .into_iter()
        .filter(|(_, text)| text.contains(needle))
        .map(|(name, _)| name)
        .collect()
}

fn source_of(relative: &str) -> String {
    let path = src_root().join(relative);
    production(&fs::read_to_string(path).unwrap())
}

#[test]
fn the_librarian_launch_target_is_chosen_only_by_the_librarian_launch_path() {
    assert_eq!(
        files_containing("LaunchTarget::Librarian"),
        vec!["task.rs", "task/librarian.rs"]
    );
}

#[test]
fn the_librarian_session_record_is_written_only_by_the_registry_through_the_task_runner() {
    assert_eq!(
        occurrences(".register_librarian_session("),
        vec![("task.rs".to_string(), 1)]
    );
    let kind_writers: Vec<String> = sources()
        .into_iter()
        .filter(|(_, text)| {
            text.contains("kind: SessionKind::Librarian")
                || text.contains("kind = SessionKind::Librarian")
        })
        .map(|(name, _)| name)
        .collect();
    assert!(kind_writers.is_empty(), "{kind_writers:?}");
    let registry = source_of("hub/registry.rs");
    assert_eq!(registry.matches("SessionKind::Librarian,").count(), 1);
}

#[test]
fn a_registry_binds_without_a_workspace_only_from_the_librarian_launch() {
    assert_eq!(
        occurrences(".bind_registry("),
        vec![("task/librarian.rs".to_string(), 1)]
    );
    assert_eq!(
        files_containing("fn bind_registry("),
        vec!["harness_adapter/binding.rs"]
    );
}

#[test]
fn the_librarian_launch_has_no_in_process_loop_no_worktree_and_no_process() {
    let launch = source_of("task/librarian.rs");
    let hits: Vec<&str> = FORBIDDEN_IN_LIBRARIAN_LAUNCH
        .iter()
        .copied()
        .filter(|token| launch.contains(token))
        .collect();
    assert!(hits.is_empty(), "{hits:?}");
    assert!(launch.contains("route.executor().run("));
}

#[test]
fn the_pi_executor_is_run_from_exactly_the_repo_flow_and_the_librarian_flow() {
    let callers: Vec<(String, usize)> = sources()
        .into_iter()
        .map(|(name, text)| (name, text.matches(".executor()").count()))
        .filter(|(_, count)| *count > 0)
        .collect();
    assert_eq!(
        callers,
        vec![
            ("task.rs".to_string(), 1),
            ("task/librarian.rs".to_string(), 1)
        ]
    );
}

#[test]
fn a_librarian_without_a_route_has_no_fallback_branch_in_the_run_dispatch() {
    let task = source_of("task.rs");
    let branch = "if queued.launch == LaunchTarget::Librarian {\n            return self.run_librarian(queued, side).await;\n        }";
    assert!(task.contains(branch));
    let before = task.split(branch).next().unwrap();
    let run_start = before.rfind("async fn run(self: Arc<Self>").unwrap();
    assert!(before[run_start..].lines().count() <= 3);
}

#[test]
fn only_a_repo_entry_reaches_the_queue_store_and_a_stored_other_target_is_dropped() {
    let dispatch = source_of("scheduler/dispatch.rs");
    assert_eq!(dispatch.matches("self.store.insert(").count(), 1);
    let enqueue = dispatch.split("pub async fn enqueue(").nth(1).unwrap();
    let guard = enqueue.find("stored.launch.persisted()").unwrap();
    let insert = enqueue.find("self.store.insert(").unwrap();
    assert!(guard < insert);
    let open = dispatch.split("pub fn open_with_clock(").nth(1).unwrap();
    assert!(open.contains("split_persisted(store.load()?)"));
    assert!(dispatch.contains("fn split_persisted("));
    assert!(dispatch.contains("entries.into_iter().partition(|t| t.launch.persisted())"));
    assert_eq!(dispatch.matches("self.store.remove(").count(), 2);
    let forget = dispatch.split("fn forget_entry(").nth(1).unwrap();
    assert!(
        forget.find("launch.persisted()").unwrap() < forget.find("self.store.remove(").unwrap()
    );
}

#[test]
fn only_the_librarian_launch_enqueues_without_the_store() {
    assert_eq!(
        occurrences(".enqueue_ephemeral("),
        vec![("task/librarian.rs".to_string(), 1)]
    );
}

#[test]
fn the_scanner_strips_comments_test_modules_and_carriage_returns() {
    let source = "fn a() { x }\r\n// LaunchTarget::Librarian\r\n#[cfg(test)]\r\nmod declared;\r\n#[cfg(test)]\r\nmod t {\r\n LaunchTarget::Librarian }\r\n";
    let kept = production(source);
    assert!(kept.contains("fn a()"));
    assert!(!kept.contains("Librarian"));
    assert!(!kept.contains('\r'));
    assert!(is_test_file("task/librarian_tests.rs"));
    assert!(is_test_file("hub/recv/tests/root_inbox_tests.rs"));
    assert!(!is_test_file("task/librarian.rs"));
}
