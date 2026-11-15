use chrono::Utc;
use harness::escalation::{
    Escalation, EscalationLog, EscalationSource, Severity, AGENT_SESSION_ENV,
};
use std::path::Path;
use std::process::{Command, Output};

const REPO: &str = "example/repo";
const ID: &str = "inc-1";

fn held_log(path: &Path) {
    let log = EscalationLog::open(path).unwrap();
    let escalation = Escalation::new(
        Severity::Incident,
        EscalationSource::Auditor,
        REPO,
        "rollout halted",
    )
    .with_id(ID);
    log.hold(&escalation, Utc::now()).unwrap();
    assert!(log.production_held(REPO));
}

fn still_held(path: &Path) -> bool {
    EscalationLog::open(path).unwrap().production_held(REPO)
}

fn resolve(path: &Path, id: &str, agent_session: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanna"));
    command
        .args(["escalation", "resolve", id, "--path"])
        .arg(path)
        .env_remove(AGENT_SESSION_ENV);
    if let Some(marker) = agent_session {
        command.env(AGENT_SESSION_ENV, marker);
    }
    command.output().unwrap()
}

#[test]
fn resolve_without_a_terminal_is_refused_and_the_hold_stays() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("escalations.jsonl");
    held_log(&path);
    let output = resolve(&path, ID, None);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stdin and stdout are not a terminal"),
        "{stderr}"
    );
    assert!(still_held(&path));
}

#[test]
fn resolve_under_an_agent_session_marker_is_refused_and_the_hold_stays() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("escalations.jsonl");
    held_log(&path);
    let output = resolve(&path, ID, Some("task-1"));
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "{AGENT_SESSION_ENV}=task-1 marks an agent session"
        )),
        "{stderr}"
    );
    assert!(still_held(&path));
}

#[cfg(unix)]
mod terminal {
    use super::*;
    use std::fs::File;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::Stdio;

    fn pty() -> (OwnedFd, OwnedFd) {
        let mut master = 0;
        let mut slave = 0;
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(rc, 0, "openpty failed");
        unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
    }

    fn resolve_on_a_terminal(path: &Path, id: &str) -> (bool, String) {
        let (master, slave) = pty();
        let stdin = File::from(slave.try_clone().unwrap());
        let stdout = File::from(slave);
        let output = Command::new(env!("CARGO_BIN_EXE_nanna"))
            .args(["escalation", "resolve", id, "--path"])
            .arg(path)
            .env_remove(AGENT_SESSION_ENV)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        drop(master);
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    #[test]
    fn resolve_on_a_terminal_clears_the_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escalations.jsonl");
        held_log(&path);
        let (success, stderr) = resolve_on_a_terminal(&path, ID);
        assert!(success, "{stderr}");
        assert!(!still_held(&path));
    }

    #[test]
    fn resolve_on_a_terminal_of_an_unknown_id_fails_and_keeps_the_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escalations.jsonl");
        held_log(&path);
        let (success, stderr) = resolve_on_a_terminal(&path, "inc-404");
        assert!(!success);
        assert!(stderr.contains("inc-404"), "{stderr}");
        assert!(still_held(&path));
    }
}
