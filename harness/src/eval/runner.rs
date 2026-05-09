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
use crate::agent::{AgentConfig, AgentContext, AgentLoop, AgentRunReport, AgentRunResult};
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
use tracing::{info, warn};

const SWEBENCH_TAG: &str = "swebench-verified";
const DEFAULT_SWEBENCH_DATASET_REL: &str = "datasets/swebench-verified-sample.jsonl";

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
    #[error(
        "could not locate the `nanna` binary. Set NANNA_HARNESS_BIN to an \
         absolute path, or `cargo install --path harness`, or \
         `nix profile install .#nanna`."
    )]
    BinaryNotFound,
    #[error("nanna subprocess spawn failed: {0}")]
    SubprocessSpawn(String),
    #[error("nanna subprocess wrote no output JSON to {0}: {1}")]
    SubprocessNoOutput(PathBuf, String),
}

/// The agent execution outcome — either captured in-process (fixture
/// cases) or read back from the `nanna agent --output-json` subprocess
/// (SWE-bench cases).
#[derive(Debug, Clone)]
pub enum AgentOutcome {
    InProcess(AgentRunResult),
    Subprocess(AgentRunReport),
}

impl AgentOutcome {
    pub fn iterations(&self) -> usize {
        match self {
            AgentOutcome::InProcess(r) => r.iterations,
            AgentOutcome::Subprocess(r) => r.iterations,
        }
    }

    pub fn task_completed(&self) -> bool {
        match self {
            AgentOutcome::InProcess(r) => r.task_completed,
            AgentOutcome::Subprocess(r) => r.task_completed,
        }
    }

    pub fn token_usage(&self) -> Option<TokenUsage> {
        match self {
            AgentOutcome::InProcess(r) => r.token_usage.clone(),
            AgentOutcome::Subprocess(r) => r.token_usage.as_ref().map(|t| TokenUsage {
                prompt_tokens: t.prompt_tokens,
                completion_tokens: t.completion_tokens,
                total_tokens: t.total_tokens,
            }),
        }
    }

    pub fn as_in_process(&self) -> Option<&AgentRunResult> {
        match self {
            AgentOutcome::InProcess(r) => Some(r),
            AgentOutcome::Subprocess(_) => None,
        }
    }
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
    /// Override path to the SWE-bench dataset JSONL. When `None`, swebench
    /// cases resolve `<case_dir>/../../datasets/swebench-verified-sample.jsonl`.
    pub swebench_dataset_path: Option<PathBuf>,
    /// HuggingFace dataset name passed through to the upstream Python
    /// harness for swebench cases.
    pub swebench_hf_dataset: String,
    /// Run-id label used by the upstream harness output tree. Defaults to
    /// the wall-clock seconds at config-construction time.
    pub swebench_run_id: String,
    /// When `true`, the runner captures the agent's patch but skips the
    /// upstream Python harness call. Used by batched scoring (`harness-score`)
    /// where verification happens once across all instances after every
    /// agent has run, rather than per-instance.
    pub swebench_skip_verify: bool,
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
            swebench_skip_verify: false,
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

