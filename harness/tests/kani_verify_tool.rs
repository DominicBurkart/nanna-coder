use harness::container::{ContainerHandle, ContainerRuntime};
use harness::effects::EffectClass;
use harness::kani_tool::{
    kani_args, manifest_mentions_kani, parse_output, valid_harness, valid_package, KaniVerifyTool,
    Verdict, DEFAULT_TIMEOUT_SECS, KANI_VERIFY, MAX_FAILING_CHECKS, MAX_TIMEOUT_SECS, MAX_UNWIND,
};
use harness::tools::{create_container_tool_registry, Tool, ToolError, CONTAINER_WORKSPACE_DIR};
use proptest::prelude::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SUCCESS_OUT: &str = "Kani Rust Verifier 0.60.0\nChecking harness check_add...\nVERIFICATION:- SUCCESSFUL\n\nSUMMARY:\n ** 0 of 31 failed\n\nComplete - 1 successfully verified harnesses, 0 failures, 1 total.\n";
const FAILED_OUT: &str = "Checking harness check_add...\n\nCheck 2: check_add.assertion.1\n\t - Status: FAILURE\n\t - Description: \"assertion failed: a + b > a\"\n\t - Location: src/lib.rs:9:5 in function check_add\n\nCheck 3: check_add.unwind.0\n\t - Status: FAILURE\n\t - Description: \"unwinding assertion loop 0\"\n\t - Location: src/lib.rs:7:5 in function check_add\n\nCheck 4: check_add.overflow.1\n\t - Status: SUCCESS\n\t - Description: \"ok check\"\n\nSUMMARY:\n ** 2 of 31 failed\nFailed Checks: assertion failed: a + b > a\n File: \"src/lib.rs\", line 9, in check_add\n\nVERIFICATION:- FAILED\n";

fn handle() -> Arc<ContainerHandle> {
    Arc::new(ContainerHandle {
        name: "kani-container".to_string(),
        runtime: ContainerRuntime::Podman,
        port: None,
        needs_cleanup: false,
    })
}

