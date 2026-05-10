//! State machine and aggregation for the SWE-bench Lite scoring loop.
//!
//! The `harness-score` binary uses these helpers to persist per-instance
//! state across runs (so a 300-instance run can be done across multiple
//! sittings) and to aggregate verdicts into a single JSON line appended to
//! `evals/scorecards/index.jsonl`.
//!
//! Pure logic only: filesystem I/O is delegated to the binary so the
//! state-machine transitions can be unit-tested against in-memory values.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::eval::swebench_verify::InstanceVerdict;

/// Schema version for [`InstanceState`] state files. Bump on any breaking
/// change to the on-disk per-instance JSON.
pub const INSTANCE_STATE_SCHEMA_VERSION: u32 = 1;

/// Schema version for [`Scorecard`] rows in `evals/scorecards/index.jsonl`.
///
/// v2 (2026-05) introduced [`ScorecardCategory`], `orchestrator_model`, and
/// `worker_model` so that nanna-solo, claude-solo, and the two MCP-delegated
/// claude variants can coexist as distinct rows in the same metric file.
pub const SCORECARD_SCHEMA_VERSION: u32 = 2;

/// Discriminator that names which evaluation track a [`Scorecard`] row
/// belongs to.
///
/// Each variant corresponds to one of the four categories the repo records
/// SWE-bench Lite scores against:
///
/// - `nanna_solo`: nanna's agent loop driving a local Ollama model.
/// - `claude_solo`: Claude as the agent loop, no delegation.
/// - `claude_mcp_claude`: Claude orchestrator delegating subtasks to a
///   Claude worker via nanna's MCP interface.
/// - `claude_mcp_nanna_gemma4`: Claude orchestrator delegating subtasks to
///   a gemma4-backed nanna worker via the same MCP interface.
///
/// Serialised as the snake_case form of the variant. Unknown strings are
/// rejected at parse time — a typo in `--category` is a hard error rather
/// than a silently-accepted new category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScorecardCategory {
    NannaSolo,
    ClaudeSolo,
    ClaudeMcpClaude,
    ClaudeMcpNannaGemma4,
}

impl ScorecardCategory {
    /// All variants in declaration order.
    pub const ALL: [ScorecardCategory; 4] = [
        ScorecardCategory::NannaSolo,
        ScorecardCategory::ClaudeSolo,
        ScorecardCategory::ClaudeMcpClaude,
        ScorecardCategory::ClaudeMcpNannaGemma4,
    ];

    /// Stable snake_case label that matches the JSON serialisation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NannaSolo => "nanna_solo",
            Self::ClaudeSolo => "claude_solo",
            Self::ClaudeMcpClaude => "claude_mcp_claude",
            Self::ClaudeMcpNannaGemma4 => "claude_mcp_nanna_gemma4",
        }
    }
}

impl fmt::Display for ScorecardCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ScorecardCategory {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nanna_solo" => Ok(Self::NannaSolo),
            "claude_solo" => Ok(Self::ClaudeSolo),
            "claude_mcp_claude" => Ok(Self::ClaudeMcpClaude),
            "claude_mcp_nanna_gemma4" => Ok(Self::ClaudeMcpNannaGemma4),
            other => Err(format!(
                "unknown scorecard category {other:?}; expected one of {}",
                Self::ALL
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// Status field of a single instance's state file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    /// Materialize succeeded, agent ran, diff captured. Awaiting batch verify.
    CompletedDiff,
    /// Agent error or timeout. Diff is `None`.
    AgentError,
    /// Materialize failed (clone or `git apply`). Diff is `None`.
    MaterializeError,
    /// Diff was passed to the upstream harness and a verdict was returned.
    Verified,
}

impl InstanceStatus {
    /// `true` when re-running the bin with the same state-dir should skip
    /// this instance. `Verified` and `MaterializeError` are terminal in
    /// both modes; `CompletedDiff` is terminal in the default mode and
    /// gets re-read in `--finalize`; `AgentError` means we should not
    /// retry until the user clears the state file.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::CompletedDiff | Self::Verified | Self::MaterializeError | Self::AgentError
        )
    }
}

