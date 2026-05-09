//! SWE-bench Verified dataset adapter.
//!
//! Closes issue #91: translate SWE-bench Verified JSONL tasks into nanna's
//! [`EvalCase`] format and materialize their fixture repos.
//!
//! The adapter is language-agnostic; it simply produces `EvalCase` values
//! with `task.language = "python"` (SWE-bench Verified is Python-only).
//! Executing these cases requires a Python-aware runner, which is a separate
//! follow-up — the current `crate::eval::runner` only dispatches
//! `cargo build` / `cargo test` and won't pass them yet.
//!
//! ```rust,no_run
//! use std::path::Path;
//! use harness::eval::swebench::{load_swebench_dataset, adapt_to_eval_case};
//!
//! let tasks = load_swebench_dataset(Path::new("evals/datasets/swebench-verified-sample.jsonl"))?;
//! for task in &tasks {
//!     let case = adapt_to_eval_case(task);
//!     println!("{}: {}", case.case.id, case.task.prompt.lines().next().unwrap_or(""));
//! }
//! # Ok::<(), harness::eval::swebench::SWEBenchError>(())
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Deserializer};

use crate::agent::eval_case::{CaseInfo, CaseMetadata, EvalCase, ExpectedResult, TaskSpec};

/// A single SWE-bench task as parsed from the upstream JSONL format.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SWEBenchTask {
    pub instance_id: String,
    pub repo: String,
    pub base_commit: String,
    pub patch: String,
    pub test_patch: String,
    pub problem_statement: String,
    #[serde(default)]
    pub hints_text: String,
    #[serde(default)]
    pub version: String,
    #[serde(
        rename = "FAIL_TO_PASS",
        deserialize_with = "deserialize_string_or_vec"
    )]
    pub fail_to_pass: Vec<String>,
    #[serde(
        rename = "PASS_TO_PASS",
        deserialize_with = "deserialize_string_or_vec"
    )]
    pub pass_to_pass: Vec<String>,
    #[serde(default)]
    pub environment_setup_commit: Option<String>,
}

/// Errors that can occur loading, adapting, or materializing SWE-bench tasks.
#[derive(Debug, thiserror::Error)]
pub enum SWEBenchError {
    #[error("failed to read {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("failed to parse JSONL at line {line}: {source}")]
    Parse {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("git operation failed: {0}")]
    Git(#[from] git2::Error),
    #[error("invalid base_commit oid: {0}")]
    InvalidOid(String),
    #[error("git apply failed to apply test_patch: {0}")]
    PatchApply(String),
}

impl From<std::io::Error> for SWEBenchError {
    fn from(e: std::io::Error) -> Self {
        SWEBenchError::PatchApply(format!("io error during git apply: {e}"))
    }
}

/// Load a SWE-bench dataset from a JSONL file.
///
/// Each non-empty line must be a valid SWE-bench instance record. Blank
/// lines are skipped.
pub fn load_swebench_dataset(path: &Path) -> Result<Vec<SWEBenchTask>, SWEBenchError> {
    let content =
        std::fs::read_to_string(path).map_err(|e| SWEBenchError::Io(path.to_path_buf(), e))?;
    parse_jsonl(&content)
}

fn parse_jsonl(content: &str) -> Result<Vec<SWEBenchTask>, SWEBenchError> {
    let mut out = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let task: SWEBenchTask = serde_json::from_str(line).map_err(|e| SWEBenchError::Parse {
            line: idx + 1,
            source: e,
        })?;
        out.push(task);
    }
    Ok(out)
}

/// Convert a SWE-bench task into nanna's [`EvalCase`] format.
///
/// The reference `patch` is only used to extract `expected.files_changed`;
/// it is not applied during materialization (that is the agent's job).
pub fn adapt_to_eval_case(task: &SWEBenchTask) -> EvalCase {
    let repo_short = task
        .repo
        .rsplit('/')
        .next()
        .unwrap_or(&task.repo)
        .to_string();
    let files_changed = extract_changed_files(&task.patch);

    let prompt = if task.hints_text.is_empty() {
        task.problem_statement.clone()
    } else {
        format!(
            "{}

Hints:
{}",
            task.problem_statement, task.hints_text
        )
    };

    EvalCase {
        case: CaseInfo {
            id: task.instance_id.clone(),
            name: task.instance_id.clone(),
            description: task.problem_statement.clone(),
        },
        task: TaskSpec {
            prompt,
            language: "python".to_string(),
        },
        expected: ExpectedResult {
            files_changed,
            build_must_pass: false,
            tests_must_pass: true,
            required_symbols: Vec::new(),
        },
        metadata: CaseMetadata {
            difficulty: "unknown".to_string(),
            tags: vec!["swebench-verified".to_string(), repo_short],
            timeout_secs: 1800,
        },
    }
}

