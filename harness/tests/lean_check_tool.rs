use harness::container::{ContainerHandle, ContainerRuntime};
use harness::effects::EffectClass;
use harness::lean_tool::{
    decide_verdict, lean_args, lean_project_dir, output_uses_sorry, parse_errors, scan_files,
    scan_source, valid_target, Forbidden, Job, LeanCheckTool, Verdict, DEFAULT_TIMEOUT_SECS,
    LEAN_CHECK, MAX_ERRORS, MAX_OFFENCES, MAX_SCAN_BYTES, MAX_SCAN_FILES, MAX_TIMEOUT_SECS,
};
use harness::tools::{create_container_tool_registry, Tool, ToolError, CONTAINER_WORKSPACE_DIR};
use proptest::prelude::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const GOOD_THEOREM: &str = "theorem t : 1 + 1 = 2 := by\n  decide\n";
const LEAN_ERROR_OUT: &str = "Foo.lean:3:4: error: unknown identifier 'x'\nFoo.lean:9:0: warning: unused variable\nFoo.lean:12:2: error: type mismatch\n  <<< boom\n";
const LAKE_ERROR_OUT: &str = "\u{2716} [2/3] Building Foo\nerror: ././Foo.lean:3:4: unknown identifier 'x'\nerror: Lean exited with code 1\nSome required builds logged failures:\n- Foo\nerror: build failed\n";

fn handle() -> Arc<ContainerHandle> {
    Arc::new(ContainerHandle {
        name: "lean-container".to_string(),
        runtime: ContainerRuntime::Podman,
        port: None,
        needs_cleanup: false,
    })
}

fn tool_in(dir: &Path) -> LeanCheckTool {
    LeanCheckTool::new(dir.to_path_buf(), handle(), CONTAINER_WORKSPACE_DIR)
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "lakefile.toml", "name = \"p\"\n");
    for (rel, body) in files {
        write(dir.path(), rel, body);
    }
    dir
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

struct FakePodman {
    _dir: tempfile::TempDir,
    argv_log: PathBuf,
    saved_path: Option<std::ffi::OsString>,
}

impl FakePodman {
    fn install(stdout: &str, stderr: &str, exit: i32) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let argv_log = dir.path().join("argv.txt");
        let out = dir.path().join("stdout.txt");
        let err = dir.path().join("stderr.txt");
        std::fs::write(&out, stdout).unwrap();
        std::fs::write(&err, stderr).unwrap();
        let script = format!(
            "#!/bin/sh\necho \"$*\" > '{}'\nwhile IFS= read -r line; do echo \"$line\"; done < '{}'\nwhile IFS= read -r line; do echo \"$line\" >&2; done < '{}'\nexit {exit}\n",
            argv_log.display(),
            out.display(),
            err.display()
        );
        let bin = dir.path().join("podman");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let saved_path = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        Self {
            _dir: dir,
            argv_log,
            saved_path,
        }
    }

    fn argv(&self) -> String {
        std::fs::read_to_string(&self.argv_log).unwrap()
    }

    fn ran(&self) -> bool {
        self.argv_log.exists()
    }
}

