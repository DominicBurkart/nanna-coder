//! `harness-score` — drive nanna across a SWE-bench dataset, persist
//! per-instance state across sittings, and emit a leaderboard-comparable
//! score line into `evals/scorecards/index.jsonl`.
//!
//! Most of the testable logic lives in [`harness::eval::scoring`]; this
//! binary is the async glue that calls `run_eval` (per instance) and
//! `verify_predictions` (once in `--finalize`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;
use std::time::Duration;

use clap::Parser;
use harness::eval::runner::{
    run_eval, EvalRunResult, EvalRunnerConfig, EvalRunnerError, RunnerMode,
};
use harness::eval::scoring::{
    aggregate_scorecard, append_line, instance_state_from_outcome, load_all_states,
    outcome_from_run_eval, predictions_dir, read_state, resolve_model, sanitize_model_for_path,
    should_attempt, state_path, states_dir, write_state, InstanceState, InstanceStatus, Scorecard,
    ScorecardCategory, ScorecardMetadata, StoredVerdict,
};
use harness::eval::swebench::{adapt_to_eval_case, load_swebench_dataset, SWEBenchTask};
use harness::eval::swebench_verify::{verify_predictions, Prediction, VerifyConfig};

fn parse_category(s: &str) -> Result<ScorecardCategory, String> {
    ScorecardCategory::from_str(s)
}

/// Allowed values for the `--mode` flag. Drives which runner path
/// `run_eval` takes per instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum CliMode {
    /// Default. Drive nanna's `AgentLoop` (with the configured Ollama
    /// model) — the `nanna_solo` / `claude_mcp_nanna_gemma4` worker path.
    #[clap(name = "swebench")]
    Swebench,
    /// Spawn `claude -p` per instance — the `claude_solo` (cat 1) path.
    #[clap(name = "claude-code-direct")]
    ClaudeCodeDirect,
}

#[derive(Parser, Debug)]
#[command(
    name = "harness-score",
    about = "Run nanna across a SWE-bench dataset, persist state, emit a scorecard line"
)]
struct Args {
    #[arg(long)]
    dataset: PathBuf,

    #[arg(long)]
    state_dir: PathBuf,

    /// Eval-track discriminator. Required so the scorecard row written to
    /// `evals/scorecards/index.jsonl` is unambiguously attributable to one
    /// of the four categories (nanna_solo, claude_solo, claude_mcp_claude,
    /// claude_mcp_nanna_gemma4).
    #[arg(long, value_parser = parse_category)]
    category: ScorecardCategory,

    /// Foundational model that drives the agent loop / orchestrator.
    /// Takes precedence over the deprecated `--model` flag and over the
    /// `NANNA_EVAL_MODEL` / `MODEL` env vars.
    #[arg(long)]
    orchestrator_model: Option<String>,

    /// Worker-side model when running an MCP-delegated category. Recorded
    /// on the scorecard row but not yet wired through to a worker provider
    /// (that lands in the runner-mode PR).
    #[arg(long)]
    worker_model: Option<String>,

    /// Deprecated alias for `--orchestrator-model`. Retained so resumed
    /// runs from before schema v2 do not break.
    #[arg(long)]
    model: Option<String>,

    /// Which runner path drives each eval instance.
    ///
    /// * `swebench` (default) — drive nanna's `AgentLoop` with the
    ///   configured Ollama model. Used for `nanna_solo` and the
    ///   `claude_mcp_nanna_gemma4` worker.
    /// * `claude-code-direct` — spawn `claude -p` per instance. Used for
    ///   the `claude_solo` (cat 1) scorecard track.
    #[arg(long, value_enum, default_value_t = CliMode::Swebench)]
    mode: CliMode,

    /// Per-instance USD spending cap, forwarded to `claude -p
    /// --max-budget-usd`. Only honoured when `--mode claude-code-direct`.
    /// `None` means no cap.
    #[arg(long)]
    max_budget_usd: Option<f64>,

    /// Comma-separated `--allowed-tools` for `claude -p`. Empty leaves
    /// the wrapper's default set (Read, Edit, Write, Bash, Glob, Grep).
    /// Only honoured when `--mode claude-code-direct`.
    #[arg(long, value_delimiter = ',')]
    allowed_tools: Vec<String>,

    #[arg(long)]
    max_instances: Option<usize>,

    #[arg(long, default_value_t = 4)]
    max_workers: usize,

    #[arg(long)]
    timeout_secs: Option<u64>,

    #[arg(long, default_value_t = 100)]
    max_iterations: usize,

    #[arg(long, default_value = "princeton-nlp/SWE-bench_Lite")]
    dataset_name: String,

