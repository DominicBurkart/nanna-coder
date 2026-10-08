//! Endpoint manifest QA: a declarative list of paths to request on a running
//! application, the checker that requests them and the report it produces.
//!
//! # Manifest grammar
//!
//! A manifest is a text file (the repository's `CHECKS`) with one check per
//! line. Blank lines and lines whose first non-blank character is `#` are
//! ignored. Every other line is:
//!
//! ```text
//! check    := path ( blank+ option )*
//! path     := "/" [^ blank ]*
//! option   := "expect=" status | "contains=" text
//! status   := three-digit HTTP status, 100..=599
//! text     := everything up to the end of the line
//! ```
//!
//! A plain path expects status `200`. `expect=` sets the expected status,
//! `contains=` requires the response body to contain `text`; because `text`
//! runs to the end of the line, `contains=` must be the last option. Each
//! option may appear at most once.
//!
//! ```
//! use harness::qa::manifest::{Check, Manifest};
//!
//! let manifest = Manifest::parse(
//!     "# smoke checks\n/\n/health/v1 contains={\"status\":\"ok\"}\n/missing expect=404\n",
//! )
//! .unwrap();
//! assert_eq!(manifest.checks.len(), 3);
//! assert_eq!(manifest.checks[0], Check::new("/"));
//! assert_eq!(manifest.checks[1].contains.as_deref(), Some("{\"status\":\"ok\"}"));
//! assert_eq!(manifest.checks[2].expect, 404);
//!
//! let err = Manifest::parse("health/v1\n").unwrap_err();
//! assert_eq!(err.to_string(), "line 1: path must start with '/', got `health/v1`");
//! ```
//!
//! When a repository has no manifest, [`Manifest::derived`] builds one from
//! what the profile knows: the health path, the index page and the static
//! asset roots.

use crate::apprun::exec_args;
use crate::container::ContainerHandle;
use crate::sidecar::CommandRunner;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

/// Status a check without `expect=` requires.
pub const DEFAULT_EXPECTED_STATUS: u16 = 200;
/// Characters of the response body kept in a [`CheckResult::snippet`].
pub const SNIPPET_CHARS: usize = 200;
/// Seconds a single container probe may take.
pub const DEFAULT_PROBE_TIMEOUT_SECS: u64 = 15;
/// Separator the container probe prints between the body and the status.
const STATUS_SEPARATOR: char = '\n';

/// One line of the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// Absolute path requested on the base URL.
    pub path: String,
    /// HTTP status the response must have.
    pub expect: u16,
    /// Substring the response body must contain.
    pub contains: Option<String>,
}

impl Check {
    /// A check of `path` expecting [`DEFAULT_EXPECTED_STATUS`] and any body.
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            expect: DEFAULT_EXPECTED_STATUS,
            contains: None,
        }
    }

    /// Parse one non-blank, non-comment manifest line.
    pub fn parse(line: &str) -> Result<Self, String> {
        let line = line.trim();
        let (path, rest) = match line.find(char::is_whitespace) {
            Some(i) => (&line[..i], line[i..].trim_start()),
            None => (line, ""),
        };
        if !path.starts_with('/') {
            return Err(format!("path must start with '/', got `{path}`"));
        }
        let mut check = Self::new(path);
        let mut expect_seen = false;
        let mut rest = rest;
        while !rest.is_empty() {
            if let Some(text) = rest.strip_prefix("contains=") {
                if text.is_empty() {
                    return Err("contains= needs a non-empty text".to_string());
                }
                check.contains = Some(text.to_string());
                return Ok(check);
            }
            let (option, tail) = match rest.find(char::is_whitespace) {
                Some(i) => (&rest[..i], rest[i..].trim_start()),
                None => (rest, ""),
            };
            let Some(status) = option.strip_prefix("expect=") else {
                return Err(format!("unknown option `{option}`"));
            };
            if expect_seen {
                return Err("expect= given twice".to_string());
            }
            check.expect = parse_status(status)?;
            expect_seen = true;
            rest = tail;
        }
        Ok(check)
    }
}

fn parse_status(raw: &str) -> Result<u16, String> {
    let status: u16 = raw
        .parse()
        .map_err(|_| format!("expect= needs an HTTP status, got `{raw}`"))?;
    if !(100..=599).contains(&status) {
        return Err(format!("expect= needs a status in 100..=599, got {status}"));
    }
    Ok(status)
}

