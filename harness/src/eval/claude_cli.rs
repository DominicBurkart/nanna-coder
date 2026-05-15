//! `ClaudeCodeRunner` — headless `claude -p` subprocess wrapper with
//! NDJSON event capture.
//!
//! Spawns the Claude Code CLI in non-interactive print mode with
//! `--output-format stream-json --verbose`, streams every NDJSON event
//! from stdout, optionally tees the stream to a per-invocation log
//! file, and parses the final-result envelope for token/cost summary.
//!
//! The on-disk NDJSON log is the canonical "complete verbose logs"
//! record: it captures every assistant message, every `tool_use` /
//! `tool_result`, and the final summary, regardless of whether the
//! result-envelope parser handles a future schema change.
//!
//! The wrapper is otherwise *pure*: it spawns the subprocess,
//! captures stdout/stderr, parses, and returns. The caller is
//! responsible for setting up the repo state (`materialize`) before
//! invocation and for capturing `git diff` after the subprocess
//! exits.
//!
//! Used by:
//! - The cat 1 (`claude_solo`) eval-runner mode (`ClaudeCodeDirect`).
//! - The cat 2 (`claude_mcp_claude`) worker dispatch inside the
//!   nanna MCP server's `assign_task` handler.
//!
//! ## Final-result envelope schema
//!
//! In `--output-format stream-json`, the *last* NDJSON line is expected
//! to be a result event with the shape:
//!
//! ```json
//! {
//!   "type": "result",
//!   "result": "...",
//!   "session_id": "...",
//!   "usage": { "input_tokens": N, "output_tokens": N,
//!              "cache_creation_input_tokens": N?,
//!              "cache_read_input_tokens": N? },
//!   "total_cost_usd": 0.0,
//!   "is_error": false
//! }
//! ```
//!
//! [`parse_output`] is schema-defensive: it scans lines from the end
//! and returns the first one that deserialises as a result envelope.
//! Intermediate `message_start` / `message_delta` events that also
//! carry partial `usage` fields are skipped in favour of the final
//! cumulative result line. If Claude Code adds new event types or
//! reorders fields, the on-disk log remains intact and the parser
//! can be adapted in a follow-up.
//!
//! Source: <https://code.claude.com/docs/en/headless.md>,
//! <https://code.claude.com/docs/en/agent-sdk/streaming-output>.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Deserialize;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Default model when [`ClaudeCodeRunner::model`] is not overridden.
pub const DEFAULT_MODEL: &str = "claude-opus-4-7";

/// Default tool surface for SWE-bench-style file/bash-driven tasks.
/// Mirrors what a Claude Code user would see by default minus the
/// network-touching tools (`WebFetch`, `WebSearch`) which are not
/// useful for a closed-form bug-fix benchmark.
pub const DEFAULT_ALLOWED_TOOLS: &[&str] = &["Read", "Edit", "Write", "Bash", "Glob", "Grep"];

/// Spawn-time configuration for a single `claude -p` invocation.
///
/// Construct with [`ClaudeCodeRunner::new`] / [`ClaudeCodeRunner::default`]
/// and customise via the builder-style setters.
#[derive(Debug, Clone)]
pub struct ClaudeCodeRunner {
    /// Path to the `claude` executable. Defaults to `"claude"` (relies
    /// on `PATH`). Override for testing with a stub binary.
    pub claude_bin: PathBuf,
    /// `--model` value.
    pub model: String,
    /// `--max-budget-usd` cap. `None` ⇒ no per-invocation cap.
    pub max_budget_usd: Option<f64>,
    /// `--allowed-tools` set. Empty ⇒ flag omitted (Claude Code default).
    pub allowed_tools: Vec<String>,
    /// `--bare` — skip CLAUDE.md auto-discovery / hooks / plugin sync.
    /// Strongly recommended for hermetic eval runs so that the SWE-bench
    /// repo's own `CLAUDE.md` (if any) does not pollute the agent.
    pub bare: bool,
    /// `--permission-mode` value (default `bypassPermissions` so the
    /// subprocess never blocks on tool prompts).
    pub permission_mode: String,
    /// `--mcp-config` path. Used by the cat-2 orchestrator runner mode.
    pub mcp_config: Option<PathBuf>,
    /// When set, every NDJSON event from `claude -p --output-format
    /// stream-json` is teed to a file at `<log_dir>/<log_file_name>.ndjson`.
    /// Caller is responsible for ensuring the directory exists. `None`
    /// disables log capture entirely (suitable for unit tests that only
    /// care about the final summary).
    pub log_dir: Option<PathBuf>,
    /// Base name of the NDJSON log file written under [`log_dir`]. The
    /// `.ndjson` suffix is added automatically. Defaults to `claude_p`
    /// when unset; callers running multiple invocations (orchestrator +
    /// workers) should set this to a unique tag per invocation.
    pub log_file_name: String,
    /// Extra args appended after the standard ones, for forward-compat.
    pub extra_args: Vec<OsString>,
}

