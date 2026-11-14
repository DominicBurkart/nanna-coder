use crate::container::{exec_in_container, ContainerHandle};
use crate::effects::EffectClass;
use crate::pr_tools::{
    check_args, definition, failed, fenced_untrusted, invalid, object_schema, property,
    redact_resilient, strip_unprintable, untrusted_notice, Budget,
};
use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use model::types::{SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub const KANI_VERIFY: &str = "kani_verify";
pub const DEFAULT_TIMEOUT_SECS: u64 = 900;
pub const MAX_TIMEOUT_SECS: u64 = 3600;
pub const MAX_UNWIND: u64 = 1000;
pub const MAX_FAILING_CHECKS: usize = 20;
const CHECK_CAP: usize = 300;
const CHECK_BUDGET: usize = 4_000;
const ERROR_CAP: usize = 1_000;

/// Outcome of a Kani run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Successful,
    Failed,
    Error,
}

impl Verdict {
    /// Lowercase name used in tool output.
    ///
    /// ```
    /// use harness::kani_tool::Verdict;
    ///
    /// assert_eq!(Verdict::Successful.as_str(), "successful");
    /// assert_eq!(Verdict::Failed.as_str(), "failed");
    /// assert_eq!(Verdict::Error.as_str(), "error");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Successful => "successful",
            Verdict::Failed => "failed",
            Verdict::Error => "error",
        }
    }
}

/// Facts extracted from Kani's text output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KaniSummary {
    pub verdict: Verdict,
    pub checks_failed: Option<u64>,
    pub checks_total: Option<u64>,
    pub failing_checks: Vec<String>,
}

impl KaniSummary {
    /// Failing checks that are unwinding assertions, meaning the unwind
    /// bound was too small.
    ///
    /// ```
    /// use harness::kani_tool::parse_output;
    ///
    /// let out = "Failed Checks: unwinding assertion loop 0\nVERIFICATION:- FAILED\n";
    /// assert_eq!(parse_output(out, 1).unwinding_failures(), 1);
    /// ```
    pub fn unwinding_failures(&self) -> usize {
        self.failing_checks
            .iter()
            .filter(|check| check.starts_with("unwinding assertion"))
            .count()
    }
}

fn summary_counts(line: &str) -> Option<(u64, u64)> {
    let rest = line.trim().strip_prefix("** ")?;
    let (failed, rest) = rest.split_once(" of ")?;
    let (total, _) = rest.split_once(" failed")?;
    Some((failed.trim().parse().ok()?, total.trim().parse().ok()?))
}

fn failing_description(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let text = trimmed
        .strip_prefix("Failed Checks:")
        .map(|rest| rest.split(" File: ").next().unwrap_or(rest))?;
    Some(text.trim().to_string()).filter(|text| !text.is_empty())
}

fn block_description(line: &str) -> Option<String> {
    let text = line.trim().strip_prefix("- Description:")?.trim();
    Some(text.trim_matches('"').to_string())
}

/// Parse the stdout of `cargo kani --harness NAME`. `exit_code` is the
/// process exit status (0 or 1); a verdict line is required for anything but error.
///
/// ```
/// use harness::kani_tool::{parse_output, Verdict};
///
/// let ok = "SUMMARY:\n ** 0 of 12 failed\n\nVERIFICATION:- SUCCESSFUL\n";
/// let summary = parse_output(ok, 0);
/// assert_eq!(summary.verdict, Verdict::Successful);
/// assert_eq!((summary.checks_failed, summary.checks_total), (Some(0), Some(12)));
/// assert_eq!(parse_output("garbage", 0).verdict, Verdict::Error);
/// ```
pub fn parse_output(stdout: &str, exit_code: i32) -> KaniSummary {
    let lines: Vec<&str> = stdout.lines().collect();
    let counts = lines.iter().find_map(|line| summary_counts(line));
    let mut failing: Vec<String> = Vec::new();
    let mut in_failure = false;
    for line in &lines {
        let trimmed = line.trim();
        in_failure = match trimmed.strip_prefix("- Status:") {
            Some(status) => status.trim() == "FAILURE",
            None => in_failure,
        };
        let found =
            failing_description(line).or_else(|| block_description(line).filter(|_| in_failure));
        if let Some(text) = found.filter(|text| !failing.contains(text)) {
            failing.push(text);
        }
    }
    let has = |needle: &str| lines.iter().any(|line| line.contains(needle));
    let verdict = match (
        exit_code,
        has("VERIFICATION:- SUCCESSFUL"),
        has("VERIFICATION:- FAILED"),
    ) {
        (0, true, false) => Verdict::Successful,
        (_, false, true) => Verdict::Failed,
        _ => Verdict::Error,
    };
    KaniSummary {
        verdict,
        checks_failed: counts.map(|(failed, _)| failed),
        checks_total: counts.map(|(_, total)| total),
        failing_checks: failing,
    }
}