/// Errors from reading or parsing a manifest.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("line {line}: {message}")]
    Line { line: usize, message: String },
    #[error("could not read manifest {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("manifest declares no checks")]
    Empty,
}

/// The checks to run, in manifest order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub checks: Vec<Check>,
}

impl Manifest {
    /// Parse manifest text; see the module documentation for the grammar.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let mut checks = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let check = Check::parse(line).map_err(|message| ManifestError::Line {
                line: index + 1,
                message,
            })?;
            checks.push(check);
        }
        if checks.is_empty() {
            return Err(ManifestError::Empty);
        }
        Ok(Self { checks })
    }

    /// Read and parse the manifest at `path`.
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text)
    }

    /// The manifest used when a repository has none: the health path, the
    /// index page and every static asset root, without duplicates.
    ///
    /// An asset root is a directory (`trunk_asset_roots` returns the
    /// directory of each absolute reference, not a file inside it), so a
    /// server that 404s directory listings will fail this check even when
    /// the assets themselves are fine; a repository that hits this should
    /// write a `CHECKS` manifest naming actual files instead.
    ///
    /// ```
    /// use harness::qa::manifest::Manifest;
    ///
    /// let derived = Manifest::derived("/health/v1", &["/assets/".to_string(), "/".to_string()]);
    /// let paths: Vec<&str> = derived.checks.iter().map(|c| c.path.as_str()).collect();
    /// assert_eq!(paths, ["/health/v1", "/", "/assets/"]);
    /// ```
    pub fn derived(health_path: &str, asset_roots: &[String]) -> Self {
        let mut checks: Vec<Check> = Vec::new();
        let candidates = [health_path, "/"]
            .into_iter()
            .chain(asset_roots.iter().map(String::as_str));
        for path in candidates {
            if !checks.iter().any(|c| c.path == path) {
                checks.push(Check::new(path));
            }
        }
        Self { checks }
    }

    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }
}

/// Status and body of one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResponse {
    pub status: u16,
    pub body: String,
}

/// Errors from issuing a request.
#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("could not spawn `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("request to {url} failed: {detail}")]
    Request { url: String, detail: String },
    #[error("no status code in the probe output for {url}: {output:?}")]
    Malformed { url: String, output: String },
}

/// Issues `GET` requests for the checker; the container implementation is
/// [`ContainerProbe`], tests inject a stub.
pub trait HttpProbe: Send + Sync {
    fn get(&self, url: &str) -> Result<ProbeResponse, ProbeError>;
}

/// [`HttpProbe`] that runs `curl` inside the dev container through a
/// [`CommandRunner`], so the checker reaches the per-task port the
/// application listens on there.
pub struct ContainerProbe {
    handle: Arc<ContainerHandle>,
    runner: Arc<dyn CommandRunner>,
    timeout_secs: u64,
}

impl ContainerProbe {
    pub fn new(handle: Arc<ContainerHandle>, runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            handle,
            runner,
            timeout_secs: DEFAULT_PROBE_TIMEOUT_SECS,
        }
    }

    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// `curl` printing the body followed by a newline and the status code.
    pub fn argv(url: &str, timeout_secs: u64) -> Vec<String> {
        vec![
            "curl".to_string(),
            "-sS".to_string(),
            "--max-time".to_string(),
            timeout_secs.to_string(),
            "-w".to_string(),
            format!("{STATUS_SEPARATOR}%{{http_code}}"),
            url.to_string(),
        ]
    }

    /// Split the output of [`ContainerProbe::argv`] into body and status.
    pub fn parse_output(url: &str, stdout: &str) -> Result<ProbeResponse, ProbeError> {
        let malformed = || ProbeError::Malformed {
            url: url.to_string(),
            output: stdout.to_string(),
        };
        let (body, status) = stdout.rsplit_once(STATUS_SEPARATOR).ok_or_else(malformed)?;
        let status: u16 = status.trim().parse().map_err(|_| malformed())?;
        if status == 0 {
            return Err(malformed());
        }
        Ok(ProbeResponse {
            status,
            body: body.to_string(),
        })
    }
}