    pub fn with_swebench_skip_verify(mut self, skip: bool) -> Self {
        self.swebench_skip_verify = skip;
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
    /// The underlying agent outcome, if the agent ran successfully. For
    /// SWE-bench cases this is [`AgentOutcome::Subprocess`] (returned by
    /// the `nanna agent --output-json` subprocess); for fixtures it is
    /// [`AgentOutcome::InProcess`] (the in-process `AgentLoop` result).
    pub agent_result: Option<AgentOutcome>,
    /// Unified-diff patch the agent produced against the materialized repo.
    /// Populated only for SWE-bench cases (`swebench-verified` tag); `None`
    /// for happy-path fixtures. Set whether or not `swebench_skip_verify`
    /// is enabled, so callers can persist or batch-verify the patch later.
    pub swebench_patch: Option<String>,
}

/// Run a single eval case end-to-end.
///
/// 1. Copies the fixture repo (or for SWE-bench cases, clones the upstream
///    repo at `base_commit`) into an isolated temporary directory.
/// 2. Initialises and runs the [`AgentLoop`] with the task prompt.
/// 3. For SWE-bench cases, captures the agent's diff and shells out to the
///    upstream Python harness for the verdict. For other cases, runs the
///    in-tree `cargo build`/`cargo test`/file/symbol checks.
/// 4. Returns structured metrics.
pub async fn run_eval(
    eval_case: &EvalCase,
    case_dir: &Path,
    config: &EvalRunnerConfig,
) -> Result<EvalRunResult, EvalRunnerError> {
    let start = Instant::now();
    let is_swebench = is_swebench_case(eval_case);

    // --- 1. Isolate: copy fixture repo (or materialize swebench repo) into a temp dir ---
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

    // --- 2. Run agent ---
    //
    // SWE-bench cases drive the installed `nanna` binary as a subprocess so
    // the score reflects nanna-as-deployed (pod, observability, MCP wiring,
    // and any future container-only surface) rather than the agent loop
    // excised from it. Fixture cases keep the in-process path so existing
    // happy-path tests stay fast and don't require an installed binary.
    let timeout = Duration::from_secs(eval_case.metadata.timeout_secs);
    let agent_attempt = if is_swebench {
        run_agent_subprocess(work_dir, eval_case, config, timeout).await
    } else {
        run_agent_in_process(work_dir, eval_case, config, timeout).await
    };

    let (agent_result, mut failures) = match agent_attempt {
        Ok(Ok(outcome)) => (Some(outcome), Vec::new()),
        Ok(Err(soft)) => {
            let f = vec![format!("Agent error: {soft}")];
            if let Some(task) = &swebench_task {
                return finish_swebench(eval_case, task, work_dir, config, start, None, f).await;
            }
            let verification =
                run_verification(work_dir, &eval_case.expected, &eval_case.task.language).await;
            let execution_time = start.elapsed();
            let success = false;
            let mut f = f;
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
                swebench_patch: None,
            });
        }
        Err(hard) => return Err(hard),
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

    // --- 3. Verification ---
    let verification =
        run_verification(work_dir, &eval_case.expected, &eval_case.task.language).await;

    // --- 4. Collect metrics ---
    let iterations = agent_result.as_ref().map_or(0, |r| r.iterations());
    let task_completed = agent_result.as_ref().is_some_and(|r| r.task_completed());

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
        .and_then(|r| r.token_usage())
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
        swebench_patch: None,
    })
}

/// Run the agent in-process for fixture (non-SWE-bench) cases.
///
/// Returns:
/// - `Ok(Ok(outcome))` on success.
/// - `Ok(Err(msg))` on soft agent errors — the eval records the message in
///   `failures` and continues to verification.
/// - `Err(EvalRunnerError)` on hard infrastructure errors (timeout, model
///   provider construction failure).
async fn run_agent_in_process(
    work_dir: &Path,
    eval_case: &EvalCase,
    config: &EvalRunnerConfig,
    timeout: Duration,
) -> Result<Result<AgentOutcome, String>, EvalRunnerError> {
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

    match tokio::time::timeout(timeout, agent.run_tool_loop(context)).await {
        Ok(Ok(result)) => Ok(Ok(AgentOutcome::InProcess(result))),
        Ok(Err(e)) => Ok(Err(e.to_string())),
        Err(_elapsed) => Err(EvalRunnerError::Timeout(timeout)),
    }
}

