use crate::container::{exec_in_container, ContainerHandle};
use crate::effects::EffectClass;
use crate::pr_tools::{
    check_args, definition, failed, fenced_untrusted, invalid, object_schema, property,
    redact_resilient, strip_unprintable, untrusted_notice, Budget,
};
use crate::tools::{member_working_dir, Tool, ToolResult};
use async_trait::async_trait;
use model::types::{SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub const LEAN_CHECK: &str = "lean_check";
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;
pub const MAX_TIMEOUT_SECS: u64 = 3600;
pub const MAX_ERRORS: usize = 20;
pub const MAX_OFFENCES: usize = 20;
pub const MAX_SCAN_FILES: usize = 5000;
pub const MAX_SCAN_BYTES: u64 = 1024 * 1024;
pub const PROOFS_DIR: &str = "tests/proofs/lean";
pub const PROJECT_MARKERS: [&str; 3] = ["lakefile.lean", "lakefile.toml", "lean-toolchain"];
const SKIPPED_DIRS: [&str; 4] = ["lake-packages", "build", "target", "node_modules"];
const ERROR_CAP: usize = 400;
const ERROR_BUDGET: usize = 6_000;
const OFFENCE_CAP: usize = 200;
const OFFENCE_BUDGET: usize = 6_000;
const DETAIL_CAP: usize = 1_000;

/// Outcome of a Lean check.
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
    /// use harness::lean_tool::Verdict;
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

/// A construct a proof must not use to count as checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forbidden {
    Sorry,
    Admit,
    NativeDecide,
    Axiom,
}

impl Forbidden {
    /// Name of the offending construct.
    ///
    /// ```
    /// use harness::lean_tool::Forbidden;
    ///
    /// assert_eq!(Forbidden::Sorry.as_str(), "sorry");
    /// assert_eq!(Forbidden::Admit.as_str(), "admit");
    /// assert_eq!(Forbidden::NativeDecide.as_str(), "native_decide");
    /// assert_eq!(Forbidden::Axiom.as_str(), "axiom");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Forbidden::Sorry => "sorry",
            Forbidden::Admit => "admit",
            Forbidden::NativeDecide => "native_decide",
            Forbidden::Axiom => "axiom",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "sorry" | "sorryAx" => Some(Forbidden::Sorry),
            "admit" => Some(Forbidden::Admit),
            "native_decide" => Some(Forbidden::NativeDecide),
            "axiom" => Some(Forbidden::Axiom),
            _ => None,
        }
    }
}

/// One use of a forbidden construct in source code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offence {
    pub line: usize,
    pub kind: Forbidden,
    pub text: String,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '\'' | '!' | '?')
}

fn skip_block_comment(chars: &[char], mut at: usize, line: &mut usize) -> usize {
    let mut depth = 1usize;
    at += 2;
    while depth > 0 && at < chars.len() {
        match (chars[at], chars.get(at + 1)) {
            ('/', Some('-')) => {
                depth += 1;
                at += 2;
            }
            ('-', Some('/')) => {
                depth -= 1;
                at += 2;
            }
            (c, _) => {
                *line += usize::from(c == '\n');
                at += 1;
            }
        }
    }
    at
}

fn skip_string(chars: &[char], mut at: usize, line: &mut usize) -> usize {
    at += 1;
    while at < chars.len() && chars[at] != '"' {
        let step = 1 + usize::from(chars[at] == '\\' && at + 1 < chars.len());
        *line += chars[at..at + step].iter().filter(|c| **c == '\n').count();
        at += step;
    }
    at + 1
}

fn skip_raw_string(chars: &[char], mut at: usize, line: &mut usize) -> Option<usize> {
    let hashes = chars[at..].iter().take_while(|c| **c == '#').count();
    at += hashes;
    (chars.get(at) == Some(&'"')).then(|| {
        at += 1;
        while at < chars.len() {
            let closes = chars[at] == '"'
                && chars[at + 1..].iter().take_while(|c| **c == '#').count() >= hashes;
            if closes {
                return at + 1 + hashes;
            }
            *line += usize::from(chars[at] == '\n');
            at += 1;
        }
        at
    })
}

fn skip_char_literal(chars: &[char], at: usize) -> usize {
    match (chars.get(at + 1), chars.get(at + 2), chars.get(at + 3)) {
        (Some('\\'), Some(_), Some('\'')) => at + 4,
        (Some(c), Some('\''), _) if *c != '\\' => at + 3,
        _ => at + 1,
    }
}