impl HttpProbe for ContainerProbe {
    fn get(&self, url: &str) -> Result<ProbeResponse, ProbeError> {
        let argv = Self::argv(url, self.timeout_secs);
        let args = exec_args(&self.handle, &argv, None);
        let program = self.handle.runtime.command();
        let output = self
            .runner
            .run(program, &args)
            .map_err(|source| ProbeError::Spawn {
                command: format!("{program} {}", args.join(" ")),
                source,
            })?;
        if !output.success {
            return Err(ProbeError::Request {
                url: url.to_string(),
                detail: output.stderr.trim().to_string(),
            });
        }
        Self::parse_output(url, &output.stdout)
    }
}

/// Outcome of one check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub path: String,
    pub url: String,
    pub expected: u16,
    /// Status received, `None` when the request itself failed.
    pub status: Option<u16>,
    pub contains: Option<String>,
    pub passed: bool,
    /// Start of the response body, whitespace collapsed.
    pub snippet: String,
    /// Why the request failed, when it did.
    pub error: Option<String>,
}

/// Every check of a run against one base URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestReport {
    pub base_url: String,
    pub checks: Vec<CheckResult>,
    pub passed: usize,
    pub failed: usize,
}

impl ManifestReport {
    pub fn all_passed(&self) -> bool {
        self.failed == 0
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("ManifestReport serialises")
    }

    /// One line per check plus a totals line.
    pub fn render_text(&self) -> String {
        let mut out = format!("endpoint checks against {}\n", self.base_url);
        for check in &self.checks {
            let mark = if check.passed { "PASS" } else { "FAIL" };
            let status = check.status.map_or("-".to_string(), |s| s.to_string());
            let _ = write!(
                out,
                "{mark} {} expected {} got {status}",
                check.path, check.expected
            );
            if let Some(error) = &check.error {
                let _ = write!(out, " ({error})");
            } else if !check.passed {
                let _ = write!(out, ": {}", check.snippet);
            }
            out.push('\n');
        }
        let _ = write!(out, "{} passed, {} failed", self.passed, self.failed);
        out
    }
}

/// `base_url` and `path` joined with exactly one slash between them.
pub fn join_url(base_url: &str, path: &str) -> String {
    format!("{}{path}", base_url.trim_end_matches('/'))
}

/// The first [`SNIPPET_CHARS`] characters of `body` with whitespace runs
/// collapsed to single spaces.
pub fn snippet(body: &str) -> String {
    let collapsed: Vec<&str> = body.split_whitespace().collect();
    collapsed.join(" ").chars().take(SNIPPET_CHARS).collect()
}

/// Static asset roots referenced by the frontend's `index.html`: the
/// directory of every absolute `href=`/`src=` attribute (`link`, `script`,
/// `img`), deduplicated and sorted. Trunk-injected assets such as the wasm
/// bundle are relative and so never appear here; this only catches
/// hand-written absolute references to a static directory. Returns an
/// empty list when `frontend_dir` has no `index.html`.
///
/// ```
/// use harness::qa::manifest::trunk_asset_roots;
///
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(
///     dir.path().join("index.html"),
///     r#"<link rel="icon" href="/static/favicon.ico"><script src="/static/js/app.js"></script>"#,
/// )
/// .unwrap();
/// assert_eq!(trunk_asset_roots(dir.path()).unwrap(), vec!["/static/", "/static/js/"]
///     .into_iter().map(str::to_string).collect::<Vec<_>>());
/// ```
pub fn trunk_asset_roots(frontend_dir: &Path) -> std::io::Result<Vec<String>> {
    let path = frontend_dir.join("index.html");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let html = std::fs::read_to_string(&path)?;
    let mut roots: Vec<String> = asset_references(&html)
        .filter_map(|reference| asset_root(&reference))
        .collect();
    roots.sort();
    roots.dedup();
    Ok(roots)
}

fn asset_references(html: &str) -> impl Iterator<Item = String> + '_ {
    ["href=\"", "src=\""].into_iter().flat_map(move |marker| {
        html.match_indices(marker).filter_map(move |(index, _)| {
            let start = index + marker.len();
            let rest = &html[start..];
            let end = rest.find('"')?;
            Some(rest[..end].to_string())
        })
    })
}

