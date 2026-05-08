//! Eval runner — execute nanna agent against single eval cases.

use crate::agent::eval_case::{EvalCase, EvalCaseError};
use crate::agent::{AgentConfig, AgentContext, AgentLoop, AgentRunResult};
use crate::eval::swebench::{load_swebench_dataset, materialize, SWEBenchError, SWEBenchTask};
use crate::eval::swebench_verify::{
    verify_predictions, InstanceVerdict, Prediction, VerifyConfig, VerifyError,
};
use crate::tools::create_tool_registry;
use model::config::OllamaConfig;
use model::ollama::OllamaProvider;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

const SWEBENCH_TAG: &str = "swebench-verified";
const DEFAULT_SWEBENCH_DATASET_REL: &str = "datasets/swebench-verified-sample.jsonl";

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
    #[error("SWE-bench dataset not found at {0}")]
    SwebenchDatasetMissing(PathBuf),
    #[error("SWE-bench instance {0} not present in dataset")]
    SwebenchInstanceMissing(String),
    #[error("SWE-bench materialize failed: {0}")]
    SwebenchMaterialize(#[from] SWEBenchError),
    #[error("SWE-bench verify failed: {0}")]
    SwebenchVerify(#[from] VerifyError),
    #[error("git diff capture failed: {0}")]
    GitDiff(String),
}

#[derive(Debug, Clone)]
pub struct EvalRunnerConfig {
    pub model_name: String,
    pub model_base_url: Option<String>,
    pub verbose: bool,
    pub max_iterations: usize,
    pub swebench_dataset_path: Option<PathBuf>,
    pub swebench_hf_dataset: String,
    pub swebench_run_id: String,
}

impl Default for EvalRunnerConfig {
    fn default() -> Self {
        let run_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| format!("nanna-{}", d.as_secs()))
            .unwrap_or_else(|_| "nanna-run".to_string());
        Self {
            model_name: "qwen3:0.6b".to_string(),
            model_base_url: None,
            verbose: false,
            max_iterations: 100,
            swebench_dataset_path: None,
            swebench_hf_dataset: "princeton-nlp/SWE-bench_Verified".to_string(),
            swebench_run_id: run_id,
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

pub type TokenUsage = model::types::Usage;

fn default_token_usage() -> TokenUsage {
    TokenUsage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    }
}

#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub build_passed: Option<bool>,
    pub tests_passed: Option<bool>,
    pub files_found: Vec<String>,
    pub missing_files: Vec<String>,
    pub symbols_found: Vec<String>,
    pub missing_symbols: Vec<String>,
}

impl VerificationResult {
    pub fn all_passed(&self) -> bool {
        self.build_passed.unwrap_or(true)
            && self.tests_passed.unwrap_or(true)
            && self.missing_files.is_empty()
            && self.missing_symbols.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct EvalRunResult {
    pub case_id: String,
    pub success: bool,
    pub execution_time: Duration,
    pub iterations: usize,
    pub token_usage: TokenUsage,
    pub verification: VerificationResult,
    pub failures: Vec<String>,
    pub agent_result: Option<AgentRunResult>,
}

pub async fn run_eval(
    eval_case: &EvalCase,
    case_dir: &Path,
    config: &EvalRunnerConfig,
) -> Result<EvalRunResult, EvalRunnerError> {
    let start = Instant::now();
    let is_swebench = is_swebench_case(eval_case);

    let tmp_dir = tempfile::TempDir::new()?;
    let work_dir = tmp_dir.path();
    let swebench_task: Option<SWEBenchTask> = if is_swebench {
        let task = load_swebench_task(eval_case, case_dir, config)?;
        materialize(&task, work_dir)?;
        Some(task)
    } else {
        let repo_src = case_dir.join("repo");
        if repo_src.is_dir() {
            copy_dir_recursive(&repo_src, work_dir)?;
        }
        None
    };

    let agent_config = AgentConfig {
        max_iterations: config.max_iterations,
        verbose: config.verbose,
        system_prompt: String::new(),
        model_name: config.model_name.clone(),
    };

    let tool_registry = create_tool_registry(work_dir);
    let entity_store = crate::entities::InMemoryEntityStore::new();

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

    let agent_outcome = tokio::time::timeout(timeout, agent.run_tool_loop(context)).await;

    let (agent_result, mut failures) = match agent_outcome {
        Ok(Ok(result)) => {
            let f = Vec::new();
            (Some(result), f)
        }
        Ok(Err(e)) => {
            let mut f = vec![format!("Agent error: {e}")];
            if let Some(task) = &swebench_task {
                return finish_swebench(eval_case, task, work_dir, config, start, None, f).await;
            }
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

    if let Some(task) = &swebench_task {
        return finish_swebench(
            eval_case,
            task,
            work_dir,
            config,
            start,
            agent_result,
            failures,
        )
        .await;
    }

    let verification =
        run_verification(work_dir, &eval_case.expected, &eval_case.task.language).await;

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

fn is_swebench_case(case: &EvalCase) -> bool {
    case.metadata.tags.iter().any(|t| t == SWEBENCH_TAG)
}

fn resolve_swebench_dataset_path(
    case_dir: &Path,
    config: &EvalRunnerConfig,
) -> Result<PathBuf, EvalRunnerError> {
    if let Some(p) = &config.swebench_dataset_path {
        return Ok(p.clone());
    }
    if let Ok(env) = std::env::var("NANNA_SWEBENCH_DATASET") {
        return Ok(PathBuf::from(env));
    }
    let mut anc = case_dir.ancestors();
    anc.next();
    let cases_dir = anc
        .next()
        .ok_or_else(|| EvalRunnerError::SwebenchDatasetMissing(case_dir.to_path_buf()))?;
    let evals_dir = anc
        .next()
        .ok_or_else(|| EvalRunnerError::SwebenchDatasetMissing(cases_dir.to_path_buf()))?;
    Ok(evals_dir.join(DEFAULT_SWEBENCH_DATASET_REL))
}

fn load_swebench_task(
    eval_case: &EvalCase,
    case_dir: &Path,
    config: &EvalRunnerConfig,
) -> Result<SWEBenchTask, EvalRunnerError> {
    let dataset_path = resolve_swebench_dataset_path(case_dir, config)?;
    if !dataset_path.is_file() {
        return Err(EvalRunnerError::SwebenchDatasetMissing(dataset_path));
    }
    let tasks = load_swebench_dataset(&dataset_path)?;
    let needle = eval_case
        .case
        .id
        .strip_prefix("swebench-")
        .unwrap_or(eval_case.case.id.as_str());
    tasks
        .into_iter()
        .find(|t| t.instance_id == needle)
        .ok_or_else(|| EvalRunnerError::SwebenchInstanceMissing(needle.to_string()))
}

async fn capture_swebench_patch(
    work_dir: &Path,
    base_commit: &str,
) -> Result<String, EvalRunnerError> {
    let add = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(work_dir)
        .output()
        .await
        .map_err(|e| EvalRunnerError::GitDiff(format!("spawn git add: {e}")))?;
    if !add.status.success() {
        return Err(EvalRunnerError::GitDiff(format!(
            "git add -A failed: {}",
            String::from_utf8_lossy(&add.stderr)
        )));
    }
    let diff = tokio::process::Command::new("git")
        .args(["diff", base_commit, "--binary"])
        .current_dir(work_dir)
        .output()
        .await
        .map_err(|e| EvalRunnerError::GitDiff(format!("spawn git diff: {e}")))?;
    if !diff.status.success() {
        return Err(EvalRunnerError::GitDiff(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&diff.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&diff.stdout).into_owned())
}

async fn finish_swebench(
    eval_case: &EvalCase,
    task: &SWEBenchTask,
    work_dir: &Path,
    config: &EvalRunnerConfig,
    start: Instant,
    agent_result: Option<AgentRunResult>,
    mut failures: Vec<String>,
) -> Result<EvalRunResult, EvalRunnerError> {
    let model_patch = capture_swebench_patch(work_dir, &task.base_commit).await?;

    let verify_dir = work_dir.join("__nanna_verify");
    std::fs::create_dir_all(&verify_dir)?;
    let verify_config = VerifyConfig {
        dataset_name: config.swebench_hf_dataset.clone(),
        model_name_or_path: format!("nanna__{}", config.model_name.replace([':', '/'], "-")),
        run_id: config.swebench_run_id.clone(),
        work_dir: verify_dir,
        max_workers: 1,
    };

    let predictions = vec![Prediction {
        instance_id: task.instance_id.clone(),
        model_patch,
    }];

    let verdicts = verify_predictions(&predictions, &verify_config).await?;
    let verdict: Option<&InstanceVerdict> =
        verdicts.iter().find(|v| v.instance_id == task.instance_id);

    let resolved = verdict.map(|v| v.resolved).unwrap_or(false);
    if let Some(v) = verdict {
        if let Some(err) = &v.error {
            failures.push(format!("SWE-bench verifier: {err}"));
        }
        if !v.resolved && v.error.is_none() {
            failures.push("SWE-bench verdict: not resolved".to_string());
        }
    } else {
        failures.push(format!(
            "SWE-bench verdict missing for instance {}",
            task.instance_id
        ));
    }

    let task_completed = agent_result.as_ref().is_some_and(|r| r.task_completed);
    if !task_completed {
        failures.push("Agent did not complete the task".to_string());
    }

    let iterations = agent_result.as_ref().map_or(0, |r| r.iterations);
    let token_usage = agent_result
        .as_ref()
        .and_then(|r| r.token_usage.clone())
        .unwrap_or_else(default_token_usage);

    let success = resolved && task_completed;
    let execution_time = start.elapsed();

    Ok(EvalRunResult {
        case_id: eval_case.case.id.clone(),
        success,
        execution_time,
        iterations,
        token_usage,
        verification: VerificationResult {
            build_passed: None,
            tests_passed: None,
            files_found: Vec::new(),
            missing_files: Vec::new(),
            symbols_found: Vec::new(),
            missing_symbols: Vec::new(),
        },
        failures,
        agent_result,
    })
}

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
        if contains_whole_word(&source_content, symbol) {
            found.push(symbol.clone());
        } else {
            missing.push(symbol.clone());
        }
    }
    (found, missing)
}

fn contains_whole_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let bytes = haystack.as_bytes();
    let nlen = needle.len();
    let mut start = 0;
    while let Some(idx) = haystack[start..].find(needle) {
        let abs = start + idx;
        let before_ok = abs == 0 || {
            let prev = haystack[..abs].chars().next_back();
            !prev.map(is_ident).unwrap_or(false)
        };
        let end = abs + nlen;
        let after_ok = end >= bytes.len() || {
            let next = haystack[end..].chars().next();
            !next.map(is_ident).unwrap_or(false)
        };
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
        if start >= haystack.len() {
            break;
        }
    }
    false
}

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
        unknown => {
            tracing::warn!(
                language = unknown,
                "unknown language in eval case; defaulting to Rust extensions"
            );
            vec!["rs"]
        }
    }
}

const SOURCE_IGNORE_DIRS: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    "dist",
    "build",
    ".idea",
    ".vscode",
];

fn collect_source_content(dir: &Path, extensions: &[&str]) -> String {
    let mut content = String::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if SOURCE_IGNORE_DIRS.contains(&name) {
                        continue;
                    }
                }
                content.push_str(&collect_source_content(&path, extensions));
            } else if file_type.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if extensions.contains(&ext) {
                        if let Ok(text) = std::fs::read_to_string(&path) {
                            content.push_str(&text);
                            content.push('\n');
                        }
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

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "fixture repo contains a symlink ({}); symlinks are not supported \
                     by the eval runner. Remove or replace the symlink in the fixture.",
                    src_path.display()
                ),
            ));
        }
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
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
    fn test_extensions_for_language_rust() {
        assert_eq!(extensions_for_language("rust"), vec!["rs"]);
        assert_eq!(extensions_for_language("Rust"), vec!["rs"]);
    }

    #[test]
    fn test_extensions_for_language_python() {
        assert_eq!(extensions_for_language("python"), vec!["py"]);
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
    fn test_contains_whole_word_matches_on_boundary() {
        assert!(contains_whole_word("pub fn greet() {}", "greet"));
        assert!(contains_whole_word("greet()", "greet"));
        assert!(contains_whole_word("greet", "greet"));
    }

    #[test]
    fn test_contains_whole_word_rejects_substring() {
        assert!(!contains_whole_word("fn greetings() {}", "greet"));
        assert!(!contains_whole_word("fn ungreet() {}", "greet"));
    }

    #[test]
    fn test_contains_whole_word_empty_needle() {
        assert!(!contains_whole_word("anything", ""));
    }

    #[test]
    fn test_is_swebench_case_detects_tag() {
        let mut case = sample_eval_case();
        case.metadata.tags = vec!["other".to_string(), SWEBENCH_TAG.to_string()];
        assert!(is_swebench_case(&case));
    }

    #[test]
    fn test_is_swebench_case_rejects_missing_tag() {
        let mut case = sample_eval_case();
        case.metadata.tags = vec!["other".to_string()];
        assert!(!is_swebench_case(&case));
    }

    #[test]
    fn test_resolve_swebench_dataset_path_explicit() {
        let case_dir = Path::new("/tmp/evals/cases/swebench-x");
        let config = EvalRunnerConfig {
            swebench_dataset_path: Some(PathBuf::from("/custom/dataset.jsonl")),
            ..EvalRunnerConfig::default()
        };
        let resolved = resolve_swebench_dataset_path(case_dir, &config).unwrap();
        assert_eq!(resolved, PathBuf::from("/custom/dataset.jsonl"));
    }

    #[test]
    fn test_resolve_swebench_dataset_path_default_layout() {
        let case_dir = Path::new("/tmp/evals/cases/swebench-x");
        let config = EvalRunnerConfig::default();
        let resolved = resolve_swebench_dataset_path(case_dir, &config).unwrap();
        assert_eq!(
            resolved,
            PathBuf::from("/tmp/evals/datasets/swebench-verified-sample.jsonl")
        );
    }

    fn sample_eval_case() -> EvalCase {
        use crate::agent::eval_case::{CaseInfo, CaseMetadata, ExpectedResult, TaskSpec};
        EvalCase {
            case: CaseInfo {
                id: "swebench-django__django-11099".to_string(),
                name: "django__django-11099".to_string(),
                description: String::new(),
            },
            task: TaskSpec {
                prompt: String::new(),
                language: "python".to_string(),
            },
            expected: ExpectedResult::default(),
            metadata: CaseMetadata::default(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_recursive_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let src = tempfile::TempDir::new().unwrap();
        let dst = tempfile::TempDir::new().unwrap();

        std::fs::write(src.path().join("real.txt"), "content").unwrap();
        symlink(src.path().join("real.txt"), src.path().join("link.txt")).unwrap();

        let err = copy_dir_recursive(src.path(), dst.path()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn test_collect_source_content_skips_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("outside.rs"), "fn outside_fn() {}").unwrap();
        std::fs::write(dir.path().join("inside.rs"), "fn inside_fn() {}").unwrap();
        symlink(
            outside.path().join("outside.rs"),
            dir.path().join("link.rs"),
        )
        .unwrap();

        let content = collect_source_content(dir.path(), &["rs"]);
        assert!(content.contains("inside_fn"));
        assert!(!content.contains("outside_fn"));
    }
}
