//! Runner that executes an `EvalCase` against nanna's agent loop.
//!
//! Closes part of issue #89: given an `EvalCase` + a fixture repo, copy the
//! repo to a workspace directory, start a dev container, drive `AgentLoop`
//! with the task prompt, and validate the outcome against `expected.*`.
//!
//! The runner is the integration point between the checked-in eval case
//! format (`evals/cases/**`) and the live agent — prior to this, evaluation
//! went through `AgentEvaluator` in `crate::agent::eval`, which only runs
//! hardcoded in-memory scenarios and does not talk to a real model against
//! a real repo.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::agent::eval_case::EvalCase;
use crate::agent::{AgentConfig, AgentContext, AgentLoop};
use crate::container::{
    detect_runtime, exec_in_container, load_image_from_path, start_container_with_fallback,
    ContainerConfig, ContainerHandle,
};
use crate::entities::context::types::ToolCallRecord;
use crate::entities::InMemoryEntityStore;
use crate::onboarding::{DeterministicOnboarder, Onboarder};
use crate::tools::{
    ListDirTool, ReadFileTool, RunCommandTool, SearchTool, ToolRegistry, WriteFileTool,
    CONTAINER_WORKSPACE_DIR,
};
use model::provider::ModelProvider;

/// Runtime configuration for a single eval case run.
#[derive(Debug, Clone)]
pub struct EvalRunConfig {
    /// Model name passed to `AgentConfig.model_name`.
    pub model_name: String,
    /// Cap on agent iterations independent of the per-case wall-clock timeout.
    pub max_iterations: usize,
    /// Emit verbose logs from the agent loop.
    pub verbose: bool,
}

impl EvalRunConfig {
    pub fn new(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            max_iterations: 32,
            verbose: false,
        }
    }

    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

/// Structured result of a single case run.
#[derive(Debug, Clone)]
pub struct EvalRunResult {
    pub case_id: String,
    pub passed: bool,
    pub duration: Duration,
    pub iterations: usize,
    pub agent_completed: bool,
    /// `Some(true/false)` when `expected.build_must_pass` was checked; `None` when skipped.
    pub build_passed: Option<bool>,
    /// `Some(true/false)` when `expected.tests_must_pass` was checked; `None` when skipped.
    pub tests_passed: Option<bool>,
    /// Files observed to differ between source fixture and post-agent workspace,
    /// paths relative to the workspace root.
    pub files_changed: Vec<String>,
    /// Symbols from `expected.required_symbols` not found in any `src/**/*.rs`.
    pub missing_symbols: Vec<String>,
    /// Summary diagnostic when `passed == false`.
    pub failure_reason: Option<String>,
    pub tool_calls: Vec<ToolCallRecord>,
}

impl EvalRunResult {
    fn failure(case_id: &str, start: Instant, reason: impl Into<String>) -> Self {
        Self {
            case_id: case_id.to_string(),
            passed: false,
            duration: start.elapsed(),
            iterations: 0,
            agent_completed: false,
            build_passed: None,
            tests_passed: None,
            files_changed: Vec::new(),
            missing_symbols: Vec::new(),
            failure_reason: Some(reason.into()),
            tool_calls: Vec::new(),
        }
    }
}

