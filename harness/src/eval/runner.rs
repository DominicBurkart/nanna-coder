//! Eval runner — execute nanna agent against single eval cases.
//!
//! Copies fixture repositories into isolated temporary directories,
//! runs the [`AgentLoop`] with the task prompt, verifies the result,
//! and returns structured metrics.
//!
//! # Example
//!
//! ```rust,no_run
//! use harness::eval::runner::{run_eval, EvalRunnerConfig};
//! use harness::agent::eval_case::EvalCase;
//! use std::path::Path;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let task_toml = Path::new("evals/cases/happy-path-001/task.toml");
//! let case = EvalCase::from_toml_file(task_toml)?;
//! let case_dir = task_toml.parent().unwrap();
//! let config = EvalRunnerConfig::default();
//!
//! let result = run_eval(&case, case_dir, &config).await?;
//! println!("Success: {}, Iterations: {}", result.success, result.iterations);
//! # Ok(())
//! # }
//! ```

use crate::agent::eval_case::{EvalCase, EvalCaseError};
use crate::agent::{AgentConfig, AgentContext, AgentLoop, AgentRunResult};
use crate::tools::create_tool_registry;
use model::config::OllamaConfig;
use model::ollama::OllamaProvider;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Errors that can occur when running an eval case.
#[derive(Debug, Error)]
pub enum EvalRunnerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Agent error: {0}")]
    Agent(#[from] crate::agent::AgentError),
    #[error("Eval case error: {0}")]
    EvalCase(#[from] EvalCaseError),
    #[error("Model provider error: {0}")]
    ModelProvider(String),
    #[error("Timeout after {0:?}")]
    Timeout(Duration),
}

/// Configuration for the eval runner.
#[derive(Debug, Clone)]
pub struct EvalRunnerConfig {
    /// Model name to use (e.g. `"qwen3:0.6b"`).
    pub model_name: String,
    /// Base URL for the model provider (Ollama). `None` means localhost default.
    pub model_base_url: Option<String>,
    /// Enable verbose logging during agent execution.
    pub verbose: bool,
    /// Maximum iterations for the agent loop.
    pub max_iterations: usize,
}

impl Default for EvalRunnerConfig {
    fn default() -> Self {
        Self {
            model_name: "qwen3:0.6b".to_string(),
            model_base_url: None,
            verbose: false,
            max_iterations: 100,
        }
    }
}

impl EvalRunnerConfig {
    pub fn with_model(mut self, model: &str) -> Self {
        self.model_name = model.to_string();
        self
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.model_base_url = Some(url.to_string());
        self
    }

    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.max_iterations = max_iterations;
        self
    }
}

/// Aggregated token usage for an eval run (re-export of [`model::types::Usage`]).
pub type TokenUsage = model::types::Usage;

fn default_token_usage() -> TokenUsage {
    TokenUsage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    }
}

/// Results of post-completion verification checks.
#[derive(Debug, Clone)]
pub struct VerificationResult {
    /// Whether `cargo build` passed (`None` if not required).
    pub build_passed: Option<bool>,
    /// Whether `cargo test` passed (`None` if not required).
    pub tests_passed: Option<bool>,
    /// Expected files that were found in the working directory.
    pub files_found: Vec<String>,
    /// Expected files that were NOT found.
    pub missing_files: Vec<String>,
    /// Required symbols that were found in source files.
    pub symbols_found: Vec<String>,
    /// Required symbols that were NOT found.
    pub missing_symbols: Vec<String>,
}

impl VerificationResult {
    /// Returns `true` when all verification checks passed.
    pub fn all_passed(&self) -> bool {
        self.build_passed.unwrap_or(true)
            && self.tests_passed.unwrap_or(true)
            && self.missing_files.is_empty()
            && self.missing_symbols.is_empty()
    }
}

/// The result of running a single eval case.
#[derive(Debug, Clone)]
pub struct EvalRunResult {
    /// The case ID from the task.toml.
    pub case_id: String,
    /// Whether the eval passed all checks.
    pub success: bool,
    /// Wall-clock execution time.
    pub execution_time: Duration,
    /// Number of agent loop iterations.
    pub iterations: usize,
    /// Token usage aggregated across all LLM calls.
    pub token_usage: TokenUsage,
    /// Post-completion verification results.
    pub verification: VerificationResult,
    /// Failure descriptions (empty when `success` is true).
    pub failures: Vec<String>,
    /// The underlying agent result, if the agent ran successfully.
    pub agent_result: Option<AgentRunResult>,
}

