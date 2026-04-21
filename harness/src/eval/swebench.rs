//! SWE-bench Verified dataset adapter.
//!
//! Closes issue #91: translate SWE-bench Verified JSONL tasks into nanna's
//! [`EvalCase`] format and materialize their fixture repos.
//!
//! The adapter is language-agnostic; it simply produces `EvalCase` values
//! with `task.language = "python"` (SWE-bench Verified is Python-only).
//! Executing these cases requires a Python-aware runner, which is a separate
//! follow-up — the current [`crate::eval::runner`] only dispatches
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

use std::path::{Path, PathBuf};

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
        format!("{}

Hints:
{}", task.problem_statement, task.hints_text)
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
            // Format: `a/<path> b/<path>` (paths may contain spaces — SWE-bench
            // tasks use paths without spaces so the simple split is safe).
            let (_a, b) = rest.split_once(' ')?;
            b.strip_prefix("b/").map(str::to_string)
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
/// Network required. For tests, call [`materialize_from_url`] with a local
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
        let diff = git2::Diff::from_buffer(test_patch.as_bytes())?;
        repo.apply(&diff, git2::ApplyLocation::WorkDir, None)?;
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
        let s = canonical.to_string_lossy().replace('\\', "/");
        // `s` is already absolute (starts with `/` on Unix, `C:/` on Windows).
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
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(work_path)
            .status()
            .unwrap();
        for (key, val) in [("user.email", "test@example.com"), ("user.name", "Test")] {
            Command::new("git")
                .args(["-C"])
                .arg(work_path)
                .args(["config", key, val])
                .status()
                .unwrap();
        }
        std::fs::write(work_path.join("hello.py"), "print('hi')\n").unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(work_path)
            .args(["add", "hello.py"])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(work_path)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap();
        let oid_out = Command::new("git")
            .args(["-C"])
            .arg(work_path)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let oid = String::from_utf8(oid_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        // Clone as a bare repo that materialize_from_url can fetch from.
        Command::new("git")
            .args(["clone", "--bare", "--quiet"])
            .arg(work_path)
            .arg(bare_path)
            .status()
            .unwrap();
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
    fn materialize_from_url_rejects_invalid_oid() {
        let seed = tempdir().unwrap();
        let bare = tempdir().unwrap();
        let _oid = init_local_fixture(seed.path(), bare.path());

        let workspace = tempdir().unwrap();
        let target = workspace.path().join("repo");
        let url = path_to_file_url(bare.path());
        let err = materialize_from_url(&url, "not-a-real-oid", "", &target).unwrap_err();
        matches!(err, SWEBenchError::InvalidOid(_));
    }
}
