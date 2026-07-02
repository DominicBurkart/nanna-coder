use clap::Parser;
use harness::agent::eval_case::EvalCase;
use harness::eval::runner::{run_eval, EvalRunResult, EvalRunnerConfig, EvalRunnerError};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "harness-eval",
    about = "Run nanna-coder eval cases via harness::eval::runner::run_eval"
)]
struct Args {
    #[arg(long, default_value = "evals/cases")]
    cases_dir: PathBuf,

    #[arg(long)]
    filter: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long)]
    base_url: Option<String>,

    #[arg(long, default_value_t = 100)]
    max_iterations: usize,

    #[arg(long)]
    timeout_secs: Option<u64>,

    #[arg(long)]
    verbose: bool,
}

fn resolve_model(cli: Option<String>) -> String {
    cli.or_else(|| std::env::var("NANNA_EVAL_MODEL").ok())
        .or_else(|| std::env::var("MODEL").ok())
        .unwrap_or_else(|| "qwen3:0.6b".to_string())
}

fn case_matches(case_id: &str, filter: &Option<String>) -> bool {
    match filter {
        Some(needle) if !needle.is_empty() => case_id.contains(needle.as_str()),
        _ => true,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let model = resolve_model(args.model);
    let cases = match EvalCase::discover(&args.cases_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "::error::failed to discover eval cases under {}: {e}",
                args.cases_dir.display()
            );
            return ExitCode::from(2);
        }
    };
    let selected: Vec<_> = cases
        .into_iter()
        .filter(|(c, _)| case_matches(&c.case.id, &args.filter))
        .collect();

    if selected.is_empty() {
        eprintln!(
            "::error::no eval cases matched filter {:?} under {}",
            args.filter,
            args.cases_dir.display()
        );
        return ExitCode::from(2);
    }

    println!(
        "Running {} eval case(s) with model `{model}`",
        selected.len()
    );
    let mut results: Vec<(String, Result<EvalRunResult, EvalRunnerError>)> = Vec::new();

    for (case, case_dir) in &selected {
        let config = EvalRunnerConfig::default()
            .with_model(&model)
            .with_max_iterations(args.max_iterations)
            .with_verbose(args.verbose);
        let config = match &args.base_url {
            Some(url) => config.with_base_url(url),
            None => config,
        };

        let timeout = Duration::from_secs(args.timeout_secs.unwrap_or(case.metadata.timeout_secs));

        println!(
            "── case {} (timeout {}s) ──",
            case.case.id,
            timeout.as_secs()
        );

        let res = match tokio::time::timeout(timeout, run_eval(case, case_dir, &config)).await {
            Ok(inner) => inner,
            Err(_) => Err(EvalRunnerError::Timeout(timeout)),
        };

        match &res {
            Ok(r) => println!(
                "  → {} in {:.2}s ({} iters, {} prompt + {} completion tokens)",
                if r.success { "PASS" } else { "FAIL" },
                r.execution_time.as_secs_f64(),
                r.iterations,
                r.token_usage.prompt_tokens,
                r.token_usage.completion_tokens,
            ),
            Err(e) => println!("  → ERROR: {e}"),
        }
        results.push((case.case.id.clone(), res));
    }

    let total = results.len();
    let passed = results
        .iter()
        .filter(|(_, r)| matches!(r, Ok(rr) if rr.success))
        .count();
    let failed: Vec<&(String, Result<EvalRunResult, EvalRunnerError>)> = results
        .iter()
        .filter(|(_, r)| !matches!(r, Ok(rr) if rr.success))
        .collect();

    println!();
    println!("=== Summary ===");
    println!("Model: {model}");
    println!("Total: {total}, Passed: {passed}, Failed: {}", failed.len());
    if !failed.is_empty() {
        println!("Failures:");
        for (id, res) in &failed {
            match res {
                Ok(r) => {
                    let why = if r.failures.is_empty() {
                        "verification failed".to_string()
                    } else {
                        r.failures.join("; ")
                    };
                    println!("  - {id}: {why}");
                }
                Err(e) => println!("  - {id}: {e}"),
            }
        }
    }

    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_matches_no_filter_always_true() {
        assert!(case_matches("any-case-id", &None));
    }

    #[test]
    fn case_matches_empty_filter_always_true() {
        assert!(case_matches("any-case-id", &Some(String::new())));
    }

    #[test]
    fn case_matches_needle_found() {
        assert!(case_matches(
            "repo__fix-login-bug",
            &Some("login".to_string())
        ));
    }

    #[test]
    fn case_matches_needle_not_found() {
        assert!(!case_matches(
            "repo__fix-login-bug",
            &Some("perf".to_string())
        ));
    }

    #[test]
    fn resolve_model_cli_arg_wins() {
        assert_eq!(
            resolve_model(Some("my-custom-model".to_string())),
            "my-custom-model"
        );
    }

    #[test]
    fn resolve_model_none_returns_nonempty() {
        let model = resolve_model(None);
        assert!(!model.is_empty());
    }
}