/// Extract the post-image filenames from a unified diff's `diff --git` headers.
///
/// Returns sorted, de-duplicated paths.
pub(crate) fn extract_changed_files(patch: &str) -> Vec<String> {
    let mut files: Vec<String> = patch
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("diff --git ")?;
            // Format: `a/<path> b/<path>`. Split on " b/" to correctly
            // handle paths that contain spaces.
            let (_a, b) = rest.split_once(" b/")?;
            Some(b.to_string())
        })
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Materialize a fixture repository for a SWE-bench task at `workspace`.
///
/// 1. Clones the upstream GitHub repo (`https://github.com/{repo}.git`).
/// 2. Checks out `base_commit` as a detached HEAD.
/// 3. Applies `test_patch` against the working directory so failing tests
///    are present when the agent starts.
///
/// Network required. For tests, call `materialize_from_url` with a local
/// file:// URL instead.
pub fn materialize(task: &SWEBenchTask, workspace: &Path) -> Result<(), SWEBenchError> {
    let url = format!("https://github.com/{}.git", task.repo);
    materialize_from_url(&url, &task.base_commit, &task.test_patch, workspace)
}

/// Internal core of [`materialize`] — accepts an arbitrary git URL so unit
/// tests can use a local file:// fixture without network.
pub(crate) fn materialize_from_url(
    url: &str,
    base_commit: &str,
    test_patch: &str,
    workspace: &Path,
) -> Result<(), SWEBenchError> {
    let repo = git2::Repository::clone(url, workspace)?;

    let oid = git2::Oid::from_str(base_commit)
        .map_err(|_| SWEBenchError::InvalidOid(base_commit.to_string()))?;
    let obj = repo.find_object(oid, Some(git2::ObjectType::Commit))?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_tree(&obj, Some(&mut checkout))?;
    repo.set_head_detached(oid)?;

    if !test_patch.trim().is_empty() {
        apply_patch(workspace, test_patch)?;
    }

    Ok(())
}