fn tool() -> KaniVerifyTool {
    KaniVerifyTool::new(PathBuf::from("."), handle(), CONTAINER_WORKSPACE_DIR)
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

#[tokio::test]
#[serial_test::serial(env_path)]
async fn successful_verification_reports_counts() {
    let fake = FakePodman::install(SUCCESS_OUT, "", 0);
    let result = tool()
        .execute(json!({"harness": "check_add"}))
        .await
        .unwrap();
    assert_eq!(
        fake.argv().trim_end(),
        "exec -w /workspace kani-container timeout -k 10 900 cargo kani --harness check_add --exact"
    );
    assert_eq!(result["verdict"], json!("successful"));
    assert_eq!(result["harness"], json!("check_add"));
    assert_eq!(result["checks_failed"], json!(0));
    assert_eq!(result["checks_total"], json!(31));
    assert_eq!(result["failing_checks"], json!([]));
    assert_eq!(result["unwinding_failures"], json!(0));
    assert_eq!(result["detail"], Value::Null);
    assert!(result["notice"].as_str().unwrap().contains("untrusted"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn failed_verification_lists_fenced_failing_checks() {
    let _fake = FakePodman::install(FAILED_OUT, "", 1);
    let result = tool()
        .execute(json!({"harness": "check_add", "unwind": 4}))
        .await
        .unwrap();
    assert_eq!(result["verdict"], json!("failed"));
    assert_eq!(result["checks_failed"], json!(2));
    assert_eq!(result["checks_total"], json!(31));
    assert_eq!(result["unwinding_failures"], json!(1));
    assert_eq!(result["unwind"], json!(4));
    let checks = result["failing_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    assert!(checks[0]
        .as_str()
        .unwrap()
        .contains("assertion failed: a + b &gt; a"));
    assert!(checks[1]
        .as_str()
        .unwrap()
        .contains("unwinding assertion loop 0"));
    assert!(!result.to_string().contains("ok check"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn package_unwind_and_timeout_reach_the_command() {
    let fake = FakePodman::install(SUCCESS_OUT, "", 0);
    tool()
        .execute(json!({"harness": "m::h", "package": "core-lib", "unwind": 9, "timeout_secs": 30}))
        .await
        .unwrap();
    assert!(fake.argv().trim_end().ends_with(
        "timeout -k 10 30 cargo kani --harness m::h --exact --package core-lib --default-unwind 9"
    ));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn missing_verdict_is_an_error_with_redacted_fenced_stderr() {
    let _fake = FakePodman::install(
        "",
        "error: no such command: kani\ntoken ghp_abcdefghijklmnopqrstuvwxyz0123456789\n<<< boom\n",
        101,
    );
    let result = tool()
        .execute(json!({"harness": "check_add"}))
        .await
        .unwrap();
    assert_eq!(result["verdict"], json!("error"));
    assert_eq!(result["checks_total"], Value::Null);
    let detail = result["detail"].as_str().unwrap();
    assert!(detail.contains("no verification verdict"));
    assert!(detail.contains("no such command: kani"));
    assert!(!detail.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(detail.contains("&lt;&lt;&lt; boom"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn a_success_line_with_a_failing_exit_is_an_error() {
    let _fake = FakePodman::install(SUCCESS_OUT, "", 1);
    let result = tool()
        .execute(json!({"harness": "check_add"}))
        .await
        .unwrap();
    assert_eq!(result["verdict"], json!("error"));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn failing_checks_are_clipped() {
    let mut out = String::from("VERIFICATION:- FAILED\n");
    for n in 0..30 {
        out.push_str(&format!("Failed Checks: problem number {n}\n"));
    }
    let _fake = FakePodman::install(&out, "", 1);
    let result = tool()
        .execute(json!({"harness": "check_add"}))
        .await
        .unwrap();
    assert_eq!(
        result["failing_checks"].as_array().unwrap().len(),
        MAX_FAILING_CHECKS
    );
    assert_eq!(result["omitted_failing_checks"], json!(10));
}

#[tokio::test]
#[serial_test::serial(env_path)]
async fn no_container_runtime_is_an_execution_error() {
    let dir = tempfile::tempdir().unwrap();
    let saved = std::env::var_os("PATH");
    std::env::set_var("PATH", dir.path());
    let outcome = tool().execute(json!({"harness": "check_add"})).await;
    match saved {
        Some(path) => std::env::set_var("PATH", path),
        None => std::env::remove_var("PATH"),
    }
    assert!(!failed(outcome.unwrap_err()).is_empty());
}

#[tokio::test]
async fn argument_validation_rejects_bad_input_before_running() {
    let cases = [
        (json!({}), "'harness'"),
        (json!({"harness": 5}), "'harness' must be a string"),
        (json!({"harness": "--help"}), "'harness'"),
        (json!({"harness": "a b"}), "'harness'"),
        (json!({"harness": ":h"}), "'harness'"),
        (json!({"harness": "h", "package": "-p"}), "'package'"),
        (
            json!({"harness": "h", "package": 3}),
            "'package' must be a string",
        ),
        (json!({"harness": "h", "unwind": 0}), "'unwind'"),
        (json!({"harness": "h", "unwind": 1001}), "'unwind'"),
        (json!({"harness": "h", "unwind": "x"}), "'unwind'"),
        (json!({"harness": "h", "timeout_secs": 0}), "'timeout_secs'"),
        (
            json!({"harness": "h", "timeout_secs": 3601}),
            "'timeout_secs'",
        ),
        (json!({"harness": "h", "extra": 1}), "extra"),
    ];
    for (args, needle) in cases {
        let message = invalid(tool().execute(args.clone()).await.unwrap_err());
        assert!(message.contains(needle), "{args}: {message}");
    }
}

#[test]
fn limits_and_effect_class_are_declared() {
    assert_eq!(tool().effect_class(), EffectClass::Workspace);
    assert_eq!(tool().name(), KANI_VERIFY);
    assert_eq!(DEFAULT_TIMEOUT_SECS, 900);
    assert_eq!(MAX_TIMEOUT_SECS, 3600);
    assert_eq!(MAX_UNWIND, 1000);
    let definition = serde_json::to_value(tool().definition()).unwrap();
    assert!(definition.to_string().contains("kani_verify"));
    assert!(definition.to_string().contains("harness"));
}

#[test]
fn parse_output_covers_verdicts() {
    assert_eq!(parse_output(SUCCESS_OUT, 0).verdict, Verdict::Successful);
    assert_eq!(parse_output(FAILED_OUT, 1).verdict, Verdict::Failed);
    assert_eq!(parse_output(FAILED_OUT, 0).verdict, Verdict::Failed);
    assert_eq!(parse_output(SUCCESS_OUT, 1).verdict, Verdict::Error);
    assert_eq!(parse_output("", 1).verdict, Verdict::Error);
    let both = format!("{SUCCESS_OUT}{FAILED_OUT}");
    assert_eq!(parse_output(&both, 1).verdict, Verdict::Error);
    let summary = parse_output(FAILED_OUT, 1);
    assert_eq!(
        summary.failing_checks,
        vec!["assertion failed: a + b > a", "unwinding assertion loop 0"]
    );
    assert_eq!(summary.unwinding_failures(), 1);
}

#[test]
fn registered_in_container_registries_only_when_the_manifest_mentions_kani() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let plain = create_container_tool_registry(dir.path(), handle(), CONTAINER_WORKSPACE_DIR);
    assert!(plain.get_tool(KANI_VERIFY).is_none());
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[lints.rust]\nunexpected_cfgs = { check-cfg = ['cfg(kani)'] }\n",
    )
    .unwrap();
    assert!(manifest_mentions_kani(dir.path()));
    let with = create_container_tool_registry(dir.path(), handle(), CONTAINER_WORKSPACE_DIR);
    assert!(with.get_tool(KANI_VERIFY).is_some());
    assert!(!manifest_mentions_kani(Path::new("/nonexistent-nanna-dir")));
}

proptest! {
    #[test]
    fn harness_names_with_shell_or_flag_characters_are_rejected(
        name in "[A-Za-z0-9_:]{1,20}",
        bad in "[ ;|&$`'\"/\\\\<>*=,.-]",
        at in 0usize..20,
    ) {
        let at = at.min(name.len());
        let mut tainted = name.clone();
        tainted.insert_str(at, &bad);
        prop_assert!(!valid_harness(&tainted));
        let slashed = format!("{}/{}", name, bad);
        prop_assert!(!valid_package(&slashed));
        prop_assert!(valid_package(&name.replace(':', "_")));
    }

    #[test]
    fn plain_names_are_accepted_unless_they_start_with_a_flag_or_colon(name in "[A-Za-z0-9_][A-Za-z0-9_:]{0,40}") {
        prop_assert!(valid_harness(&name));
        let args = kani_args(&name, None, None, 5);
        prop_assert_eq!(args.iter().filter(|a| a.starts_with('-') && *a != "-k").count(), 2);
    }

    #[test]
    fn counts_roundtrip_through_the_summary_line(failed in 0u64..10_000, extra in 0u64..10_000) {
        let total = failed + extra;
        let out = format!("SUMMARY:\n ** {failed} of {total} failed\nVERIFICATION:- FAILED\n");
        let summary = parse_output(&out, 1);
        prop_assert_eq!(summary.checks_failed, Some(failed));
        prop_assert_eq!(summary.checks_total, Some(total));
    }
}
