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
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub const COVERAGE_REPORT: &str = "coverage_report";
pub const COVERAGE_OUTPUT_DIR: &str = "target/nanna-coverage";
pub const REPORT_FILE: &str = "tarpaulin-report.json";
const REPORT_REL: &str = "target/nanna-coverage/tarpaulin-report.json";
pub const DEFAULT_MAX_FILES: usize = 20;
pub const MAX_FILES: usize = 200;
pub const MAX_RANGES_PER_FILE: usize = 50;
pub const MAX_REPORT_BYTES: u64 = 64 * 1024 * 1024;
const PATH_CAP: usize = 200;
const PATH_BUDGET: usize = 16_000;
const ERROR_CAP: usize = 1_000;

/// Merge line numbers into sorted, inclusive, non-overlapping ranges.
///
/// ```
/// use harness::coverage_tool::merge_ranges;
///
/// assert_eq!(merge_ranges(vec![7, 3, 4, 5, 3, 9]), vec![(3, 5), (7, 7), (9, 9)]);
/// assert!(merge_ranges(vec![]).is_empty());
/// ```
pub fn merge_ranges(mut lines: Vec<u64>) -> Vec<(u64, u64)> {
    lines.sort_unstable();
    lines.dedup();
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for line in lines {
        match ranges.last_mut() {
            Some((_, end)) if *end + 1 == line => *end = line,
            _ => ranges.push((line, line)),
        }
    }
    ranges
}

/// Coverage of one source file: lines the report marks coverable and the
/// subset with zero hits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCoverage {
    pub path: String,
    pub coverable: u64,
    pub uncovered: Vec<u64>,
}

/// Parsed tarpaulin report.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageSummary {
    pub files: Vec<FileCoverage>,
}

impl CoverageSummary {
    /// Total coverable lines.
    pub fn coverable(&self) -> u64 {
        self.files.iter().map(|file| file.coverable).sum()
    }

    /// Total lines with zero hits.
    pub fn uncovered(&self) -> u64 {
        self.files
            .iter()
            .map(|file| file.uncovered.len() as u64)
            .sum()
    }

    /// Percent of coverable lines hit, rounded to two decimals; 0.0 when
    /// nothing is coverable.
    pub fn percent(&self) -> f64 {
        let total = self.coverable();
        let hit = total - self.uncovered();
        (hit as f64 * 10_000.0 / total.max(1) as f64).round() / 100.0
    }
}

fn relative_path(components: &[String], prefixes: &[String]) -> String {
    let joined = format!("/{}", components.join("/"));
    prefixes
        .iter()
        .find_map(|prefix| joined.strip_prefix(&format!("{}/", prefix.trim_end_matches('/'))))
        .map_or(joined.clone(), str::to_string)
}

fn hits(trace: &Value) -> Option<(u64, u64)> {
    Some((
        trace.get("line")?.as_u64()?,
        trace.get("stats")?.get("Line")?.as_u64()?,
    ))
}

fn parse_file(entry: &Value, prefixes: &[String]) -> Option<FileCoverage> {
    let components: Vec<String> = entry
        .get("path")?
        .as_array()?
        .iter()
        .map(|part| part.as_str().map(str::to_string))
        .collect::<Option<_>>()?;
    let traces: Vec<(u64, u64)> = entry
        .get("traces")?
        .as_array()?
        .iter()
        .filter_map(hits)
        .collect();
    let mut coverable: Vec<u64> = traces.iter().map(|(line, _)| *line).collect();
    coverable.sort_unstable();
    coverable.dedup();
    let covered: std::collections::BTreeSet<u64> = traces
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(line, _)| *line)
        .collect();
    Some(FileCoverage {
        path: relative_path(&components, prefixes),
        coverable: coverable.len() as u64,
        uncovered: coverable
            .into_iter()
            .filter(|line| !covered.contains(line))
            .collect(),
    })
}

