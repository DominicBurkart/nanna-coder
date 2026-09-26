//! `harness-sdlc-eval` — run the end-to-end SDLC orchestrator scenario
//! (issue #657, extending #367) and append a scored row to
//! `evals/scorecards/sdlc_e2e.jsonl`.
//!
//! Unlike `harness-score`/`harness-eval`, this scenario is fully scripted
//! (a [`ScriptedModelProvider`](harness::eval::sdlc) planner and action
//! auditor, [`FakeGithub`](harness::eval::sdlc), a fake rollout adapter) so
//! it needs no model server, no GitHub token, and no cloud credentials —
//! see `harness::eval::sdlc`'s module docs for what it actually exercises.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use harness::eval::sdlc::{
    append_scorecard_row, run_e2e_scenario, SdlcE2eScorecardRow, SDLC_E2E_SCORECARD_SCHEMA_VERSION,
};

#[derive(Parser, Debug)]
#[command(
    name = "harness-sdlc-eval",
    about = "Run the end-to-end SDLC orchestrator eval and append a scorecard row"
)]
struct Args {
    /// Where to append the scored row.
    #[arg(long, default_value = "evals/scorecards/sdlc_e2e.jsonl")]
    scorecard_path: PathBuf,

    /// PR number to attribute this run to, when known.
    #[arg(long)]
    pr: Option<u64>,
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

    let outcome = match run_e2e_scenario().await {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("::error::sdlc e2e eval failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("=== SDLC E2E Eval ===");
    println!("task_success:       {}", outcome.task_success);
    println!(
        "auditor:            total={} false_allow_rate={:.3} false_block_rate={:.3}",
        outcome.auditor.total, outcome.auditor.false_allow_rate, outcome.auditor.false_block_rate
    );
    println!("effect_counts:      {:?}", outcome.effect_counts);
    println!(
        "wall_clock:         inner={:.3}s middle={:.3}s outer={:.3}s",
        outcome.wall_clock.inner_secs,
        outcome.wall_clock.middle_secs,
        outcome.wall_clock.outer_secs
    );

    let commit = git_capture(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let branch = git_capture(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let task_success = outcome.task_success;
    let row = SdlcE2eScorecardRow {
        schema_version: SDLC_E2E_SCORECARD_SCHEMA_VERSION,
        date: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        commit,
        branch,
        pr: args.pr,
        scenario: "fullstack-fixture-implementer-shepherd-deployer".to_string(),
        outcome,
    };

    if let Err(e) = append_scorecard_row(&args.scorecard_path, &row) {
        eprintln!("::error::failed to append scorecard row: {e}");
        return ExitCode::FAILURE;
    }
    println!("appended to {}", args.scorecard_path.display());

    if task_success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