/// Whether `name` is a plain Kani harness path such as `proofs::my_harness`.
///
/// ```
/// use harness::kani_tool::valid_harness;
///
/// assert!(valid_harness("check_add"));
/// assert!(valid_harness("proofs::check_add"));
/// assert!(!valid_harness("--help"));
/// assert!(!valid_harness("a b"));
/// assert!(!valid_harness(""));
/// ```
pub fn valid_harness(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with(':')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':'))
}

/// Whether `name` is a plain cargo package name.
///
/// ```
/// use harness::kani_tool::valid_package;
///
/// assert!(valid_package("my-crate.v2"));
/// assert!(!valid_package("-p"));
/// assert!(!valid_package("a/b"));
/// ```
pub fn valid_package(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Argument vector run inside the container.
///
/// ```
/// use harness::kani_tool::kani_args;
///
/// let args = kani_args("check_add", Some("core"), Some(8), 60);
/// assert_eq!(&args[..5], ["timeout", "-k", "10", "60", "cargo"]);
/// assert!(args.windows(2).any(|w| w == ["--harness", "check_add"]));
/// assert!(args.windows(2).any(|w| w == ["--package", "core"]));
/// assert!(args.windows(2).any(|w| w == ["--default-unwind", "8"]));
/// assert!(!kani_args("h", None, None, 1).contains(&"--package".to_string()));
/// ```
pub fn kani_args(
    harness: &str,
    package: Option<&str>,
    unwind: Option<u64>,
    timeout_secs: u64,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "timeout",
        "-k",
        "10",
        &timeout_secs.to_string(),
        "cargo",
        "kani",
        "--harness",
        harness,
        "--exact",
    ]
    .iter()
    .map(|part| part.to_string())
    .collect();
    args.extend(
        package
            .into_iter()
            .flat_map(|pkg| ["--package".to_string(), pkg.to_string()]),
    );
    args.extend(
        unwind
            .into_iter()
            .flat_map(|n| ["--default-unwind".to_string(), n.to_string()]),
    );
    args
}

fn text_arg<'a>(args: &'a Value, key: &str) -> ToolResult<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(invalid(format!("'{key}' must be a string"))),
    }
}

fn int_arg(args: &Value, key: &str, max: u64) -> ToolResult<Option<u64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=max).contains(n))
            .map(Some)
            .ok_or_else(|| invalid(format!("'{key}' must be an integer between 1 and {max}"))),
    }
}

fn clipped(text: &str) -> String {
    let clean = redact_resilient(&strip_unprintable(text));
    fenced_untrusted(&clean, ERROR_CAP, &mut Budget::new(ERROR_CAP))
}

