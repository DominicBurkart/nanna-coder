//! `ClaudeCodeRunner` — headless `claude -p` subprocess wrapper.
//!
//! Spawns the Claude Code CLI in non-interactive print mode with
//! `--output-format json` and parses the single JSON object from stdout.
//! The wrapper is *pure*: it spawns the subprocess, captures stdout,
//! parses, and returns. The caller is responsible for setting up the
//! repo state (`materialize`) before invocation and for capturing
//! `git diff` after the subprocess exits.
//!
//! Used by:
//! - The cat 1 (`claude_solo`) eval-runner mode (`ClaudeCodeDirect`).
//! - The cat 2 (`claude_mcp_claude`) worker dispatch inside the
//!   nanna MCP server's `assign_task` handler.
//!
//! The `claude -p --output-format json` schema this module pins to:
//!
//! ```json
//! {
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
//! Source: <https://code.claude.com/docs/en/headless.md>. The schema may
//! drift across Claude Code releases; the parser tolerates unknown
//! fields but asserts on the required ones (`result`, `usage`,
//! `is_error`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Deserialize;
use thiserror::Error;
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
            extra_args: Vec::new(),
        }
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
        args.push("json".into());
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

        let output = cmd
            .output()
            .await
            .map_err(|e| ClaudeCodeError::Spawn(format!("{}: {e}", self.claude_bin.display())))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr_text = String::from_utf8_lossy(&output.stderr).into_owned();

        // `claude -p` may exit non-zero with a structured JSON body
        // describing the failure (e.g. budget-exceeded, is_error=true).
        // Try to parse first; only surface a NonZeroExit if parse fails.
        let parsed = parse_output(&stdout);
        match (parsed, output.status.success()) {
            (Ok(run), _) => Ok(ClaudeCodeRun {
                stderr: stderr_text,
                exit_status: output.status.code().unwrap_or(-1),
                ..run
            }),
            (Err(e), true) => Err(e),
            (Err(_), false) => Err(ClaudeCodeError::NonZeroExit {
                code: output.status.code().unwrap_or(-1),
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

/// Parse the single-JSON-object output produced by `claude -p
/// --output-format json`. Tolerates unknown top-level fields (the
/// schema may add fields in future Claude Code releases).
fn parse_output(stdout: &str) -> Result<ClaudeCodeRun, ClaudeCodeError> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(ClaudeCodeError::ParseOutput("empty stdout".to_string()));
    }
    let raw: RawOutput = serde_json::from_str(trimmed).map_err(|e| {
        // Surface a useful error: the parse error + the first line of
        // stdout so we can see what shape we got.
        let preview: String = trimmed.lines().take(3).collect::<Vec<_>>().join(" / ");
        ClaudeCodeError::ParseOutput(format!("{e}; stdout preview: {preview}"))
    })?;
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
    fn parse_output_invalid_json_surfaces_preview() {
        let r = parse_output("not json {").unwrap_err();
        let msg = format!("{r}");
        assert!(msg.contains("preview"), "error includes stdout preview");
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
        assert_eq!(joined[i + 1], "json");
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
}