/// Execute a single eval case against the live agent.
///
/// `source_repo` is the checked-in fixture (read-only from the runner's
/// perspective). `workspace` is a caller-owned directory into which the
/// fixture is copied before the agent runs; it will be bind-mounted into
/// the dev container at `CONTAINER_WORKSPACE_DIR`. Keeping workspace
/// lifecycle outside the runner lets callers choose between `tempfile`
/// (tests) and fixed paths (report collection).
pub async fn run_eval_case(
    case: &EvalCase,
    source_repo: &Path,
    workspace: &Path,
    config: &EvalRunConfig,
    provider: Arc<dyn ModelProvider>,
) -> EvalRunResult {
    let start = Instant::now();
    let case_id = case.case.id.clone();

    if let Err(e) = copy_dir_all(source_repo, workspace) {
        return EvalRunResult::failure(&case_id, start, format!("fixture copy failed: {}", e));
    }

    let runtime = detect_runtime();
    if !runtime.is_available() {
        return EvalRunResult::failure(
            &case_id,
            start,
            "no container runtime (podman/docker) available",
        );
    }

    // Fixture repos under `evals/cases/**/repo` don't carry a `flake.nix`
    // (they're minimal Cargo crates), so onboard the workspace copy first
    // to generate one before building the dev container.
    if !workspace.join("flake.nix").exists() {
        let onboarder = DeterministicOnboarder;
        if let Err(e) = onboarder.onboard(workspace).await {
            return EvalRunResult::failure(&case_id, start, format!("onboarding failed: {}", e));
        }
    }

    let image_path = match image_builder::build_dev_container(workspace) {
        Ok(p) => p,
        Err(e) => {
            return EvalRunResult::failure(
                &case_id,
                start,
                format!("dev container build failed: {}", e),
            )
        }
    };

    let image_ref = match load_image_from_path(&runtime, &image_path) {
        Ok(r) => r,
        Err(e) => {
            return EvalRunResult::failure(&case_id, start, format!("load image failed: {}", e))
        }
    };

    let container_name = format!("nanna-eval-{}-{}", case.case.id, uuid::Uuid::new_v4());
    let additional_args = vec![format!(
        "-v={}:{CONTAINER_WORKSPACE_DIR}",
        workspace.display()
    )];
    let container_config = ContainerConfig {
        base_image: image_ref,
        test_image: None,
        container_name,
        port_mapping: None,
        model_to_pull: None,
        startup_timeout: Duration::from_secs(5),
        health_check_timeout: Duration::from_secs(5),
        env_vars: vec![],
        additional_args,
    };

    let handle = match start_container_with_fallback(&container_config).await {
        Ok(h) => h,
        Err(e) => {
            return EvalRunResult::failure(
                &case_id,
                start,
                format!("container start failed: {}", e),
            )
        }
    };
    let handle = Arc::new(handle);

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadFileTool::new(workspace.to_path_buf())));
    registry.register(Box::new(WriteFileTool::new(workspace.to_path_buf())));
    registry.register(Box::new(ListDirTool::new(workspace.to_path_buf())));
    registry.register(Box::new(SearchTool::new(workspace.to_path_buf())));
    registry.register(Box::new(RunCommandTool::new(
        Arc::clone(&handle),
        Some(CONTAINER_WORKSPACE_DIR.to_string()),
    )));

    let agent_config = AgentConfig {
        max_iterations: config.max_iterations,
        verbose: config.verbose,
        system_prompt: build_system_prompt(case),
        model_name: config.model_name.clone(),
    };
    let context = AgentContext {
        user_prompt: case.task.prompt.clone(),
        conversation_history: vec![],
        app_state_id: uuid::Uuid::new_v4().to_string(),
    };

    let mut agent =
        AgentLoop::with_tools(agent_config, InMemoryEntityStore::new(), provider, registry);

    let agent_timeout = Duration::from_secs(case.metadata.timeout_secs);
    let agent_result = match tokio::time::timeout(agent_timeout, agent.run(context)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return EvalRunResult::failure(&case_id, start, format!("agent error: {}", e))
        }
        Err(_) => {
            return EvalRunResult::failure(
                &case_id,
                start,
                format!("agent timed out after {}s", case.metadata.timeout_secs),
            )
        }
    };

    let files_changed = detect_changed_files(source_repo, workspace);
    let missing_symbols = find_missing_symbols(workspace, &case.expected.required_symbols);

    let build_passed = if case.expected.build_must_pass {
        Some(run_cargo_in_container(&handle, "build"))
    } else {
        None
    };
    let tests_passed = if case.expected.tests_must_pass {
        Some(run_cargo_in_container(&handle, "test"))
    } else {
        None
    };

    let (passed, failure_reason) = classify_outcome(
        agent_result.task_completed,
        build_passed,
        tests_passed,
        &missing_symbols,
    );

    EvalRunResult {
        case_id,
        passed,
        duration: start.elapsed(),
        iterations: agent_result.iterations,
        agent_completed: agent_result.task_completed,
        build_passed,
        tests_passed,
        files_changed,
        missing_symbols,
        failure_reason,
        tool_calls: agent_result.tool_calls_made,
    }
}