/// Find every use of `sorry`, `admit`, `native_decide` or `axiom` in Lean
/// source, ignoring line comments, nested block comments, string literals
/// and longer identifiers that merely contain the word.
///
/// ```
/// use harness::lean_tool::{scan_source, Forbidden};
///
/// let hits = scan_source("-- sorry\ntheorem t : True := by\n  sorry\n");
/// assert_eq!(hits.len(), 1);
/// assert_eq!((hits[0].line, hits[0].kind), (3, Forbidden::Sorry));
/// assert!(scan_source("/- axiom /- nested -/ sorry -/\ndef sorryish := 1").is_empty());
/// ```
pub fn scan_source(source: &str) -> Vec<Offence> {
    let chars: Vec<char> = source.chars().collect();
    let lines: Vec<&str> = source.lines().collect();
    let mut hits = Vec::new();
    let mut line = 1usize;
    let mut at = 0usize;
    while at < chars.len() {
        let here = chars[at];
        let next = chars.get(at + 1).copied();
        match (here, next) {
            ('\n', _) => {
                line += 1;
                at += 1;
            }
            ('-', Some('-')) => {
                while at < chars.len() && chars[at] != '\n' {
                    at += 1;
                }
            }
            ('/', Some('-')) => at = skip_block_comment(&chars, at, &mut line),
            ('"', _) => at = skip_string(&chars, at, &mut line),
            ('\'', _) => at = skip_char_literal(&chars, at),
            (c, _) if is_ident(c) => {
                let start = at;
                while at < chars.len() && is_ident(chars[at]) {
                    at += 1;
                }
                let token: String = chars[start..at].iter().collect();
                if token == "r" && matches!(chars.get(at), Some('"' | '#')) {
                    at = skip_raw_string(&chars, at, &mut line).unwrap_or(at);
                } else if let Some(kind) = Forbidden::from_token(&token) {
                    let text = lines.get(line - 1).map_or("", |l| l.trim()).to_string();
                    hits.push(Offence { line, kind, text });
                }
            }
            _ => at += 1,
        }
    }
    hits
}

/// One error reported by Lean or Lake. Errors without a source position have
/// no file and line 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeanError {
    pub file: Option<String>,
    pub line: u64,
    pub message: String,
}

fn position(head: &str) -> Option<(String, u64)> {
    let mut parts = head.rsplitn(3, ':');
    parts.next()?.trim().parse::<u64>().ok()?;
    let line = parts.next()?.trim().parse::<u64>().ok()?;
    Some((parts.next()?.trim().to_string(), line))
}

fn error_line(line: &str) -> Option<LeanError> {
    let trimmed = line.trim();
    let (head, message) = match trimmed.split_once(": error: ") {
        Some((head, message)) => (Some(head), message),
        None => {
            let rest = trimmed.strip_prefix("error: ")?;
            match rest.split_once(": ") {
                Some((head, message)) if position(head).is_some() => (Some(head), message),
                _ => (None, rest),
            }
        }
    };
    let (file, line) = head
        .and_then(position)
        .map_or((None, 0), |(file, line)| (Some(file), line));
    Some(LeanError {
        file,
        line,
        message: message.trim().to_string(),
    })
}

fn is_summary(error: &LeanError) -> bool {
    error.file.is_none()
        && (error.message.starts_with("build failed")
            || error.message.starts_with("Lean exited with code"))
}

/// Errors in Lean or Lake output. Handles `file:line:col: error: msg` and
/// Lake's `error: file:line:col: msg`, and drops Lake's closing summary lines.
///
/// ```
/// use harness::lean_tool::parse_errors;
///
/// let out = "A.lean:3:4: error: unknown identifier 'x'\nerror: build failed\nB.lean:1:1: warning: unused\n";
/// let errors = parse_errors(out);
/// assert_eq!(errors.len(), 1);
/// assert_eq!((errors[0].file.as_deref(), errors[0].line), (Some("A.lean"), 3));
/// ```
pub fn parse_errors(output: &str) -> Vec<LeanError> {
    output
        .lines()
        .filter_map(error_line)
        .filter(|error| !is_summary(error))
        .collect()
}

/// Whether Lean itself reported a `sorry` in the output.
///
/// ```
/// use harness::lean_tool::output_uses_sorry;
///
/// assert!(output_uses_sorry("A.lean:1:8: warning: declaration uses 'sorry'"));
/// assert!(!output_uses_sorry("all good"));
/// ```
pub fn output_uses_sorry(output: &str) -> bool {
    output.contains("declaration uses 'sorry'") || output.contains("declaration uses `sorry`")
}

