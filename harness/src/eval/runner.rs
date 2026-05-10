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
            stderr
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
    // Remove report from work_dir before capture_swebench_patch runs git add -A;
    // leaving it would include this harness artefact in the SWE-bench patch.
    let _ = std::fs::remove_file(&report_path);
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
    anc.next();
    let evals_dir = anc.next().ok_or_else(|| {
        EvalRunnerError::SwebenchDatasetMissing(case_dir.join("../../").join(DEFAULT_SWEBENCH_DATASET_REL))
    })?;
    Ok(evals_dir.join(DEFAULT_SWEBENCH_DATASET_REL))
}

fn load_swebench_task(
    eval_case: &EvalCase,
    case_dir: &Path,
    config: &EvalRunnerConfig,
) -> Result<SWEBenchTask, EvalRunnerError> {
    let dataset_path = resolve_swebench_dataset_path(case_dir, config)?;
    if !dataset_path.exists() {
        return Err(EvalRunnerError::SwebenchDatasetMissing(dataset_path));
    }
    let tasks = load_swebench_dataset(&dataset_path)?;
    tasks
        .into_iter()
        .find(|t| t.instance_id == eval_case.case.id)
        .ok_or_else(|| EvalRunnerError::SwebenchInstanceMissing(eval_case.case.id.clone()))
}

async fn finish_swebench<F, Fut>(
    eval_case: &EvalCase,
    task: &SWEBenchTask,
    work_dir: &Path,
    config: &EvalRunnerConfig,
    start: Instant,
    agent_result: Option<AgentOutcome>,
    mut failures: Vec<String>,
) -> Result<EvalRunResult, EvalRunnerError>
where
    F: Fn(Vec<Prediction>, VerifyConfig) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<InstanceVerdict>, VerifyError>>,
{
    finish_swebench_with(
        eval_case,
        task,
        work_dir,
        config,
        start,
        agent_result,
        failures,
        verify_predictions,
    )
    .await
}

async fn finish_swebench_with<F, Fut>(
    eval_case: &EvalCase,
    task: &SWEBenchTask,
    work_dir: &Path,
    config: &EvalRunnerConfig,
    start: Instant,
    agent_result: Option<AgentOutcome>,
    mut failures: Vec<String>,
    verifier: F,
) -> Result<EvalRunResult, EvalRunnerError>
where
    F: Fn(Vec<Prediction>, VerifyConfig) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<InstanceVerdict>, VerifyError>>,
{
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
        let cfg = VerifyConfig {
            run_id: config.swebench_run_id.clone(),
            work_dir: work_dir.to_path_buf(),
            max_workers: 1,
        };
        let predictions = vec![Prediction {
            instance_id: task.instance_id.clone(),
            model_patch: model_patch.clone(),
            model_name_or_path: config.model_name.clone(),
        }];
        match tokio::pin!(verify_predictions(predictions, cfg)) {
        Ok(verdicts) => {
                if let Some(v) = verdicts.iter().find(|v| v.instance_id == task.instance_id) {
                    v.resolved
                } else {
                    failures.push(format!(
                        "SWE-bench verifier returned no verdict for instance {}",
                        task.instance_id
                    ));
                    false
                }
            }
            Err(e) => {
                failures.push(format!("SWE-bench verify error: {e}"));
                false
            }
        }
    };

    if !resolved && !config.swebench_skip_verify {
        failures.push(format!(
            "SWE-bench instance {} not resolved",
            task.instance_id
        ));
    }

    let success = failures.is_empty();
    let execution_time = start.elapsed();

    Ok(EvalRunResult {
        case_id: eval_case.case.id.clone(),
        success,
        execution_time,
        iterations,
        token_usage,
        verification: VerificationResult {
            build_passed: None,
            tests_passed: Some(resolved),
            files_found: vec![],
            missing_files: vec![],
            symbols_found: vec![],
            missing_symbols: vec![],
        },
        failures,
        agent_result,
        swebench_patch: Some(model_patch),
    })
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

async fn run_verification(
    work_dir: &Path,
    expected: &crate::agent::eval_case::ExpectedResult,
    language: &str,
) -> VerificationResult {
    let build_passed = if expected.build_must_pass {
        Some(verify_build(work_dir, language).await)
    } else {
        None
    };

    let tests_passed = if expected.tests_must_pass {
        Some(verify_tests(work_dir, language).await)
    } else {
        None
    };

    let (files_found, missing_files) = check_files(work_dir, &expected.files_changed);
    let (symbols_found, missing_symbols) =
        check_symbols(work_dir, &expected.required_symbols, language);

    VerificationResult {
        build_passed,
        tests_passed,
        files_found,
        missing_files,
        symbols_found,
        missing_symbols,
    }
}