impl Default for ClaudeCodeRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeCodeRunner {
    /// Construct with sensible defaults: `claude` on PATH, model
    /// `claude-opus-4-7`, the default file/bash tool surface, `--bare`,
    /// `--permission-mode bypassPermissions`.
    pub fn new() -> Self {
        Self {
            claude_bin: PathBuf::from("claude"),
            model: DEFAULT_MODEL.to_string(),
            max_budget_usd: None,
            allowed_tools: DEFAULT_ALLOWED_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            bare: true,
            permission_mode: "bypassPermissions".to_string(),
            mcp_config: None,
            log_dir: None,
            log_file_name: "claude_p".to_string(),
            extra_args: Vec::new(),
        }
    }

    pub fn with_log_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.log_dir = Some(dir.into());
        self
    }

    pub fn with_log_file_name(mut self, name: impl Into<String>) -> Self {
        self.log_file_name = name.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_max_budget_usd(mut self, usd: f64) -> Self {
        self.max_budget_usd = Some(usd);
        self
    }

    pub fn with_allowed_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_mcp_config(mut self, path: impl Into<PathBuf>) -> Self {
        self.mcp_config = Some(path.into());
        self
    }

    pub fn with_claude_bin(mut self, path: impl Into<PathBuf>) -> Self {
        self.claude_bin = path.into();
        self
    }

    /// Build the argv that would be passed to `claude` for the given
    /// task and repo. Extracted so tests can verify the invocation
    /// shape without spawning a subprocess.
    pub fn build_args(&self, task_description: &str, repo_path: &Path) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::with_capacity(24);
        args.push("-p".into());
        args.push(task_description.into());
        args.push("--output-format".into());
        // stream-json: one NDJSON event per line. Captures every
        // message, tool_use, and tool_result that `claude -p` emits,
        // so the per-instance log file is a complete transcript.
        args.push("stream-json".into());
        // stream-json + --print requires --verbose; the CLI errors out
        // otherwise. (See `claude --help` under --output-format.)
        args.push("--verbose".into());
        args.push("--no-session-persistence".into());
        args.push("--permission-mode".into());
        args.push(self.permission_mode.as_str().into());
        args.push("--add-dir".into());
        args.push(repo_path.as_os_str().to_owned());
        args.push("--model".into());
        args.push(self.model.as_str().into());
        if self.bare {
            args.push("--bare".into());
        }
        if let Some(b) = self.max_budget_usd {
            args.push("--max-budget-usd".into());
            args.push(b.to_string().into());
        }
        if !self.allowed_tools.is_empty() {
            args.push("--allowed-tools".into());
            args.push(self.allowed_tools.join(",").into());
        }
        if let Some(mcp) = &self.mcp_config {
            args.push("--mcp-config".into());
            args.push(mcp.as_os_str().to_owned());
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }

    /// Spawn `claude -p` with the configured options. Returns the
    /// parsed run summary on success.
    ///
    /// `task_description` becomes the prompt (positional arg to `-p`).
    /// `repo_path` is passed to `--add-dir` *and* used as the
    /// subprocess `cwd` so any tool-emitted relative paths resolve
    /// against the repo.
    ///
    /// Stdout is read line-by-line. When `log_dir` is set, every NDJSON
    /// event is teed to a file at `<log_dir>/<log_file_name>.ndjson`
    /// before being collected for result parsing. The log file is the
    /// authoritative source for "complete verbose logs" telemetry —
    /// even if [`parse_output`] cannot identify a final-result line,
    /// the on-disk transcript is intact.
    pub async fn run(
        &self,
        task_description: &str,
        repo_path: &Path,
    ) -> Result<ClaudeCodeRun, ClaudeCodeError> {
        let args = self.build_args(task_description, repo_path);
        let mut cmd = Command::new(&self.claude_bin);
        cmd.args(&args);
        cmd.current_dir(repo_path);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| ClaudeCodeError::Spawn(format!("{}: {e}", self.claude_bin.display())))?;
        let stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| ClaudeCodeError::Spawn("stdout pipe missing".to_string()))?;
        let stderr_pipe = child.stderr.take();

        // Set up optional log-file tee.
        let log_path = self
            .log_dir
            .as_ref()
            .map(|d| d.join(format!("{}.ndjson", self.log_file_name)));
        let mut log_file = match log_path.as_ref() {
            Some(p) => {
                if let Some(parent) = p.parent() {
                    if let Err(e) = tokio::fs::create_dir_all(parent).await {
                        return Err(ClaudeCodeError::Spawn(format!(
                            "create log_dir {}: {e}",
                            parent.display()
                        )));
                    }
                }
                Some(tokio::fs::File::create(p).await.map_err(|e| {
                    ClaudeCodeError::Spawn(format!("open log file {}: {e}", p.display()))
                })?)
            }
            None => None,
        };

        // Stream stdout: each line is one NDJSON event. Tee to log
        // file (if configured) and accumulate in memory for the
        // result parser. Per-line latency is paid by the parser only;
        // log-file writes are best-effort (a failed write is logged
        // but does not abort the run, since the subprocess is still
        // generating useful data).
        let mut lines: Vec<String> = Vec::new();
        let mut reader = BufReader::new(stdout_pipe).lines();
        while let Some(line) = reader
            .next_line()
            .await
            .map_err(|e| ClaudeCodeError::Spawn(format!("read claude stdout: {e}")))?
        {
            if let Some(f) = log_file.as_mut() {
                use tokio::io::AsyncWriteExt as _;
                if let Err(e) = f.write_all(line.as_bytes()).await {
                    tracing::warn!("tee to log file failed: {e}");
                }
                if let Err(e) = f.write_all(b"\n").await {
                    tracing::warn!("tee newline to log file failed: {e}");
                }
            }
            lines.push(line);
        }
        if let Some(mut f) = log_file {
            use tokio::io::AsyncWriteExt as _;
            let _ = f.flush().await;
        }

        // Collect stderr after stdout drains (subprocess will have
        // exited or be about to). `wait_with_output` is not usable
        // because we already took stdout, so wait manually + drain
        // stderr.
        let stderr_text = if let Some(mut stderr) = stderr_pipe {
            use tokio::io::AsyncReadExt as _;
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        } else {
            String::new()
        };
        let status = child
            .wait()
            .await
            .map_err(|e| ClaudeCodeError::Spawn(format!("wait claude: {e}")))?;

        // Try to parse first; only surface a NonZeroExit if parse fails.
        // Claude Code may exit non-zero with a structured JSON body
        // describing in-band failure (e.g. budget-exceeded), and we
        // want the partial token usage even then.
        let parsed = parse_output(&lines.join("\n"));
        match (parsed, status.success()) {
            (Ok(run), _) => Ok(ClaudeCodeRun {
                stderr: stderr_text,
                exit_status: status.code().unwrap_or(-1),
                log_path,
                ..run
            }),
            (Err(e), true) => Err(e),
            (Err(_), false) => Err(ClaudeCodeError::NonZeroExit {
                code: status.code().unwrap_or(-1),
                stderr: stderr_text,
            }),
        }
    }
}

