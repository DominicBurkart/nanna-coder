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
use crate::eval::claude_cli::{ClaudeCodeError, ClaudeCodeRunner};
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
    #[error("claude -p subprocess failed: {0}")]
    ClaudeCli(#[from] ClaudeCodeError),
}

/// Which agent loop drives a single eval invocation.
///
/// `AgentLoop` is the existing nanna path (used by `nanna_solo` /
/// `claude_mcp_nanna_gemma4`). `ClaudeCodeDirect` short-circuits to a
/// `claude -p` subprocess with file/bash tools, used for the `claude_solo`
/// scorecard category — the cat 1 baseline against which cat 2's
/// MCP-mediated delegation overhead is measured.
#[derive(Debug, Clone, Default)]
pub enum RunnerMode {
    /// Drive nanna's [`AgentLoop`] with the configured Ollama model.
    #[default]
    AgentLoop,
    /// Spawn `claude -p` (Claude Code in headless mode) per instance.
    /// `model` overrides the default `claude-opus-4-7`. `max_budget_usd`
    /// caps per-instance API spend (forwarded to `--max-budget-usd`).
    /// `allowed_tools` overrides the default Read/Edit/Write/Bash/Glob/Grep
    /// surface; an empty vec leaves the flag unset (Claude Code default).
    ClaudeCodeDirect {
        model: String,
        max_budget_usd: Option<f64>,
        allowed_tools: Vec<String>,
    },
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
    /// Which agent loop to drive. Defaults to [`RunnerMode::AgentLoop`]
    /// (nanna). Set to [`RunnerMode::ClaudeCodeDirect`] for the cat 1
    /// `claude_solo` scorecard track.
    pub mode: RunnerMode,
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
            mode: RunnerMode::default(),
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