/// Drive the installed `nanna` binary as a subprocess for SWE-bench cases.
///
/// Same return shape as [`run_agent_in_process`]: hard infrastructure
/// errors (binary missing, timeout, JSON shape unexpected) propagate as
/// `Err(EvalRunnerError)`; binary-side soft failures (agent loop error
/// surfaced as exit-non-zero with `Agent error: ...` on stderr) come back
/// as `Ok(Err(msg))` so the caller can record them in `failures` without
/// failing the whole run.
async fn run_agent_subprocess(
    work_dir: &Path,
    eval_case: &EvalCase,
    config: &EvalRunnerConfig,
    timeout: Duration,
) -> Result<Result<AgentOutcome, String>, EvalRunnerError> {
    let bin = locate_nanna_binary()?;
    let report_path = work_dir.join("__nanna_agent_report.json");
    let max_iter = config.max_iterations.to_string();

    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("agent")
        .arg("--prompt")
        .arg(&eval_case.task.prompt)
        .arg("--model")
        .arg(&config.model_name)
        .arg("--max-iterations")
        .arg(&max_iter)
        .arg("--work-dir")
        .arg(work_dir)
        .arg("--output-json")
        .arg(&report_path)
        // `--tools` is unconditional for SWE-bench subprocess runs: a
        // toolless agent has no way to mutate the materialized repo, so
        // any patch we'd capture would be empty by construction. The
        // CLI's `tools: bool` flag stays user-facing for `nanna agent`
        // human use.
        .arg("--tools");
    if let Some(url) = &config.model_base_url {
        cmd.arg("--ollama-url").arg(url);
    }
    cmd.kill_on_drop(true);

    info!(
        binary = %bin.display(),
        instance_id = %eval_case.case.id,
        "spawning nanna agent subprocess"
    );

    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Err(EvalRunnerError::SubprocessSpawn(format!(
                "could not spawn `{}`: {e}",
                bin.display()
            )));
        }
        Err(_elapsed) => return Err(EvalRunnerError::Timeout(timeout)),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        warn!(status = %output.status, stderr = %stderr, "nanna subprocess non-zero exit");
        return Ok(Err(format!(
            "nanna exited {}: {}",
            output.status,
            if stderr.is_empty() {
                "(no stderr)"
            } else {
                &stderr
            }
        )));
    }

    if !report_path.exists() {
        return Err(EvalRunnerError::SubprocessNoOutput(
            report_path,
            "exit-0 but no JSON".to_string(),
        ));
    }

    let report = AgentRunReport::read_from_path(&report_path)
        .map_err(|e| EvalRunnerError::SubprocessNoOutput(report_path.clone(), e.to_string()))?;
    Ok(Ok(AgentOutcome::Subprocess(report)))
}

fn locate_nanna_binary() -> Result<PathBuf, EvalRunnerError> {
    if let Ok(p) = std::env::var("NANNA_HARNESS_BIN") {
        let path = PathBuf::from(p);
        // Reject directories: `path.exists()` alone matches a bare dir,
        // and the subsequent `cmd.output()` would then fail with a cryptic
        // OS-level "is a directory" rather than the structured
        // `BinaryNotFound`. See the "[Bug / confusing error]
        // locate_nanna_binary accepts a directory path" review note.
        if !path.is_file() {
            return Err(EvalRunnerError::BinaryNotFound);
        }
        return Ok(path);
    }
    which::which("nanna").map_err(|_| EvalRunnerError::BinaryNotFound)
}

// ---------------------------------------------------------------------------
// SWE-bench helpers
// ---------------------------------------------------------------------------

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
    agent_result: Option<AgentOutcome>,
    mut failures: Vec<String>,
) -> Result<EvalRunResult, EvalRunnerError> {
    let model_patch = capture_swebench_patch(work_dir, &task.base_commit).await?;

    let task_completed = agent_result.as_ref().is_some_and(|r| r.task_completed());
    if !task_completed {
        failures.push("Agent did not complete the task".to_string());
    }
    let iterations = agent_result.as_ref().map_or(0, |r| r.iterations());
    let token_usage = agent_result
        .as_ref()
        .and_then(|r| r.token_usage())
        .unwrap_or_else(default_token_usage);

    let resolved = if config.swebench_skip_verify {
        failures.push("verifier skipped — score-mode batched run".to_string());
        false
    } else {
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
            model_patch: model_patch.clone(),
        }];
        let verdicts = verify_predictions(&predictions, &verify_config).await?;
        let verdict: Option<&InstanceVerdict> =
            verdicts.iter().find(|v| v.instance_id == task.instance_id);
        if let Some(v) = verdict {
            if let Some(err) = &v.error {
                failures.push(format!("SWE-bench verifier: {err}"));
            }
            if !v.resolved && v.error.is_none() {
                failures.push("SWE-bench verdict: not resolved".to_string());
            }
            v.resolved
        } else {
            failures.push(format!(
                "SWE-bench verdict missing for instance {}",
                task.instance_id
            ));
            false
        }
    };

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
        swebench_patch: Some(model_patch),
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
        if contains_whole_word(&source_content, symbol) {
            found.push(symbol.clone());
        } else {
            missing.push(symbol.clone());
        }
    }
    (found, missing)
}

/// Returns `true` if `haystack` contains `needle` bounded by non-identifier
/// characters (or haystack boundaries) on both sides.
///
/// This avoids spurious matches where the required symbol happens to be a
/// substring of an unrelated identifier (e.g. looking for `greet` and
/// accidentally matching `greetings`). An identifier character is any
/// alphanumeric ASCII character or `_`, matching the common identifier
/// convention shared across the supported languages.
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