/// Successful (or `is_error=true`) result from a `claude -p` invocation.
///
/// `is_error: true` is *not* automatically a `Err` — Claude Code uses
/// the field to signal in-band failure (e.g. budget exceeded), and the
/// caller may want to record the partial token usage. Inspect the flag
/// before treating `result` as authoritative output.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeCodeRun {
    pub result: Option<String>,
    pub session_id: Option<String>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub is_error: bool,
    pub stderr: String,
    pub exit_status: i32,
    /// Path to the per-invocation NDJSON log file when `log_dir` was
    /// configured on the runner. `None` means logs were not captured.
    pub log_path: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum ClaudeCodeError {
    #[error("failed to spawn `claude`: {0}")]
    Spawn(String),
    #[error("`claude` exited with code {code}; stderr: {stderr}")]
    NonZeroExit { code: i32, stderr: String },
    #[error("could not parse `claude --output-format json` output: {0}")]
    ParseOutput(String),
}

// -----------------------------------------------------------------
// JSON parser — extracted so it can be unit-tested without spawning.
// -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawOutput {
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    usage: RawUsage,
    #[serde(default)]
    total_cost_usd: Option<f64>,
    #[serde(default)]
    is_error: bool,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

/// Parse the result envelope from a `claude -p` stdout dump.
///
/// Handles both legacy `--output-format json` (a single top-level JSON
/// object on stdout) and `--output-format stream-json` (NDJSON of
/// per-event objects, with a final summary object on the last line)
/// by scanning lines from the end and returning the first that
/// deserialises as a [`RawOutput`] (i.e. carries a `usage` field).
///
/// Schema-defensive: future Claude Code releases may add or rename
/// intermediate event types, but as long as *some* line still carries
/// the `result` / `usage` / `is_error` shape, this parser keeps
/// working. The on-disk log file remains the canonical record of
/// every event regardless.
fn parse_output(stdout: &str) -> Result<ClaudeCodeRun, ClaudeCodeError> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(ClaudeCodeError::ParseOutput("empty stdout".to_string()));
    }
    // Two valid shapes:
    //   (a) `--output-format json` — one top-level JSON object on
    //       stdout (possibly spanning many lines).
    //   (b) `--output-format stream-json` — NDJSON: one event per
    //       line, with the final summary object on the last line.
    //
    // Try shape (a) first by parsing the entire stdout as a single
    // JSON value. If that fails, fall back to shape (b) and scan
    // lines from the end.
    let parsed_whole = serde_json::from_str::<RawOutput>(trimmed).ok();
    let parsed_line = parsed_whole.or_else(|| {
        trimmed
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .find_map(|line| serde_json::from_str::<RawOutput>(line.trim()).ok())
    });
    let raw = match parsed_line {
        Some(r) => r,
        None => {
            let preview: String = trimmed
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .join(" / ");
            return Err(ClaudeCodeError::ParseOutput(format!(
                "no line on stdout deserialised as a result envelope (expected fields: usage{{input_tokens, output_tokens}}, optional result/session_id/total_cost_usd/is_error); last lines: {preview}"
            )));
        }
    };
    let total_tokens = raw.usage.input_tokens + raw.usage.output_tokens;
    Ok(ClaudeCodeRun {
        result: raw.result,
        session_id: raw.session_id,
        prompt_tokens: raw.usage.input_tokens,
        completion_tokens: raw.usage.output_tokens,
        cache_creation_input_tokens: raw.usage.cache_creation_input_tokens,
        cache_read_input_tokens: raw.usage.cache_read_input_tokens,
        total_tokens,
        total_cost_usd: raw.total_cost_usd.unwrap_or(0.0),
        is_error: raw.is_error,
        stderr: String::new(),
        exit_status: 0,
        log_path: None,
    })
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_output_minimal_success_payload() {
        let json = r#"{
            "result": "ok",
            "session_id": "abc-123",
            "usage": { "input_tokens": 42, "output_tokens": 7 },
            "total_cost_usd": 0.0001,
            "is_error": false
        }"#;
        let run = parse_output(json).unwrap();
        assert_eq!(run.result.as_deref(), Some("ok"));
        assert_eq!(run.session_id.as_deref(), Some("abc-123"));
        assert_eq!(run.prompt_tokens, 42);
        assert_eq!(run.completion_tokens, 7);
        assert_eq!(run.total_tokens, 49);
        assert!((run.total_cost_usd - 0.0001).abs() < 1e-12);
        assert!(!run.is_error);
    }

    #[test]
    fn parse_output_with_cache_tokens() {
        let json = r#"{
            "result": "ok",
            "session_id": "abc-123",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_creation_input_tokens": 3,
                "cache_read_input_tokens": 100
            },
            "total_cost_usd": 0.0,
            "is_error": false
        }"#;
        let run = parse_output(json).unwrap();
        assert_eq!(run.cache_creation_input_tokens, 3);
        assert_eq!(run.cache_read_input_tokens, 100);
        // total_tokens still tracks input+output only; cache fields are
        // additional metadata, not double-counted.
        assert_eq!(run.total_tokens, 15);
    }

    #[test]
    fn parse_output_is_error_payload() {
        // Budget-exceeded: Claude Code emits is_error=true with a
        // result body explaining the failure.
        let json = r#"{
            "result": "Error: budget of $0.01 exceeded",
            "session_id": "abc-123",
            "usage": { "input_tokens": 200, "output_tokens": 0 },
            "total_cost_usd": 0.011,
            "is_error": true
        }"#;
        let run = parse_output(json).unwrap();
        assert!(run.is_error);
        assert!(run.result.unwrap().contains("budget"));
        assert_eq!(run.prompt_tokens, 200);
    }

    #[test]
    fn parse_output_tolerates_unknown_fields() {
        // Future Claude Code releases may add fields; we should not
        // break on them.
        let json = r#"{
            "result": "ok",
            "session_id": "x",
            "usage": { "input_tokens": 1, "output_tokens": 1 },
            "total_cost_usd": 0.0,
            "is_error": false,
            "future_field": "future_value",
            "duration_ms": 1234
        }"#;
        let run = parse_output(json).unwrap();
        assert_eq!(run.total_tokens, 2);
    }

    #[test]
    fn parse_output_missing_usage_is_error() {
        // `usage` is required; without it we cannot do scorecard
        // accounting and must surface the failure.
        let json = r#"{ "result": "ok", "is_error": false }"#;
        let r = parse_output(json);
        assert!(matches!(r, Err(ClaudeCodeError::ParseOutput(_))));
    }

    #[test]
    fn parse_output_empty_stdout_is_error() {
        let r = parse_output("");
        assert!(matches!(r, Err(ClaudeCodeError::ParseOutput(_))));
        let r = parse_output("   \n  ");
        assert!(matches!(r, Err(ClaudeCodeError::ParseOutput(_))));
    }

    #[test]
    fn parse_output_picks_final_line_from_stream_json() {
        // stream-json shape: many event lines, final line is the
        // result envelope. The parser must skip non-matching lines
        // (e.g. message_start / content_block_delta) and pick the
        // last one that deserialises as a result envelope.
        let stream = "\
{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"abc\"}
{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}
{\"type\":\"result\",\"result\":\"hi\",\"session_id\":\"abc\",\"usage\":{\"input_tokens\":12,\"output_tokens\":3},\"total_cost_usd\":0.0002,\"is_error\":false}
";
        let run = parse_output(stream).unwrap();
        assert_eq!(run.result.as_deref(), Some("hi"));
        assert_eq!(run.session_id.as_deref(), Some("abc"));
        assert_eq!(run.prompt_tokens, 12);
        assert_eq!(run.completion_tokens, 3);
        assert_eq!(run.total_tokens, 15);
        assert!((run.total_cost_usd - 0.0002).abs() < 1e-12);
        assert!(!run.is_error);
    }

    #[test]
    fn parse_output_picks_last_result_line_when_multiple_have_usage() {
        // If both `message_start` and the final `result` line have a
        // `usage` field, the parser scans from the end and prefers
        // the final result (which has cumulative tokens), not the
        // intermediate event's partial usage.
        let stream = "\
{\"type\":\"message_start\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}
{\"type\":\"result\",\"result\":\"done\",\"usage\":{\"input_tokens\":50,\"output_tokens\":20},\"total_cost_usd\":0.001,\"is_error\":false}
";
        let run = parse_output(stream).unwrap();
        assert_eq!(run.prompt_tokens, 50);
        assert_eq!(run.completion_tokens, 20);
        assert_eq!(run.result.as_deref(), Some("done"));
    }

    #[test]
    fn parse_output_invalid_json_surfaces_preview() {
        let r = parse_output("not json {").unwrap_err();
        let msg = format!("{r}");
        // Error message must surface enough of stdout to debug a
        // schema mismatch — the trailing tail of stdout in our case.
        assert!(
            msg.contains("last lines:"),
            "error should include the 'last lines:' preview marker; got: {msg}"
        );
        assert!(msg.contains("not json"));
    }

    // ---------------- argv construction (no subprocess) ---------------

    #[test]
    fn build_args_default_runner_emits_expected_flags() {
        let r = ClaudeCodeRunner::new();
        let args = r.build_args("solve x", Path::new("/tmp/repo"));
        // Sanity-check key flags. We don't golden-file the full vec to
        // avoid brittleness — just confirm the contract.
        let joined: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(joined[0], "-p");
        assert_eq!(joined[1], "solve x");
        assert!(joined.contains(&"--output-format".to_string()));
        let i = joined.iter().position(|a| a == "--output-format").unwrap();
        assert_eq!(joined[i + 1], "stream-json");
        // stream-json + --print requires --verbose; the CLI refuses
        // otherwise.
        assert!(joined.contains(&"--verbose".to_string()));
        assert!(joined.contains(&"--bare".to_string()));
        assert!(joined.contains(&"--no-session-persistence".to_string()));
        let p = joined
            .iter()
            .position(|a| a == "--permission-mode")
            .unwrap();
        assert_eq!(joined[p + 1], "bypassPermissions");
        let d = joined.iter().position(|a| a == "--add-dir").unwrap();
        assert_eq!(joined[d + 1], "/tmp/repo");
        let m = joined.iter().position(|a| a == "--model").unwrap();
        assert_eq!(joined[m + 1], DEFAULT_MODEL);
        assert!(joined.contains(&"--allowed-tools".to_string()));
    }

    #[test]
    fn build_args_omits_max_budget_when_unset() {
        let r = ClaudeCodeRunner::new();
        let args = r.build_args("x", Path::new("/tmp"));
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into()).collect();
        assert!(!joined.contains(&"--max-budget-usd".to_string()));
    }

    #[test]
    fn build_args_includes_max_budget_when_set() {
        let r = ClaudeCodeRunner::new().with_max_budget_usd(0.5);
        let args = r.build_args("x", Path::new("/tmp"));
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into()).collect();
        let i = joined.iter().position(|a| a == "--max-budget-usd").unwrap();
        assert_eq!(joined[i + 1], "0.5");
    }

    #[test]
    fn build_args_includes_mcp_config_when_set() {
        let r = ClaudeCodeRunner::new().with_mcp_config("/tmp/mcp.json");
        let args = r.build_args("x", Path::new("/tmp"));
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into()).collect();
        let i = joined.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!(joined[i + 1], "/tmp/mcp.json");
    }

    #[test]
    fn build_args_omits_bare_when_disabled() {
        let mut r = ClaudeCodeRunner::new();
        r.bare = false;
        let args = r.build_args("x", Path::new("/tmp"));
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into()).collect();
        assert!(!joined.contains(&"--bare".to_string()));
    }

    #[test]
    fn build_args_omits_allowed_tools_when_empty() {
        let r = ClaudeCodeRunner::new().with_allowed_tools::<_, String>(Vec::<String>::new());
        let args = r.build_args("x", Path::new("/tmp"));
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into()).collect();
        assert!(!joined.contains(&"--allowed-tools".to_string()));
    }

    #[test]
    fn with_model_overrides_default() {
        let r = ClaudeCodeRunner::new().with_model("claude-haiku-4-5");
        assert_eq!(r.model, "claude-haiku-4-5");
    }

    #[test]
    fn default_matches_new() {
        // `Default::default()` must produce the same baseline as
        // `new()`. Test exists so both code paths are covered and so
        // a future divergence between them is loud.
        let a = ClaudeCodeRunner::default();
        let b = ClaudeCodeRunner::new();
        assert_eq!(a.claude_bin, b.claude_bin);
        assert_eq!(a.model, b.model);
        assert_eq!(a.max_budget_usd, b.max_budget_usd);
        assert_eq!(a.allowed_tools, b.allowed_tools);
        assert_eq!(a.bare, b.bare);
        assert_eq!(a.permission_mode, b.permission_mode);
        assert_eq!(a.log_dir, b.log_dir);
        assert_eq!(a.log_file_name, b.log_file_name);
    }

    #[test]
    fn with_log_dir_sets_field_and_keeps_default_file_name() {
        let r = ClaudeCodeRunner::new().with_log_dir("/tmp/eval-logs");
        assert_eq!(r.log_dir.as_deref(), Some(Path::new("/tmp/eval-logs")));
        assert_eq!(r.log_file_name, "claude_p");
    }

    #[test]
    fn with_log_file_name_overrides_default() {
        let r = ClaudeCodeRunner::new().with_log_file_name("orchestrator__instance_42");
        assert_eq!(r.log_file_name, "orchestrator__instance_42");
    }

    // ---------------- subprocess via stub `claude` shim ---------------

    /// Build a stub claude binary that emits a fixed JSON blob and
    /// exits 0. Returns a temp dir whose `claude` script is on disk.
    fn stub_claude_emitting(json_blob: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude");
        let escaped = json_blob.replace('\'', "'\\''");
        let script = format!("#!/usr/bin/env bash\nprintf '%s' '{}'\nexit 0\n", escaped);
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        dir
    }

    // The four subprocess tests below must run sequentially: parallel
    // `posix_spawn` calls inherit each other's writable fds to stub
    // binaries, producing ETXTBSY when those still-open-for-write fds
    // collide with a sibling's exec. Serialising avoids the race.

    #[tokio::test]
    #[cfg(unix)]
    #[serial_test::serial]
    async fn run_with_stub_claude_parses_output() {
        let stub = stub_claude_emitting(
            r#"{"result":"ok","session_id":"sid","usage":{"input_tokens":12,"output_tokens":3},"total_cost_usd":0.0002,"is_error":false}"#,
        );
        let runner = ClaudeCodeRunner::new().with_claude_bin(stub.path().join("claude"));
        let repo = tempfile::tempdir().unwrap();
        let run = runner.run("hello", repo.path()).await.unwrap();
        assert_eq!(run.result.as_deref(), Some("ok"));
        assert_eq!(run.prompt_tokens, 12);
        assert_eq!(run.completion_tokens, 3);
        assert_eq!(run.total_tokens, 15);
        assert!(!run.is_error);
        assert_eq!(run.exit_status, 0);
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial_test::serial]
    async fn run_with_failing_stub_surfaces_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude");
        std::fs::write(
            &path,
            "#!/usr/bin/env bash\necho 'something is on fire' >&2\nexit 7\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        let runner = ClaudeCodeRunner::new().with_claude_bin(&path);
        let repo = tempfile::tempdir().unwrap();
        let err = runner.run("hello", repo.path()).await.unwrap_err();
        match err {
            ClaudeCodeError::NonZeroExit { code, stderr } => {
                assert_eq!(code, 7);
                assert!(stderr.contains("on fire"));
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial_test::serial]
    async fn run_missing_binary_returns_spawn_error() {
        let runner = ClaudeCodeRunner::new()
            .with_claude_bin("/var/empty/definitely-not-a-real-claude-binary");
        let repo = tempfile::tempdir().unwrap();
        let err = runner.run("hello", repo.path()).await.unwrap_err();
        assert!(matches!(err, ClaudeCodeError::Spawn(_)));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial_test::serial]
    async fn run_with_zero_exit_but_malformed_output_returns_parse_error() {
        // The (Err(parse), true) branch in `run`: subprocess exits 0
        // but stdout doesn't parse as the expected JSON shape. We
        // surface a `ParseOutput` so the caller sees the upstream
        // contract violation rather than a silent zero-token success.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude");
        std::fs::write(
            &path,
            "#!/usr/bin/env bash\nprintf '%s' 'this is not json'\nexit 0\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();

        let runner = ClaudeCodeRunner::new().with_claude_bin(&path);
        let repo = tempfile::tempdir().unwrap();
        let err = runner.run("hello", repo.path()).await.unwrap_err();
        assert!(matches!(err, ClaudeCodeError::ParseOutput(_)));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial_test::serial]
    async fn run_with_is_error_payload_returns_ok_with_flag() {
        // A non-zero exit *with* a parseable is_error payload (e.g.
        // budget exceeded) is *not* a hard error from our caller's
        // perspective — we want the partial token usage. Use a stub
        // that prints the payload then exits 1.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude");
        let payload = r#"{"result":"budget exceeded","session_id":"x","usage":{"input_tokens":50,"output_tokens":0},"total_cost_usd":0.011,"is_error":true}"#;
        let escaped = payload.replace('\'', "'\\''");
        std::fs::write(
            &path,
            format!("#!/usr/bin/env bash\nprintf '%s' '{}'\nexit 1\n", escaped),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        let runner = ClaudeCodeRunner::new().with_claude_bin(&path);
        let repo = tempfile::tempdir().unwrap();
        let run = runner.run("hello", repo.path()).await.unwrap();
        assert!(run.is_error);
        assert_eq!(run.prompt_tokens, 50);
        assert_eq!(run.exit_status, 1);
    }

    /// Build a stub `claude` binary that emits multiple NDJSON lines
    /// (mimicking `--output-format stream-json`) and exits 0.
    #[cfg(unix)]
    fn stub_claude_streaming(lines: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude");
        let body = lines.join("\n");
        let escaped = body.replace('\'', "'\\''");
        let script = format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' '{}'\nexit 0\n",
            escaped
        );
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        dir
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial_test::serial]
    async fn run_writes_log_when_log_dir_set() {
        // With `log_dir` configured, every NDJSON line from the
        // subprocess is teed to `<log_dir>/<log_file_name>.ndjson`.
        // The log file is the canonical record for verbose telemetry
        // even if the result parser later cannot identify the final
        // envelope.
        let stub_dir = stub_claude_streaming(&[
            r#"{"type":"system","subtype":"init","session_id":"sid"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"result","result":"hi","session_id":"sid","usage":{"input_tokens":7,"output_tokens":1},"total_cost_usd":0.0,"is_error":false}"#,
        ]);
        let log_dir = tempfile::tempdir().unwrap();
        let runner = ClaudeCodeRunner::new()
            .with_claude_bin(stub_dir.path().join("claude"))
            .with_log_dir(log_dir.path())
            .with_log_file_name("smoke");
        let repo = tempfile::tempdir().unwrap();
        let run = runner.run("hello", repo.path()).await.unwrap();

        let expected_log = log_dir.path().join("smoke.ndjson");
        assert_eq!(run.log_path.as_deref(), Some(expected_log.as_path()));
        assert!(expected_log.is_file(), "log file should exist");
        let body = std::fs::read_to_string(&expected_log).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        // 3 NDJSON events written by the stub.
        assert_eq!(lines.len(), 3, "expected 3 NDJSON lines, got {body:?}");
        assert!(lines[0].contains("\"type\":\"system\""));
        assert!(lines[2].contains("\"type\":\"result\""));

        // The result parser still finds the final envelope.
        assert_eq!(run.result.as_deref(), Some("hi"));
        assert_eq!(run.total_tokens, 8);
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial_test::serial]
    async fn run_does_not_write_log_when_log_dir_unset() {
        let stub_dir = stub_claude_streaming(&[
            r#"{"type":"result","result":"ok","usage":{"input_tokens":1,"output_tokens":1},"is_error":false}"#,
        ]);
        let runner = ClaudeCodeRunner::new().with_claude_bin(stub_dir.path().join("claude"));
        let repo = tempfile::tempdir().unwrap();
        let run = runner.run("hello", repo.path()).await.unwrap();
        assert!(
            run.log_path.is_none(),
            "no log path expected when log_dir unset"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial_test::serial]
    async fn run_creates_missing_log_dir_parents() {
        let stub_dir = stub_claude_streaming(&[
            r#"{"type":"result","result":"ok","usage":{"input_tokens":1,"output_tokens":1},"is_error":false}"#,
        ]);
        let base = tempfile::tempdir().unwrap();
        // Nested path that doesn't exist yet.
        let nested = base.path().join("nested/deeply/eval-logs");
        let runner = ClaudeCodeRunner::new()
            .with_claude_bin(stub_dir.path().join("claude"))
            .with_log_dir(&nested)
            .with_log_file_name("auto-mkdir");
        let repo = tempfile::tempdir().unwrap();
        let run = runner.run("hi", repo.path()).await.unwrap();
        assert!(nested.is_dir(), "log_dir parents should be auto-created");
        assert!(run.log_path.is_some());
    }
}