/// Per-instance metrics captured from the agent loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentMetrics {
    pub iterations: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub wall_secs: u64,
}

/// Verdict shape persisted alongside per-instance state. Mirrors
/// [`InstanceVerdict`] but is owned and serialisable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredVerdict {
    pub resolved: bool,
    pub error: Option<String>,
}

impl From<&InstanceVerdict> for StoredVerdict {
    fn from(v: &InstanceVerdict) -> Self {
        Self {
            resolved: v.resolved,
            error: v.error.clone(),
        }
    }
}

/// One per-instance state file, written to
/// `<state-dir>/state/<instance_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceState {
    pub schema_version: u32,
    pub instance_id: String,
    pub status: InstanceStatus,
    pub diff_path: Option<String>,
    pub agent_metrics: AgentMetrics,
    pub verdict: Option<StoredVerdict>,
    /// Free-form error description for `agent_error` / `materialize_error`.
    pub error: Option<String>,
}

impl InstanceState {
    pub fn completed_diff(
        instance_id: impl Into<String>,
        diff_path: impl Into<String>,
        agent_metrics: AgentMetrics,
    ) -> Self {
        Self {
            schema_version: INSTANCE_STATE_SCHEMA_VERSION,
            instance_id: instance_id.into(),
            status: InstanceStatus::CompletedDiff,
            diff_path: Some(diff_path.into()),
            agent_metrics,
            verdict: None,
            error: None,
        }
    }

    pub fn agent_error(
        instance_id: impl Into<String>,
        agent_metrics: AgentMetrics,
        error: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: INSTANCE_STATE_SCHEMA_VERSION,
            instance_id: instance_id.into(),
            status: InstanceStatus::AgentError,
            diff_path: None,
            agent_metrics,
            verdict: None,
            error: Some(error.into()),
        }
    }

    pub fn materialize_error(instance_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            schema_version: INSTANCE_STATE_SCHEMA_VERSION,
            instance_id: instance_id.into(),
            status: InstanceStatus::MaterializeError,
            diff_path: None,
            agent_metrics: AgentMetrics::default(),
            verdict: None,
            error: Some(error.into()),
        }
    }

    pub fn with_verdict(mut self, verdict: StoredVerdict) -> Self {
        self.status = InstanceStatus::Verified;
        self.verdict = Some(verdict);
        self
    }
}

/// One row of `evals/scorecards/index.jsonl`. The git history of that
/// file is the time-series metric.
///
/// Schema v2 (2026-05) added `category`, `orchestrator_model`, and
/// `worker_model` so that the four eval tracks (nanna-solo, claude-solo,
/// and the two MCP-delegated claude variants) can coexist in the same
/// JSONL without colliding on `(model, dataset, date)`.
///
/// `model` is retained as a duplicate of `orchestrator_model` so external
/// consumers reading older code paths still see the field they expect; new
/// code should read `orchestrator_model`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    pub schema_version: u32,
    /// UTC ISO 8601, second precision.
    pub date: String,
    pub commit: String,
    pub branch: Option<String>,
    pub pr: Option<u64>,
    /// Eval-track discriminator. Required since schema v2.
    pub category: ScorecardCategory,
    /// Foundational model driving the agent loop / orchestrator.
    /// Required since schema v2.
    pub orchestrator_model: String,
    /// Worker-side model when an MCP-delegated track is being scored.
    /// `None` for solo tracks.
    pub worker_model: Option<String>,
    /// Duplicate of `orchestrator_model`, retained for compatibility with
    /// readers built against schema v1.
    pub model: String,
    pub dataset: String,
    pub instances_total: usize,
    pub instances_attempted: usize,
    pub instances_resolved: usize,
    pub score_pct: f64,
    pub is_complete: bool,
    pub state_dir: String,
}

