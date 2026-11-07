use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::container::ContainerHandle;

pub const MAX_CAPTURE_BYTES: usize = 64 * 1024;

const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRun {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Executes one check command and reports how it ended. Implementations
/// must run the command inside the dev container; the host never runs
/// repository-declared commands.
pub trait CheckRunner: Send + Sync {
    fn run(&self, command: &str, timeout: Duration) -> Result<RawRun, String>;
}

/// Runs `sh -c <command>` through a launcher that executes inside a
/// container, such as `podman exec -w /workspace <name>`.
///
/// ```
/// use harness::verify::{CheckRunner, ProcessCheckRunner};
/// use std::time::Duration;
///
/// let runner = ProcessCheckRunner::new("env", vec![]);
/// let run = runner.run("echo hi", Duration::from_secs(10)).unwrap();
/// assert_eq!(run.exit_code, Some(0));
/// assert_eq!(run.stdout, "hi\n");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCheckRunner {
    program: String,
    prefix: Vec<String>,
}

impl ProcessCheckRunner {
    pub fn new(program: impl Into<String>, prefix: Vec<String>) -> Self {
        Self {
            program: program.into(),
            prefix,
        }
    }

    pub fn for_container(handle: &ContainerHandle, working_dir: &str) -> Self {
        Self::new(
            handle.runtime.command(),
            vec![
                "exec".to_string(),
                "-w".to_string(),
                working_dir.to_string(),
                handle.name.clone(),
            ],
        )
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn prefix(&self) -> &[String] {
        &self.prefix
    }
}

fn wait_failure(program: &str, error: &std::io::Error) -> String {
    format!("cannot wait for `{program}`: {error}")
}

pub(crate) fn read_tail(mut reader: impl Read) -> String {
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.extend_from_slice(&chunk[..n]);
                if kept.len() > 2 * MAX_CAPTURE_BYTES {
                    let excess = kept.len() - MAX_CAPTURE_BYTES;
                    kept.drain(..excess);
                }
            }
        }
    }
    if kept.len() > MAX_CAPTURE_BYTES {
        let excess = kept.len() - MAX_CAPTURE_BYTES;
        kept.drain(..excess);
    }
    String::from_utf8_lossy(&kept).into_owned()
}

impl CheckRunner for ProcessCheckRunner {
    fn run(&self, command: &str, timeout: Duration) -> Result<RawRun, String> {
        let mut child = Command::new(&self.program)
            .args(&self.prefix)
            .args(["sh", "-c", command])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start `{}`: {e}", self.program))?;
        let stdout = child
            .stdout
            .take()
            .map(|r| std::thread::spawn(move || read_tail(r)));
        let stderr = child
            .stderr
            .take()
            .map(|r| std::thread::spawn(move || read_tail(r)));
        let deadline = Instant::now() + timeout;
        let (exit_code, timed_out) = loop {
            let waited = child
                .try_wait()
                .map_err(|e| wait_failure(&self.program, &e))?;
            match waited {
                Some(status) => break (status.code(), false),
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (None, true);
                }
                None => std::thread::sleep(POLL_INTERVAL),
            }
        };
        let collect = |handle: Option<std::thread::JoinHandle<String>>| {
            handle.and_then(|h| h.join().ok()).unwrap_or_default()
        };
        Ok(RawRun {
            exit_code,
            timed_out,
            stdout: collect(stdout),
            stderr: collect(stderr),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{ContainerHandle, ContainerRuntime};
    use proptest::prelude::*;

    fn host() -> ProcessCheckRunner {
        ProcessCheckRunner::new("env", vec![])
    }

    const LONG: Duration = Duration::from_secs(30);

    #[test]
    fn captures_stdout_stderr_and_a_zero_exit() {
        let run = host().run("echo out; echo err >&2", LONG).unwrap();
        assert_eq!(run.exit_code, Some(0));
        assert!(!run.timed_out);
        assert_eq!(run.stdout, "out\n");
        assert_eq!(run.stderr, "err\n");
    }

    #[test]
    fn reports_the_exact_nonzero_exit_code() {
        let run = host().run("exit 101", LONG).unwrap();
        assert_eq!(run.exit_code, Some(101));
        assert!(!run.timed_out);
    }

    #[test]
    fn a_command_that_outlives_its_timeout_is_killed_and_flagged() {
        let started = Instant::now();
        let run = host()
            .run("exec sleep 30", Duration::from_millis(200))
            .unwrap();
        assert!(run.timed_out);
        assert_eq!(run.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn a_signalled_command_has_no_exit_code() {
        let run = host().run("kill -9 $$", LONG).unwrap();
        assert_eq!(run.exit_code, None);
        assert!(!run.timed_out);
    }

    #[test]
    fn a_missing_launcher_is_an_error_not_a_failed_check() {
        let runner = ProcessCheckRunner::new("/nonexistent/launcher", vec![]);
        let err = runner.run("true", LONG).unwrap_err();
        assert!(err.contains("cannot start"), "{err}");
    }

    #[test]
    fn the_command_reaches_the_shell_as_one_argument() {
        let run = host()
            .run("printf '%s' \"$0\" ; echo ';' && echo $((1+2))", LONG)
            .unwrap();
        assert!(run.stdout.contains('3'), "{}", run.stdout);
    }

    #[test]
    fn huge_output_is_capped_to_its_tail() {
        let run = host()
            .run("yes 0123456789 | head -c 1000000; echo END", LONG)
            .unwrap();
        assert!(run.stdout.len() <= MAX_CAPTURE_BYTES);
        assert!(run.stdout.ends_with("END\n"));
    }

    #[test]
    fn a_wait_failure_names_the_launcher_and_the_cause() {
        let error = std::io::Error::other("lost child");
        assert_eq!(
            wait_failure("podman", &error),
            "cannot wait for `podman`: lost child"
        );
    }

    #[test]
    fn for_container_builds_an_exec_launcher() {
        let handle = ContainerHandle {
            name: "nanna-task-1".to_string(),
            runtime: ContainerRuntime::Podman,
            port: None,
            needs_cleanup: false,
        };
        let runner = ProcessCheckRunner::for_container(&handle, "/workspace");
        assert_eq!(runner.program(), "podman");
        assert_eq!(
            runner.prefix(),
            ["exec", "-w", "/workspace", "nanna-task-1"]
        );
    }

    proptest! {
        #[test]
        fn read_tail_keeps_the_last_bytes_of_any_input(data in prop::collection::vec(any::<u8>(), 0..300_000)) {
            let tail = read_tail(&data[..]);
            let expected = &data[data.len().saturating_sub(MAX_CAPTURE_BYTES)..];
            prop_assert_eq!(tail, String::from_utf8_lossy(expected).into_owned());
        }
    }
}