/// Map a language name to its common file extensions.
///
/// Unknown languages fall through to Rust extensions since every eval case
/// fixture in `evals/cases/` is currently Rust. The fallthrough is logged at
/// `warn` level so a typo in `task.toml` doesn't silently mis-route symbol
/// search.
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

/// Directory names that are never walked when searching for source content.
///
/// These are build artefacts, VCS metadata, or dependency caches that can be
/// very large and are not part of the agent-produced source tree. Including
/// them would produce false-positive symbol matches (e.g. a required symbol
/// appearing in a compiled `target/` artefact) and slow verification
/// dramatically on any non-trivial repo.
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

/// Recursively read source files under `dir` matching the given extensions
/// and concatenate their contents.
///
/// Directories listed in [`SOURCE_IGNORE_DIRS`] are skipped. Symlinks are not
/// followed to avoid cycles and to keep verification scoped to the
/// agent-produced tree.
fn collect_source_content(dir: &Path, extensions: &[&str]) -> String {
    let mut content = String::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            // Use the DirEntry's cheap file_type rather than following symlinks.
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            // Skip symlinks entirely — verification should only inspect real
            // files produced by the agent. Following symlinks can loop and can
            // reach files outside the workspace.
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

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

/// Recursively copy the contents of `src` into `dst`.
///
/// Symlinks are **not** followed: if a symlink is encountered anywhere in the
/// source tree, this function returns an [`std::io::Error`] of kind
/// [`std::io::ErrorKind::InvalidInput`] rather than silently dereferencing
/// the target. This protects against cycles (a symlink pointing to an
/// ancestor directory would otherwise recurse forever) and against accidental
/// escape from the fixture workspace (a symlink could point outside `src`).
///
/// If a fixture repo genuinely needs symlinks, the caller should switch to
/// a richer copy utility (e.g. `fs_extra::dir::copy`) that can preserve
/// symlink targets. Current eval fixtures are plain source trees and do not
/// contain symlinks.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        // Use DirEntry::file_type so we inspect the entry itself rather than
        // a followed target (path.is_dir()/is_file() traverse symlinks).
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
        // Silently skip other entry types (e.g. sockets, block devices) —
        // they are never part of a source fixture.
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
    fn agent_outcome_subprocess_token_usage_propagates() {
        // Cover the AgentOutcome::Subprocess branch of token_usage() /
        // iterations() / task_completed() so the patch diff has more than
        // the single in-process path exercised. The subprocess runner is
        // integration-tested separately (see
        // harness/tests/nanna_subprocess_smoke.rs).
        use crate::agent::{TokenUsageDto, SCHEMA_VERSION};
        let report = AgentRunReport {
            schema_version: SCHEMA_VERSION,
            task_completed: true,
            iterations: 4,
            final_state: "Completed".to_string(),
            result_summary: "ok".to_string(),
            token_usage: Some(TokenUsageDto {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
            }),
            tool_calls: Vec::new(),
        };
        let outcome = AgentOutcome::Subprocess(report);
        assert_eq!(outcome.iterations(), 4);
        assert!(outcome.task_completed());
        let usage = outcome.token_usage().expect("expected usage");
        assert_eq!(usage.prompt_tokens, 1);
        assert_eq!(usage.completion_tokens, 2);
        assert_eq!(usage.total_tokens, 3);
        assert!(outcome.as_in_process().is_none());
    }

    #[test]
    fn agent_outcome_in_process_helpers() {
        use crate::agent::AgentState;
        let r = AgentRunResult {
            final_state: AgentState::Completed,
            iterations: 7,
            task_completed: true,
            result_summary: "ok".to_string(),
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            token_usage: Some(TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 6,
                total_tokens: 11,
            }),
        };
        let out = AgentOutcome::InProcess(r);
        assert_eq!(out.iterations(), 7);
        assert!(out.task_completed());
        assert_eq!(out.token_usage().unwrap().total_tokens, 11);
        assert!(out.as_in_process().is_some());
    }

    #[test]
    fn agent_outcome_token_usage_none_paths() {
        use crate::agent::{AgentRunReport, AgentState, SCHEMA_VERSION};

        let r = AgentRunResult {
            final_state: AgentState::Completed,
            iterations: 0,
            task_completed: false,
            result_summary: String::new(),
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            token_usage: None,
        };
        assert!(AgentOutcome::InProcess(r).token_usage().is_none());

        let report = AgentRunReport {
            schema_version: SCHEMA_VERSION,
            task_completed: false,
            iterations: 0,
            final_state: "Completed".to_string(),
            result_summary: String::new(),
            token_usage: None,
            tool_calls: vec![],
        };
        assert!(AgentOutcome::Subprocess(report).token_usage().is_none());
    }

    #[test]
    #[serial_test::serial(nanna_harness_bin_env)]
    fn locate_nanna_binary_via_env_var_pointing_at_extant_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin_path = dir.path().join("fake-nanna");
        std::fs::write(&bin_path, "#!/bin/sh\nexit 0\n").unwrap();

        let key = "NANNA_HARNESS_BIN";
        let old = std::env::var(key).ok();
        std::env::set_var(key, &bin_path);
        let resolved = locate_nanna_binary();
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        assert_eq!(resolved.unwrap(), bin_path);
    }

    #[test]
    #[serial_test::serial(nanna_harness_bin_env)]
    fn locate_nanna_binary_rejects_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let key = "NANNA_HARNESS_BIN";
        let old = std::env::var(key).ok();
        std::env::set_var(key, dir.path());
        let result = locate_nanna_binary();
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        assert!(matches!(result, Err(EvalRunnerError::BinaryNotFound)));
    }

    #[test]
    #[serial_test::serial(nanna_harness_bin_env)]
    fn locate_nanna_binary_rejects_missing_path() {
        let key = "NANNA_HARNESS_BIN";
        let old = std::env::var(key).ok();
        std::env::set_var(key, "/var/tmp/nanna-must-not-exist-zzzz-12345");
        let result = locate_nanna_binary();
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        assert!(matches!(result, Err(EvalRunnerError::BinaryNotFound)));
    }

    #[test]
    fn eval_runner_error_display_covers_new_subprocess_variants() {
        let e = EvalRunnerError::BinaryNotFound;
        assert!(e.to_string().contains("nanna"));

        let e =
            EvalRunnerError::SubprocessSpawn("could not spawn /bin/x: no such file".to_string());
        assert!(e.to_string().contains("subprocess spawn failed"));

        let e = EvalRunnerError::SubprocessNoOutput(
            std::path::PathBuf::from("/tmp/r.json"),
            "exit-0 but no JSON".to_string(),
        );
        let s = e.to_string();
        assert!(s.contains("/tmp/r.json"));
        assert!(s.contains("exit-0"));
    }

    // The subprocess tests below rely on a shebang-executable shell script
    // that mirrors the `nanna agent --output-json` binary's shape. Windows
    // (and any future non-unix CI target) doesn't honour `#!/usr/bin/env
    // bash` shebang dispatch, so the script-based test fixtures are
    // gated. The runner is feature-equivalent across platforms (it just
    // shells out via tokio::process::Command) so coverage is not
    // platform-specific from a correctness standpoint.
    #[cfg(unix)]
    fn write_fake_nanna(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("fake-nanna.sh");
        std::fs::write(&path, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    fn make_swebench_case(id: &str) -> EvalCase {
        let toml = format!(
            r#"
[case]
id = "{id}"
name = "test"
description = "subprocess unit test"

[task]
prompt = "do nothing"

[metadata]
timeout_secs = 30
tags = ["{tag}"]
"#,
            tag = SWEBENCH_TAG
        );
        EvalCase::from_toml_str(&toml).unwrap()
    }

    #[cfg(unix)]
    fn fake_report_writer_script() -> &'static str {
        r#"#!/usr/bin/env bash
set -e
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-json) out="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[[ -n "$out" ]] || { echo "no --output-json" >&2; exit 1; }
cat > "$out" <<'JSON'
{
  "schema_version": 1,
  "task_completed": true,
  "iterations": 4,
  "final_state": "Completed",
  "result_summary": "fake done",
  "token_usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
  "tool_calls": []
}
JSON
"#
    }

    #[cfg(unix)]
    async fn with_nanna_bin<F, Fut, T>(bin: &std::path::Path, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let key = "NANNA_HARNESS_BIN";
        let old = std::env::var(key).ok();
        std::env::set_var(key, bin);
        let result = f().await;
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        result
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(nanna_harness_bin_env)]
    async fn run_agent_subprocess_reads_fake_report() {
        let dir = tempfile::TempDir::new().unwrap();
        let work_dir = dir.path();
        let bin = write_fake_nanna(work_dir, fake_report_writer_script());
        let case = make_swebench_case("swebench-fake-001");
        let config = EvalRunnerConfig::default()
            .with_max_iterations(3)
            .with_base_url("http://127.0.0.1:1");

        let result = with_nanna_bin(&bin, || {
            run_agent_subprocess(work_dir, &case, &config, Duration::from_secs(10))
        })
        .await;

        let outcome = result.unwrap().unwrap();
        assert_eq!(outcome.iterations(), 4);
        assert!(outcome.task_completed());
        assert_eq!(outcome.token_usage().unwrap().total_tokens, 3);
        assert!(outcome.as_in_process().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(nanna_harness_bin_env)]
    async fn run_agent_subprocess_soft_errors_on_non_zero_exit() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = write_fake_nanna(
            dir.path(),
            "#!/usr/bin/env bash\necho 'fake nanna error' >&2\nexit 2\n",
        );
        let case = make_swebench_case("swebench-fake-002");
        let config = EvalRunnerConfig::default();

        let result = with_nanna_bin(&bin, || {
            run_agent_subprocess(dir.path(), &case, &config, Duration::from_secs(10))
        })
        .await;

        let msg = result.unwrap().unwrap_err();
        assert!(msg.contains("fake nanna error"), "got: {msg}");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(nanna_harness_bin_env)]
    async fn run_agent_subprocess_soft_error_with_empty_stderr() {
        // Cover the (no stderr) branch where the binary exits non-zero
        // without writing anything to stderr.
        let dir = tempfile::TempDir::new().unwrap();
        let bin = write_fake_nanna(dir.path(), "#!/usr/bin/env bash\nexit 7\n");
        let case = make_swebench_case("swebench-fake-005");
        let config = EvalRunnerConfig::default();

        let result = with_nanna_bin(&bin, || {
            run_agent_subprocess(dir.path(), &case, &config, Duration::from_secs(10))
        })
        .await;

        let msg = result.unwrap().unwrap_err();
        assert!(msg.contains("(no stderr)"), "got: {msg}");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(nanna_harness_bin_env)]
    async fn run_agent_subprocess_hard_errors_when_exit_zero_no_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = write_fake_nanna(dir.path(), "#!/usr/bin/env bash\nexit 0\n");
        let case = make_swebench_case("swebench-fake-003");
        let config = EvalRunnerConfig::default();

        let result = with_nanna_bin(&bin, || {
            run_agent_subprocess(dir.path(), &case, &config, Duration::from_secs(10))
        })
        .await;

        match result.unwrap_err() {
            EvalRunnerError::SubprocessNoOutput(path, why) => {
                assert!(path.to_string_lossy().contains("__nanna_agent_report.json"));
                assert!(why.contains("exit-0"));
            }
            other => panic!("expected SubprocessNoOutput, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(nanna_harness_bin_env)]
    async fn run_agent_subprocess_hard_errors_when_json_invalid() {
        // Cover the JSON parse failure branch in
        // AgentRunReport::read_from_path → SubprocessNoOutput map_err.
        let dir = tempfile::TempDir::new().unwrap();
        let bin = write_fake_nanna(
            dir.path(),
            r#"#!/usr/bin/env bash
set -e
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-json) out="$2"; shift 2 ;;
    *) shift ;;
  esac
done
echo "this is not json" > "$out"
"#,
        );
        let case = make_swebench_case("swebench-fake-006");
        let config = EvalRunnerConfig::default();

        let result = with_nanna_bin(&bin, || {
            run_agent_subprocess(dir.path(), &case, &config, Duration::from_secs(10))
        })
        .await;

        assert!(matches!(
            result.unwrap_err(),
            EvalRunnerError::SubprocessNoOutput(_, _)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(nanna_harness_bin_env)]
    async fn run_agent_subprocess_times_out() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = write_fake_nanna(dir.path(), "#!/usr/bin/env bash\nsleep 30\n");
        let case = make_swebench_case("swebench-fake-004");
        let config = EvalRunnerConfig::default();

        let result = with_nanna_bin(&bin, || {
            run_agent_subprocess(dir.path(), &case, &config, Duration::from_millis(50))
        })
        .await;

        assert!(matches!(result.unwrap_err(), EvalRunnerError::Timeout(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(nanna_harness_bin_env)]
    async fn run_agent_subprocess_propagates_binary_not_found() {
        let case = make_swebench_case("swebench-fake-007");
        let config = EvalRunnerConfig::default();

        let key = "NANNA_HARNESS_BIN";
        let old = std::env::var(key).ok();
        std::env::set_var(key, "/var/tmp/nanna-totally-missing-xyz");
        let dir = tempfile::TempDir::new().unwrap();
        let result =
            run_agent_subprocess(dir.path(), &case, &config, Duration::from_secs(10)).await;
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        assert!(matches!(
            result.unwrap_err(),
            EvalRunnerError::BinaryNotFound
        ));
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
            swebench_patch: None,
        };
        assert_eq!(result.case_id, "test-001");
        assert!(result.success);
        assert_eq!(result.iterations, 3);
        assert_eq!(result.token_usage.total_tokens, 0);
        assert!(result.failures.is_empty());
        assert!(result.agent_result.is_none());
        assert!(result.swebench_patch.is_none());
    }

    #[test]
    fn test_config_default_swebench_skip_verify_off() {
        let config = EvalRunnerConfig::default();
        assert!(
            !config.swebench_skip_verify,
            "default must preserve current per-instance verify behaviour"
        );
    }

    #[test]
    fn test_config_with_swebench_skip_verify() {
        let config = EvalRunnerConfig::default().with_swebench_skip_verify(true);
        assert!(config.swebench_skip_verify);
    }

    #[test]
    fn test_contains_whole_word_matches_on_boundary() {
        assert!(contains_whole_word("pub fn greet() {}", "greet"));
        assert!(contains_whole_word("greet()", "greet"));
        assert!(contains_whole_word("greet", "greet"));
        assert!(contains_whole_word("a.greet()", "greet"));
    }

    #[test]
    fn test_contains_whole_word_rejects_substring() {
        // `greet` should NOT match inside `greetings` or `ungreet`.
        assert!(!contains_whole_word("fn greetings() {}", "greet"));
        assert!(!contains_whole_word("fn ungreet() {}", "greet"));
        assert!(!contains_whole_word("foo_greet_bar", "greet"));
        assert!(!contains_whole_word("greet1", "greet"));
    }

    #[test]
    fn test_contains_whole_word_empty_needle() {
        assert!(!contains_whole_word("anything", ""));
    }

    #[test]
    fn test_verify_symbols_rejects_substring_match() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        // Only `greetings` is present — asking for `greet` must NOT match.
        std::fs::write(src.join("lib.rs"), "pub fn greetings() {}").unwrap();

        let (found, missing) = verify_symbols(dir.path(), &["greet".to_string()], "rust");

        assert!(
            found.is_empty(),
            "greet should not match substring of greetings"
        );
        assert_eq!(missing, vec!["greet"]);
    }

    #[test]
    fn test_collect_source_content_skips_ignored_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        // Real source file that should be included.
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        // target/ should be skipped — content inside must NOT leak into search.
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("artifact.rs"), "fn should_be_ignored() {}").unwrap();
        // .git/ should likewise be skipped.
        let git = dir.path().join(".git");
        std::fs::create_dir(&git).unwrap();
        std::fs::write(git.join("hooks.rs"), "fn git_hook() {}").unwrap();

        let content = collect_source_content(dir.path(), &["rs"]);
        assert!(content.contains("fn main()"));
        assert!(!content.contains("should_be_ignored"));
        assert!(!content.contains("git_hook"));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_recursive_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let src = tempfile::TempDir::new().unwrap();
        let dst = tempfile::TempDir::new().unwrap();

        std::fs::write(src.path().join("real.txt"), "content").unwrap();
        // Create a symlink inside the source tree pointing somewhere.
        symlink(src.path().join("real.txt"), src.path().join("link.txt")).unwrap();

        let err = copy_dir_recursive(src.path(), dst.path()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("symlink"));
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
    fn test_collect_source_content_skips_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("outside.rs"), "fn outside_fn() {}").unwrap();
        std::fs::write(dir.path().join("inside.rs"), "fn inside_fn() {}").unwrap();
        // Symlink from inside the searched dir to a file outside.
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