/// Aggregate per-instance state files into a single [`Scorecard`].
///
/// `instances_total` is the dataset's full instance count (e.g. 300 for
/// SWE-bench Lite); `instances_attempted` is the number of state files
/// supplied. `score_pct` is `100 * resolved / instances_total`, *not*
/// `100 * resolved / attempted` — leaderboard parity requires the full
/// denominator regardless of how many we got around to running.
pub fn aggregate_scorecard(
    states: &[InstanceState],
    instances_total: usize,
    metadata: ScorecardMetadata<'_>,
) -> Scorecard {
    let attempted = states.len();
    let resolved = states
        .iter()
        .filter(|s| {
            matches!(s.status, InstanceStatus::Verified)
                && s.verdict.as_ref().is_some_and(|v| v.resolved)
        })
        .count();
    let score_pct = if instances_total == 0 {
        0.0
    } else {
        (resolved as f64) * 100.0 / (instances_total as f64)
    };
    Scorecard {
        schema_version: SCORECARD_SCHEMA_VERSION,
        date: metadata.date.to_string(),
        commit: metadata.commit.to_string(),
        branch: metadata.branch.map(str::to_string),
        pr: metadata.pr,
        category: metadata.category,
        orchestrator_model: metadata.orchestrator_model.to_string(),
        worker_model: metadata.worker_model.map(str::to_string),
        model: metadata.orchestrator_model.to_string(),
        dataset: metadata.dataset.to_string(),
        instances_total,
        instances_attempted: attempted,
        instances_resolved: resolved,
        score_pct,
        is_complete: instances_total > 0
            && attempted == instances_total
            && states
                .iter()
                .all(|s| matches!(s.status, InstanceStatus::Verified)),
        state_dir: metadata.state_dir.to_string(),
    }
}

/// Bag of metadata that the binary collects (UTC clock, `git rev-parse`,
/// `--category`/`--orchestrator-model`/`--worker-model` CLI flags) and
/// feeds into [`aggregate_scorecard`].
#[derive(Debug, Clone, Copy)]
pub struct ScorecardMetadata<'a> {
    pub date: &'a str,
    pub commit: &'a str,
    pub branch: Option<&'a str>,
    pub pr: Option<u64>,
    pub category: ScorecardCategory,
    pub orchestrator_model: &'a str,
    pub worker_model: Option<&'a str>,
    pub dataset: &'a str,
    pub state_dir: &'a str,
}

// ---------------------------------------------------------------------------
// State-dir layout + I/O helpers
// ---------------------------------------------------------------------------

/// Directory holding per-instance unified-diff predictions.
pub fn predictions_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("predictions")
}

/// Directory holding per-instance state JSON files.
pub fn states_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("state")
}

/// Path to the state file for a single instance.
pub fn state_path(state_dir: &Path, instance_id: &str) -> PathBuf {
    states_dir(state_dir).join(format!("{instance_id}.json"))
}

/// Path to the diff file for a single instance.
pub fn diff_path(state_dir: &Path, instance_id: &str) -> PathBuf {
    predictions_dir(state_dir).join(format!("{instance_id}.diff"))
}

/// Read and parse a state file. Returns `None` if the file is missing or
/// malformed; the caller treats both as "no prior state".
pub fn read_state(path: &Path) -> Option<InstanceState> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write a state file under `<state-dir>/state/<instance_id>.json`,
/// creating parent directories as needed.
pub fn write_state(state_dir: &Path, state: &InstanceState) -> std::io::Result<()> {
    let path = state_path(state_dir, &state.instance_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// Walk `<state-dir>/state/` and parse every `<id>.json`. Skips files that
/// fail to parse rather than aborting — a corrupt single file should not
/// prevent the rest of the run from finalising.
pub fn load_all_states(state_dir: &Path) -> std::io::Result<Vec<InstanceState>> {
    let dir = states_dir(state_dir);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let is_json = entry.file_type()?.is_file()
            && entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "json");
        if is_json {
            if let Some(s) = read_state(&entry.path()) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
    Ok(out)
}

/// Append a single line to `path`, creating it (and parent dirs) if needed.
/// Used to append a [`Scorecard`] JSON line to `evals/scorecards/index.jsonl`.
pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::fs::OpenOptions;
    use std::io::Write as _;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

/// Decide whether an instance should be (re)attempted given the prior
/// state file (or its absence). Returns `true` when the runner should
/// run the agent for this instance; `false` when it should be skipped.
pub fn should_attempt(prior: Option<&InstanceState>) -> bool {
    match prior {
        None => true,
        Some(s) => !s.status.is_terminal(),
    }
}

/// Resolve a model name from a CLI flag, then `NANNA_EVAL_MODEL`, then
/// `MODEL`, with a final fallback default. The default mirrors the
/// `harness-eval` binary's resolution order.
pub fn resolve_model(cli: Option<&str>, default: &str) -> String {
    cli.map(str::to_string)
        .or_else(|| std::env::var("NANNA_EVAL_MODEL").ok())
        .or_else(|| std::env::var("MODEL").ok())
        .unwrap_or_else(|| default.to_string())
}

/// Sanitise a model name for use as a path component (matches the upstream
/// SWE-bench harness behaviour — `:` and `/` become `-`).
pub fn sanitize_model_for_path(model: &str) -> String {
    model.replace([':', '/'], "-")
}

// ---------------------------------------------------------------------------
// Outcome → InstanceState transformation
// ---------------------------------------------------------------------------

/// Lightweight description of what `run_eval` produced, used to decide the
/// next [`InstanceState`] without depending on the full
/// [`crate::eval::runner::EvalRunResult`] type. Lets the transformation be
/// unit-tested without spinning up an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    Success {
        patch: Option<String>,
        metrics: AgentMetrics,
    },
    MaterializeError(String),
    AgentError(String),
}