    #[arg(long)]
    run_id: Option<String>,

    #[arg(long)]
    finalize: bool,
}

fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

fn run_id_default() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("nanna-{secs}")
}

fn git_capture(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    // `--orchestrator-model` is canonical; `--model` is the back-compat
    // alias, used only when the new flag is absent.
    let cli_model = args.orchestrator_model.as_deref().or(args.model.as_deref());
    let model = resolve_model(cli_model, "qwen3:0.6b");
    let run_id = args.run_id.clone().unwrap_or_else(run_id_default);

    if let Err(e) = std::fs::create_dir_all(predictions_dir(&args.state_dir)) {
        eprintln!("::error::failed to create predictions dir: {e}");
        return ExitCode::from(2);
    }
    if let Err(e) = std::fs::create_dir_all(states_dir(&args.state_dir)) {
        eprintln!("::error::failed to create state dir: {e}");
        return ExitCode::from(2);
    }

    if args.finalize {
        return finalize(&args, &model, &run_id).await;
    }
    score_loop(&args, &model, &run_id).await
}

async fn score_loop(args: &Args, model: &str, run_id: &str) -> ExitCode {
    let tasks = match load_swebench_dataset(&args.dataset) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "::error::failed to load dataset {}: {e}",
                args.dataset.display()
            );
            return ExitCode::from(2);
        }
    };
    if tasks.is_empty() {
        eprintln!("::error::dataset {} is empty", args.dataset.display());
        return ExitCode::from(2);
    }
    println!(
        "loaded {} instances from {} (model={model}, run-id={run_id})",
        tasks.len(),
        args.dataset.display()
    );

    let mut attempted = 0usize;
    for (i, task) in tasks.iter().enumerate() {
        let prior = read_state(&state_path(&args.state_dir, &task.instance_id));
        if !should_attempt(prior.as_ref()) {
            continue;
        }
        if let Some(cap) = args.max_instances {
            if attempted >= cap {
                println!(
                    "reached --max-instances {cap}; stopping at {i}/{}",
                    tasks.len()
                );
                break;
            }
        }
        attempted += 1;
        let label = format!("[{}/{}] {}", attempted, tasks.len(), task.instance_id);
        let started = std::time::Instant::now();
        let outcome = outcome_from_run_eval(attempt_instance(args, model, run_id, task).await);
        let wall = started.elapsed().as_secs();
        let state =
            match instance_state_from_outcome(&args.state_dir, &task.instance_id, outcome, wall) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "::error::failed to materialise state for {}: {e}",
                        task.instance_id
                    );
                    return ExitCode::from(2);
                }
            };
        let status = state.status;
        if let Err(e) = write_state(&args.state_dir, &state) {
            eprintln!(
                "::error::failed to write state for {}: {e}",
                task.instance_id
            );
            return ExitCode::from(2);
        }
        println!("{label} → {:?} ({wall}s)", status);
    }

    println!("score-loop done: {attempted} instance(s) attempted in this invocation");
    ExitCode::SUCCESS
}

async fn attempt_instance(
    args: &Args,
    model: &str,
    run_id: &str,
    task: &SWEBenchTask,
) -> Result<EvalRunResult, EvalRunnerError> {
    let case = adapt_to_eval_case(task);
    let mut config = EvalRunnerConfig::default()
        .with_model(model)
        .with_max_iterations(args.max_iterations)
        .with_swebench_skip_verify(true);
    config.swebench_dataset_path = Some(args.dataset.clone());
    config.swebench_hf_dataset = args.dataset_name.clone();
    config.swebench_run_id = run_id.to_string();
    if matches!(args.mode, CliMode::ClaudeCodeDirect) {
        config = config.with_mode(RunnerMode::ClaudeCodeDirect {
            model: model.to_string(),
            max_budget_usd: args.max_budget_usd,
            allowed_tools: args.allowed_tools.clone(),
        });
    }

    let case_dir = args
        .dataset
        .parent()
        .map(|p| p.join("__synthetic"))
        .unwrap_or_else(|| std::path::PathBuf::from("__synthetic"));

    let timeout = Duration::from_secs(args.timeout_secs.unwrap_or(case.metadata.timeout_secs));
    match tokio::time::timeout(timeout, run_eval(&case, &case_dir, &config)).await {
        Ok(inner) => inner,
        Err(_) => Err(EvalRunnerError::Timeout(timeout)),
    }
}