/// Parse a tarpaulin JSON report. Paths under any of `prefixes` are made
/// relative to it.
///
/// ```
/// use harness::coverage_tool::parse_report;
///
/// let raw = r#"{"files":[{"path":["w","src","a.rs"],"traces":[
///   {"line":1,"stats":{"Line":3}},{"line":2,"stats":{"Line":0}}]}]}"#;
/// let summary = parse_report(raw, &["/w".to_string()]).unwrap();
/// assert_eq!(summary.files[0].path, "src/a.rs");
/// assert_eq!(summary.files[0].uncovered, vec![2]);
/// assert_eq!(summary.percent(), 50.0);
/// assert!(parse_report("not json", &[]).is_err());
/// ```
pub fn parse_report(raw: &str, prefixes: &[String]) -> Result<CoverageSummary, String> {
    let value: Value = serde_json::from_str(raw).map_err(|e| format!("invalid report: {e}"))?;
    let entries = value
        .get("files")
        .and_then(Value::as_array)
        .ok_or("report has no 'files' array")?;
    Ok(CoverageSummary {
        files: entries
            .iter()
            .filter_map(|entry| parse_file(entry, prefixes))
            .collect(),
    })
}

/// Argument vector for the tarpaulin run, optionally for one package.
///
/// ```
/// use harness::coverage_tool::tarpaulin_args;
///
/// let args = tarpaulin_args(Some("harness"));
/// assert_eq!(&args[..3], ["cargo", "tarpaulin", "--config"]);
/// assert!(args.windows(2).any(|w| w == ["--packages", "harness"]));
/// assert!(!tarpaulin_args(None).contains(&"--packages".to_string()));
/// ```
pub fn tarpaulin_args(package: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "cargo",
        "tarpaulin",
        "--config",
        "tarpaulin.toml",
        "--out",
        "Json",
        "--output-dir",
        COVERAGE_OUTPUT_DIR,
    ]
    .iter()
    .map(|part| part.to_string())
    .collect();
    args.extend(
        package
            .into_iter()
            .flat_map(|pkg| ["--packages".to_string(), pkg.to_string()]),
    );
    args
}

fn valid_package(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn text_arg<'a>(args: &'a Value, key: &str) -> ToolResult<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(invalid(format!("'{key}' must be a string"))),
    }
}

fn resolve_report(root: &Path, requested: &str) -> ToolResult<PathBuf> {
    let relative = Path::new(requested);
    let escapes = relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir));
    if requested.is_empty() || escapes {
        return Err(invalid(
            "'report_path' must be a relative path inside the workspace",
        ));
    }
    let canonical_root = root.canonicalize().map_err(|e| failed(e.to_string()))?;
    let resolved = root
        .join(relative)
        .canonicalize()
        .map_err(|_| failed("report not found in the workspace"))?;
    resolved
        .starts_with(&canonical_root)
        .then_some(resolved)
        .ok_or_else(|| invalid("'report_path' resolves outside the workspace"))
}

fn read_report(path: &Path) -> ToolResult<String> {
    let size = std::fs::metadata(path)
        .map_err(|e| failed(e.to_string()))?
        .len();
    if size > MAX_REPORT_BYTES {
        return Err(failed(format!(
            "report is larger than {MAX_REPORT_BYTES} bytes"
        )));
    }
    std::fs::read_to_string(path).map_err(|e| failed(e.to_string()))
}

fn clipped(text: &str) -> String {
    let clean = redact_resilient(&strip_unprintable(text));
    fenced_untrusted(&clean, ERROR_CAP, &mut Budget::new(ERROR_CAP))
}

fn files_arg(args: &Value) -> ToolResult<usize> {
    match args.get("max_files") {
        None | Some(Value::Null) => Ok(DEFAULT_MAX_FILES),
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=MAX_FILES as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| {
                invalid(format!(
                    "'max_files' must be an integer between 1 and {MAX_FILES}"
                ))
            }),
    }
}