fn asset_root(reference: &str) -> Option<String> {
    if !reference.starts_with('/') {
        return None;
    }
    let end = reference.rfind('/')? + 1;
    if end <= 1 {
        return None;
    }
    Some(reference[..end].to_string())
}

/// Judge `response` against `check`.
pub fn evaluate(
    check: &Check,
    url: &str,
    response: Result<ProbeResponse, ProbeError>,
) -> CheckResult {
    let (status, snippet, passed, error) = match response {
        Ok(response) => {
            let body_ok = check
                .contains
                .as_ref()
                .is_none_or(|needle| response.body.contains(needle));
            let passed = response.status == check.expect && body_ok;
            (Some(response.status), snippet(&response.body), passed, None)
        }
        Err(e) => (None, String::new(), false, Some(e.to_string())),
    };
    CheckResult {
        path: check.path.clone(),
        url: url.to_string(),
        expected: check.expect,
        status,
        contains: check.contains.clone(),
        passed,
        snippet,
        error,
    }
}

/// Runs every check of a manifest against a base URL through an
/// [`HttpProbe`]. The base URL may be the app running in the dev container
/// or any other origin the probe can reach.
pub struct ManifestChecker {
    probe: Arc<dyn HttpProbe>,
}

impl ManifestChecker {
    pub fn new(probe: Arc<dyn HttpProbe>) -> Self {
        Self { probe }
    }

    pub fn run(&self, base_url: &str, manifest: &Manifest) -> ManifestReport {
        let mut checks = Vec::with_capacity(manifest.checks.len());
        for check in &manifest.checks {
            let url = join_url(base_url, &check.path);
            let response = self.probe.get(&url);
            checks.push(evaluate(check, &url, response));
        }
        let passed = checks.iter().filter(|c| c.passed).count();
        ManifestReport {
            base_url: base_url.to_string(),
            failed: checks.len() - passed,
            checks,
            passed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::ContainerRuntime;
    use crate::sidecar::RunOutput;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[test]
    fn plain_line_expects_200_and_any_body() {
        assert_eq!(
            Check::parse("/health/v1").unwrap(),
            Check::new("/health/v1")
        );
        assert_eq!(Check::parse("  /  ").unwrap(), Check::new("/"));
    }

    #[test]
    fn expect_and_contains_options_are_parsed() {
        let check = Check::parse("/missing expect=404").unwrap();
        assert_eq!(check.expect, 404);
        assert_eq!(check.contains, None);
        let check = Check::parse("/api/v1/greeting contains=Hello from the fixture").unwrap();
        assert_eq!(check.expect, 200);
        assert_eq!(check.contains.as_deref(), Some("Hello from the fixture"));
        let check = Check::parse("/x  expect=503   contains=a=b c").unwrap();
        assert_eq!(check.expect, 503);
        assert_eq!(check.contains.as_deref(), Some("a=b c"));
    }

    #[test]
    fn malformed_lines_are_rejected_with_a_reason() {
        for (line, reason) in [
            ("health", "path must start with '/'"),
            ("/x flag", "unknown option `flag`"),
            ("/x expect=abc", "expect= needs an HTTP status"),
            ("/x expect=42", "status in 100..=599"),
            ("/x expect=600", "status in 100..=599"),
            ("/x expect=200 expect=201", "expect= given twice"),
            ("/x contains=", "contains= needs a non-empty text"),
        ] {
            let err = Check::parse(line).unwrap_err();
            assert!(err.contains(reason), "{line}: {err}");
        }
    }

    #[test]
    fn manifest_skips_blank_and_comment_lines_and_numbers_errors() {
        let manifest = Manifest::parse("\n# c\n  /a\n\n/b expect=301\n").unwrap();
        assert_eq!(manifest.checks.len(), 2);
        assert!(!manifest.is_empty());
        assert_eq!(manifest.checks[1].expect, 301);
        let err = Manifest::parse("/a\n\n/b what\n").unwrap_err();
        assert!(matches!(err, ManifestError::Line { line: 3, .. }), "{err}");
        assert_eq!(err.to_string(), "line 3: unknown option `what`");
        assert!(matches!(
            Manifest::parse("# only\n\n").unwrap_err(),
            ManifestError::Empty
        ));
        assert!(matches!(
            Manifest::parse("").unwrap_err(),
            ManifestError::Empty
        ));
    }

    #[test]
    fn manifest_round_trips_through_serde() {
        let manifest = Manifest::parse("/a\n/b expect=404 contains=x\n").unwrap();
        let json = serde_json::to_value(&manifest).unwrap();
        let back: Manifest = serde_json::from_value(json).unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn manifest_loads_from_a_file_and_reports_read_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CHECKS");
        std::fs::write(&path, "/\n/health/v1\n").unwrap();
        let manifest = Manifest::load(&path).unwrap();
        assert_eq!(manifest.checks.len(), 2);
        let err = Manifest::load(&dir.path().join("missing")).unwrap_err();
        assert!(matches!(err, ManifestError::Read { .. }), "{err}");
        assert!(err.to_string().contains("could not read manifest"));
    }

    #[test]
    fn fixture_manifest_parses_to_its_three_paths() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/fullstack/CHECKS");
        let manifest = Manifest::load(&path).unwrap();
        let paths: Vec<&str> = manifest.checks.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["/", "/health/v1", "/api/v1/greeting"]);
        assert!(manifest.checks.iter().all(|c| c.expect == 200));
    }

