use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::container::ContainerHandle;

pub const MAX_CAPTURE_BYTES: usize = 64 * 1024;

const POLL_INTERVAL: Duration = Duration::from_millis(25);

const READER_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRun {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

#[doc = r#"Executes one check command and reports how it ended. Implementations
must run the command inside the dev container; the host never runs
repository-declared commands."#]
pub trait CheckRunner: Send + Sync {
    fn run(&self, command: &str, timeout: Duration) -> Result<RawRun, String>;
}

#[doc = r#"Runs `sh -c <command>` through a launcher that executes inside a
container, such as `podman exec -w /workspace <name>`. When a check times
out it also force-removes that container (`<runtime> rm -f <name>`), because
killing the `exec` client does not stop the process inside it. The only way to build
one outside tests is [`ProcessCheckRunner::for_container`], so the host can
never be made to run a repository-declared command through it.

```
use harness::container::{ContainerHandle, ContainerRuntime};
use harness::verify::ProcessCheckRunner;

let handle = ContainerHandle {
    name: "nanna-task-1".to_string(),
    runtime: ContainerRuntime::Podman,
    port: None,
    needs_cleanup: false,
};
let runner = ProcessCheckRunner::for_container(&handle, "/workspace");
assert_eq!(runner.program(), "podman");
assert_eq!(runner.prefix(), ["exec", "-w", "/workspace", "nanna-task-1"]);
```

A launcher for an arbitrary host program cannot be built:

```compile_fail,E0599
use harness::verify::ProcessCheckRunner;

let _ = ProcessCheckRunner::new("env", vec![]);
```"#]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCheckRunner {
    program: String,
    prefix: Vec<String>,
    teardown: Option<Vec<String>>,
}