/// Run a single eval case end-to-end.
///
/// 1. Copies the fixture repo into an isolated temporary directory.
/// 2. Initialises and runs the [`AgentLoop`] with the task prompt.
/// 3. Runs post-completion verification checks.
/// 4. Returns structured metrics.
pub async fn run_eval(
    eval_case: &EvalCase,
    case_dir: &Path,
    config: &EvalRunnerConfig,
) -> Result<EvalRunResult, EvalRunnerError> {
    let start = Instant::now();

    // --- 1. Isolate: copy fixture repo into a temp dir ---
    let tmp_dir = tempfile::TempDir::new()?;
    let repo_src = case_dir.join("repo");
    if repo_src.is_dir() {
        copy_dir_recursive(&repo_src, tmp_dir.path())?;
    }
    let work_dir = tmp_dir.path();

    // --- 2. Build and run agent ---
    let agent_config = AgentConfig {
        max_iterations: config.max_iterations,
        verbose: config.verbose,
        system_prompt: String::new(),
        model_name: config.model_name.clone(),
    };

    let tool_registry = create_tool_registry(work_dir);
    let entity_store = crate::entities::InMemoryEntityStore::new();

    // Create LLM provider so the agent uses the tool-calling loop
    // (without a provider, the agent falls back to the entity-based loop
    // which never touches the filesystem — see issue #98).
    let mut ollama_config = OllamaConfig::new().with_timeout(Duration::from_secs(120));
    if let Some(url) = &config.model_base_url {
        ollama_config = ollama_config.with_base_url(url.clone());
    }
    let provider = OllamaProvider::new(ollama_config)
        .map_err(|e| EvalRunnerError::ModelProvider(e.to_string()))?;
    let provider = Arc::new(provider);

    let mut agent = AgentLoop::with_tools(agent_config, entity_store, provider, tool_registry);

    let context = AgentContext {
        user_prompt: eval_case.task.prompt.clone(),
        conversation_history: vec![],
        app_state_id: format!("eval_{}", eval_case.case.id),
    };

    let timeout = Duration::from_secs(eval_case.metadata.timeout_secs);

    let agent_outcome = tokio::time::timeout(timeout, agent.run(context)).await;

    let (agent_result, mut failures) = match agent_outcome {
        Ok(Ok(result)) => {
            let f = Vec::new();
            (Some(result), f)
        }
        Ok(Err(e)) => {
            let mut f = vec![format!("Agent error: {e}")];
            // Still run verification even on agent error
            let verification =
                run_verification(work_dir, &eval_case.expected, &eval_case.task.language).await;
            let execution_time = start.elapsed();
            let success = false;
            if !verification.all_passed() {
                f.extend(verification_failures(&verification));
            }
            return Ok(EvalRunResult {
                case_id: eval_case.case.id.clone(),
                success,
                execution_time,
                iterations: 0,
                token_usage: default_token_usage(),
                verification,
                failures: f,
                agent_result: None,
            });
        }
        Err(_elapsed) => {
            return Err(EvalRunnerError::Timeout(timeout));
        }
    };

    // --- 3. Verification ---
    let verification =
        run_verification(work_dir, &eval_case.expected, &eval_case.task.language).await;

    // --- 4. Collect metrics ---
    let iterations = agent_result.as_ref().map_or(0, |r| r.iterations);
    let task_completed = agent_result.as_ref().is_some_and(|r| r.task_completed);

    if !task_completed {
        failures.push("Agent did not complete the task".to_string());
    }
    if !verification.all_passed() {
        failures.extend(verification_failures(&verification));
    }

    let success = failures.is_empty();
    let execution_time = start.elapsed();

    let token_usage = agent_result
        .as_ref()
        .and_then(|r| r.token_usage.clone())
        .unwrap_or_else(default_token_usage);

    Ok(EvalRunResult {
        case_id: eval_case.case.id.clone(),
        success,
        execution_time,
        iterations,
        token_usage,
        verification,
        failures,
        agent_result,
    })
}