/// Pure classifier: given observed outcomes, decide pass/fail and compose a
/// human-readable failure summary. Split out so it can be exhaustively unit
/// tested without spinning up a container.
pub(crate) fn classify_outcome(
    agent_completed: bool,
    build_passed: Option<bool>,
    tests_passed: Option<bool>,
    missing_symbols: &[String],
) -> (bool, Option<String>) {
    let mut failures = Vec::new();
    if !agent_completed {
        failures.push("agent did not reach Completed state".to_string());
    }
    if build_passed == Some(false) {
        failures.push("cargo build failed".to_string());
    }
    if tests_passed == Some(false) {
        failures.push("cargo test failed".to_string());
    }
    if !missing_symbols.is_empty() {
        failures.push(format!("missing required symbols: {:?}", missing_symbols));
    }

    if failures.is_empty() {
        (true, None)
    } else {
        (false, Some(failures.join("; ")))
    }
}

/// Build the system prompt sent to every eval case. Deliberately
/// case-agnostic so outcomes across cases are comparable — task-specific
/// instructions come from `case.task.prompt` via `AgentContext::user_prompt`.
fn build_system_prompt(case: &EvalCase) -> String {
    format!(
        "You are a coding assistant working on a {language} project mounted at the workspace \
         directory. Use the provided file tools to read existing code, make the requested \
         change, and the run_command tool to verify with `cargo build` before declaring \
         completion. Do not stop until the change is present on disk and builds cleanly.",
        language = case.task.language,
    )
}

fn run_cargo_in_container(handle: &ContainerHandle, subcommand: &str) -> bool {
    exec_in_container(
        handle,
        &["cargo", subcommand],
        Some(CONTAINER_WORKSPACE_DIR),
    )
    .map(|out| out.success)
    .unwrap_or(false)
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Return symbols in `required` that do not appear as a word-boundary match
/// in any `src/**/*.rs` file under `workspace`. Empty input → empty output.
pub fn find_missing_symbols(workspace: &Path, required: &[String]) -> Vec<String> {
    if required.is_empty() {
        return Vec::new();
    }
    let src_dir = workspace.join("src");
    let mut haystack = String::new();
    collect_rust_sources(&src_dir, &mut haystack);
    required
        .iter()
        .filter(|sym| !symbol_appears(&haystack, sym))
        .cloned()
        .collect()
}

fn collect_rust_sources(dir: &Path, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                out.push_str(&content);
                out.push('\n');
            }
        }
    }
}

fn symbol_appears(haystack: &str, symbol: &str) -> bool {
    let pattern = format!(r"\b{}\b", regex::escape(symbol));
    regex::Regex::new(&pattern)
        .map(|re| re.is_match(haystack))
        .unwrap_or(false)
}

/// Diff two directory trees by file content. Returns paths (relative to either
/// root) of files that differ, were added, or were removed. Output is sorted.
pub fn detect_changed_files(original: &Path, modified: &Path) -> Vec<String> {
    let orig = walk_files(original);
    let modif = walk_files(modified);
    let mut changed: HashSet<String> = HashSet::new();

    for (path, content) in &orig {
        match modif.get(path) {
            Some(m) if m == content => {}
            _ => {
                changed.insert(path.to_string_lossy().into_owned());
            }
        }
    }
    for path in modif.keys() {
        if !orig.contains_key(path) {
            changed.insert(path.to_string_lossy().into_owned());
        }
    }

    let mut result: Vec<String> = changed.into_iter().collect();
    result.sort();
    result
}