impl Drop for FakePodman {
    fn drop(&mut self) {
        match &self.saved_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

fn invalid(err: ToolError) -> String {
    match err {
        ToolError::InvalidArguments { message } => message,
        other => panic!("expected InvalidArguments, got {other:?}"),
    }
}

fn failed(err: ToolError) -> String {
    match err {
        ToolError::ExecutionFailed { message } => message,
        other => panic!("expected ExecutionFailed, got {other:?}"),
    }
}

fn kinds(source: &str) -> Vec<(usize, Forbidden)> {
    scan_source(source)
        .into_iter()
        .map(|hit| (hit.line, hit.kind))
        .collect()
}

#[test]
fn scanner_finds_each_forbidden_construct_with_its_line() {
    let source = "theorem a : True := by\n  sorry\ntheorem b : True := by\n  admit\ntheorem c : 1 = 1 := by native_decide\naxiom magic : False\n";
    assert_eq!(
        kinds(source),
        vec![
            (2, Forbidden::Sorry),
            (4, Forbidden::Admit),
            (5, Forbidden::NativeDecide),
            (6, Forbidden::Axiom),
        ]
    );
    let hits = scan_source(source);
    assert_eq!(hits[0].text, "sorry");
    assert_eq!(hits[3].text, "axiom magic : False");
}

#[test]
fn scanner_flags_sorry_ax_and_qualified_uses() {
    assert_eq!(
        kinds("def x := sorryAx Nat true"),
        vec![(1, Forbidden::Sorry)]
    );
    assert_eq!(kinds("example := Nat.admit"), vec![(1, Forbidden::Admit)]);
    assert_eq!(kinds("by simp; sorry; sorry").len(), 2);
}

#[test]
fn scanner_ignores_line_comments_but_not_code_before_them() {
    assert!(scan_source("-- sorry axiom admit native_decide\n").is_empty());
    assert!(scan_source("--sorry").is_empty());
    assert_eq!(
        kinds("theorem t : True := sorry -- todo\n"),
        vec![(1, Forbidden::Sorry)]
    );
}

#[test]
fn scanner_ignores_block_and_doc_comments_including_nested_ones() {
    assert!(scan_source("/- sorry -/\n/-- admit -/\n/-! axiom -/\n").is_empty());
    assert!(scan_source("/- a /- sorry -/ sorry -/\n").is_empty());
    assert!(scan_source("/- multi\nline\nnative_decide\n-/\n").is_empty());
    assert_eq!(
        kinds("/- one\ntwo -/ axiom x : False\n"),
        vec![(2, Forbidden::Axiom)]
    );
    assert_eq!(
        kinds("/- a /- b -/ still comment sorry -/ sorry"),
        vec![(1, Forbidden::Sorry)]
    );
}

#[test]
fn an_unterminated_block_comment_hides_the_rest_of_the_file() {
    assert!(scan_source("/- never closed\nsorry\n").is_empty());
}

#[test]
fn scanner_ignores_string_literals() {
    assert!(scan_source("def s := \"sorry\"\n").is_empty());
    assert!(scan_source("def s := \"a \\\" sorry \\\" b\"\n").is_empty());
    assert!(scan_source("def s := \"multi\nline sorry\"\n").is_empty());
    assert!(scan_source("def s := \"line \\\nsorry\"\n").is_empty());
    assert_eq!(
        kinds("def s := \"x\"\nexample := sorry\n"),
        vec![(2, Forbidden::Sorry)]
    );
    assert!(scan_source("def s := \"unterminated sorry").is_empty());
    assert!(scan_source("def s := \"trailing backslash \\").is_empty());
}

#[test]
fn scanner_ignores_raw_strings_and_still_finds_code_after_them() {
    assert!(scan_source("def s := r\"sorry\"\n").is_empty());
    assert!(scan_source("def s := r#\"a \" sorry \"#\n").is_empty());
    assert!(scan_source("def s := r##\"a \"# sorry \"##\n").is_empty());
    assert!(scan_source("def s := r\"multi\nsorry\"\n").is_empty());
    assert_eq!(
        kinds("def s := r\"\\\"\nexample := sorry\n"),
        vec![(2, Forbidden::Sorry)]
    );
    assert!(scan_source("def s := r#\"never closed sorry").is_empty());
    assert_eq!(
        kinds("def r := 1\nexample := sorry"),
        vec![(2, Forbidden::Sorry)]
    );
    assert_eq!(kinds("def x := r#sorry"), vec![(1, Forbidden::Sorry)]);
}

#[test]
fn scanner_handles_char_literals_that_look_like_quotes() {
    assert_eq!(
        kinds("def q := '\"'\nexample := sorry\n"),
        vec![(2, Forbidden::Sorry)]
    );
    assert_eq!(
        kinds("def q := '\\\"'\nexample := sorry\n"),
        vec![(2, Forbidden::Sorry)]
    );
    assert_eq!(
        kinds("def q := '\\n'\nexample := sorry"),
        vec![(2, Forbidden::Sorry)]
    );
    assert_eq!(kinds("def q := 'a' ; sorry"), vec![(1, Forbidden::Sorry)]);
    assert_eq!(kinds("def x := '"), vec![]);
}

#[test]
fn scanner_does_not_flag_longer_identifiers() {
    let source = "def sorryish := 1\ndef my_axiom := 2\ndef admitted := 3\ndef native_decide_not := 4\ndef x' := 5\ndef axioms := 6\n";
    assert!(scan_source(source).is_empty());
    assert_eq!(kinds("example := sorry'"), vec![]);
}

#[test]
fn scanner_counts_lines_across_crlf_and_unicode() {
    assert_eq!(
        kinds("def \u{3b1} := 1\r\ntheorem \u{2200} : True := sorry\r\n"),
        vec![(2, Forbidden::Sorry)]
    );
}

#[test]
fn clean_source_has_no_offences() {
    assert!(scan_source(GOOD_THEOREM).is_empty());
    assert!(scan_source("").is_empty());
}

#[test]
fn parse_errors_reads_lean_and_lake_formats_and_drops_summaries() {
    let errors = parse_errors(LEAN_ERROR_OUT);
    assert_eq!(errors.len(), 2);
    assert_eq!(errors[0].file.as_deref(), Some("Foo.lean"));
    assert_eq!(errors[0].line, 3);
    assert_eq!(errors[0].message, "unknown identifier 'x'");
    assert_eq!(errors[1].line, 12);
    let lake = parse_errors(LAKE_ERROR_OUT);
    assert_eq!(lake.len(), 1);
    assert_eq!(lake[0].file.as_deref(), Some("././Foo.lean"));
    assert_eq!(lake[0].line, 3);
    assert_eq!(lake[0].message, "unknown identifier 'x'");
}

#[test]
fn parse_errors_keeps_unpositioned_errors_without_a_file() {
    let errors = parse_errors("error: unknown package 'Mathlib'\nerror: a: b\n");
    assert_eq!(errors.len(), 2);
    assert_eq!((errors[0].file.clone(), errors[0].line), (None, 0));
    assert_eq!(errors[0].message, "unknown package 'Mathlib'");
    assert_eq!(errors[1].message, "a: b");
}

#[test]
fn parse_errors_ignores_warnings_and_noise() {
    assert!(parse_errors("A.lean:1:1: warning: x\nBuild completed successfully.\n").is_empty());
    assert!(parse_errors("").is_empty());
    assert_eq!(parse_errors("A.lean:x:y: error: odd\n")[0].file, None);
}

#[test]
fn sorry_in_output_is_detected() {
    assert!(output_uses_sorry("warning: declaration uses 'sorry'"));
    assert!(output_uses_sorry("warning: declaration uses `sorry`"));
    assert!(!output_uses_sorry("declaration is fine"));
}

#[test]
fn verdict_requires_exit_ok_no_errors_and_no_offences() {
    assert_eq!(decide_verdict(true, 0, 0), Verdict::Successful);
    assert_eq!(decide_verdict(true, 0, 3), Verdict::Failed);
    assert_eq!(decide_verdict(true, 1, 0), Verdict::Failed);
    assert_eq!(decide_verdict(false, 1, 0), Verdict::Failed);
    assert_eq!(decide_verdict(false, 0, 0), Verdict::Error);
    assert_eq!(decide_verdict(false, 0, 2), Verdict::Failed);
}

#[test]
fn lean_args_cover_build_and_file_jobs() {
    assert_eq!(
        lean_args(&Job::Build(None), true, 7),
        ["timeout", "-k", "10", "7", "lake", "build"]
    );
    assert_eq!(
        lean_args(&Job::Build(Some("Foo".into())), true, 7)[6],
        "Foo"
    );
    assert_eq!(
        lean_args(&Job::File("./A.lean".into()), true, 7)[4..],
        ["lake", "env", "lean", "./A.lean"]
    );
    assert_eq!(
        lean_args(&Job::File("./A.lean".into()), false, 7)[4..],
        ["lean", "./A.lean"]
    );
}

#[test]
fn target_validation_rejects_flags_and_shell_characters() {
    for good in ["Foo", "Foo.Bar", "+Foo", "pkg/Foo:exe", "@pkg/Lib", "a_b"] {
        assert!(valid_target(good), "{good}");
    }
    let long = "a".repeat(201);
    for bad in [
        "",
        "-h",
        "--help",
        "a b",
        "a;b",
        "../x",
        "a..b",
        "a$b",
        long.as_str(),
    ] {
        assert!(!valid_target(bad), "{bad}");
    }
}

#[test]
fn project_dir_detection_prefers_the_root() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(lean_project_dir(dir.path()), None);
    write(dir.path(), "tests/proofs/lean/lean-toolchain", "x");
    assert_eq!(
        lean_project_dir(dir.path()),
        Some(PathBuf::from("tests/proofs/lean"))
    );
    for marker in ["lakefile.lean", "lakefile.toml", "lean-toolchain"] {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), marker, "");
        assert_eq!(lean_project_dir(root.path()), Some(PathBuf::new()));
    }
    let dirs = tempfile::tempdir().unwrap();
    std::fs::create_dir(dirs.path().join("lakefile.lean")).unwrap();
    assert_eq!(lean_project_dir(dirs.path()), None);
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn successful_build_runs_lake_in_the_container() {
    let dir = project(&[("Foo.lean", GOOD_THEOREM)]);
    let fake = FakePodman::install("Build completed successfully.\n", "", 0);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(
        fake.argv().trim_end(),
        "exec -w /workspace lean-container timeout -k 10 600 lake build"
    );
    assert_eq!(result["verdict"], json!("successful"));
    assert_eq!(result["error_count"], json!(0));
    assert_eq!(result["errors"], json!([]));
    assert_eq!(result["sorry_count"], json!(0));
    assert_eq!(result["uses_sorry"], json!(false));
    assert_eq!(result["axioms_ok"], json!(true));
    assert_eq!(result["offending"], json!([]));
    assert_eq!(result["detail"], Value::Null);
    assert_eq!(result["target"], Value::Null);
    assert!(result["notice"].as_str().unwrap().contains("untrusted"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn target_and_timeout_reach_the_command() {
    let dir = project(&[]);
    let fake = FakePodman::install("", "", 0);
    let result = tool_in(dir.path())
        .execute(json!({"target": "Foo.Bar", "timeout_secs": 30}))
        .await
        .unwrap();
    assert!(fake
        .argv()
        .trim_end()
        .ends_with("timeout -k 10 30 lake build Foo.Bar"));
    assert_eq!(result["target"], json!("Foo.Bar"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn the_project_under_tests_proofs_lean_sets_the_container_directory() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "tests/proofs/lean/lakefile.lean", "");
    write(dir.path(), "tests/proofs/lean/Proofs.lean", GOOD_THEOREM);
    let fake = FakePodman::install("", "", 0);
    tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert!(fake
        .argv()
        .starts_with("exec -w /workspace/tests/proofs/lean lean-container"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn failed_build_lists_fenced_errors() {
    let dir = project(&[("Foo.lean", GOOD_THEOREM)]);
    let _fake = FakePodman::install(LEAN_ERROR_OUT, "", 1);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["verdict"], json!("failed"));
    assert_eq!(result["error_count"], json!(2));
    let errors = result["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2);
    assert!(errors[0]["file"].as_str().unwrap().contains("Foo.lean"));
    assert_eq!(errors[0]["line"], json!(3));
    assert!(errors[0]["message"]
        .as_str()
        .unwrap()
        .contains("unknown identifier"));
    assert!(!result.to_string().contains("unused variable"));
    assert_eq!(result["axioms_ok"], json!(true));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn error_messages_and_paths_are_escaped_and_redacted() {
    let dir = project(&[]);
    let _fake = FakePodman::install(
        "A<b>.lean:1:1: error: <script> token ghp_abcdefghijklmnopqrstuvwxyz0123456789\n",
        "",
        1,
    );
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    let text = result["errors"].to_string();
    assert!(text.contains("&lt;script&gt;"));
    assert!(text.contains("A&lt;b&gt;.lean"));
    assert!(!text.contains("<script>"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn errors_are_clipped_and_counted() {
    let dir = project(&[]);
    let mut out = String::new();
    for n in 0..30 {
        out.push_str(&format!("A.lean:{n}:0: error: problem {n}\n"));
    }
    let _fake = FakePodman::install(&out, "", 1);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["errors"].as_array().unwrap().len(), MAX_ERRORS);
    assert_eq!(result["error_count"], json!(30));
    assert_eq!(result["omitted_errors"], json!(10));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn errors_in_output_fail_even_with_a_zero_exit() {
    let dir = project(&[]);
    let _fake = FakePodman::install("A.lean:1:1: error: nope\n", "", 0);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["verdict"], json!("failed"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn a_sorry_in_the_project_blocks_success_and_is_reported() {
    let dir = project(&[
        ("Good.lean", GOOD_THEOREM),
        (
            "Sub/Bad.lean",
            "theorem t : True := by\n  sorry\n-- sorry in comment\n",
        ),
    ]);
    let _fake = FakePodman::install("Build completed successfully.\n", "", 0);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["verdict"], json!("failed"));
    assert_eq!(result["error_count"], json!(0));
    assert_eq!(result["sorry_count"], json!(1));
    assert_eq!(result["uses_sorry"], json!(true));
    assert_eq!(result["axioms_ok"], json!(false));
    let hits = result["offending"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0]["file"].as_str().unwrap().contains("Sub/Bad.lean"));
    assert_eq!(hits[0]["line"], json!(2));
    assert_eq!(hits[0]["kind"], json!("sorry"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn admit_native_decide_and_axiom_each_block_success() {
    for (body, kind) in [
        ("theorem t : True := by admit\n", "admit"),
        ("theorem t : 1 = 1 := by native_decide\n", "native_decide"),
        ("axiom cheat : False\n", "axiom"),
    ] {
        let dir = project(&[("A.lean", body)]);
        let _fake = FakePodman::install("", "", 0);
        let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
        assert_eq!(result["verdict"], json!("failed"), "{kind}");
        assert_eq!(result["axioms_ok"], json!(false), "{kind}");
        assert_eq!(result["sorry_count"], json!(0), "{kind}");
        assert_eq!(result["offending"][0]["kind"], json!(kind));
    }
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn commented_forbidden_words_do_not_block_success() {
    let dir = project(&[(
        "A.lean",
        "/- sorry admit native_decide axiom -/\n-- sorry\ndef s := \"axiom\"\ntheorem t : True := trivial\n",
    )]);
    let _fake = FakePodman::install("", "", 0);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["verdict"], json!("successful"));
    assert_eq!(result["axioms_ok"], json!(true));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn lean_reporting_sorry_sets_uses_sorry_without_a_text_hit() {
    let dir = project(&[("A.lean", GOOD_THEOREM)]);
    let _fake = FakePodman::install("A.lean:1:8: warning: declaration uses 'sorry'\n", "", 0);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["uses_sorry"], json!(true));
    assert_eq!(result["sorry_count"], json!(0));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn offences_are_clipped() {
    let body = "def a := sorry\n".repeat(MAX_OFFENCES + 5);
    let dir = project(&[("A.lean", &body)]);
    let _fake = FakePodman::install("", "", 0);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["offending"].as_array().unwrap().len(), MAX_OFFENCES);
    assert_eq!(result["omitted_offending"], json!(5));
    assert_eq!(result["sorry_count"], json!(MAX_OFFENCES + 5));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn hidden_build_and_dependency_directories_are_not_scanned() {
    let dir = project(&[
        (".lake/packages/Mathlib/M.lean", "def x := sorry\n"),
        ("build/B.lean", "def x := sorry\n"),
        ("lake-packages/L.lean", "def x := sorry\n"),
        ("notes.txt", "sorry\n"),
    ]);
    let _fake = FakePodman::install("", "", 0);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["verdict"], json!("successful"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn symlinked_lean_files_are_not_followed_by_the_scan() {
    let outside = project(&[("Evil.lean", "def x := sorry\n")]);
    let dir = project(&[]);
    std::os::unix::fs::symlink(
        outside.path().join("Evil.lean"),
        dir.path().join("Link.lean"),
    )
    .unwrap();
    let _fake = FakePodman::install("", "", 0);
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["axioms_ok"], json!(true));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn nonzero_exit_without_errors_is_an_error_with_redacted_stderr() {
    let dir = project(&[]);
    let _fake = FakePodman::install(
        "",
        "lake: command not found\ntoken ghp_abcdefghijklmnopqrstuvwxyz0123456789\n<<< boom\n",
        127,
    );
    let result = tool_in(dir.path()).execute(json!({})).await.unwrap();
    assert_eq!(result["verdict"], json!("error"));
    let detail = result["detail"].as_str().unwrap();
    assert!(detail.contains("lake: command not found"));
    assert!(!detail.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(detail.contains("&lt;&lt;&lt; boom"));
    assert!(detail.contains("600s"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn checking_a_file_runs_lean_on_it() {
    let dir = project(&[("Sub/A.lean", GOOD_THEOREM)]);
    let fake = FakePodman::install("", "", 0);
    let result = tool_in(dir.path())
        .execute(json!({"file": "Sub/A.lean"}))
        .await
        .unwrap();
    assert_eq!(
        fake.argv().trim_end(),
        "exec -w /workspace lean-container timeout -k 10 600 lake env lean ./Sub/A.lean"
    );
    assert_eq!(result["verdict"], json!("successful"));
    assert_eq!(result["file"], json!("./Sub/A.lean"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn file_checks_use_plain_lean_without_a_lakefile() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "lean-toolchain", "leanprover/lean4:v4.0.0\n");
    write(dir.path(), "A.lean", GOOD_THEOREM);
    let fake = FakePodman::install("", "", 0);
    tool_in(dir.path())
        .execute(json!({"file": "./A.lean"}))
        .await
        .unwrap();
    assert!(fake.argv().trim_end().ends_with("lean ./A.lean"));
    assert!(!fake.argv().contains("lake"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn file_checks_only_scan_the_named_file() {
    let dir = project(&[("A.lean", GOOD_THEOREM), ("Other.lean", "def x := sorry\n")]);
    let _fake = FakePodman::install("", "", 0);
    let ok = tool_in(dir.path())
        .execute(json!({"file": "A.lean"}))
        .await
        .unwrap();
    assert_eq!(ok["axioms_ok"], json!(true));
    let bad = tool_in(dir.path())
        .execute(json!({"file": "Other.lean"}))
        .await
        .unwrap();
    assert_eq!(bad["verdict"], json!("failed"));
    assert_eq!(bad["offending"][0]["line"], json!(1));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn a_file_outside_the_lean_project_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "tests/proofs/lean/lakefile.lean", "");
    write(dir.path(), "src/Other.lean", GOOD_THEOREM);
    let fake = FakePodman::install("", "", 0);
    let message = invalid(
        tool_in(dir.path())
            .execute(json!({"file": "src/Other.lean"}))
            .await
            .unwrap_err(),
    );
    assert!(message.contains("outside the Lean project"));
    assert!(!fake.ran());
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn path_validation_rejects_escapes_before_running() {
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "Evil.lean", GOOD_THEOREM);
    let dir = project(&[("A.lean", GOOD_THEOREM), ("notlean.txt", "x")]);
    std::os::unix::fs::symlink(
        outside.path().join("Evil.lean"),
        dir.path().join("Evil.lean"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
    std::fs::create_dir(dir.path().join("Dir.lean")).unwrap();
    let fake = FakePodman::install("", "", 0);
    let absolute = outside.path().join("Evil.lean").display().to_string();
    let cases = [
        (json!({"file": ""}), "relative path"),
        (json!({"file": "../x.lean"}), "relative path"),
        (json!({"file": "a/../A.lean"}), "relative path"),
        (json!({"file": absolute}), "relative path"),
        (json!({"file": "notlean.txt"}), ".lean file"),
        (json!({"file": "noext"}), ".lean file"),
        (json!({"file": "Evil.lean"}), "outside the workspace"),
        (json!({"file": "out/Evil.lean"}), "outside the workspace"),
        (json!({"file": "Dir.lean"}), "regular file"),
        (json!({"file": 5}), "'file' must be a string"),
        (json!({"target": 5}), "'target' must be a string"),
        (json!({"target": "--help"}), "'target'"),
        (json!({"target": "A", "file": "A.lean"}), "not both"),
        (json!({"timeout_secs": 0}), "'timeout_secs'"),
        (json!({"timeout_secs": 3601}), "'timeout_secs'"),
        (json!({"timeout_secs": "x"}), "'timeout_secs'"),
        (json!({"extra": 1}), "extra"),
    ];
    for (args, needle) in cases {
        let message = invalid(tool_in(dir.path()).execute(args.clone()).await.unwrap_err());
        assert!(message.contains(needle), "{args}: {message}");
    }
    assert!(!fake.ran());
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn a_missing_file_or_project_is_an_execution_error() {
    let dir = project(&[]);
    let fake = FakePodman::install("", "", 0);
    let message = failed(
        tool_in(dir.path())
            .execute(json!({"file": "Nope.lean"}))
            .await
            .unwrap_err(),
    );
    assert!(message.contains("not found"));
    let empty = tempfile::tempdir().unwrap();
    let message = failed(tool_in(empty.path()).execute(json!({})).await.unwrap_err());
    assert!(message.contains("lakefile"));
    assert!(!fake.ran());
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn scan_failures_fail_closed_before_running() {
    let big = project(&[("Big.lean", &"x".repeat(MAX_SCAN_BYTES as usize + 1))]);
    let fake = FakePodman::install("", "", 0);
    let message = failed(tool_in(big.path()).execute(json!({})).await.unwrap_err());
    assert!(message.contains("cannot be scanned"));
    let many = project(&[]);
    for n in 0..=MAX_SCAN_FILES {
        std::fs::write(many.path().join(format!("F{n}.lean")), "").unwrap();
    }
    let message = failed(tool_in(many.path()).execute(json!({})).await.unwrap_err());
    assert!(message.contains("more than"));
    assert!(!fake.ran());
}

#[test]
fn scan_files_reports_unreadable_inputs() {
    let dir = project(&[("A.lean", "def x := 1")]);
    assert!(scan_files(dir.path(), Path::new("missing"), None).is_err());
    assert!(scan_files(
        dir.path(),
        Path::new(""),
        Some(&dir.path().join("none.lean"))
    )
    .is_err());
    let hits = scan_files(dir.path(), Path::new(""), None).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn invalid_utf8_sources_are_still_scanned() {
    let dir = project(&[]);
    std::fs::write(dir.path().join("B.lean"), b"def x := \xff sorry\n").unwrap();
    let hits = scan_files(dir.path(), Path::new(""), None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].file, "B.lean");
}

#[tokio::test]
async fn no_container_runtime_is_an_execution_error() {
    let dir = project(&[("A.lean", GOOD_THEOREM)]);
    let tool = LeanCheckTool::new(
        dir.path().to_path_buf(),
        Arc::new(ContainerHandle {
            name: "x".to_string(),
            runtime: ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        }),
        CONTAINER_WORKSPACE_DIR,
    );
    let message = failed(tool.execute(json!({})).await.unwrap_err());
    assert!(message.contains("No container runtime"));
}

#[test]
fn limits_and_effect_class_are_declared() {
    let tool = tool_in(Path::new("."));
    assert_eq!(tool.effect_class(), EffectClass::Workspace);
    assert_eq!(tool.name(), LEAN_CHECK);
    assert_eq!(DEFAULT_TIMEOUT_SECS, 600);
    assert_eq!(MAX_TIMEOUT_SECS, 3600);
    let definition = serde_json::to_value(tool.definition()).unwrap().to_string();
    assert!(definition.contains("lean_check"));
    assert!(definition.contains("sorry"));
    assert_eq!(Verdict::Successful.as_str(), "successful");
}

#[test]
fn registered_in_container_registries_only_when_a_lean_project_exists() {
    let dir = tempfile::tempdir().unwrap();
    let none = create_container_tool_registry(dir.path(), handle(), CONTAINER_WORKSPACE_DIR);
    assert!(none.get_tool(LEAN_CHECK).is_none());
    write(dir.path(), "tests/proofs/lean/lean-toolchain", "x");
    let proofs = create_container_tool_registry(dir.path(), handle(), CONTAINER_WORKSPACE_DIR);
    assert!(proofs.get_tool(LEAN_CHECK).is_some());
    let root = project(&[]);
    write(root.path(), "Cargo.toml", "[package]\nname = \"x\"\n");
    let with = create_container_tool_registry(root.path(), handle(), CONTAINER_WORKSPACE_DIR);
    assert!(with.get_tool(LEAN_CHECK).is_some());
}

proptest! {
    #[test]
    fn code_hits_are_found_and_comment_hits_are_not(
        word in prop::sample::select(vec!["sorry", "admit", "native_decide", "axiom"]),
        pad in "[a-z ]{0,10}",
    ) {
        let code = format!("{pad} {word}");
        prop_assert_eq!(scan_source(&code).len(), 1);
        let line = format!("-- {code}");
        let block = format!("/- {code} -/");
        let quoted = format!("\"{word}\"");
        let longer = format!("{word}x {word}_");
        prop_assert!(scan_source(&line).is_empty());
        prop_assert!(scan_source(&block).is_empty());
        prop_assert!(scan_source(&quoted).is_empty());
        prop_assert!(scan_source(&longer).is_empty());
    }

    #[test]
    fn identifiers_without_forbidden_words_are_clean(src in "[a-zA-Z0-9_ \n:=().]{0,80}") {
        let tokens_ok = !src.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|t| matches!(t, "sorry" | "admit" | "native_decide" | "axiom" | "sorryAx"));
        prop_assume!(tokens_ok);
        prop_assert!(scan_source(&src).is_empty());
    }

    #[test]
    fn target_names_with_flag_or_shell_characters_are_rejected(
        name in "[A-Za-z0-9_]{1,20}",
        bad in "[ ;|&$`'\"\\\\<>*=,!-]",
        at in 0usize..20,
    ) {
        let mut tainted = name.clone();
        tainted.insert_str(at.min(name.len()), &bad);
        prop_assert!(!valid_target(&tainted));
        prop_assert!(valid_target(&name));
    }

    #[test]
    fn error_lines_roundtrip(line in 1u64..100_000, col in 0u64..500, msg in "[a-z][a-z ]{0,30}") {
        let lean = parse_errors(&format!("F.lean:{line}:{col}: error: {msg}\n"));
        prop_assert_eq!(lean[0].line, line);
        prop_assert_eq!(lean[0].message.as_str(), msg.trim());
        let lake = parse_errors(&format!("error: F.lean:{line}:{col}: {msg}\n"));
        prop_assert_eq!(lake[0].line, line);
        prop_assert_eq!(lake[0].file.as_deref(), Some("F.lean"));
    }
}