/// Apply a unified diff to `workspace` by piping it into `git apply`.
///
/// Shells out to `git apply --whitespace=nowarn` rather than going through
/// `git2::Repository::apply`. The libgit2 implementation is stricter than
/// upstream `git apply` and rejects hunks that the porcelain accepts (e.g.
/// when the patch's context lines have shifted by a few rows or when binary
/// patch sections are present). The SWE-bench dataset's `test_patch` fields
/// are produced by upstream `git`, so they round-trip cleanly through the
/// porcelain but trip libgit2's hunk matcher.
fn apply_patch(workspace: &Path, patch: &str) -> Result<(), SWEBenchError> {
    let mut child = Command::new("git")
        .args(["apply", "--whitespace=nowarn"])
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .expect("stdin was set to Stdio::piped")
        .write_all(patch.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(SWEBenchError::PatchApply(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Vec(Vec<String>),
        Str(String),
    }

    match StringOrVec::deserialize(deserializer)? {
        StringOrVec::Vec(v) => Ok(v),
        StringOrVec::Str(s) => {
            if s.trim().is_empty() {
                return Ok(Vec::new());
            }
            serde_json::from_str(&s).map_err(serde::de::Error::custom)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn sample_task() -> SWEBenchTask {
        SWEBenchTask {
            instance_id: "django__django-11099".to_string(),
            repo: "django/django".to_string(),
            base_commit: "d26b2424437dabeeca94d7900b37d2df4410da0c".to_string(),
            patch: concat!(
                "diff --git a/django/contrib/auth/validators.py b/django/contrib/auth/validators.py\n",
                "--- a/django/contrib/auth/validators.py\n",
                "+++ b/django/contrib/auth/validators.py\n",
                "@@ -7 +7 @@\n",
                "-    regex = r'^[\\w.@+-]+$'\n",
                "+    regex = r'\\A[\\w.@+-]+\\Z'\n",
            )
            .to_string(),
            test_patch: "".to_string(),
            problem_statement: "UsernameValidator allows trailing newline in usernames".to_string(),
            hints_text: String::new(),
            version: "3.0".to_string(),
            fail_to_pass: vec!["tests/auth_tests/test_validators.py::t1".to_string()],
            pass_to_pass: vec!["tests/auth_tests/test_validators.py::t2".to_string()],
            environment_setup_commit: None,
        }
    }

    /// Convert a local filesystem path to a `file:///` URL that is valid on
    /// all platforms, including Windows where `Path::display()` uses
    /// backslashes and paths start with a drive letter (e.g. `C:\...`).
    ///
    /// RFC 8089 §2: a local file URL has the form `file:///path` (three
    /// slashes — an empty authority followed by an absolute path).  On
    /// Windows the absolute path begins with a drive letter, so the result
    /// is `file:///C:/Users/...`.
    fn path_to_file_url(path: &Path) -> String {
        // Canonicalize to resolve any symlinks (tempdir can return symlinks
        // on macOS via /var -> /private/var), then normalise separators.
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut s = canonical.to_string_lossy().replace('\\', "/");
        // On Windows, `canonicalize()` yields a UNC verbatim path like
        // `\\?\C:\Users\...` → after the separator swap this becomes
        // `//?/C:/Users/...`.  libgit2 can't parse that, so strip the
        // verbatim prefix before building the URL.
        if let Some(stripped) = s.strip_prefix("//?/") {
            s = stripped.to_string();
        }
        // `s` is now absolute (starts with `/` on Unix, `C:/` on Windows).
        // Prepend `file:///`; on Unix the leading `/` makes it `file:////tmp/…`
        // which git2/libgit2 normalises correctly, but it is cleaner to strip
        // a leading `/` before adding the three-slash prefix.
        if s.starts_with('/') {
            format!("file://{s}")
        } else {
            // Windows: s = "C:/Users/…" → "file:///C:/Users/…"
            format!("file:///{s}")
        }
    }

    #[test]
    fn parse_jsonl_skips_blank_lines() {
        let jsonl = r#"

{"instance_id":"a","repo":"x/y","base_commit":"abc","patch":"","test_patch":"","problem_statement":"p","hints_text":"","version":"1.0","FAIL_TO_PASS":[],"PASS_TO_PASS":[]}

{"instance_id":"b","repo":"x/y","base_commit":"def","patch":"","test_patch":"","problem_statement":"p","hints_text":"","version":"1.0","FAIL_TO_PASS":[],"PASS_TO_PASS":[]}
"#;
        let tasks = parse_jsonl(jsonl).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].instance_id, "a");
        assert_eq!(tasks[1].instance_id, "b");
    }

    #[test]
    fn parse_jsonl_accepts_string_encoded_fail_to_pass() {
        let jsonl = r#"{"instance_id":"a","repo":"x/y","base_commit":"abc","patch":"","test_patch":"","problem_statement":"p","version":"1.0","FAIL_TO_PASS":"[\"t1\",\"t2\"]","PASS_TO_PASS":"[]"}"#;
        let tasks = parse_jsonl(jsonl).unwrap();
        assert_eq!(
            tasks[0].fail_to_pass,
            vec!["t1".to_string(), "t2".to_string()]
        );
        assert!(tasks[0].pass_to_pass.is_empty());
    }

    #[test]
    fn parse_jsonl_accepts_array_fail_to_pass() {
        let jsonl = r#"{"instance_id":"a","repo":"x/y","base_commit":"abc","patch":"","test_patch":"","problem_statement":"p","version":"1.0","FAIL_TO_PASS":["t1"],"PASS_TO_PASS":[]}"#;
        let tasks = parse_jsonl(jsonl).unwrap();
        assert_eq!(tasks[0].fail_to_pass, vec!["t1".to_string()]);
    }

    #[test]
    fn parse_jsonl_accepts_empty_string_for_pass_to_pass() {
        let jsonl = r#"{"instance_id":"a","repo":"x/y","base_commit":"abc","patch":"","test_patch":"","problem_statement":"p","version":"1.0","FAIL_TO_PASS":"","PASS_TO_PASS":""}"#;
        let tasks = parse_jsonl(jsonl).unwrap();
        assert!(tasks[0].fail_to_pass.is_empty());
        assert!(tasks[0].pass_to_pass.is_empty());
    }

    #[test]
    fn parse_jsonl_reports_line_number_on_error() {
        let jsonl = "{\"instance_id\":\"a\",\"repo\":\"x/y\",\"base_commit\":\"abc\",\"patch\":\"\",\"test_patch\":\"\",\"problem_statement\":\"p\",\"version\":\"1.0\",\"FAIL_TO_PASS\":[],\"PASS_TO_PASS\":[]}\nnot json";
        match parse_jsonl(jsonl) {
            Err(SWEBenchError::Parse { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected Parse error, got {:?}", other),
        }
    }

    #[test]
    fn extract_changed_files_single_file() {
        let patch = "diff --git a/src/lib.rs b/src/lib.rs\n@@ ...\n";
        assert_eq!(extract_changed_files(patch), vec!["src/lib.rs".to_string()]);
    }

    #[test]
    fn extract_changed_files_multi_file_sorted_deduped() {
        let patch = concat!(
            "diff --git a/z.py b/z.py\n",
            "@@ ...\n",
            "diff --git a/a.py b/a.py\n",
            "@@ ...\n",
            "diff --git a/z.py b/z.py\n",
            "@@ ...\n",
        );
        assert_eq!(
            extract_changed_files(patch),
            vec!["a.py".to_string(), "z.py".to_string()]
        );
    }

    #[test]
    fn extract_changed_files_empty_patch_returns_empty() {
        assert!(extract_changed_files("").is_empty());
    }

    #[test]
    fn extract_changed_files_ignores_rename_lines() {
        let patch = concat!(
            "diff --git a/old.py b/new.py\n",
            "similarity index 95%\n",
            "rename from old.py\n",
            "rename to new.py\n",
        );
        // Post-image filename is what matters for validation.
        assert_eq!(extract_changed_files(patch), vec!["new.py".to_string()]);
    }

    #[test]
    fn adapt_to_eval_case_basic_mapping() {
        let task = sample_task();
        let case = adapt_to_eval_case(&task);
        assert_eq!(case.case.id, "django__django-11099");
        assert_eq!(case.case.name, "django__django-11099");
        assert_eq!(
            case.case.description,
            "UsernameValidator allows trailing newline in usernames"
        );
        assert_eq!(case.task.language, "python");
        assert!(!case.expected.build_must_pass);
        assert!(case.expected.tests_must_pass);
        assert_eq!(
            case.expected.files_changed,
            vec!["django/contrib/auth/validators.py".to_string()]
        );
        assert_eq!(case.metadata.difficulty, "unknown");
        assert_eq!(
            case.metadata.tags,
            vec!["swebench-verified".to_string(), "django".to_string()]
        );
        assert_eq!(case.metadata.timeout_secs, 1800);
    }

    #[test]
    fn adapt_to_eval_case_appends_hints_to_prompt() {
        let mut task = sample_task();
        task.hints_text = "Check the regex anchors.".to_string();
        let case = adapt_to_eval_case(&task);
        assert!(case.task.prompt.contains("Hints:"));
        assert!(case.task.prompt.contains("regex anchors"));
    }

    #[test]
    fn adapt_to_eval_case_omits_hints_section_when_empty() {
        let task = sample_task();
        let case = adapt_to_eval_case(&task);
        assert!(!case.task.prompt.contains("Hints:"));
    }

    #[test]
    fn adapt_to_eval_case_round_trips_through_toml() {
        let task = sample_task();
        let case = adapt_to_eval_case(&task);
        // Serialize as TOML and re-parse via EvalCase::from_toml_str — this is
        // the acceptance path for checked-in `task.toml` fixtures.
        let toml_str = toml::to_string(&TomlEvalCase::from(&case)).unwrap();
        let reparsed = EvalCase::from_toml_str(&toml_str).unwrap();
        assert_eq!(reparsed.case.id, case.case.id);
        assert_eq!(reparsed.task.language, case.task.language);
        assert_eq!(reparsed.expected.files_changed, case.expected.files_changed);
    }

    // Minimal TOML-serializable mirror of EvalCase for round-trip testing.
    // EvalCase itself only derives Deserialize; we need Serialize here.
    #[derive(serde::Serialize)]
    struct TomlEvalCase<'a> {
        case: TomlCaseInfo<'a>,
        task: TomlTaskSpec<'a>,
        expected: TomlExpected<'a>,
        metadata: TomlMetadata<'a>,
    }
    #[derive(serde::Serialize)]
    struct TomlCaseInfo<'a> {
        id: &'a str,
        name: &'a str,
        description: &'a str,
    }
    #[derive(serde::Serialize)]
    struct TomlTaskSpec<'a> {
        prompt: &'a str,
        language: &'a str,
    }
    #[derive(serde::Serialize)]
    struct TomlExpected<'a> {
        files_changed: &'a [String],
        build_must_pass: bool,
        tests_must_pass: bool,
        required_symbols: &'a [String],
    }
    #[derive(serde::Serialize)]
    struct TomlMetadata<'a> {
        difficulty: &'a str,
        tags: &'a [String],
        timeout_secs: u64,
    }

    impl<'a> From<&'a EvalCase> for TomlEvalCase<'a> {
        fn from(c: &'a EvalCase) -> Self {
            TomlEvalCase {
                case: TomlCaseInfo {
                    id: &c.case.id,
                    name: &c.case.name,
                    description: &c.case.description,
                },
                task: TomlTaskSpec {
                    prompt: &c.task.prompt,
                    language: &c.task.language,
                },
                expected: TomlExpected {
                    files_changed: &c.expected.files_changed,
                    build_must_pass: c.expected.build_must_pass,
                    tests_must_pass: c.expected.tests_must_pass,
                    required_symbols: &c.expected.required_symbols,
                },
                metadata: TomlMetadata {
                    difficulty: &c.metadata.difficulty,
                    tags: &c.metadata.tags,
                    timeout_secs: c.metadata.timeout_secs,
                },
            }
        }
    }

    /// Build a tiny local bare git repo at `bare_path` with one file and a
    /// single commit. Returns the commit OID as hex.
    fn init_local_fixture(work_path: &Path, bare_path: &Path) -> String {
        // Helper to run a git command, asserting success and printing stderr on
        // failure so test output is actionable.
        fn git_must<I, S>(args: I, cwd: &Path, extra_env: &[(&str, &str)])
        where
            I: IntoIterator<Item = S>,
            S: AsRef<std::ffi::OsStr>,
        {
            let mut cmd = Command::new("git");
            cmd.args(args).current_dir(cwd);
            // Isolate from any system/global git config (e.g. commit signing)
            // so the fixture works in both developer sandboxes and Nix CI.
            cmd.env("GIT_CONFIG_NOSYSTEM", "1");
            cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
            cmd.env("HOME", cwd); // belt-and-suspenders: empty home dir
            for (k, v) in extra_env {
                cmd.env(k, v);
            }
            let out = cmd.output().unwrap();
            assert!(
                out.status.success(),
                "git command failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        git_must(["init", "--quiet"], work_path, &[]);
        git_must(["config", "user.email", "test@example.com"], work_path, &[]);
        git_must(["config", "user.name", "Test"], work_path, &[]);
        git_must(["config", "commit.gpgsign", "false"], work_path, &[]);

        std::fs::write(work_path.join("hello.py"), "print('hi')\n").unwrap();
        git_must(["add", "hello.py"], work_path, &[]);
        git_must(
            ["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"],
            work_path,
            &[
                ("GIT_AUTHOR_NAME", "Test"),
                ("GIT_AUTHOR_EMAIL", "test@example.com"),
                ("GIT_COMMITTER_NAME", "Test"),
                ("GIT_COMMITTER_EMAIL", "test@example.com"),
            ],
        );

        let mut cmd = Command::new("git");
        cmd.args(["rev-parse", "HEAD"])
            .current_dir(work_path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", work_path);
        let oid_out = cmd.output().unwrap();
        assert!(
            oid_out.status.success(),
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&oid_out.stderr)
        );
        let oid = String::from_utf8(oid_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        // Clone as a bare repo that materialize_from_url can fetch from.
        git_must(
            [
                "clone",
                "--bare",
                "--quiet",
                work_path.to_str().unwrap(),
                bare_path.to_str().unwrap(),
            ],
            work_path,
            &[],
        );
        oid
    }

    #[test]
    fn materialize_from_url_checks_out_base_commit() {
        let seed = tempdir().unwrap();
        let bare = tempdir().unwrap();
        let oid = init_local_fixture(seed.path(), bare.path());

        let workspace = tempdir().unwrap();
        let target = workspace.path().join("repo");
        let url = path_to_file_url(bare.path());
        materialize_from_url(&url, &oid, "", &target).unwrap();

        assert!(target.join("hello.py").is_file());
        let head = Command::new("git")
            .args(["-C"])
            .arg(&target)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(head.stdout).unwrap().trim(), oid);
    }

    #[test]
    fn materialize_from_url_applies_test_patch() {
        let seed = tempdir().unwrap();
        let bare = tempdir().unwrap();
        let oid = init_local_fixture(seed.path(), bare.path());

        let workspace = tempdir().unwrap();
        let target = workspace.path().join("repo");
        let url = path_to_file_url(bare.path());

        let test_patch = concat!(
            "diff --git a/test_new.py b/test_new.py\n",
            "new file mode 100644\n",
            "--- /dev/null\n",
            "+++ b/test_new.py\n",
            "@@ -0,0 +1,2 @@\n",
            "+def test_stub():\n",
            "+    assert False\n",
        );
        materialize_from_url(&url, &oid, test_patch, &target).unwrap();

        let added = target.join("test_new.py");
        assert!(
            added.is_file(),
            "test_patch should have created test_new.py"
        );
        let content = std::fs::read_to_string(&added).unwrap();
        assert!(content.contains("test_stub"));
    }

    #[test]
    fn materialize_from_url_rejects_unappliable_patch_with_patch_apply_error() {
        let seed = tempdir().unwrap();
        let bare = tempdir().unwrap();
        let oid = init_local_fixture(seed.path(), bare.path());

        let workspace = tempdir().unwrap();
        let target = workspace.path().join("repo");
        let url = path_to_file_url(bare.path());

        // Patch references a file that does not exist in the fixture, so
        // `git apply` must reject it. The fix path returns PatchApply rather
        // than the old git2-flavoured Git error.
        let bogus_patch = concat!(
            "diff --git a/does_not_exist.py b/does_not_exist.py\n",
            "--- a/does_not_exist.py\n",
            "+++ b/does_not_exist.py\n",
            "@@ -1,1 +1,1 @@\n",
            "-foo\n",
            "+bar\n",
        );
        let err = materialize_from_url(&url, &oid, bogus_patch, &target).unwrap_err();
        assert!(
            matches!(err, SWEBenchError::PatchApply(_)),
            "expected PatchApply, got {err:?}"
        );
    }

    #[test]
    fn from_io_error_maps_to_patch_apply() {
        let io_err = std::io::Error::other("boom");
        let err: SWEBenchError = io_err.into();
        assert!(matches!(err, SWEBenchError::PatchApply(_)));
        assert!(err.to_string().contains("boom"));
    }

    #[test]
    fn materialize_constructs_https_github_url_and_delegates() {
        // Exercise the public `materialize` wrapper: the URL is built from
        // `repo` and forwarded to `materialize_from_url`. We use a fixture
        // path that points at a *file://* URL the repo doesn't host on
        // github.com, so the underlying clone fails fast — but the URL
        // construction (and the call) execute. Avoids a real network hit.
        let bare = tempdir().unwrap();
        let task = SWEBenchTask {
            instance_id: "x".to_string(),
            repo: "definitely/not-a-real-org-9c3d8f".to_string(),
            base_commit: "0000000000000000000000000000000000000000".to_string(),
            patch: String::new(),
            test_patch: String::new(),
            problem_statement: String::new(),
            hints_text: String::new(),
            version: String::new(),
            fail_to_pass: vec![],
            pass_to_pass: vec![],
            environment_setup_commit: None,
        };
        // The clone target must not exist beforehand.
        let target = bare.path().join("never-cloned");
        // Real github would 404 fast; CI runners reach github reliably, so
        // we accept the network dependency. Either branch returns Err.
        let err = materialize(&task, &target);
        assert!(err.is_err(), "materialize against a bogus repo must fail");
    }

    #[test]
    fn materialize_from_url_rejects_invalid_oid() {
        let seed = tempdir().unwrap();
        let bare = tempdir().unwrap();
        let _oid = init_local_fixture(seed.path(), bare.path());

        let workspace = tempdir().unwrap();
        let target = workspace.path().join("repo");
        let url = path_to_file_url(bare.path());
        let err = materialize_from_url(&url, "not-a-real-oid", "", &target).unwrap_err();
        assert!(matches!(err, SWEBenchError::InvalidOid(_)));
    }
}