fn walk_files(root: &Path) -> HashMap<PathBuf, Vec<u8>> {
    let mut files = HashMap::new();
    if root.is_dir() {
        collect_files(root, root, &mut files);
    }
    files
}

fn collect_files(root: &Path, current: &Path, out: &mut HashMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                if let Ok(content) = std::fs::read(&path) {
                    out.insert(rel.to_path_buf(), content);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn find_missing_symbols_empty_required_returns_empty() {
        let dir = tempdir().unwrap();
        let missing = find_missing_symbols(dir.path(), &[]);
        assert!(missing.is_empty());
    }

    #[test]
    fn find_missing_symbols_matches_word_boundary() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("src/lib.rs"),
            "pub fn greet(name: &str) -> String { format!(\"Hello, {}!\", name) }",
        );
        let missing = find_missing_symbols(dir.path(), &["greet".to_string()]);
        assert!(missing.is_empty(), "greet should be found");
    }

    #[test]
    fn find_missing_symbols_reports_missing() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("src/lib.rs"), "pub fn greet() {}");
        let required = vec!["greet".to_string(), "farewell".to_string()];
        let missing = find_missing_symbols(dir.path(), &required);
        assert_eq!(missing, vec!["farewell".to_string()]);
    }

    #[test]
    fn find_missing_symbols_rejects_substring_match() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("src/lib.rs"), "pub fn greeter() {}");
        let missing = find_missing_symbols(dir.path(), &["greet".to_string()]);
        assert_eq!(missing, vec!["greet".to_string()]);
    }

    #[test]
    fn find_missing_symbols_scans_nested_dirs() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("src/utils/math.rs"), "pub fn add() {}");
        let missing = find_missing_symbols(dir.path(), &["add".to_string()]);
        assert!(missing.is_empty());
    }

    #[test]
    fn find_missing_symbols_ignores_non_rs_files() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("src/notes.txt"), "greet greet greet");
        let missing = find_missing_symbols(dir.path(), &["greet".to_string()]);
        assert_eq!(missing, vec!["greet".to_string()]);
    }

    #[test]
    fn find_missing_symbols_missing_src_dir_reports_all() {
        let dir = tempdir().unwrap();
        let missing = find_missing_symbols(dir.path(), &["anything".to_string()]);
        assert_eq!(missing, vec!["anything".to_string()]);
    }

    #[test]
    fn detect_changed_files_no_changes() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        write(&a.path().join("src/lib.rs"), "same content");
        write(&b.path().join("src/lib.rs"), "same content");
        assert!(detect_changed_files(a.path(), b.path()).is_empty());
    }

    #[test]
    fn detect_changed_files_modified_file() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        write(&a.path().join("src/lib.rs"), "old");
        write(&b.path().join("src/lib.rs"), "new");
        let changed = detect_changed_files(a.path(), b.path());
        assert_eq!(changed, vec![PathBuf::from("src/lib.rs").to_string_lossy()]);
    }

    #[test]
    fn detect_changed_files_added_file() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        write(&a.path().join("src/lib.rs"), "x");
        write(&b.path().join("src/lib.rs"), "x");
        write(&b.path().join("src/utils.rs"), "new module");
        let changed = detect_changed_files(a.path(), b.path());
        assert_eq!(
            changed,
            vec![PathBuf::from("src/utils.rs").to_string_lossy()]
        );
    }

    #[test]
    fn detect_changed_files_removed_file() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        write(&a.path().join("src/lib.rs"), "x");
        write(&a.path().join("src/gone.rs"), "dead");
        write(&b.path().join("src/lib.rs"), "x");
        let changed = detect_changed_files(a.path(), b.path());
        assert_eq!(
            changed,
            vec![PathBuf::from("src/gone.rs").to_string_lossy()]
        );
    }

    #[test]
    fn detect_changed_files_sorted_output() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        write(&a.path().join("z.rs"), "x");
        write(&b.path().join("z.rs"), "y");
        write(&b.path().join("a.rs"), "added");
        let changed = detect_changed_files(a.path(), b.path());
        assert_eq!(changed, vec!["a.rs".to_string(), "z.rs".to_string()]);
    }

    #[test]
    fn eval_run_config_builder_defaults() {
        let cfg = EvalRunConfig::new("qwen3:0.6b");
        assert_eq!(cfg.model_name, "qwen3:0.6b");
        assert_eq!(cfg.max_iterations, 32);
        assert!(!cfg.verbose);
    }

    #[test]
    fn eval_run_config_builder_overrides() {
        let cfg = EvalRunConfig::new("gemma4:e4b")
            .with_max_iterations(64)
            .with_verbose(true);
        assert_eq!(cfg.max_iterations, 64);
        assert!(cfg.verbose);
    }

    #[test]
    fn eval_run_result_failure_constructor() {
        let start = Instant::now();
        let result = EvalRunResult::failure("abc", start, "nope");
        assert_eq!(result.case_id, "abc");
        assert!(!result.passed);
        assert_eq!(result.failure_reason.as_deref(), Some("nope"));
        assert!(result.files_changed.is_empty());
    }

    #[test]
    fn build_system_prompt_is_case_agnostic() {
        let case = EvalCase::from_toml_str(
            r#"
[case]
id = "ut-001"
name = "x"
description = "should not appear"
[task]
prompt = "do stuff"
language = "rust"
"#,
        )
        .unwrap();
        let prompt = build_system_prompt(&case);
        assert!(
            !prompt.contains("should not appear"),
            "system prompt must not leak case.description"
        );
        assert!(prompt.contains("rust"));
        assert!(prompt.contains("run_command"));
    }

    #[test]
    fn classify_outcome_all_green() {
        let (passed, reason) = classify_outcome(true, Some(true), Some(true), &[]);
        assert!(passed);
        assert!(reason.is_none());
    }

    #[test]
    fn classify_outcome_build_skipped_tests_skipped_symbols_empty() {
        let (passed, reason) = classify_outcome(true, None, None, &[]);
        assert!(passed);
        assert!(reason.is_none());
    }

    #[test]
    fn classify_outcome_agent_incomplete() {
        let (passed, reason) = classify_outcome(false, Some(true), None, &[]);
        assert!(!passed);
        assert_eq!(
            reason.as_deref(),
            Some("agent did not reach Completed state")
        );
    }

    #[test]
    fn classify_outcome_build_failed() {
        let (passed, reason) = classify_outcome(true, Some(false), None, &[]);
        assert!(!passed);
        assert_eq!(reason.as_deref(), Some("cargo build failed"));
    }

    #[test]
    fn classify_outcome_tests_failed() {
        let (passed, reason) = classify_outcome(true, Some(true), Some(false), &[]);
        assert!(!passed);
        assert_eq!(reason.as_deref(), Some("cargo test failed"));
    }

    #[test]
    fn classify_outcome_missing_symbols() {
        let missing = vec!["greet".to_string(), "farewell".to_string()];
        let (passed, reason) = classify_outcome(true, Some(true), None, &missing);
        assert!(!passed);
        let reason = reason.unwrap();
        assert!(reason.contains("greet"));
        assert!(reason.contains("farewell"));
    }

    #[test]
    fn classify_outcome_combines_failures() {
        let missing = vec!["x".to_string()];
        let (passed, reason) = classify_outcome(false, Some(false), Some(false), &missing);
        assert!(!passed);
        let reason = reason.unwrap();
        assert!(reason.contains("agent did not"));
        assert!(reason.contains("cargo build"));
        assert!(reason.contains("cargo test"));
        assert!(reason.contains("missing required symbols"));
        assert_eq!(reason.matches(';').count(), 3);
    }

    #[test]
    fn copy_dir_all_copies_nested() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        write(&src.path().join("a/b/c.txt"), "x");
        copy_dir_all(src.path(), dst.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.path().join("a/b/c.txt")).unwrap(),
            "x"
        );
    }
}