/// Translate the `Result` returned by `run_eval` into an [`AttemptOutcome`]
/// the state-machine knows how to record. Pure; no I/O.
pub fn outcome_from_run_eval(
    res: Result<crate::eval::runner::EvalRunResult, crate::eval::runner::EvalRunnerError>,
) -> AttemptOutcome {
    use crate::eval::runner::EvalRunnerError;
    match res {
        Ok(r) => AttemptOutcome::Success {
            patch: r.swebench_patch,
            metrics: AgentMetrics {
                iterations: r.iterations,
                prompt_tokens: r.token_usage.prompt_tokens as u64,
                completion_tokens: r.token_usage.completion_tokens as u64,
                wall_secs: 0,
            },
        },
        Err(EvalRunnerError::SwebenchMaterialize(e)) => {
            AttemptOutcome::MaterializeError(e.to_string())
        }
        Err(EvalRunnerError::Timeout(d)) => {
            AttemptOutcome::AgentError(format!("agent timeout after {d:?}"))
        }
        Err(other) => AttemptOutcome::AgentError(other.to_string()),
    }
}

/// Map an [`AttemptOutcome`] to the next state file. Side-effect: writes
/// the diff file under `<state-dir>/predictions/<id>.diff` on success.
pub fn instance_state_from_outcome(
    state_dir: &Path,
    instance_id: &str,
    outcome: AttemptOutcome,
    wall_secs: u64,
) -> std::io::Result<InstanceState> {
    Ok(match outcome {
        AttemptOutcome::Success {
            patch: Some(patch),
            metrics,
        } => {
            let dpath = diff_path(state_dir, instance_id);
            if let Some(parent) = dpath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dpath, &patch)?;
            let metrics = AgentMetrics {
                wall_secs,
                ..metrics
            };
            InstanceState::completed_diff(instance_id, dpath.display().to_string(), metrics)
        }
        AttemptOutcome::Success {
            patch: None,
            metrics,
        } => {
            let metrics = AgentMetrics {
                wall_secs,
                ..metrics
            };
            InstanceState::agent_error(
                instance_id,
                metrics,
                "agent produced no swebench_patch (run_eval skipped swebench branch)",
            )
        }
        AttemptOutcome::MaterializeError(e) => InstanceState::materialize_error(instance_id, e),
        AttemptOutcome::AgentError(e) => InstanceState::agent_error(
            instance_id,
            AgentMetrics {
                wall_secs,
                ..AgentMetrics::default()
            },
            e,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> ScorecardMetadata<'static> {
        ScorecardMetadata {
            date: "2026-05-08T08:30:00Z",
            commit: "abc1234",
            branch: Some("feat/score"),
            pr: None,
            category: ScorecardCategory::NannaSolo,
            orchestrator_model: "gemma4:e4b",
            worker_model: None,
            dataset: "princeton-nlp/SWE-bench_Lite",
            state_dir: "evals/scorecards/state/local-1",
        }
    }

    fn verdict(resolved: bool) -> StoredVerdict {
        StoredVerdict {
            resolved,
            error: None,
        }
    }

    #[test]
    fn aggregate_uses_full_denominator_for_score_pct() {
        let states = vec![
            InstanceState::completed_diff("a", "predictions/a.diff", AgentMetrics::default())
                .with_verdict(verdict(true)),
            InstanceState::completed_diff("b", "predictions/b.diff", AgentMetrics::default())
                .with_verdict(verdict(true)),
            InstanceState::completed_diff("c", "predictions/c.diff", AgentMetrics::default())
                .with_verdict(verdict(false)),
        ];
        let card = aggregate_scorecard(&states, 300, meta());
        assert_eq!(card.instances_attempted, 3);
        assert_eq!(card.instances_resolved, 2);
        assert!((card.score_pct - (200.0 / 300.0)).abs() < 1e-9);
        assert!(!card.is_complete);
    }

    #[test]
    fn aggregate_marks_complete_only_when_all_verified() {
        let states = vec![
            InstanceState::completed_diff("a", "predictions/a.diff", AgentMetrics::default())
                .with_verdict(verdict(true)),
            InstanceState::completed_diff("b", "predictions/b.diff", AgentMetrics::default())
                .with_verdict(verdict(false)),
        ];
        let card = aggregate_scorecard(&states, 2, meta());
        assert!(card.is_complete);
        assert_eq!(card.score_pct, 50.0);
    }

    #[test]
    fn aggregate_does_not_count_non_verified_as_resolved() {
        let states = vec![
            InstanceState::completed_diff("a", "predictions/a.diff", AgentMetrics::default()),
            InstanceState::agent_error("b", AgentMetrics::default(), "timeout"),
            InstanceState::materialize_error("c", "patch failed"),
        ];
        let card = aggregate_scorecard(&states, 3, meta());
        assert_eq!(card.instances_resolved, 0);
        assert!(!card.is_complete);
    }

    #[test]
    fn aggregate_zero_total_yields_zero_score() {
        let card = aggregate_scorecard(&[], 0, meta());
        assert_eq!(card.score_pct, 0.0);
        assert!(!card.is_complete);
    }

    #[test]
    fn instance_status_terminal_set() {
        assert!(InstanceStatus::CompletedDiff.is_terminal());
        assert!(InstanceStatus::Verified.is_terminal());
        assert!(InstanceStatus::MaterializeError.is_terminal());
        assert!(InstanceStatus::AgentError.is_terminal());
    }

    #[test]
    fn instance_state_round_trips_through_json() {
        let s = InstanceState::completed_diff(
            "django__django-11099",
            "predictions/django__django-11099.diff",
            AgentMetrics {
                iterations: 7,
                prompt_tokens: 1024,
                completion_tokens: 256,
                wall_secs: 42,
            },
        );
        let json = serde_json::to_string(&s).unwrap();
        let parsed: InstanceState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, s);
    }

    #[test]
    fn scorecard_round_trips_through_json() {
        let states =
            vec![
                InstanceState::completed_diff("a", "predictions/a.diff", AgentMetrics::default())
                    .with_verdict(verdict(true)),
            ];
        let card = aggregate_scorecard(&states, 1, meta());
        let json = serde_json::to_string(&card).unwrap();
        let parsed: Scorecard = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, card);
    }

    #[test]
    fn path_helpers_assemble_state_dir_layout() {
        let root = Path::new("/run/local-1");
        assert_eq!(predictions_dir(root), Path::new("/run/local-1/predictions"));
        assert_eq!(states_dir(root), Path::new("/run/local-1/state"));
        assert_eq!(
            state_path(root, "django__django-11099"),
            Path::new("/run/local-1/state/django__django-11099.json")
        );
        assert_eq!(
            diff_path(root, "django__django-11099"),
            Path::new("/run/local-1/predictions/django__django-11099.diff")
        );
    }

    #[test]
    fn write_state_then_read_state_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let s = InstanceState::completed_diff(
            "x",
            "predictions/x.diff",
            AgentMetrics {
                iterations: 1,
                ..AgentMetrics::default()
            },
        );
        write_state(dir.path(), &s).unwrap();
        let read = read_state(&state_path(dir.path(), "x")).unwrap();
        assert_eq!(read, s);
    }

    #[test]
    fn read_state_returns_none_for_missing_or_garbage() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_state(&dir.path().join("nope.json")).is_none());
        let garbage = dir.path().join("garbage.json");
        std::fs::write(&garbage, "{not json").unwrap();
        assert!(read_state(&garbage).is_none());
    }

    #[test]
    fn load_all_states_skips_unparseable_files_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let s_b = InstanceState::completed_diff("b", "p/b.diff", AgentMetrics::default());
        let s_a = InstanceState::completed_diff("a", "p/a.diff", AgentMetrics::default());
        write_state(dir.path(), &s_b).unwrap();
        write_state(dir.path(), &s_a).unwrap();
        let garbage = states_dir(dir.path()).join("garbage.json");
        std::fs::write(&garbage, "[not valid").unwrap();
        let txt = states_dir(dir.path()).join("readme.txt");
        std::fs::write(&txt, "skipped").unwrap();

        let states = load_all_states(dir.path()).unwrap();
        let ids: Vec<_> = states.iter().map(|s| s.instance_id.clone()).collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn load_all_states_returns_empty_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let states = load_all_states(&dir.path().join("not-a-run")).unwrap();
        assert!(states.is_empty());
    }

    #[test]
    fn append_line_creates_file_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("scorecards/index.jsonl");
        append_line(&p, "{\"a\":1}").unwrap();
        append_line(&p, "{\"b\":2}").unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert_eq!(content, "{\"a\":1}\n{\"b\":2}\n");
    }

    #[test]
    fn should_attempt_skips_terminal_runs() {
        assert!(should_attempt(None));
        let s = InstanceState::completed_diff("x", "p", AgentMetrics::default());
        assert!(!should_attempt(Some(&s)));
        let m = InstanceState::materialize_error("x", "boom");
        assert!(!should_attempt(Some(&m)));
        let a = InstanceState::agent_error("x", AgentMetrics::default(), "timeout");
        assert!(!should_attempt(Some(&a)));
    }

    #[test]
    fn resolve_model_priority_cli_then_env_then_default() {
        // CLI wins.
        let m = resolve_model(Some("from-cli"), "fallback");
        assert_eq!(m, "from-cli");

        // No env vars: default.
        // Save and clear NANNA_EVAL_MODEL / MODEL for the duration of this test.
        let prior_nanna = std::env::var("NANNA_EVAL_MODEL").ok();
        let prior_model = std::env::var("MODEL").ok();
        std::env::remove_var("NANNA_EVAL_MODEL");
        std::env::remove_var("MODEL");
        let m = resolve_model(None, "fallback");
        assert_eq!(m, "fallback");

        std::env::set_var("MODEL", "from-MODEL");
        let m = resolve_model(None, "fallback");
        assert_eq!(m, "from-MODEL");

        std::env::set_var("NANNA_EVAL_MODEL", "from-NANNA");
        let m = resolve_model(None, "fallback");
        assert_eq!(m, "from-NANNA");

        // Restore.
        std::env::remove_var("NANNA_EVAL_MODEL");
        std::env::remove_var("MODEL");
        if let Some(v) = prior_nanna {
            std::env::set_var("NANNA_EVAL_MODEL", v);
        }
        if let Some(v) = prior_model {
            std::env::set_var("MODEL", v);
        }
    }

    #[test]
    fn sanitize_model_for_path_replaces_colons_and_slashes() {
        assert_eq!(sanitize_model_for_path("gemma4:e4b"), "gemma4-e4b");
        assert_eq!(sanitize_model_for_path("org/family:tag"), "org-family-tag");
        assert_eq!(sanitize_model_for_path("plain"), "plain");
    }

    #[test]
    fn outcome_success_with_patch_writes_diff_and_returns_completed_diff() {
        let dir = tempfile::tempdir().unwrap();
        let metrics = AgentMetrics {
            iterations: 7,
            prompt_tokens: 100,
            completion_tokens: 50,
            wall_secs: 0,
        };
        let state = instance_state_from_outcome(
            dir.path(),
            "x",
            AttemptOutcome::Success {
                patch: Some("diff --git a b\n".to_string()),
                metrics,
            },
            42,
        )
        .unwrap();
        assert_eq!(state.status, InstanceStatus::CompletedDiff);
        assert_eq!(state.agent_metrics.iterations, 7);
        assert_eq!(state.agent_metrics.wall_secs, 42, "wall_secs is overridden");
        let dpath = diff_path(dir.path(), "x");
        assert!(dpath.is_file());
        assert_eq!(std::fs::read_to_string(&dpath).unwrap(), "diff --git a b\n");
    }

    #[test]
    fn outcome_success_with_no_patch_returns_agent_error() {
        let dir = tempfile::tempdir().unwrap();
        let state = instance_state_from_outcome(
            dir.path(),
            "x",
            AttemptOutcome::Success {
                patch: None,
                metrics: AgentMetrics::default(),
            },
            10,
        )
        .unwrap();
        assert_eq!(state.status, InstanceStatus::AgentError);
        assert_eq!(state.agent_metrics.wall_secs, 10);
    }

    #[test]
    fn outcome_materialize_error_returns_materialize_error_state() {
        let dir = tempfile::tempdir().unwrap();
        let state = instance_state_from_outcome(
            dir.path(),
            "x",
            AttemptOutcome::MaterializeError("hunk did not apply".to_string()),
            0,
        )
        .unwrap();
        assert_eq!(state.status, InstanceStatus::MaterializeError);
        assert_eq!(state.error.as_deref(), Some("hunk did not apply"));
    }

    #[test]
    fn outcome_from_run_eval_maps_ok_to_success() {
        use crate::eval::runner::{EvalRunResult, VerificationResult};
        let result = EvalRunResult {
            case_id: "x".to_string(),
            success: false,
            execution_time: std::time::Duration::from_secs(0),
            iterations: 9,
            token_usage: model::types::Usage {
                prompt_tokens: 11,
                completion_tokens: 22,
                total_tokens: 33,
            },
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
            swebench_patch: Some("diff".to_string()),
        };
        let o = outcome_from_run_eval(Ok(result));
        match o {
            AttemptOutcome::Success { patch, metrics } => {
                assert_eq!(patch.as_deref(), Some("diff"));
                assert_eq!(metrics.iterations, 9);
                assert_eq!(metrics.prompt_tokens, 11);
                assert_eq!(metrics.completion_tokens, 22);
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn outcome_from_run_eval_maps_timeout_to_agent_error() {
        use crate::eval::runner::EvalRunnerError;
        let o = outcome_from_run_eval(Err(EvalRunnerError::Timeout(
            std::time::Duration::from_secs(60),
        )));
        match o {
            AttemptOutcome::AgentError(msg) => assert!(msg.contains("timeout")),
            other => panic!("expected AgentError, got {other:?}"),
        }
    }

    #[test]
    fn outcome_from_run_eval_maps_other_errors_to_agent_error() {
        use crate::eval::runner::EvalRunnerError;
        let o = outcome_from_run_eval(Err(EvalRunnerError::ModelProvider(
            "ollama refused".to_string(),
        )));
        match o {
            AttemptOutcome::AgentError(msg) => assert!(msg.contains("ollama refused")),
            other => panic!("expected AgentError, got {other:?}"),
        }
    }

    #[test]
    fn outcome_agent_error_returns_agent_error_state_with_wall_secs() {
        let dir = tempfile::tempdir().unwrap();
        let state = instance_state_from_outcome(
            dir.path(),
            "x",
            AttemptOutcome::AgentError("timeout".to_string()),
            33,
        )
        .unwrap();
        assert_eq!(state.status, InstanceStatus::AgentError);
        assert_eq!(state.agent_metrics.wall_secs, 33);
        assert_eq!(state.error.as_deref(), Some("timeout"));
    }

    // ---------------- ScorecardCategory + v2 schema ----------------

    #[test]
    fn category_serialises_to_snake_case() {
        for cat in ScorecardCategory::ALL {
            let json = serde_json::to_string(&cat).unwrap();
            // Round-trip via serde to confirm the `rename_all = "snake_case"`
            // shape matches `as_str()`.
            assert_eq!(json, format!("\"{}\"", cat.as_str()));
            let parsed: ScorecardCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, cat);
        }
    }

    #[test]
    fn category_from_str_matches_serialised_form() {
        for cat in ScorecardCategory::ALL {
            assert_eq!(ScorecardCategory::from_str(cat.as_str()).unwrap(), cat);
        }
    }

    #[test]
    fn category_from_str_rejects_typos_and_unknown() {
        let err = ScorecardCategory::from_str("nanna-solo").unwrap_err();
        assert!(err.contains("nanna-solo"), "error mentions the bad input");
        assert!(err.contains("nanna_solo"), "error lists valid options");
        assert!(ScorecardCategory::from_str("").is_err());
        assert!(ScorecardCategory::from_str("Nanna_Solo").is_err());
    }

    #[test]
    fn deserialise_rejects_unknown_category_string() {
        let bad = "\"claude_solo_typo\"";
        assert!(serde_json::from_str::<ScorecardCategory>(bad).is_err());
    }

    #[test]
    fn scorecard_v2_round_trips_with_category_and_models() {
        let states =
            vec![
                InstanceState::completed_diff("a", "predictions/a.diff", AgentMetrics::default())
                    .with_verdict(verdict(true)),
            ];
        let m = ScorecardMetadata {
            category: ScorecardCategory::ClaudeMcpClaude,
            orchestrator_model: "claude-opus-4-7",
            worker_model: Some("claude-opus-4-7"),
            ..meta()
        };
        let card = aggregate_scorecard(&states, 1, m);
        assert_eq!(card.schema_version, SCORECARD_SCHEMA_VERSION);
        assert_eq!(card.category, ScorecardCategory::ClaudeMcpClaude);
        assert_eq!(card.orchestrator_model, "claude-opus-4-7");
        assert_eq!(card.worker_model.as_deref(), Some("claude-opus-4-7"));
        // `model` is the v1-compat duplicate of orchestrator_model.
        assert_eq!(card.model, card.orchestrator_model);

        let json = serde_json::to_string(&card).unwrap();
        let parsed: Scorecard = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, card);
    }

    #[test]
    fn scorecard_solo_track_has_no_worker_model() {
        let states =
            vec![
                InstanceState::completed_diff("a", "predictions/a.diff", AgentMetrics::default())
                    .with_verdict(verdict(true)),
            ];
        let m = ScorecardMetadata {
            category: ScorecardCategory::ClaudeSolo,
            orchestrator_model: "claude-opus-4-7",
            worker_model: None,
            ..meta()
        };
        let card = aggregate_scorecard(&states, 1, m);
        assert_eq!(card.category, ScorecardCategory::ClaudeSolo);
        assert!(card.worker_model.is_none());

        // JSON should serialise `worker_model` as null (or omit if None
        // — serde_json default emits `"worker_model":null`); a v2 reader
        // must accept both shapes.
        let json = serde_json::to_string(&card).unwrap();
        assert!(
            json.contains("\"worker_model\":null"),
            "expected explicit null in JSON: {json}"
        );
    }

    #[test]
    fn scorecard_v1_payload_is_rejected_under_v2_struct() {
        // A v1-shaped JSON line lacks `category` / `orchestrator_model`
        // and must therefore fail to parse as a [`Scorecard`]. This is
        // the *intended* breakage: pause-the-loop coordination notes in
        // the dev plan exist precisely so no v1 row ever gets written.
        let v1 = serde_json::json!({
            "schema_version": 1,
            "date": "2026-05-08T08:30:00Z",
            "commit": "abc1234",
            "branch": "feat/score",
            "pr": null,
            "model": "gemma4:e4b",
            "dataset": "princeton-nlp/SWE-bench_Lite",
            "instances_total": 1,
            "instances_attempted": 1,
            "instances_resolved": 1,
            "score_pct": 100.0,
            "is_complete": true,
            "state_dir": "evals/scorecards/state/local-1"
        });
        let r: Result<Scorecard, _> = serde_json::from_value(v1);
        assert!(r.is_err(), "v1 row must not deserialise as v2 Scorecard");
    }
}
