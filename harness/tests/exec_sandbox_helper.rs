use std::process::Command;

fn helper() -> Command {
    Command::new(env!("CARGO_BIN_EXE_nanna-sandbox"))
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[test]
fn selftest_passes_as_root_and_refuses_otherwise() {
    let out = helper().arg("selftest").output().unwrap();
    if is_root() {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    } else {
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("must start as root"));
    }
}

#[test]
fn a_task_uid_outside_the_range_never_executes_the_program() {
    let out = helper()
        .args(["run", "--uid", "0", "--", "/bin/sh", "-c", "echo ran"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("ran"));
}

#[test]
fn a_non_root_helper_refuses_to_run_the_program() {
    if is_root() {
        return;
    }
    let out = helper()
        .args(["run", "--uid", "20001", "--", "/bin/sh", "-c", "echo ran"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(126));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("ran"));
}

#[test]
fn the_probe_function_matches_the_selftest() {
    let result =
        harness::exec_sandbox::probe(std::path::Path::new(env!("CARGO_BIN_EXE_nanna-sandbox")));
    assert_eq!(result.is_ok(), is_root());
}