/// Combine the exit status, parsed errors and anti-cheat hits.
///
/// ```
/// use harness::lean_tool::{decide_verdict, Verdict};
///
/// assert_eq!(decide_verdict(true, 0, 0), Verdict::Successful);
/// assert_eq!(decide_verdict(true, 0, 1), Verdict::Failed);
/// assert_eq!(decide_verdict(true, 2, 0), Verdict::Failed);
/// assert_eq!(decide_verdict(false, 1, 0), Verdict::Failed);
/// assert_eq!(decide_verdict(false, 0, 0), Verdict::Error);
/// ```
pub fn decide_verdict(exit_ok: bool, errors: usize, offences: usize) -> Verdict {
    match (exit_ok, errors, offences) {
        (true, 0, 0) => Verdict::Successful,
        (false, 0, _) if offences == 0 => Verdict::Error,
        _ => Verdict::Failed,
    }
}

/// Whether `target` is a plain Lake build target such as `MyLib`, `Foo.Bar`
/// or `pkg/Foo:exe`.
///
/// ```
/// use harness::lean_tool::valid_target;
///
/// assert!(valid_target("Foo.Bar"));
/// assert!(valid_target("+Foo"));
/// assert!(!valid_target("--help"));
/// assert!(!valid_target("a b"));
/// assert!(!valid_target("../x"));
/// assert!(!valid_target(""));
/// ```
pub fn valid_target(target: &str) -> bool {
    !target.is_empty()
        && target.len() <= 200
        && !target.contains("..")
        && target.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'/' | b'@' | b'+')
        })
}

/// Workspace-relative directory of the Lean project: the root when it holds a
/// `lakefile.lean`, `lakefile.toml` or `lean-toolchain`, else
/// `tests/proofs/lean` when that does.
///
/// ```
/// use harness::lean_tool::lean_project_dir;
/// use std::path::PathBuf;
///
/// let dir = tempfile::tempdir().unwrap();
/// assert_eq!(lean_project_dir(dir.path()), None);
/// std::fs::create_dir_all(dir.path().join("tests/proofs/lean")).unwrap();
/// std::fs::write(dir.path().join("tests/proofs/lean/lakefile.toml"), "").unwrap();
/// assert_eq!(lean_project_dir(dir.path()), Some(PathBuf::from("tests/proofs/lean")));
/// std::fs::write(dir.path().join("lean-toolchain"), "").unwrap();
/// assert_eq!(lean_project_dir(dir.path()), Some(PathBuf::new()));
/// ```
pub fn lean_project_dir(workspace_root: &Path) -> Option<PathBuf> {
    let has_marker = |dir: &Path| PROJECT_MARKERS.iter().any(|m| dir.join(m).is_file());
    [PathBuf::new(), PathBuf::from(PROOFS_DIR)]
        .into_iter()
        .find(|rel| has_marker(&workspace_root.join(rel)))
}

/// Argument vector run inside the container: `lake build [TARGET]`,
/// `lean FILE`, or `lake env lean FILE` when the project has a Lake file.
///
/// ```
/// use harness::lean_tool::{lean_args, Job};
///
/// let build = lean_args(&Job::Build(Some("Foo".into())), false, 60);
/// assert_eq!(build, ["timeout", "-k", "10", "60", "lake", "build", "Foo"]);
/// let file = lean_args(&Job::File("./A.lean".into()), true, 5);
/// assert_eq!(&file[4..], ["lake", "env", "lean", "./A.lean"]);
/// assert_eq!(&lean_args(&Job::File("./A.lean".into()), false, 5)[4..], ["lean", "./A.lean"]);
/// ```
pub fn lean_args(job: &Job, has_lake: bool, timeout_secs: u64) -> Vec<String> {
    let mut args: Vec<String> = ["timeout", "-k", "10", &timeout_secs.to_string()]
        .iter()
        .map(|part| part.to_string())
        .collect();
    match job {
        Job::Build(target) => {
            args.extend(["lake".to_string(), "build".to_string()]);
            args.extend(target.iter().cloned());
        }
        Job::File(file) => {
            if has_lake {
                args.extend(["lake".to_string(), "env".to_string()]);
            }
            args.extend(["lean".to_string(), file.clone()]);
        }
    }
    args
}

/// What to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Build(Option<String>),
    File(String),
}

fn text_arg<'a>(args: &'a Value, key: &str) -> ToolResult<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(invalid(format!("'{key}' must be a string"))),
    }
}

fn timeout_arg(args: &Value) -> ToolResult<u64> {
    match args.get("timeout_secs") {
        None | Some(Value::Null) => Ok(DEFAULT_TIMEOUT_SECS),
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=MAX_TIMEOUT_SECS).contains(n))
            .ok_or_else(|| {
                invalid(format!(
                    "'timeout_secs' must be an integer between 1 and {MAX_TIMEOUT_SECS}"
                ))
            }),
    }
}

