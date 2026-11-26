#![cfg(unix)]

use std::path::{Path, PathBuf};

const PI_BIN_ENV: &str = "NANNA_PI_BIN";
const REQUIRE_PI_ENV: &str = "NANNA_REQUIRE_PI";

const SKIP_BANNER: &str = "\n\
    ################################################################\n\
    # SKIPPED: the real-pi hub test did NOT run.                    #\n\
    # NANNA_PI_BIN is unset and NANNA_REQUIRE_PI is not 1.          #\n\
    # Nothing here verified the pi steer shape. Set NANNA_PI_BIN    #\n\
    # to a pi build, or NANNA_REQUIRE_PI=1 to make absence a FAIL.  #\n\
    ################################################################";

fn is_usable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path);
    meta.is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn usable_pi(bin: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(bin);
    let usable = !bin.is_empty() && is_usable(&path);
    let why = format!("{PI_BIN_ENV} is set to `{bin}`, which is not an executable file");
    if usable {
        Ok(path)
    } else {
        Err(why)
    }
}

fn absent_pi(require: Option<&str>) -> Result<Option<PathBuf>, String> {
    let why = format!("{REQUIRE_PI_ENV}=1 requires pi but {PI_BIN_ENV} is not set");
    if require == Some("1") {
        Err(why)
    } else {
        Ok(None)
    }
}

fn pi_gate(bin: Option<&str>, require: Option<&str>) -> Result<Option<PathBuf>, String> {
    match bin {
        Some(bin) => usable_pi(bin).map(Some),
        None => absent_pi(require),
    }
}

fn real_scenario(pi: &Path) -> Result<String, String> {
    Err(format!(
        "pi is present at {} but the container-driven steer scenario is not implemented: it needs \
         the pinned agent image and a model gateway reachable from the no-network container; \
         nothing was verified",
        pi.display()
    ))
}

fn run(bin: Option<&str>, require: Option<&str>) -> Result<String, String> {
    let verdict = pi_gate(bin, require)?;
    let ran = verdict.as_ref().map(|pi| real_scenario(pi)).transpose()?;
    Ok(ran.unwrap_or_else(|| SKIP_BANNER.to_string()))
}

#[test]
fn real_pi_steers_a_running_turn_and_msg_recv_then_returns_the_fenced_message() {
    let bin = std::env::var(PI_BIN_ENV).ok();
    let require = std::env::var(REQUIRE_PI_ENV).ok();
    let report = run(bin.as_deref(), require.as_deref());
    eprintln!("{}", report.unwrap_or_else(|why| panic!("{why}")));
}

fn executable(dir: &Path, name: &str, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[test]
fn a_pi_that_is_set_but_missing_empty_a_directory_or_not_executable_fails_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let plain = executable(dir.path(), "plain", 0o644);
    let missing = dir.path().join("missing");
    for bad in [
        plain.as_path(),
        missing.as_path(),
        dir.path(),
        Path::new(""),
    ] {
        let text = bad.to_str().unwrap();
        for require in [None, Some("1"), Some("0")] {
            let verdict = pi_gate(Some(text), require);
            let why = verdict.unwrap_err();
            assert!(why.contains(PI_BIN_ENV), "{why}");
        }
    }
}

#[test]
fn a_usable_pi_passes_the_gate_whatever_the_require_flag_says() {
    let dir = tempfile::tempdir().unwrap();
    let pi = executable(dir.path(), "pi", 0o755);
    let text = pi.to_str().unwrap();
    for require in [None, Some("1"), Some("0")] {
        assert_eq!(pi_gate(Some(text), require), Ok(Some(pi.clone())));
    }
}

#[test]
fn an_absent_pi_fails_only_when_it_is_required() {
    let why = pi_gate(None, Some("1")).unwrap_err();
    assert!(
        why.contains(REQUIRE_PI_ENV) && why.contains(PI_BIN_ENV),
        "{why}"
    );
    for lenient in [None, Some(""), Some("0"), Some("true"), Some("yes")] {
        assert_eq!(pi_gate(None, lenient), Ok(None));
    }
}

#[test]
fn an_unconfigured_run_skips_with_a_loud_banner_and_passes() {
    let report = run(None, None).unwrap();
    assert_eq!(report, SKIP_BANNER);
    assert!(report.contains("SKIPPED") && report.contains(PI_BIN_ENV));
    assert_eq!(run(None, Some("0")).unwrap(), SKIP_BANNER);
}

#[test]
fn a_required_but_absent_pi_fails_the_run() {
    assert!(run(None, Some("1")).is_err());
}

#[test]
fn a_present_pi_never_passes_without_the_container_scenario_having_run() {
    let dir = tempfile::tempdir().unwrap();
    let pi = executable(dir.path(), "pi", 0o755);
    let why = run(Some(pi.to_str().unwrap()), None).unwrap_err();
    assert!(why.contains("nothing was verified"), "{why}");
}