async fn finalize(args: &Args, model: &str, run_id: &str) -> ExitCode {
    let states = match load_all_states(&args.state_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("::error::failed to read state dir: {e}");
            return ExitCode::from(2);
        }
    };
    if states.is_empty() {
        eprintln!("::error::no state files under {}", args.state_dir.display());
        return ExitCode::from(2);
    }

    let pending: Vec<&InstanceState> = states
        .iter()
        .filter(|s| s.status == InstanceStatus::CompletedDiff)
        .collect();
    println!(
        "finalize: {} state file(s); {} awaiting verify",
        states.len(),
        pending.len()
    );

    let updated_states = if pending.is_empty() {
        states.clone()
    } else {
        let mut predictions: Vec<Prediction> = Vec::with_capacity(pending.len());
        for s in &pending {
            let dpath = match &s.diff_path {
                Some(p) => p,
                None => continue,
            };
            let model_patch = match std::fs::read_to_string(dpath) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("::warning::failed to read diff for {}: {e}", s.instance_id);
                    continue;
                }
            };
            predictions.push(Prediction {
                instance_id: s.instance_id.clone(),
                model_patch,
            });
        }

        let verify_dir = args.state_dir.join("verify");
        if let Err(e) = std::fs::create_dir_all(&verify_dir) {
            eprintln!("::error::failed to create verify dir: {e}");
            return ExitCode::from(2);
        }
        let verify_config = VerifyConfig {
            dataset_name: args.dataset_name.clone(),
            model_name_or_path: format!(
                "{}__{}",
                args.category.as_str(),
                sanitize_model_for_path(model)
            ),
            run_id: run_id.to_string(),
            work_dir: verify_dir,
            max_workers: args.max_workers,
        };
        let verdicts = match verify_predictions(&predictions, &verify_config).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("::error::verify_predictions failed: {e}");
                return ExitCode::from(2);
            }
        };

        let mut verdict_by_id = std::collections::HashMap::new();
        for v in &verdicts {
            verdict_by_id.insert(v.instance_id.clone(), StoredVerdict::from(v));
        }

        let mut updated = Vec::with_capacity(states.len());
        for s in states.into_iter() {
            let next = if s.status == InstanceStatus::CompletedDiff {
                let v = verdict_by_id
                    .remove(&s.instance_id)
                    .unwrap_or(StoredVerdict {
                        resolved: false,
                        error: Some("no upstream report".to_string()),
                    });
                s.with_verdict(v)
            } else {
                s
            };
            if let Err(e) = write_state(&args.state_dir, &next) {
                eprintln!(
                    "::error::failed to update state for {}: {e}",
                    next.instance_id
                );
                return ExitCode::from(2);
            }
            updated.push(next);
        }
        updated
    };

    let instances_total = match load_swebench_dataset(&args.dataset) {
        Ok(t) => t.len(),
        Err(e) => {
            eprintln!(
                "::error::failed to count instances in {}: {e}",
                args.dataset.display()
            );
            return ExitCode::from(2);
        }
    };

    let commit = git_capture(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let branch = git_capture(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let date = iso_now();
    let state_dir_label = args.state_dir.display().to_string();
    let metadata = ScorecardMetadata {
        date: &date,
        commit: &commit,
        branch: branch.as_deref(),
        pr: None,
        category: args.category,
        orchestrator_model: model,
        worker_model: args.worker_model.as_deref(),
        dataset: &args.dataset_name,
        state_dir: &state_dir_label,
    };
    let card = aggregate_scorecard(&updated_states, instances_total, metadata);
    let line = match serde_json::to_string(&card) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("::error::failed to serialize scorecard: {e}");
            return ExitCode::from(2);
        }
    };

    let index_path = std::path::Path::new("evals/scorecards/index.jsonl");
    if let Err(e) = append_line(index_path, &line) {
        eprintln!("::error::failed to append scorecard: {e}");
        return ExitCode::from(2);
    }

    print_summary(&card);
    ExitCode::SUCCESS
}

fn print_summary(card: &Scorecard) {
    println!();
    println!("=== Scorecard ===");
    println!("category:           {}", card.category);
    println!("dataset:            {}", card.dataset);
    println!("orchestrator_model: {}", card.orchestrator_model);
    if let Some(w) = card.worker_model.as_deref() {
        println!("worker_model:       {w}");
    }
    println!(
        "instances:          attempted={}, resolved={}, total={}",
        card.instances_attempted, card.instances_resolved, card.instances_total
    );
    println!("score_pct:          {:.2}", card.score_pct);
    println!(
        "is_complete:        {} (only true rows are leaderboard-comparable)",
        card.is_complete
    );
    println!("commit:             {}", card.commit);
    println!("appended to evals/scorecards/index.jsonl");
}