    #[test]
    fn trunk_asset_roots_collects_absolute_href_and_src_directories() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("index.html"),
            r#"<html><head>
<link data-trunk rel="rust" data-bin="ui" />
<link rel="icon" href="/static/favicon.ico">
<script src="/static/js/app.js"></script>
<script src="/static/js/vendor.js"></script>
<img src="/img/logo.png">
<a href="relative/page.html">no</a>
<a href="/">root, no directory</a>
</head><body></body></html>"#,
        )
        .unwrap();
        let roots = trunk_asset_roots(dir.path()).unwrap();
        assert_eq!(
            roots,
            vec![
                "/img/".to_string(),
                "/static/".to_string(),
                "/static/js/".to_string()
            ]
        );
    }

    #[test]
    fn trunk_asset_roots_is_empty_without_an_index_or_absolute_references() {
        let dir = TempDir::new().unwrap();
        assert_eq!(trunk_asset_roots(dir.path()).unwrap(), Vec::<String>::new());
        std::fs::write(dir.path().join("index.html"), "<html><body></body></html>").unwrap();
        assert_eq!(trunk_asset_roots(dir.path()).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn derived_manifest_covers_health_index_and_asset_roots_once() {
        let derived = Manifest::derived("/health", &["/".to_string(), "/static/".to_string()]);
        let paths: Vec<&str> = derived.checks.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["/health", "/", "/static/"]);
        let bare = Manifest::derived("/", &[]);
        assert_eq!(bare.checks, vec![Check::new("/")]);
    }

    #[test]
    fn join_url_never_doubles_the_slash() {
        assert_eq!(join_url("http://h:1", "/a"), "http://h:1/a");
        assert_eq!(join_url("http://h:1/", "/a"), "http://h:1/a");
        assert_eq!(
            join_url("https://example.invalid/base/", "/"),
            "https://example.invalid/base/"
        );
    }

    #[test]
    fn snippet_collapses_whitespace_and_truncates() {
        assert_eq!(snippet("  a \n\t b  c "), "a b c");
        let long = "x".repeat(SNIPPET_CHARS + 50);
        assert_eq!(snippet(&long).chars().count(), SNIPPET_CHARS);
        assert_eq!(snippet(""), "");
    }

    #[test]
    fn evaluate_judges_status_and_body() {
        let ok = |status, body: &str| {
            Ok(ProbeResponse {
                status,
                body: body.to_string(),
            })
        };
        let plain = Check::new("/");
        let result = evaluate(&plain, "u", ok(200, "<html>"));
        assert!(result.passed);
        assert_eq!(result.status, Some(200));
        assert_eq!(result.snippet, "<html>");
        assert_eq!(result.error, None);
        assert!(!evaluate(&plain, "u", ok(500, "boom")).passed);
        let mut contains = Check::new("/g");
        contains.contains = Some("Hello".to_string());
        assert!(evaluate(&contains, "u", ok(200, "Hello there")).passed);
        let miss = evaluate(&contains, "u", ok(200, "bye"));
        assert!(!miss.passed);
        assert_eq!(miss.contains.as_deref(), Some("Hello"));
        let mut not_found = Check::new("/n");
        not_found.expect = 404;
        assert!(evaluate(&not_found, "u", ok(404, "")).passed);
        assert!(!evaluate(&not_found, "u", ok(200, "")).passed);
        let failed = evaluate(
            &plain,
            "u",
            Err(ProbeError::Request {
                url: "u".to_string(),
                detail: "refused".to_string(),
            }),
        );
        assert!(!failed.passed);
        assert_eq!(failed.status, None);
        assert_eq!(failed.snippet, "");
        assert_eq!(
            failed.error.as_deref(),
            Some("request to u failed: refused")
        );
    }

    struct StubProbe {
        responses: HashMap<String, (u16, String)>,
        calls: Mutex<Vec<String>>,
    }

    impl StubProbe {
        fn new(responses: &[(&str, u16, &str)]) -> Arc<Self> {
            Arc::new(Self {
                responses: responses
                    .iter()
                    .map(|(u, s, b)| (u.to_string(), (*s, b.to_string())))
                    .collect(),
                calls: Mutex::new(Vec::new()),
            })
        }
    }

    impl HttpProbe for StubProbe {
        fn get(&self, url: &str) -> Result<ProbeResponse, ProbeError> {
            self.calls.lock().unwrap().push(url.to_string());
            match self.responses.get(url) {
                Some((status, body)) => Ok(ProbeResponse {
                    status: *status,
                    body: body.clone(),
                }),
                None => Err(ProbeError::Request {
                    url: url.to_string(),
                    detail: "connection refused".to_string(),
                }),
            }
        }
    }

    #[test]
    fn checker_runs_every_check_in_order_and_counts() {
        let probe = StubProbe::new(&[
            ("http://app/", 200, "<html>index</html>"),
            ("http://app/health/v1", 200, "{\"status\":\"ok\"}"),
            (
                "http://app/api/v1/greeting",
                500,
                "route broken by FIXTURE_BREAK_ROUTE=1",
            ),
        ]);
        let manifest =
            Manifest::parse("/\n/health/v1 contains=ok\n/api/v1/greeting\n/gone expect=404\n")
                .unwrap();
        let report = ManifestChecker::new(probe.clone()).run("http://app/", &manifest);
        assert_eq!(report.base_url, "http://app/");
        assert_eq!(report.passed, 2);
        assert_eq!(report.failed, 2);
        assert!(!report.all_passed());
        assert_eq!(
            probe.calls.lock().unwrap().clone(),
            vec![
                "http://app/",
                "http://app/health/v1",
                "http://app/api/v1/greeting",
                "http://app/gone"
            ]
        );
        let greeting = &report.checks[2];
        assert_eq!(greeting.status, Some(500));
        assert_eq!(greeting.snippet, "route broken by FIXTURE_BREAK_ROUTE=1");
        assert!(!greeting.passed);
        let gone = &report.checks[3];
        assert_eq!(
            gone.error.as_deref(),
            Some("request to http://app/gone failed: connection refused")
        );
        let json = report.to_json();
        assert_eq!(json["passed"], 2);
        assert_eq!(json["checks"][1]["contains"], "ok");
        assert_eq!(json["checks"][3]["status"], Value::Null);
        let back: ManifestReport = serde_json::from_value(json).unwrap();
        assert_eq!(back, report);
        let text = report.render_text();
        assert!(
            text.starts_with("endpoint checks against http://app/\n"),
            "{text}"
        );
        assert!(text.contains("PASS / expected 200 got 200\n"), "{text}");
        assert!(
            text.contains("FAIL /api/v1/greeting expected 200 got 500: route broken"),
            "{text}"
        );
        assert!(text.contains("FAIL /gone expected 404 got - (request to http://app/gone failed: connection refused)"), "{text}");
        assert!(text.ends_with("2 passed, 2 failed"), "{text}");
    }

    #[test]
    fn checker_reports_all_passed_for_a_healthy_app() {
        let probe = StubProbe::new(&[("http://app/health", 200, "ok")]);
        let report =
            ManifestChecker::new(probe).run("http://app", &Manifest::derived("/health", &[]));
        assert_eq!(report.passed, 1);
        assert_eq!(
            report.failed, 1,
            "the derived index page is unreachable in this stub"
        );
        let probe = StubProbe::new(&[("http://app/health", 200, "ok"), ("http://app/", 200, "")]);
        let report =
            ManifestChecker::new(probe).run("http://app", &Manifest::derived("/health", &[]));
        assert!(report.all_passed());
        assert!(report.render_text().ends_with("2 passed, 0 failed"));
    }

    #[test]
    fn container_probe_argv_and_output_parsing() {
        let argv = ContainerProbe::argv("http://127.0.0.1:18000/x", 7);
        assert_eq!(
            argv,
            [
                "curl",
                "-sS",
                "--max-time",
                "7",
                "-w",
                "\n%{http_code}",
                "http://127.0.0.1:18000/x"
            ]
        );
        let parsed = ContainerProbe::parse_output("u", "{\"a\":1}\n200").unwrap();
        assert_eq!(
            parsed,
            ProbeResponse {
                status: 200,
                body: "{\"a\":1}".to_string()
            }
        );
        let multi = ContainerProbe::parse_output("u", "line1\nline2\n\n404").unwrap();
        assert_eq!(multi.status, 404);
        assert_eq!(multi.body, "line1\nline2\n");
        let empty = ContainerProbe::parse_output("u", "\n204").unwrap();
        assert_eq!(empty.body, "");
        for bad in ["", "no status", "body\nabc", "\n000"] {
            let err = ContainerProbe::parse_output("u", bad).unwrap_err();
            assert!(
                matches!(err, ProbeError::Malformed { .. }),
                "{bad:?}: {err}"
            );
            assert!(err.to_string().contains("no status code"));
        }
    }

    struct ScriptedRunner {
        output: std::io::Result<RunOutput>,
        seen: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl ScriptedRunner {
        fn new(output: std::io::Result<RunOutput>) -> Arc<Self> {
            Arc::new(Self {
                output,
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, program: &str, args: &[String]) -> std::io::Result<RunOutput> {
            self.seen
                .lock()
                .unwrap()
                .push((program.to_string(), args.to_vec()));
            match &self.output {
                Ok(out) => Ok(out.clone()),
                Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
            }
        }
    }

    fn handle() -> Arc<ContainerHandle> {
        Arc::new(ContainerHandle {
            name: "dev-1".to_string(),
            runtime: ContainerRuntime::Podman,
            port: None,
            needs_cleanup: false,
        })
    }

    #[test]
    fn container_probe_execs_curl_in_the_container() {
        let runner = ScriptedRunner::new(Ok(RunOutput {
            success: true,
            stdout: "hello\n200".to_string(),
            stderr: String::new(),
        }));
        let probe = ContainerProbe::new(handle(), runner.clone()).with_timeout(3);
        let response = probe.get("http://127.0.0.1:18000/").unwrap();
        assert_eq!(
            response,
            ProbeResponse {
                status: 200,
                body: "hello".to_string()
            }
        );
        let seen = runner.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "podman");
        assert_eq!(
            seen[0].1,
            [
                "exec",
                "dev-1",
                "curl",
                "-sS",
                "--max-time",
                "3",
                "-w",
                "\n%{http_code}",
                "http://127.0.0.1:18000/"
            ]
        );
    }

    #[test]
    fn container_probe_maps_curl_failure_and_spawn_errors() {
        let runner = ScriptedRunner::new(Ok(RunOutput {
            success: false,
            stdout: "\n000".to_string(),
            stderr: "curl: (7) Failed to connect\n".to_string(),
        }));
        let err = ContainerProbe::new(handle(), runner)
            .get("http://127.0.0.1:1/")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "request to http://127.0.0.1:1/ failed: curl: (7) Failed to connect"
        );
        let runner = ScriptedRunner::new(Err(std::io::Error::other("no podman")));
        let err = ContainerProbe::new(handle(), runner)
            .get("http://x/")
            .unwrap_err();
        assert!(matches!(err, ProbeError::Spawn { .. }), "{err}");
        assert!(
            err.to_string()
                .starts_with("could not spawn `podman exec dev-1 curl"),
            "{err}"
        );
        assert!(err.to_string().ends_with("no podman"), "{err}");
    }
}