// ---------------------------------------------------------------------------
// Verification helpers
// ---------------------------------------------------------------------------

async fn run_verification(
    work_dir: &Path,
    expected: &crate::agent::eval_case::ExpectedResult,
    language: &str,
) -> VerificationResult {
    let build_passed = if expected.build_must_pass {
        Some(verify_build(work_dir).await)
    } else {
        None
    };

    let tests_passed = if expected.tests_must_pass {
        Some(verify_tests(work_dir).await)
    } else {
        None
    };

    let (files_found, missing_files) = verify_files(work_dir, &expected.files_changed);
    let (symbols_found, missing_symbols) =
        verify_symbols(work_dir, &expected.required_symbols, language);

    VerificationResult {
        build_passed,
        tests_passed,
        files_found,
        missing_files,
        symbols_found,
        missing_symbols,
    }
}

async fn verify_build(work_dir: &Path) -> bool {
    let output = tokio::process::Command::new("cargo")
        .arg("build")
        .current_dir(work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;
    match output {
        Ok(o) => {
            if !o.status.success() {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!("Build verification failed:\n{stderr}");
            }
            o.status.success()
        }
        Err(e) => {
            tracing::warn!("Build verification could not run: {e}");
            false
        }
    }
}

async fn verify_tests(work_dir: &Path) -> bool {
    let output = tokio::process::Command::new("cargo")
        .arg("test")
        .current_dir(work_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;
    match output {
        Ok(o) => {
            if !o.status.success() {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!("Test verification failed:\n{stderr}");
            }
            o.status.success()
        }
        Err(e) => {
            tracing::warn!("Test verification could not run: {e}");
            false
        }
    }
}

fn verify_files(work_dir: &Path, expected_files: &[String]) -> (Vec<String>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for file in expected_files {
        if work_dir.join(file).exists() {
            found.push(file.clone());
        } else {
            missing.push(file.clone());
        }
    }
    (found, missing)
}

fn verify_symbols(
    work_dir: &Path,
    required_symbols: &[String],
    language: &str,
) -> (Vec<String>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();

    if required_symbols.is_empty() {
        return (found, missing);
    }

    let extensions = extensions_for_language(language);
    let source_content = collect_source_content(work_dir, &extensions);

    for symbol in required_symbols {
        if source_content.contains(symbol.as_str()) {
            found.push(symbol.clone());
        } else {
            missing.push(symbol.clone());
        }
    }
    (found, missing)
}

/// Map a language name to its common file extensions.
fn extensions_for_language(language: &str) -> Vec<&'static str> {
    match language.to_lowercase().as_str() {
        "rust" => vec!["rs"],
        "python" => vec!["py"],
        "javascript" | "js" => vec!["js", "jsx", "mjs"],
        "typescript" | "ts" => vec!["ts", "tsx"],
        "go" | "golang" => vec!["go"],
        "java" => vec!["java"],
        "c" => vec!["c", "h"],
        "cpp" | "c++" => vec!["cpp", "cc", "cxx", "hpp", "h"],
        "ruby" => vec!["rb"],
        _ => vec!["rs"], // default to Rust for backwards compatibility
    }
}

/// Recursively read source files under `dir` matching the given extensions
/// and concatenate their contents.
fn collect_source_content(dir: &Path, extensions: &[&str]) -> String {
    let mut content = String::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                content.push_str(&collect_source_content(&path, extensions));
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext) {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        content.push_str(&text);
                        content.push('\n');
                    }
                }
            }
        }
    }
    content
}