    pub fn with_mode(mut self, mode: RunnerMode) -> Self {
        self.mode = mode;
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

    // Branch on `RunnerMode`. ClaudeCodeDirect short-circuits the
    // AgentLoop entirely — `claude -p` is the agent loop in that mode.
    if let RunnerMode::ClaudeCodeDirect {
        model,
        max_budget_usd,
        allowed_tools,
    } = &config.mode
    {
        return run_claude_code_direct(
            eval_case,
            work_dir,
            swebench_task.as_ref(),
            config,
            model,
            *max_budget_usd,
            allowed_tools,
            start,
        )
        .await;
    }

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
                swebench_patch: None,
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
        swebench_patch: None,
    })
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
    agent_result: Option<AgentRunResult>,
    mut failures: Vec<String>,
) -> Result<EvalRunResult, EvalRunnerError> {
    let model_patch = capture_swebench_patch(work_dir, &task.base_commit).await?;

    let task_completed = agent_result.as_ref().is_some_and(|r| r.task_completed);
    if !task_completed {
        failures.push("Agent did not complete the task".to_string());
    }
    let iterations = agent_result.as_ref().map_or(0, |r| r.iterations);
    let token_usage = agent_result
        .as_ref()
        .and_then(|r| r.token_usage.clone())
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
// ClaudeCodeDirect — `claude -p` as the agent loop
// ---------------------------------------------------------------------------

/// Translate a [`crate::eval::claude_cli::ClaudeCodeRun`]'s u64 token
/// counters into the workspace's u32 [`TokenUsage`] shape, saturating at
/// [`u32::MAX`] for the (extremely unlikely) overflow case.
fn token_usage_from_claude_run(run: &crate::eval::claude_cli::ClaudeCodeRun) -> TokenUsage {
    TokenUsage {
        prompt_tokens: u32::try_from(run.prompt_tokens).unwrap_or(u32::MAX),
        completion_tokens: u32::try_from(run.completion_tokens).unwrap_or(u32::MAX),
        total_tokens: u32::try_from(run.total_tokens).unwrap_or(u32::MAX),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_claude_code_direct(
    eval_case: &EvalCase,
    work_dir: &Path,
    swebench_task: Option<&SWEBenchTask>,
    config: &EvalRunnerConfig,
    model: &str,
    max_budget_usd: Option<f64>,
    allowed_tools: &[String],
    start: Instant,
) -> Result<EvalRunResult, EvalRunnerError> {
    let mut runner = ClaudeCodeRunner::new().with_model(model);
    if let Some(b) = max_budget_usd {
        runner = runner.with_max_budget_usd(b);
    }
    if !allowed_tools.is_empty() {
        runner = runner.with_allowed_tools(allowed_tools.iter().cloned());
    }
    if let Ok(bin) = std::env::var("NANNA_CLAUDE_BIN") {
        runner = runner.with_claude_bin(bin);
    }

    let task_prompt = &eval_case.task.prompt;
    let timeout = Duration::from_secs(eval_case.metadata.timeout_secs);
    let claude_run = match tokio::time::timeout(timeout, runner.run(task_prompt, work_dir)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(EvalRunnerError::ClaudeCli(e)),
        Err(_) => return Err(EvalRunnerError::Timeout(timeout)),
    };

    let token_usage = token_usage_from_claude_run(&claude_run);
    let mut failures: Vec<String> = Vec::new();
    if claude_run.is_error {
        failures.push(format!(
            "claude -p reported is_error=true: {}",
            claude_run.result.as_deref().unwrap_or("(no result body)")
        ));
    }

    if let Some(task) = swebench_task {
        // SWE-bench branch: capture diff + (optionally) run upstream verifier.
        let model_patch = capture_swebench_patch(work_dir, &task.base_commit).await?;

        let resolved = if config.swebench_skip_verify {
            failures.push("verifier skipped — score-mode batched run".to_string());
            false
        } else {
            let verify_dir = work_dir.join("__nanna_verify");
            std::fs::create_dir_all(&verify_dir)?;
            let verify_config = VerifyConfig {
                dataset_name: config.swebench_hf_dataset.clone(),
                model_name_or_path: format!("claude_solo__{}", model.replace([':', '/'], "-")),
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

        return Ok(EvalRunResult {
            case_id: eval_case.case.id.clone(),
            success: resolved && !claude_run.is_error,
            execution_time: start.elapsed(),
            iterations: 1,
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
            agent_result: None,
            swebench_patch: Some(model_patch),
        });
    }

    // Non-SWE-bench branch: run the in-tree build/test/file/symbol checks
    // against whatever the subprocess wrote into work_dir.
    let verification =
        run_verification(work_dir, &eval_case.expected, &eval_case.task.language).await;
    if !verification.all_passed() {
        failures.extend(verification_failures(&verification));
    }
    let success = failures.is_empty();
    Ok(EvalRunResult {
        case_id: eval_case.case.id.clone(),
        success,
        execution_time: start.elapsed(),
        iterations: 1,
        token_usage,
        verification,
        failures,
        agent_result: None,
        swebench_patch: None,
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

    // ----------------- ClaudeCodeDirect runner mode -----------------

    #[test]
    fn runner_mode_default_is_agent_loop() {
        let m = RunnerMode::default();
        assert!(matches!(m, RunnerMode::AgentLoop));
    }

    #[test]
    fn config_with_mode_sets_claude_code_direct() {
        let cfg = EvalRunnerConfig::default().with_mode(RunnerMode::ClaudeCodeDirect {
            model: "claude-haiku-4-5".to_string(),
            max_budget_usd: Some(0.25),
            allowed_tools: vec!["Read".to_string(), "Edit".to_string()],
        });
        match cfg.mode {
            RunnerMode::ClaudeCodeDirect {
                model,
                max_budget_usd,
                allowed_tools,
            } => {
                assert_eq!(model, "claude-haiku-4-5");
                assert_eq!(max_budget_usd, Some(0.25));
                assert_eq!(allowed_tools, vec!["Read", "Edit"]);
            }
            other => panic!("expected ClaudeCodeDirect, got {other:?}"),
        }
    }

    #[test]
    fn token_usage_from_claude_run_saturates_at_u32_max() {
        use crate::eval::claude_cli::ClaudeCodeRun;
        let r = ClaudeCodeRun {
            result: None,
            session_id: None,
            prompt_tokens: u64::MAX,
            completion_tokens: u64::MAX,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            total_tokens: u64::MAX,
            total_cost_usd: 0.0,
            is_error: false,
            stderr: String::new(),
            exit_status: 0,
        };
        let u = token_usage_from_claude_run(&r);
        assert_eq!(u.prompt_tokens, u32::MAX);
        assert_eq!(u.completion_tokens, u32::MAX);
        assert_eq!(u.total_tokens, u32::MAX);
    }

    #[test]
    fn token_usage_from_claude_run_normal_values() {
        use crate::eval::claude_cli::ClaudeCodeRun;
        let r = ClaudeCodeRun {
            result: None,
            session_id: None,
            prompt_tokens: 100,
            completion_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            total_tokens: 150,
            total_cost_usd: 0.0,
            is_error: false,
            stderr: String::new(),
            exit_status: 0,
        };
        let u = token_usage_from_claude_run(&r);
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(u.total_tokens, 150);
    }

    /// Build a stub `claude` binary that:
    /// - parses `--add-dir <DIR>` from its argv;
    /// - writes `<DIR>/<file_to_create>` with `<file_contents>`;
    /// - prints a fixed JSON-success blob and exits 0.
    #[cfg(unix)]
    fn write_stub_claude_creating(
        bin_dir: &Path,
        file_to_create: &str,
        file_contents: &str,
    ) -> PathBuf {
        let path = bin_dir.join("claude");
        let escaped_contents = file_contents.replace('\'', "'\\''");
        let escaped_file = file_to_create.replace('\'', "'\\''");
        let json = r#"{"result":"ok","session_id":"sid","usage":{"input_tokens":10,"output_tokens":5},"total_cost_usd":0.0,"is_error":false}"#;
        let escaped_json = json.replace('\'', "'\\''");
        let script = format!(
            "#!/usr/bin/env bash\nset -e\nADD_DIR=\"\"\nwhile [[ $# -gt 0 ]]; do\n  case \"$1\" in\n    --add-dir) shift; ADD_DIR=\"$1\";;\n  esac\n  shift || true\ndone\nmkdir -p \"$(dirname \"$ADD_DIR/{}\")\"\nprintf '%s' '{}' > \"$ADD_DIR/{}\"\nprintf '%s' '{}'\n",
            escaped_file, escaped_contents, escaped_file, escaped_json
        );
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial_test::serial]
    async fn run_eval_in_claude_code_direct_mode_picks_up_subprocess_changes() {
        // Set up a non-swebench eval case whose expected outcome is
        // creation of `extra.rs` containing the symbol `extra`. The
        // stub `claude` shim writes that file when invoked, so the
        // verification block (file presence + symbol search) passes.
        let case_root = tempfile::tempdir().unwrap();
        let repo_src = case_root.path().join("repo");
        std::fs::create_dir_all(repo_src.join("src")).unwrap();
        std::fs::write(repo_src.join("src/lib.rs"), "// stub crate\n").unwrap();
        std::fs::write(
            repo_src.join("Cargo.toml"),
            "[package]\nname = \"stub\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[lib]\npath = \"src/lib.rs\"\n",
        )
        .unwrap();

        let toml_str = r#"
[case]
id = "claude-cli-smoke"
name = "ClaudeCodeDirect smoke"
description = "Verify claude -p subprocess wiring"

[task]
prompt = "create extra.rs"
language = "rust"

[expected]
files_changed = ["src/extra.rs"]
required_symbols = ["extra"]

[metadata]
timeout_secs = 30
"#;
        let case = EvalCase::from_toml_str(toml_str).unwrap();

        let bin_dir = tempfile::tempdir().unwrap();
        let stub = write_stub_claude_creating(
            bin_dir.path(),
            "src/extra.rs",
            "pub fn extra() -> &'static str { \"hi\" }\n",
        );
        // Hand the stub to the runner via NANNA_CLAUDE_BIN.
        std::env::set_var("NANNA_CLAUDE_BIN", &stub);

        let config = EvalRunnerConfig::default().with_mode(RunnerMode::ClaudeCodeDirect {
            model: "claude-haiku-4-5".to_string(),
            max_budget_usd: Some(0.05),
            allowed_tools: vec![],
        });

        let result = run_eval(&case, case_root.path(), &config).await.unwrap();
        std::env::remove_var("NANNA_CLAUDE_BIN");

        assert_eq!(result.case_id, "claude-cli-smoke");
        assert_eq!(result.iterations, 1, "claude -p counts as one iteration");
        assert_eq!(result.token_usage.prompt_tokens, 10);
        assert_eq!(result.token_usage.completion_tokens, 5);
        assert!(result.agent_result.is_none(), "no AgentLoop in this mode");
        assert!(
            result
                .verification
                .files_found
                .contains(&"src/extra.rs".to_string()),
            "verification should find the file the stub created; got {:?}",
            result.verification.files_found
        );
        assert!(
            result
                .verification
                .symbols_found
                .contains(&"extra".to_string()),
            "verification should find the `extra` symbol; got {:?}",
            result.verification.symbols_found
        );
    }
}