fn clipped(text: &str, cap: usize) -> String {
    let clean = redact_resilient(&strip_unprintable(text));
    fenced_untrusted(&clean, cap, &mut Budget::new(cap))
}

fn resolve_file(root: &Path, project: &Path, requested: &str) -> ToolResult<(PathBuf, String)> {
    let relative = Path::new(requested);
    let escapes = relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir));
    if requested.is_empty() || escapes {
        return Err(invalid(
            "'file' must be a relative path inside the workspace",
        ));
    }
    if relative.extension().is_none_or(|ext| ext != "lean") {
        return Err(invalid("'file' must be a .lean file"));
    }
    let canonical_root = root.canonicalize().map_err(|e| failed(e.to_string()))?;
    let resolved = root
        .join(relative)
        .canonicalize()
        .map_err(|_| failed("file not found in the workspace"))?;
    if !resolved.starts_with(&canonical_root) {
        return Err(invalid("'file' resolves outside the workspace"));
    }
    let inside = resolved
        .strip_prefix(canonical_root.join(project))
        .map_err(|_| invalid("'file' is outside the Lean project"))?;
    let shown = inside
        .to_str()
        .filter(|_| resolved.is_file())
        .ok_or_else(|| invalid("'file' must be a regular file with a UTF-8 name"))?;
    Ok((resolved.clone(), format!("./{shown}")))
}

fn read_lean(path: &Path) -> ToolResult<String> {
    let size = std::fs::metadata(path)
        .map_err(|e| failed(e.to_string()))?
        .len();
    if size > MAX_SCAN_BYTES {
        return Err(failed(format!(
            "{} is larger than {MAX_SCAN_BYTES} bytes and cannot be scanned",
            path.display()
        )));
    }
    let bytes = std::fs::read(path).map_err(|e| failed(e.to_string()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn collect_lean_files(dir: &Path, found: &mut Vec<PathBuf>) -> ToolResult<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| failed(e.to_string()))? {
        let entry = entry.map_err(|e| failed(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type().map_err(|e| failed(e.to_string()))?;
        let skipped = name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_str());
        if kind.is_dir() && !skipped {
            collect_lean_files(&entry.path(), found)?;
        } else if kind.is_file() && name.ends_with(".lean") {
            found.push(entry.path());
        }
        if found.len() > MAX_SCAN_FILES {
            return Err(failed(format!("more than {MAX_SCAN_FILES} .lean files")));
        }
    }
    Ok(())
}

/// An offence together with the workspace-relative file it was found in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOffence {
    pub file: String,
    pub offence: Offence,
}

/// Scan the given files (or every `.lean` file under `project` when `only` is
/// `None`) relative to `root`. Symlinks and hidden or build directories are
/// not followed.
pub fn scan_files(
    root: &Path,
    project: &Path,
    only: Option<&Path>,
) -> ToolResult<Vec<FileOffence>> {
    let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut files = Vec::new();
    match only {
        Some(file) => files.push(file.to_path_buf()),
        None => collect_lean_files(&base.join(project), &mut files)?,
    }
    files.sort();
    let mut hits = Vec::new();
    for file in files {
        let shown = file
            .strip_prefix(&base)
            .unwrap_or(&file)
            .display()
            .to_string();
        for offence in scan_source(&read_lean(&file)?) {
            hits.push(FileOffence {
                file: shown.clone(),
                offence,
            });
        }
    }
    Ok(hits)
}

struct Report<'a> {
    job: &'a Job,
    verdict: Verdict,
    errors: Vec<LeanError>,
    offences: Vec<FileOffence>,
    output_sorry: bool,
    detail: Option<String>,
}