async fn verify_build(work_dir: &Path, language: &str) -> bool {
    match language {
        "rust" => verify_with_cmd(work_dir, "cargo", "build", "Build").await,
        _ => {
            warn!(language, "no build verifier for language");
            true
        }
    }
}

async fn verify_tests(work_dir: &Path, language: &str) -> bool {
    match language {
        "rust" => verify_with_cmd(work_dir, "cargo", "test", "Test").await,
        _ => {
            warn!(language, "no test verifier for language");
            true
        }
    }
}

async fn verify_with_cmd(work_dir: &Path, cmd: &str, sub: &str, label: &str) -> bool {
    match tokio::process::Command::new(cmd)
        .arg(sub)
        .current_dir(work_dir)
        .output()
        .await
    {
        Ok(output) => {
            if !output.status.success() {
                warn!(
                    label,
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "verification failed"
                );
            }
            output.status.success()
        }
        Err(e) => {
            warn!(label, error = %e, "verification command failed to spawn");
            false
        }
    }
}

fn check_files(work_dir: &Path, expected: &[String]) -> (Vec<String>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for f in expected {
        if work_dir.join(f).exists() {
            found.push(f.clone());
        } else {
            missing.push(f.clone());
        }
    }
    (found, missing)
}

fn check_symbols(work_dir: &Path, symbols: &[String], _language: &str) -> (Vec<String>, Vec<String>) {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for sym in symbols {
        let found_in_any = walkdir::WalkDir::new(work_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .any(|e| {
                std::fs::read_to_string(e.path())
                    .map(|c| c.contains(sym.as_str()))
                    .unwrap_or(false)
            });
        if found_in_any {
            found.push(sym.clone());
        } else {
            missing.push(sym.clone());
        }
    }
    (found, missing)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.map_err(|e| std::io::Error::other(e.to_string()))?;
        let rel = entry.path().strip_prefix(src).unwrap();
        let dest = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn verification_failures(v: &VerificationResult) -> Vec<String> {
    let mut out = Vec::new();
    if v.build_passed == Some(false) {
        out.push("Build failed".to_string());
    }
    if v.tests_passed == Some(false) {
        out.push("Tests failed".to_string());
    }
    for f in &v.missing_files {
        out.push(format!("Missing file: {f}"));
    }
    for s in &v.missing_symbols {
        out.push(format!("Missing symbol: {s}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::eval_case::{CaseInfo, CaseMetadata, EvalCase, ExpectedResult, TaskSpec};
    use std::process::Command;
    use tempfile::tempdir;

    fn make_case(id: &str, language: &str, tags: Vec<String>) -> EvalCase {
        EvalCase {
            case: CaseInfo {
                id: id.to_string(),
                name: id.to_string(),
                description: String::new(),
            },
            task: TaskSpec {
                prompt: "fix the bug".to_string(),
                language: language.to_string(),
            },
            expected: ExpectedResult {
                files_changed: vec![],
                build_must_pass: false,
                tests_must_pass: false,
                required_symbols: vec![],
            },
            metadata: CaseMetadata {
                difficulty: "easy".to_string(),
                tags,
                timeout_secs: 30,
            },
        }
    }

    fn make_swebench_case(id: &str) -> EvalCase {
        make_case(id, "python", vec!["swebench-verified".to_string()])
    }

    fn init_local_bare_repo(seed: &Path, bare: &Path) -> String {
        fn git_must<I, S>(args: I, cwd: &Path, extra_env: &[(&str, &str)])
        where
            I: IntoIterator<Item = S>,
            S: AsRef<std::ffi::OsStr>,
        {
            let mut cmd = Command::new("git");
            cmd.args(args).current_dir(cwd);
            cmd.env("GIT_CONFIG_NOSYSTEM", "1");
            cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
            cmd.env("HOME", cwd);
            for (k, v) in extra_env {
                cmd.env(k, v);
            }
            let out = cmd.output().unwrap();
            assert!(
                out.status.success(),
                "git command failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        git_must(["init", "--quiet"], seed, &[]);
        git_must(["config", "user.email", "test@example.com"], seed, &[]);
        git_must(["config", "user.name", "Test"], seed, &[]);
        git_must(["config", "commit.gpgsign", "false"], seed, &[]);
        std::fs::write(seed.join("solution.py"), "# placeholder\n").unwrap();
        git_must(["add", "solution.py"], seed, &[]);
        git_must(
            ["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"],
            seed,
            &[
                ("GIT_AUTHOR_NAME", "Test"),
                ("GIT_AUTHOR_EMAIL", "test@example.com"),
                ("GIT_COMMITTER_NAME", "Test"),
                ("GIT_COMMITTER_EMAIL", "test@example.com"),
            ],
        );
        let oid_out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(seed)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", seed)
            .output()
            .unwrap();
        assert!(oid_out.status.success());
        let oid = String::from_utf8(oid_out.stdout).unwrap().trim().to_string();
        git_must(
            [
                "clone",
                "--bare",
                "--quiet",
                seed.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            seed,
            &[],
        );
        oid
    }

    // ---------------------------------------------------------------------------
    // Unit tests
    // ---------------------------------------------------------------------------

    #[test]
    fn is_swebench_case_true_for_swebench_tag() {
        let case = make_swebench_case("inst-001");
        assert!(is_swebench_case(&case));
    }

    #[test]
    fn is_swebench_case_false_for_no_tag() {
        let case = make_case("inst-001", "rust", vec![]);
        assert!(!is_swebench_case(&case));
    }

    #[test]
    fn is_swebench_case_false_for_unrelated_tag() {
        let case = make_case("inst-001", "rust", vec!["some-other-tag".to_string()]);
        assert!(!is_swebench_case(&case));
    }

    #[test]
    fn resolve_dataset_path_prefers_explicit_config() {
        let case = make_swebench_case("x");
        let dir = tempdir().unwrap();
        let cfg = EvalRunnerConfig {
            swebench_dataset_path: Some(PathBuf::from("/explicit/path.jsonl")),
            ..EvalRunnerConfig::default()
        };
        let path = resolve_swebench_dataset_path(dir.path(), &cfg).unwrap();
        assert_eq!(path, PathBuf::from("/explicit/path.jsonl"));
    }

    #[test]
    fn resolve_dataset_path_prefers_env_over_ancestor_walk() {
        let dir = tempdir().unwrap();
        std::env::set_var("NANNA_SWEBENCH_DATASET", "/env/override.jsonl");
        let cfg = EvalRunnerConfig::default();
        let path = resolve_swebench_dataset_path(dir.path(), &cfg).unwrap();
        std::env::remove_var("NANNA_SWEBENCH_DATASET");
        assert_eq!(path, PathBuf::from("/env/override.jsonl"));
    }

    #[test]
    fn locate_nanna_binary_returns_err_when_env_points_at_directory() {
        let dir = tempdir().unwrap();
        std::env::set_var("NANNA_HARNESS_BIN", dir.path());
        let result = locate_nanna_binary();
        std::env::remove_var("NANNA_HARNESS_BIN");
        assert!(matches!(result, Err(EvalRunnerError::BinaryNotFound)));
    }

    #[test]
    fn check_files_finds_existing_and_reports_missing() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("present.py"), "content").unwrap();
        let (found, missing) = check_files(
            dir.path(),
            &["present.py".to_string(), "missing.py".to_string()],
        );
        assert_eq!(found, vec!["present.py".to_string()]);
        assert_eq!(missing, vec!["missing.py".to_string()]);
    }

    #[test]
    fn check_files_empty_expected_returns_empty_vecs() {
        let dir = tempdir().unwrap();
        let (found, missing) = check_files(dir.path(), &[]);
        assert!(found.is_empty());
        assert!(missing.is_empty());
    }

    #[test]
    fn check_symbols_finds_symbol_in_file() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("code.py"),
            "def my_function():\n    pass\n",
        )
        .unwrap();
        let (found, missing) = check_symbols(
            dir.path(),
            &["my_function".to_string(), "nonexistent_sym".to_string()],
            "python",
        );
        assert_eq!(found, vec!["my_function".to_string()]);
        assert_eq!(missing, vec!["nonexistent_sym".to_string()]);
    }

    #[test]
    fn copy_dir_recursive_copies_nested_structure() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        std::fs::create_dir(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub").join("file.txt"), "hello").unwrap();
        copy_dir_recursive(src.path(), dst.path()).unwrap();
        let content =
            std::fs::read_to_string(dst.path().join("sub").join("file.txt")).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn verification_failures_lists_all_failure_kinds() {
        let v = VerificationResult {
            build_passed: Some(false),
            tests_passed: Some(false),
            files_found: vec![],
            missing_files: vec!["a.py".to_string()],
            symbols_found: vec![],
            missing_symbols: vec!["foo".to_string()],
        };
        let failures = verification_failures(&v);
        assert!(failures.iter().any(|f| f.contains("Build")));
        assert!(failures.iter().any(|f| f.contains("Test")));
        assert!(failures.iter().any(|f| f.contains("a.py")));
        assert!(failures.iter().any(|f| f.contains("foo")));
    }

    #[test]
    fn default_runner_config_has_expected_defaults() {
        let cfg = EvalRunnerConfig::default();
        assert_eq!(cfg.model_name, "qwen3:0.6b");
        assert!(!cfg.verbose);
        assert_eq!(cfg.max_iterations, 100);
        assert!(!cfg.swebench_skip_verify);
    }

    #[test]
    fn runner_config_builder_methods_set_fields() {
        let cfg = EvalRunnerConfig::default()
            .with_model("llama3")
            .with_base_url("http://localhost:11434")
            .with_verbose(true)
            .with_max_iterations(5)
            .with_swebench_skip_verify(true);
        assert_eq!(cfg.model_name, "llama3");
        assert_eq!(
            cfg.model_base_url,
            Some("http://localhost:11434".to_string())
        );
        assert!(cfg.verbose);
        assert_eq!(cfg.max_iterations, 5);
        assert!(cfg.swebench_skip_verify);
    }

    #[test]
    fn agent_outcome_delegates_iterations_to_inner() {
        let in_process = AgentOutcome::InProcess(AgentRunResult {
            iterations: 7,
            task_completed: true,
            token_usage: None,
        });
        assert_eq!(in_process.iterations(), 7);
    }

    // ---------------------------------------------------------------------------
    // Integration tests (require git on PATH)
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn capture_swebench_patch_errors_when_work_dir_is_not_a_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        let result = capture_swebench_patch(dir.path(), "deadbeef").await;
        assert!(matches!(result, Err(EvalRunnerError::GitDiff(_))));
    }

    #[tokio::test]
    async fn capture_swebench_patch_errors_when_base_commit_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", dir.path())
            .status()
            .unwrap();
        let result =
            capture_swebench_patch(dir.path(), "0000000000000000000000000000000000000000").await;
        assert!(matches!(result, Err(EvalRunnerError::GitDiff(_))));
    }

    #[tokio::test]
    async fn verify_with_cmd_returns_false_when_binary_is_missing() {
        // Cover the `Err(e)` arm of verify_build/verify_tests when the
        // command isn't on PATH. We pass a name that's vanishingly
        // unlikely to exist; tokio::process::Command::output returns Err.
        let dir = tempfile::tempdir().unwrap();
        let result =
            verify_with_cmd(dir.path(), "nanna-no-such-binary-xyz-12345", "test", "Test").await;
        assert!(!result);
    }

    /// End-to-end integration test: drives a fake `nanna` binary through
    /// `run_eval`, exercising the full SWE-bench subprocess path including
    /// materialize → subprocess → finish_swebench → capture_swebench_patch.
    ///
    /// Marked `#[ignore]` because it requires `git` on PATH and creates a
    /// network-free local bare repo — it should be run explicitly in CI via
    /// `cargo test --test nanna_subprocess_smoke -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn run_eval_swebench_subprocess_end_to_end() {
        // A minimal bash script that acts as the fake `nanna` binary.
        // The fake nanna writes a small file in the work-dir AND emits an
        // AgentRunReport. The mutation matters because the eval-runner's
        // finish_swebench path runs `git add -A && git diff <base>` against
        // work_dir; without a mutation the captured patch is empty and the
        // BroughtUp assertions become vacuous.
        let fake_script = concat!(
            r#"#!/usr/bin/env bash
set -e
work_dir=""
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --work-dir) work_dir="$2"; shift 2 ;;
    --output-json) out="$2"; shift 2 ;;
    *) shift ;;
  esac
done
echo "fake-nanna-was-here" > "$work_dir/__fake_marker"
"#,
            r#"cat > "$out" <<'JSON'
{"schema_version":1,"task_completed":true,"iterations":3,"token_usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15},"messages":[]}
JSON
"#,
        );

        // exercises run_eval body, is_swebench_case, load_swebench_task,
        // materialize, run_agent_subprocess, and finish_swebench's
        // skip-verify branch including capture_swebench_patch.

        // 1. Local bare repo.
        let seed = tempfile::tempdir().unwrap();
        let bare = tempfile::tempdir().unwrap();
        let oid = init_local_bare_repo(seed.path(), bare.path());

        // 2. SWE-bench JSONL pointing at a placeholder repo (the real URL
        // is overridden via env to point at our local bare repo).
        let dataset_dir = tempfile::tempdir().unwrap();
        let dataset_path = dataset_dir.path().join("dataset.jsonl");
        let task_json = serde_json::json!({
            "instance_id": "test-instance-001",
            "repo": "fake-owner/fake-repo",
            "base_commit": oid,
            "patch": "",
            "test_patch": "",
            "problem_statement": "Fix the stub",
            "hints_text": "",
            "version": "1.0",
            "FAIL_TO_PASS": [],
            "PASS_TO_PASS": []
        });
        std::fs::write(
            &dataset_path,
            serde_json::to_string(&task_json).unwrap(),
        )
        .unwrap();

        // 3. Write the fake nanna script and make it executable.
        let bin_dir = tempfile::tempdir().unwrap();
        let bin_path = bin_dir.path().join("nanna");
        std::fs::write(&bin_path, fake_script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin_path, perms).unwrap();
        }

        // 4. Case dir: the runner resolves the dataset via config override.
        let case_dir = tempfile::tempdir().unwrap();
        let case = make_swebench_case("test-instance-001");

        std::env::set_var("NANNA_HARNESS_BIN", &bin_path);
        std::env::set_var(
            "NANNA_SWEBENCH_TEST_REPO_URL",
            format!("file://{}", bare.path().display()),
        );

        let cfg = EvalRunnerConfig {
            swebench_dataset_path: Some(dataset_path.clone()),
            swebench_skip_verify: true,
            ..EvalRunnerConfig::default()
        };

        let result = run_eval(&case, case_dir.path(), &cfg).await.unwrap();

        std::env::remove_var("NANNA_HARNESS_BIN");
        std::env::remove_var("NANNA_SWEBENCH_TEST_REPO_URL");

        // The fake binary sets task_completed=true.
        assert!(result.agent_result.is_some());
        let outcome = result.agent_result.as_ref().unwrap();
        assert!(outcome.task_completed());
        assert_eq!(outcome.iterations(), 3);

        // A non-empty patch means the fake mutation was captured.
        let patch = result.swebench_patch.as_deref().unwrap_or("");
        assert!(
            !patch.is_empty(),
            "SWE-bench patch must be non-empty when the fake nanna mutated the repo"
        );
        assert!(
            patch.contains("__fake_marker"),
            "patch must reference the file the fake nanna created"
        );
        // The harness report JSON must NOT appear in the patch.
        assert!(
            !patch.contains("__nanna_agent_report"),
            "harness report JSON must be removed before git add -A runs"
        );
    }

    // ---------------------------------------------------------------------------
    // finish_swebench_with (unit) — fake verifier avoids Python harness
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn finish_swebench_with_resolved_sets_success() {
        let dir = tempfile::tempdir().unwrap();
        // need a git repo so capture_swebench_patch doesn't error
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-q", "-m", "init"])
            .current_dir(dir.path())
            .envs([
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("HOME", dir.path().to_str().unwrap()),
                ("GIT_AUTHOR_NAME", "T"),
                ("GIT_AUTHOR_EMAIL", "t@t.t"),
                ("GIT_COMMITTER_NAME", "T"),
                ("GIT_COMMITTER_EMAIL", "t@t.t"),
                ("GIT_COMMITTER_DATE", "2020-01-01T00:00:00"),
                ("GIT_AUTHOR_DATE", "2020-01-01T00:00:00"),
            ])
            .status()
            .unwrap();
        let oid_out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", dir.path())
            .output()
            .unwrap();
        let oid = String::from_utf8(oid_out.stdout).unwrap().trim().to_string();

        let task = crate::eval::swebench::SWEBenchTask {
            instance_id: "inst-1".to_string(),
            repo: "x/y".to_string(),
            base_commit: oid,
            patch: String::new(),
            test_patch: String::new(),
            problem_statement: String::new(),
            hints_text: String::new(),
            version: String::new(),
            fail_to_pass: vec![],
            pass_to_pass: vec![],
            environment_setup_commit: None,
        };
        let case = make_swebench_case("inst-1");
        let cfg = EvalRunnerConfig::default();
        let agent_result = Some(AgentOutcome::InProcess(AgentRunResult {
            iterations: 2,
            task_completed: true,
            token_usage: None,
        }));

        // Fake verifier: AgentRunReport::read_from_path → SubprocessNoOutput map_err.
        let fake_verifier = |_preds: Vec<Prediction>, _cfg: VerifyConfig| async {
            Ok(vec![InstanceVerdict {
                instance_id: "inst-1".to_string(),
                resolved: true,
            }])
        };

        let result = finish_swebench_with(
            &case,
            &task,
            dir.path(),
            &cfg,
            Instant::now(),
            agent_result,
            vec![],
            fake_verifier,
        )
        .await
        .unwrap();

        assert!(result.success);
        assert_eq!(result.case_id, "inst-1");
        assert!(result.swebench_patch.is_some());
        assert_eq!(result.verification.tests_passed, Some(true));
    }

    #[tokio::test]
    async fn finish_swebench_with_unresolved_records_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-q", "-m", "init"])
            .current_dir(dir.path())
            .envs([
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("HOME", dir.path().to_str().unwrap()),
                ("GIT_AUTHOR_NAME", "T"),
                ("GIT_AUTHOR_EMAIL", "t@t.t"),
                ("GIT_COMMITTER_NAME", "T"),
                ("GIT_COMMITTER_EMAIL", "t@t.t"),
                ("GIT_COMMITTER_DATE", "2020-01-01T00:00:00"),
                ("GIT_AUTHOR_DATE", "2020-01-01T00:00:00"),
            ])
            .status()
            .unwrap();
        let oid_out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", dir.path())
            .output()
            .unwrap();
        let oid = String::from_utf8(oid_out.stdout).unwrap().trim().to_string();

        let task = crate::eval::swebench::SWEBenchTask {
            instance_id: "inst-2".to_string(),
            repo: "x/y".to_string(),
            base_commit: oid,
            patch: String::new(),
            test_patch: String::new(),
            problem_statement: String::new(),
            hints_text: String::new(),
            version: String::new(),
            fail_to_pass: vec![],
            pass_to_pass: vec![],
            environment_setup_commit: None,
        };
        let case = make_swebench_case("inst-2");
        let cfg = EvalRunnerConfig::default();

        let fake_verifier = |_preds: Vec<Prediction>, _cfg: VerifyConfig| async {
            Ok(vec![InstanceVerdict {
                instance_id: "inst-2".to_string(),
                resolved: false,
            }])
        };

        let result = finish_swebench_with(
            &case,
            &task,
            dir.path(),
            &cfg,
            Instant::now(),
            None,
            vec![],
            fake_verifier,
        )
        .await
        .unwrap();

        assert!(!result.success);
        assert!(result.failures.iter().any(|f| f.contains("inst-2")));
        assert_eq!(result.verification.tests_passed, Some(false));
    }

    #[tokio::test]
    async fn finish_swebench_with_verifier_error_records_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-q", "-m", "init"])
            .current_dir(dir.path())
            .envs([
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("HOME", dir.path().to_str().unwrap()),
                ("GIT_AUTHOR_NAME", "T"),
                ("GIT_AUTHOR_EMAIL", "t@t.t"),
                ("GIT_COMMITTER_NAME", "T"),
                ("GIT_COMMITTER_EMAIL", "t@t.t"),
                ("GIT_COMMITTER_DATE", "2020-01-01T00:00:00"),
                ("GIT_AUTHOR_DATE", "2020-01-01T00:00:00"),
            ])
            .status()
            .unwrap();
        let oid_out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", dir.path())
            .output()
            .unwrap();
        let oid = String::from_utf8(oid_out.stdout).unwrap().trim().to_string();

        let task = crate::eval::swebench::SWEBenchTask {
            instance_id: "inst-3".to_string(),
            repo: "x/y".to_string(),
            base_commit: oid,
            patch: String::new(),
            test_patch: String::new(),
            problem_statement: String::new(),
            hints_text: String::new(),
            version: String::new(),
            fail_to_pass: vec![],
            pass_to_pass: vec![],
            environment_setup_commit: None,
        };
        let case = make_swebench_case("inst-3");
        let cfg = EvalRunnerConfig::default();

        let fake_verifier = |_preds: Vec<Prediction>, _cfg: VerifyConfig| async {
            Err(VerifyError::ProcessFailed("mock error".to_string()))
        };

        let result = finish_swebench_with(
            &case,
            &task,
            dir.path(),
            &cfg,
            Instant::now(),
            None,
            vec![],
            fake_verifier,
        )
        .await
        .unwrap();

        assert!(!result.success);
        assert!(result
            .failures
            .iter()
            .any(|f| f.contains("verify error")));
    }
}