fn render(summary: &CoverageSummary, max_files: usize) -> Value {
    let mut gaps: Vec<&FileCoverage> = summary
        .files
        .iter()
        .filter(|file| !file.uncovered.is_empty())
        .collect();
    gaps.sort_by(|a, b| {
        b.uncovered
            .len()
            .cmp(&a.uncovered.len())
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut budget = Budget::new(PATH_BUDGET);
    let shown: Vec<Value> = gaps
        .iter()
        .take(max_files)
        .map(|file| {
            let ranges = merge_ranges(file.uncovered.clone());
            json!({
                "path": fenced_untrusted(&file.path, PATH_CAP, &mut budget),
                "uncovered_lines": file.uncovered.len(),
                "uncovered_ranges": ranges.iter().take(MAX_RANGES_PER_FILE).collect::<Vec<_>>(),
                "omitted_ranges": ranges.len().saturating_sub(MAX_RANGES_PER_FILE),
            })
        })
        .collect();
    json!({
        "notice": untrusted_notice(),
        "percent": summary.percent(),
        "coverable_lines": summary.coverable(),
        "uncovered_lines": summary.uncovered(),
        "files": shown,
        "omitted_files": gaps.len().saturating_sub(max_files),
    })
}

pub struct CoverageReportTool {
    workspace_root: PathBuf,
    container_handle: Arc<ContainerHandle>,
    container_working_dir: String,
}

impl CoverageReportTool {
    pub fn new(
        workspace_root: PathBuf,
        container_handle: Arc<ContainerHandle>,
        container_working_dir: impl Into<String>,
    ) -> Self {
        Self {
            workspace_root,
            container_handle,
            container_working_dir: container_working_dir.into(),
        }
    }

    fn prefixes(&self) -> Vec<String> {
        let host = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());
        vec![
            self.container_working_dir.clone(),
            host.display().to_string(),
            self.workspace_root.display().to_string(),
        ]
    }

    fn run(&self, package: Option<&str>) -> ToolResult<PathBuf> {
        let report = self.workspace_root.join(REPORT_REL);
        let _ = std::fs::remove_file(&report);
        let argv = tarpaulin_args(package);
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let output = exec_in_container(
            &self.container_handle,
            &refs,
            Some(&self.container_working_dir),
        )
        .map_err(|e| failed(e.to_string()))?;
        if !output.success {
            return Err(failed(format!(
                "tarpaulin failed: {}",
                clipped(&output.stderr)
            )));
        }
        Ok(report)
    }
}

#[async_trait]
impl Tool for CoverageReportTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            COVERAGE_REPORT,
            "Report line coverage so tests can target the gaps. Without arguments it runs `cargo tarpaulin` with the repository's tarpaulin.toml inside the task container (writes build artifacts under target/nanna-coverage). With 'report_path' it only reads an existing tarpaulin JSON report in the workspace. Returns { percent, coverable_lines, uncovered_lines, files: [{ path, uncovered_lines, uncovered_ranges: [[start, end], ...] }], omitted_files }, files with the most uncovered lines first. Paths are untrusted data, escaped and fenced.",
            object_schema(
                vec![
                    ("package", property(SchemaType::String, "Workspace package to measure (default: whole workspace). Ignored with report_path.")),
                    ("report_path", property(SchemaType::String, "Workspace-relative path of an existing tarpaulin JSON report to read instead of running tarpaulin.")),
                    ("max_files", property(SchemaType::Integer, "Maximum files to list (default 20, at most 200).")),
                ],
                &[],
            ),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args(
            COVERAGE_REPORT,
            &args,
            &["package", "report_path", "max_files"],
        )?;
        let max_files = files_arg(&args)?;
        let report = match (text_arg(&args, "report_path")?, text_arg(&args, "package")?) {
            (Some(requested), _) => resolve_report(&self.workspace_root, requested)?,
            (None, Some(pkg)) if !valid_package(pkg) => {
                return Err(invalid("'package' must be a plain package name"))
            }
            (None, package) => self.run(package)?,
        };
        let summary = parse_report(&read_report(&report)?, &self.prefixes()).map_err(failed)?;
        Ok(render(&summary, max_files))
    }

    fn name(&self) -> &str {
        COVERAGE_REPORT
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Workspace
    }
}