fn render(report: &Report) -> Value {
    let mut budget = Budget::new(ERROR_BUDGET);
    let errors: Vec<Value> = report
        .errors
        .iter()
        .take(MAX_ERRORS)
        .map(|error| {
            json!({
                "file": error.file.as_deref().map(|f| fenced_untrusted(f, OFFENCE_CAP, &mut budget)),
                "line": error.line,
                "message": fenced_untrusted(&error.message, ERROR_CAP, &mut budget),
            })
        })
        .collect();
    let mut hit_budget = Budget::new(OFFENCE_BUDGET);
    let offending: Vec<Value> = report
        .offences
        .iter()
        .take(MAX_OFFENCES)
        .map(|hit| {
            json!({
                "file": fenced_untrusted(&hit.file, OFFENCE_CAP, &mut hit_budget),
                "line": hit.offence.line,
                "kind": hit.offence.kind.as_str(),
                "text": fenced_untrusted(&hit.offence.text, OFFENCE_CAP, &mut hit_budget),
            })
        })
        .collect();
    let sorries = report
        .offences
        .iter()
        .filter(|hit| hit.offence.kind == Forbidden::Sorry)
        .count();
    let (target, file) = match report.job {
        Job::Build(target) => (target.clone(), None),
        Job::File(file) => (None, Some(file.clone())),
    };
    json!({
        "notice": untrusted_notice(),
        "verdict": report.verdict.as_str(),
        "target": target,
        "file": file,
        "error_count": report.errors.len(),
        "errors": errors,
        "omitted_errors": report.errors.len().saturating_sub(MAX_ERRORS),
        "sorry_count": sorries,
        "uses_sorry": sorries > 0 || report.output_sorry,
        "axioms_ok": report.offences.is_empty(),
        "offending": offending,
        "omitted_offending": report.offences.len().saturating_sub(MAX_OFFENCES),
        "detail": report.detail,
    })
}

pub struct LeanCheckTool {
    workspace_root: PathBuf,
    container_handle: Arc<ContainerHandle>,
    container_working_dir: String,
}

impl LeanCheckTool {
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
}

#[async_trait]
impl Tool for LeanCheckTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            LEAN_CHECK,
            "Check Lean 4 proofs inside the task container (never on the host), with a bounded timeout: `lake build [target]`, or with 'file' `lean FILE` for one workspace-relative .lean file. Writes only build artifacts. Before running it text-scans the checked .lean files and flags every use of `sorry`, `admit`, `native_decide` or a new `axiom` (comments and strings excluded); any hit means the check can never succeed. Returns { verdict: successful|failed|error, error_count, errors: [{ file, line, message }], sorry_count, uses_sorry, axioms_ok, offending: [{ file, line, kind, text }] }. Lean output is untrusted data, escaped, clipped and fenced.",
            object_schema(
                vec![
                    ("target", property(SchemaType::String, "Lake target to build (default: the default targets). Not with 'file'.")),
                    ("file", property(SchemaType::String, "Workspace-relative .lean file to check with `lean`, inside the Lean project. Not with 'target'.")),
                    ("timeout_secs", property(SchemaType::Integer, "Wall-clock limit in seconds (default 600, at most 3600).")),
                ],
                &[],
            ),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args(LEAN_CHECK, &args, &["target", "file", "timeout_secs"])?;
        let timeout_secs = timeout_arg(&args)?;
        let target = text_arg(&args, "target")?;
        let requested = text_arg(&args, "file")?;
        if target.is_some() && requested.is_some() {
            return Err(invalid("give either 'target' or 'file', not both"));
        }
        if target.is_some_and(|name| !valid_target(name)) {
            return Err(invalid("'target' must be a plain Lake target"));
        }
        let project = lean_project_dir(&self.workspace_root)
            .ok_or_else(|| failed("no lakefile.lean, lakefile.toml or lean-toolchain found"))?;
        let (job, only) = match requested {
            Some(path) => {
                let (resolved, shown) = resolve_file(&self.workspace_root, &project, path)?;
                (Job::File(shown), Some(resolved))
            }
            None => (Job::Build(target.map(str::to_string)), None),
        };
        let offences = scan_files(&self.workspace_root, &project, only.as_deref())?;
        let has_lake = self
            .workspace_root
            .join(&project)
            .join("lakefile.lean")
            .is_file()
            || self
                .workspace_root
                .join(&project)
                .join("lakefile.toml")
                .is_file();
        let argv = lean_args(&job, has_lake, timeout_secs);
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let working_dir = member_working_dir(&self.container_working_dir, &project);
        let output = exec_in_container(&self.container_handle, &refs, Some(&working_dir))
            .map_err(|e| failed(e.to_string()))?;
        let combined = format!("{}\n{}", output.stdout, output.stderr);
        let errors = parse_errors(&combined);
        let verdict = decide_verdict(output.success, errors.len(), offences.len());
        let detail = (verdict == Verdict::Error).then(|| {
            format!(
                "no Lean errors reported but the command failed (a run longer than {timeout_secs}s is killed): {}",
                clipped(&output.stderr, DETAIL_CAP)
            )
        });
        Ok(render(&Report {
            job: &job,
            verdict,
            errors,
            offences,
            output_sorry: output_uses_sorry(&combined),
            detail,
        }))
    }

    fn name(&self) -> &str {
        LEAN_CHECK
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Workspace
    }
}