fn verification_failures(v: &VerificationResult) -> Vec<String> {
    let mut out = Vec::new();
    if v.build_passed == Some(false) {
        out.push("Build verification failed".to_string());
    }
    if v.tests_passed == Some(false) {
        out.push("Test verification failed".to_string());
    }
    for f in &v.missing_files {
        out.push(format!("Expected file not found: {f}"));
    }
    for s in &v.missing_symbols {
        out.push(format!("Required symbol not found: {s}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

/// Recursively copy the contents of `src` into `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = EvalRunnerConfig::default();
        assert_eq!(config.model_name, "qwen3:0.6b");
        assert!(config.model_base_url.is_none());
        assert!(!config.verbose);
        assert_eq!(config.max_iterations, 100);
    }

    #[test]
    fn test_config_builder() {
        let config = EvalRunnerConfig::default()
            .with_model("llama3:8b")
            .with_base_url("http://localhost:11434")
            .with_verbose(true)
            .with_max_iterations(50);

        assert_eq!(config.model_name, "llama3:8b");
        assert_eq!(
            config.model_base_url.as_deref(),
            Some("http://localhost:11434")
        );
        assert!(config.verbose);
        assert_eq!(config.max_iterations, 50);
    }

    #[test]
    fn test_token_usage_default() {
        let usage = default_token_usage();
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn test_verification_all_passed() {
        let v = VerificationResult {
            build_passed: Some(true),
            tests_passed: Some(true),
            files_found: vec!["src/lib.rs".to_string()],
            missing_files: vec![],
            symbols_found: vec!["greet".to_string()],
            missing_symbols: vec![],
        };
        assert!(v.all_passed());
    }

    #[test]
    fn test_verification_build_failed() {
        let v = VerificationResult {
            build_passed: Some(false),
            tests_passed: None,
            files_found: vec![],
            missing_files: vec![],
            symbols_found: vec![],
            missing_symbols: vec![],
        };
        assert!(!v.all_passed());
    }

    #[test]
    fn test_verification_missing_files() {
        let v = VerificationResult {
            build_passed: None,
            tests_passed: None,
            files_found: vec![],
            missing_files: vec!["src/foo.rs".to_string()],
            symbols_found: vec![],
            missing_symbols: vec![],
        };
        assert!(!v.all_passed());
    }

    #[test]
    fn test_verification_not_required_passes() {
        let v = VerificationResult {
            build_passed: None,
            tests_passed: None,
            files_found: vec![],
            missing_files: vec![],
            symbols_found: vec![],
            missing_symbols: vec![],
        };
        assert!(v.all_passed());
    }

    #[test]
    fn test_copy_dir_recursive() {
        let src = tempfile::TempDir::new().unwrap();
        let dst = tempfile::TempDir::new().unwrap();

        // Create a nested structure
        let sub = src.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(src.path().join("root.txt"), "hello").unwrap();
        std::fs::write(sub.join("nested.txt"), "world").unwrap();

        copy_dir_recursive(src.path(), dst.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join("root.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join("sub/nested.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn test_verify_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "fn greet() {}").unwrap();

        let (found, missing) = verify_files(
            dir.path(),
            &["src/lib.rs".to_string(), "src/missing.rs".to_string()],
        );

        assert_eq!(found, vec!["src/lib.rs"]);
        assert_eq!(missing, vec!["src/missing.rs"]);
    }

    #[test]
    fn test_verify_symbols() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn greet() {}\npub fn hello() {}").unwrap();

        let (found, missing) = verify_symbols(
            dir.path(),
            &[
                "greet".to_string(),
                "hello".to_string(),
                "missing_fn".to_string(),
            ],
            "rust",
        );

        assert_eq!(found, vec!["greet", "hello"]);
        assert_eq!(missing, vec!["missing_fn"]);
    }

    #[test]
    fn test_verification_failures() {
        let v = VerificationResult {
            build_passed: Some(false),
            tests_passed: Some(false),
            files_found: vec![],
            missing_files: vec!["a.rs".to_string()],
            symbols_found: vec![],
            missing_symbols: vec!["foo".to_string()],
        };
        let failures = verification_failures(&v);
        assert_eq!(failures.len(), 4);
        assert!(failures[0].contains("Build"));
        assert!(failures[1].contains("Test"));
        assert!(failures[2].contains("a.rs"));
        assert!(failures[3].contains("foo"));
    }

    #[test]
    fn test_verification_failures_empty_when_all_pass() {
        let v = VerificationResult {
            build_passed: Some(true),
            tests_passed: Some(true),
            files_found: vec!["lib.rs".to_string()],
            missing_files: vec![],
            symbols_found: vec!["greet".to_string()],
            missing_symbols: vec![],
        };
        let failures = verification_failures(&v);
        assert!(failures.is_empty());
    }

    #[test]
    fn test_verification_failures_none_checks() {
        let v = VerificationResult {
            build_passed: None,
            tests_passed: None,
            files_found: vec![],
            missing_files: vec![],
            symbols_found: vec![],
            missing_symbols: vec![],
        };
        let failures = verification_failures(&v);
        assert!(failures.is_empty());
    }

    #[test]
    fn test_verification_missing_symbols() {
        let v = VerificationResult {
            build_passed: None,
            tests_passed: None,
            files_found: vec![],
            missing_files: vec![],
            symbols_found: vec![],
            missing_symbols: vec!["bar".to_string(), "baz".to_string()],
        };
        assert!(!v.all_passed());
    }

    #[test]
    fn test_verification_tests_failed() {
        let v = VerificationResult {
            build_passed: Some(true),
            tests_passed: Some(false),
            files_found: vec![],
            missing_files: vec![],
            symbols_found: vec![],
            missing_symbols: vec![],
        };
        assert!(!v.all_passed());
    }

    #[test]
    fn test_extensions_for_language_rust() {
        assert_eq!(extensions_for_language("rust"), vec!["rs"]);
        assert_eq!(extensions_for_language("Rust"), vec!["rs"]);
    }

    #[test]
    fn test_extensions_for_language_python() {
        assert_eq!(extensions_for_language("python"), vec!["py"]);
        assert_eq!(extensions_for_language("Python"), vec!["py"]);
    }

    #[test]
    fn test_extensions_for_language_javascript() {
        assert_eq!(
            extensions_for_language("javascript"),
            vec!["js", "jsx", "mjs"]
        );
        assert_eq!(extensions_for_language("js"), vec!["js", "jsx", "mjs"]);
    }

    #[test]
    fn test_extensions_for_language_typescript() {
        assert_eq!(extensions_for_language("typescript"), vec!["ts", "tsx"]);
        assert_eq!(extensions_for_language("ts"), vec!["ts", "tsx"]);
    }

    #[test]
    fn test_extensions_for_language_go() {
        assert_eq!(extensions_for_language("go"), vec!["go"]);
        assert_eq!(extensions_for_language("golang"), vec!["go"]);
    }

    #[test]
    fn test_extensions_for_language_java() {
        assert_eq!(extensions_for_language("java"), vec!["java"]);
    }

    #[test]
    fn test_extensions_for_language_c() {
        assert_eq!(extensions_for_language("c"), vec!["c", "h"]);
    }

    #[test]
    fn test_extensions_for_language_cpp() {
        assert_eq!(
            extensions_for_language("cpp"),
            vec!["cpp", "cc", "cxx", "hpp", "h"]
        );
        assert_eq!(
            extensions_for_language("c++"),
            vec!["cpp", "cc", "cxx", "hpp", "h"]
        );
    }

    #[test]
    fn test_extensions_for_language_ruby() {
        assert_eq!(extensions_for_language("ruby"), vec!["rb"]);
    }

    #[test]
    fn test_extensions_for_language_unknown_defaults_to_rust() {
        assert_eq!(extensions_for_language("haskell"), vec!["rs"]);
        assert_eq!(extensions_for_language(""), vec!["rs"]);
    }

    #[test]
    fn test_collect_source_content_basic() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("readme.md"), "# Hello").unwrap();

        let content = collect_source_content(dir.path(), &["rs"]);
        assert!(content.contains("fn main()"));
        assert!(!content.contains("# Hello"));
    }

    #[test]
    fn test_collect_source_content_recursive() {
        let dir = tempfile::TempDir::new().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("lib.rs"), "pub fn hello() {}").unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let content = collect_source_content(dir.path(), &["rs"]);
        assert!(content.contains("pub fn hello()"));
        assert!(content.contains("fn main()"));
    }

    #[test]
    fn test_collect_source_content_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let content = collect_source_content(dir.path(), &["rs"]);
        assert!(content.is_empty());
    }

    #[test]
    fn test_collect_source_content_nonexistent_dir() {
        let content = collect_source_content(Path::new("/nonexistent/path"), &["rs"]);
        assert!(content.is_empty());
    }

    #[test]
    fn test_collect_source_content_multiple_extensions() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("app.js"), "const x = 1;").unwrap();
        std::fs::write(dir.path().join("comp.jsx"), "export default () => {};").unwrap();
        std::fs::write(dir.path().join("style.css"), ".foo {}").unwrap();

        let content = collect_source_content(dir.path(), &["js", "jsx"]);
        assert!(content.contains("const x = 1;"));
        assert!(content.contains("export default"));
        assert!(!content.contains(".foo"));
    }

    #[test]
    fn test_verify_files_empty_list() {
        let dir = tempfile::TempDir::new().unwrap();
        let (found, missing) = verify_files(dir.path(), &[]);
        assert!(found.is_empty());
        assert!(missing.is_empty());
    }

    #[test]
    fn test_verify_symbols_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let (found, missing) = verify_symbols(dir.path(), &[], "rust");
        assert!(found.is_empty());
        assert!(missing.is_empty());
    }

    #[test]
    fn test_copy_dir_recursive_to_nonexistent_dst() {
        let src = tempfile::TempDir::new().unwrap();
        let dst_base = tempfile::TempDir::new().unwrap();
        let dst = dst_base.path().join("new_dir");

        std::fs::write(src.path().join("file.txt"), "content").unwrap();
        copy_dir_recursive(src.path(), &dst).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("file.txt")).unwrap(),
            "content"
        );
    }

    #[tokio::test]
    async fn test_run_verification_no_requirements() {
        let dir = tempfile::TempDir::new().unwrap();
        let expected = crate::agent::eval_case::ExpectedResult {
            files_changed: vec![],
            build_must_pass: false,
            tests_must_pass: false,
            required_symbols: vec![],
        };
        let result = run_verification(dir.path(), &expected, "rust").await;
        assert!(result.all_passed());
        assert!(result.build_passed.is_none());
        assert!(result.tests_passed.is_none());
        assert!(result.files_found.is_empty());
        assert!(result.missing_files.is_empty());
        assert!(result.symbols_found.is_empty());
        assert!(result.missing_symbols.is_empty());
    }

    #[tokio::test]
    async fn test_run_verification_files_and_symbols() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn greet() {}").unwrap();

        let expected = crate::agent::eval_case::ExpectedResult {
            files_changed: vec!["src/lib.rs".to_string(), "src/missing.rs".to_string()],
            build_must_pass: false,
            tests_must_pass: false,
            required_symbols: vec!["greet".to_string(), "absent".to_string()],
        };
        let result = run_verification(dir.path(), &expected, "rust").await;
        assert!(!result.all_passed());
        assert_eq!(result.files_found, vec!["src/lib.rs"]);
        assert_eq!(result.missing_files, vec!["src/missing.rs"]);
        assert_eq!(result.symbols_found, vec!["greet"]);
        assert_eq!(result.missing_symbols, vec!["absent"]);
    }

    #[tokio::test]
    async fn test_verify_build_nonexistent_dir() {
        // verify_build on a dir with no Cargo.toml should fail
        let dir = tempfile::TempDir::new().unwrap();
        let result = verify_build(dir.path()).await;
        assert!(!result);
    }

    #[tokio::test]
    async fn test_verify_tests_nonexistent_dir() {
        // verify_tests on a dir with no Cargo.toml should fail
        let dir = tempfile::TempDir::new().unwrap();
        let result = verify_tests(dir.path()).await;
        assert!(!result);
    }

    #[test]
    fn test_eval_runner_error_display() {
        let io_err = EvalRunnerError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));
        assert!(io_err.to_string().contains("IO error"));

        let model_err = EvalRunnerError::ModelProvider("connection refused".to_string());
        assert!(model_err.to_string().contains("Model provider error"));

        let timeout_err = EvalRunnerError::Timeout(Duration::from_secs(30));
        assert!(timeout_err.to_string().contains("Timeout"));
    }

    #[test]
    fn test_eval_run_result_fields() {
        let result = EvalRunResult {
            case_id: "test-001".to_string(),
            success: true,
            execution_time: Duration::from_millis(500),
            iterations: 3,
            token_usage: default_token_usage(),
            verification: VerificationResult {
                build_passed: None,
                tests_passed: None,
                files_found: vec![],
                missing_files: vec![],
                symbols_found: vec![],
                missing_symbols: vec![],
            },
            failures: vec![],
            agent_result: None,
        };
        assert_eq!(result.case_id, "test-001");
        assert!(result.success);
        assert_eq!(result.iterations, 3);
        assert_eq!(result.token_usage.total_tokens, 0);
        assert!(result.failures.is_empty());
        assert!(result.agent_result.is_none());
    }
}