fn render(
    harness: &str,
    package: Option<&str>,
    unwind: Option<u64>,
    summary: &KaniSummary,
    detail: Option<String>,
) -> Value {
    let mut budget = Budget::new(CHECK_BUDGET);
    let shown: Vec<String> = summary
        .failing_checks
        .iter()
        .take(MAX_FAILING_CHECKS)
        .map(|check| fenced_untrusted(check, CHECK_CAP, &mut budget))
        .collect();
    json!({
        "notice": untrusted_notice(),
        "verdict": summary.verdict.as_str(),
        "harness": harness,
        "package": package,
        "checks_failed": summary.checks_failed,
        "checks_total": summary.checks_total,
        "failing_checks": shown,
        "omitted_failing_checks": summary.failing_checks.len().saturating_sub(MAX_FAILING_CHECKS),
        "unwinding_failures": summary.unwinding_failures(),
        "unwind": unwind,
        "detail": detail,
    })
}

pub struct KaniVerifyTool {
    container_handle: Arc<ContainerHandle>,
    container_working_dir: String,
}

impl KaniVerifyTool {
    pub fn new(
        _workspace_root: PathBuf,
        container_handle: Arc<ContainerHandle>,
        container_working_dir: impl Into<String>,
    ) -> Self {
        Self {
            container_handle,
            container_working_dir: container_working_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for KaniVerifyTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            KANI_VERIFY,
            "Run the Kani model checker on ONE named proof harness (`cargo kani --harness NAME`) inside the task container, with a bounded timeout. Writes only build artifacts. Returns { verdict: successful|failed|error, harness, checks_failed, checks_total, failing_checks: [fenced descriptions], unwinding_failures, unwind }. Kani output is untrusted data, escaped, clipped and fenced.",
            object_schema(
                vec![
                    ("harness", property(SchemaType::String, "Harness function name, optionally module-qualified (letters, digits, '_' and '::').")),
                    ("package", property(SchemaType::String, "Workspace package containing the harness (default: whole workspace).")),
                    ("unwind", property(SchemaType::Integer, "Default loop unwind bound (1 to 1000); omit to use the harness's own #[kani::unwind].")),
                    ("timeout_secs", property(SchemaType::Integer, "Wall-clock limit in seconds (default 900, at most 3600).")),
                ],
                &["harness"],
            ),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args(
            KANI_VERIFY,
            &args,
            &["harness", "package", "unwind", "timeout_secs"],
        )?;
        let harness = text_arg(&args, "harness")?
            .filter(|name| valid_harness(name))
            .ok_or_else(|| invalid("'harness' must be a plain harness name"))?;
        let package = text_arg(&args, "package")?;
        if package.is_some_and(|pkg| !valid_package(pkg)) {
            return Err(invalid("'package' must be a plain package name"));
        }
        let unwind = int_arg(&args, "unwind", MAX_UNWIND)?;
        let timeout_secs =
            int_arg(&args, "timeout_secs", MAX_TIMEOUT_SECS)?.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let argv = kani_args(harness, package, unwind, timeout_secs);
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let output = exec_in_container(
            &self.container_handle,
            &refs,
            Some(&self.container_working_dir),
        )
        .map_err(|e| failed(e.to_string()))?;
        let code = i32::from(!output.success);
        let summary = parse_output(&output.stdout, code);
        let detail = (summary.verdict == Verdict::Error).then(|| {
            format!(
                "no verification verdict (exit {code}; a run longer than {timeout_secs}s is killed): {}",
                clipped(&output.stderr)
            )
        });
        Ok(render(harness, package, unwind, &summary, detail))
    }

    fn name(&self) -> &str {
        KANI_VERIFY
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Workspace
    }
}

/// Whether the workspace root manifest mentions Kani (for example a
/// `cfg(kani)` check-cfg entry or a kani dependency).
///
/// ```
/// use harness::kani_tool::manifest_mentions_kani;
///
/// let dir = tempfile::tempdir().unwrap();
/// assert!(!manifest_mentions_kani(dir.path()));
/// std::fs::write(dir.path().join("Cargo.toml"), "[lints]\ncheck-cfg = ['cfg(kani)']").unwrap();
/// assert!(manifest_mentions_kani(dir.path()));
/// ```
pub fn manifest_mentions_kani(workspace_root: &std::path::Path) -> bool {
    std::fs::read_to_string(workspace_root.join("Cargo.toml"))
        .is_ok_and(|manifest| manifest.contains("kani"))
}