impl ProcessCheckRunner {
    #[cfg(test)]
    pub(crate) fn new(program: impl Into<String>, prefix: Vec<String>) -> Self {
        Self {
            program: program.into(),
            prefix,
            teardown: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_teardown(mut self, argv: Vec<String>) -> Self {
        self.teardown = Some(argv);
        self
    }

    pub fn for_container(handle: &ContainerHandle, working_dir: &str) -> Self {
        Self {
            program: handle.runtime.command().to_string(),
            prefix: vec![
                "exec".to_string(),
                "-w".to_string(),
                working_dir.to_string(),
                handle.name.clone(),
            ],
            teardown: Some(vec![
                handle.runtime.command().to_string(),
                "rm".to_string(),
                "-f".to_string(),
                handle.name.clone(),
            ]),
        }
    }

    pub fn teardown(&self) -> Option<&[String]> {
        self.teardown.as_deref()
    }

    fn tear_down(&self) -> Option<String> {
        let (program, args) = self.teardown.as_ref()?.split_first()?;
        match Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(format!(
                "container teardown `{program}` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(e) => Some(format!("cannot start container teardown `{program}`: {e}")),
        }
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

fn polled(
    result: std::io::Result<Option<ExitStatus>>,
    program: &str,
) -> Result<Option<ExitStatus>, String> {
    result.map_err(|e| wait_failure(program, &e))
}

fn read_into(mut reader: impl Read, sink: &Mutex<Vec<u8>>) {
    let mut chunk = [0u8; 8192];
    while let Ok(n @ 1..) = reader.read(&mut chunk) {
        let mut kept = sink.lock().unwrap_or_else(PoisonError::into_inner);
        kept.extend_from_slice(&chunk[..n]);
        if kept.len() > 2 * MAX_CAPTURE_BYTES {
            let excess = kept.len() - MAX_CAPTURE_BYTES;
            kept.drain(..excess);
        }
    }
}

fn tail_text(sink: &Mutex<Vec<u8>>) -> String {
    let kept = sink.lock().unwrap_or_else(PoisonError::into_inner);
    let start = kept.len().saturating_sub(MAX_CAPTURE_BYTES);
    String::from_utf8_lossy(&kept[start..]).into_owned()
}

#[cfg(test)]
pub(crate) fn read_tail(reader: impl Read) -> String {
    let sink = Mutex::new(Vec::new());
    read_into(reader, &sink);
    tail_text(&sink)
}

struct Capture {
    sink: Arc<Mutex<Vec<u8>>>,
    done: Receiver<()>,
}

fn capture(stream: Option<impl Read + Send + 'static>) -> Option<Capture> {
    stream.map(|reader| {
        let sink: Arc<Mutex<Vec<u8>>> = Arc::default();
        let (finished, done) = mpsc::channel();
        let writer = Arc::clone(&sink);
        std::thread::spawn(move || {
            read_into(reader, &writer);
            let _ = finished.send(());
        });
        Capture { sink, done }
    })
}

fn collect(capture: Option<Capture>) -> String {
    capture.map_or_else(String::new, |capture| {
        let _ = capture.done.recv_timeout(READER_GRACE);
        tail_text(&capture.sink)
    })
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
        let stdout = capture(child.stdout.take());
        let stderr = capture(child.stderr.take());
        let deadline = Instant::now() + timeout;
        let (exit_code, timed_out) = loop {
            let waited = polled(child.try_wait(), &self.program)?;
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
        let teardown_note = if timed_out { self.tear_down() } else { None };
        let mut stderr = collect(stderr);
        if let Some(note) = teardown_note {
            if !stderr.is_empty() && !stderr.ends_with('\n') {
                stderr.push('\n');
            }
            stderr.push_str(&note);
        }
        Ok(RawRun {
            exit_code,
            timed_out,
            stdout: collect(stdout),
            stderr,
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn output_printed_before_a_timeout_is_kept() {
        let run = host()
            .run("echo started; exec sleep 30", Duration::from_secs(1))
            .unwrap();
        assert!(run.timed_out);
        assert_eq!(run.stdout, "started\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_background_process_holding_the_pipes_cannot_stall_the_run() {
        let started = Instant::now();
        let run = host().run("sleep 6 & echo done", LONG).unwrap();
        assert_eq!(run.exit_code, Some(0));
        assert_eq!(run.stdout, "done\n");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_failed_poll_is_reported_with_the_launcher_name() {
        let failed = polled(Err(std::io::Error::other("gone")), "docker");
        assert_eq!(failed, Err("cannot wait for `docker`: gone".to_string()));
        assert_eq!(polled(Ok(None), "docker"), Ok(None));
    }

    struct Broken;

    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("closed"))
        }
    }

    #[test]
    fn a_failing_reader_yields_what_was_read_before_the_error() {
        assert_eq!(read_tail(Broken), "");
        let chained = (&b"abc"[..]).chain(Broken);
        assert_eq!(read_tail(chained), "abc");
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
        assert_eq!(
            runner.teardown().unwrap(),
            ["podman", "rm", "-f", "nanna-task-1"]
        );
    }

    fn marking(dir: &std::path::Path) -> (ProcessCheckRunner, std::path::PathBuf) {
        let marker = dir.join("removed");
        let runner = host().with_teardown(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo gone > \"$0\"".to_string(),
            marker.display().to_string(),
        ]);
        (runner, marker)
    }

    #[test]
    fn a_timeout_force_removes_the_container_but_a_normal_exit_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let (runner, marker) = marking(dir.path());
        let run = runner.run("echo fine", LONG).unwrap();
        assert_eq!(run.exit_code, Some(0));
        assert!(!marker.exists());
        let failed = runner.run("exit 3", LONG).unwrap();
        assert_eq!(failed.exit_code, Some(3));
        assert!(!marker.exists());
        let run = runner
            .run("exec sleep 600", Duration::from_millis(300))
            .unwrap();
        assert!(run.timed_out);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "gone\n");
        assert_eq!(run.stderr, "");
    }

    #[test]
    fn a_teardown_that_fails_or_cannot_start_is_reported_in_the_output() {
        let short = Duration::from_millis(300);
        let refused = host().with_teardown(vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo nope >&2; exit 3".to_string(),
        ]);
        let run = refused.run("exec sleep 600", short).unwrap();
        assert!(run.timed_out);
        assert_eq!(run.stderr, "container teardown `sh` failed: nope");
        let missing = host().with_teardown(vec!["/nonexistent/rm".to_string()]);
        let run = missing
            .run("echo before >&2; exec sleep 600", short)
            .unwrap();
        assert!(run
            .stderr
            .starts_with("before\ncannot start container teardown"));
        let unterminated = missing
            .run("printf before >&2; exec sleep 600", short)
            .unwrap();
        assert!(unterminated.stderr.starts_with("before\ncannot start"));
        let empty = host().with_teardown(vec![]);
        assert_eq!(empty.run("exec sleep 600", short).unwrap().stderr, "");
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
