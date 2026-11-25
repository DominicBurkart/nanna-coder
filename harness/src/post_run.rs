use crate::tool_contract::BuildState;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

const STDERR_TAIL_CHARS: usize = 2000;
pub const FILES_PLACEHOLDER: &str = "{files}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatRun {
    Missing,
    Finished { success: bool, stderr: String },
    TimedOut,
}

#[async_trait]
pub trait FormatRunner: Send + Sync {
    async fn run(&self, argv: &[String], timeout: Duration) -> FormatRun;
}

#[async_trait]
pub trait FinalCheckRunner: Send + Sync {
    async fn check(&self) -> BuildState;
}

pub struct CommandFormatRunner {
    pub cwd: PathBuf,
}

#[async_trait]
impl FormatRunner for CommandFormatRunner {
    async fn run(&self, argv: &[String], timeout: Duration) -> FormatRun {
        let Some((program, args)) = argv.split_first() else {
            return FormatRun::Missing;
        };
        let child = tokio::process::Command::new(program)
            .args(args)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let child = match child {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return FormatRun::Missing
            }
            Err(error) => {
                return FormatRun::Finished {
                    success: false,
                    stderr: error.to_string(),
                }
            }
        };
        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => FormatRun::Finished {
                success: output.status.success(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Ok(Err(error)) => FormatRun::Finished {
                success: false,
                stderr: error.to_string(),
            },
            Err(_) => FormatRun::TimedOut,
        }
    }
}

pub struct PostRunSpec<'a> {
    pub touched_rs: Vec<String>,
    pub format_argv: Vec<String>,
    pub format_runner: &'a dyn FormatRunner,
    pub format_timeout: Duration,
    pub check_runner: &'a dyn FinalCheckRunner,
    pub check_timeout: Duration,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FmtReport {
    pub ran: bool,
    pub ok: bool,
    pub files: Vec<String>,
    pub stderr_tail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostRunReport {
    pub fmt: FmtReport,
    pub build: BuildState,
}

pub fn expand_format_argv(template: &[String], files: &[String]) -> Vec<String> {
    let mut argv = Vec::new();
    let mut placed = false;
    for token in template {
        if token == FILES_PLACEHOLDER {
            argv.extend(files.iter().cloned());
            placed = true;
        } else {
            argv.push(token.clone());
        }
    }
    if !placed && !argv.is_empty() {
        argv.extend(files.iter().cloned());
    }
    argv
}

fn tail(text: &str) -> String {
    let count = text.chars().count();
    if count <= STDERR_TAIL_CHARS {
        return text.trim_end().to_string();
    }
    text.chars()
        .skip(count - STDERR_TAIL_CHARS)
        .collect::<String>()
        .trim_end()
        .to_string()
}

async fn format_touched(spec: &PostRunSpec<'_>) -> FmtReport {
    let files = spec.touched_rs.clone();
    if files.is_empty() || spec.format_argv.is_empty() {
        return FmtReport {
            files,
            ..FmtReport::default()
        };
    }
    let argv = expand_format_argv(&spec.format_argv, &files);
    match spec.format_runner.run(&argv, spec.format_timeout).await {
        FormatRun::Missing => FmtReport {
            ran: false,
            ok: false,
            files,
            stderr_tail: format!("formatter not found: {}", argv[0]),
        },
        FormatRun::Finished { success, stderr } => FmtReport {
            ran: true,
            ok: success,
            files,
            stderr_tail: tail(&stderr),
        },
        FormatRun::TimedOut => FmtReport {
            ran: true,
            ok: false,
            files,
            stderr_tail: "formatter timed out".to_string(),
        },
    }
}

pub async fn run_post_run(spec: PostRunSpec<'_>) -> PostRunReport {
    let fmt = format_touched(&spec).await;
    let build = match tokio::time::timeout(spec.check_timeout, spec.check_runner.check()).await {
        Ok(state) => state,
        Err(_) => BuildState::TimedOut,
    };
    PostRunReport { fmt, build }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_contract::{Diagnostic, Severity};
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;

    struct FakeFormat {
        log: Log,
        result: FormatRun,
    }

    #[async_trait]
    impl FormatRunner for FakeFormat {
        async fn run(&self, argv: &[String], _timeout: Duration) -> FormatRun {
            self.log
                .lock()
                .unwrap()
                .push(format!("fmt {}", argv.join(" ")));
            self.result.clone()
        }
    }

    struct FakeCheck {
        log: Log,
        state: BuildState,
        delay: Option<Duration>,
    }

    #[async_trait]
    impl FinalCheckRunner for FakeCheck {
        async fn check(&self) -> BuildState {
            self.log.lock().unwrap().push("check".to_string());
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            self.state.clone()
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    async fn run(
        files: &[&str],
        template: &[&str],
        result: FormatRun,
        state: BuildState,
    ) -> (PostRunReport, Vec<String>) {
        let log: Log = Arc::default();
        let format = FakeFormat {
            log: log.clone(),
            result,
        };
        let check = FakeCheck {
            log: log.clone(),
            state,
            delay: None,
        };
        let report = run_post_run(PostRunSpec {
            touched_rs: argv(files),
            format_argv: argv(template),
            format_runner: &format,
            format_timeout: Duration::from_secs(5),
            check_runner: &check,
            check_timeout: Duration::from_secs(5),
        })
        .await;
        let entries = log.lock().unwrap().clone();
        (report, entries)
    }

    fn ok_run() -> FormatRun {
        FormatRun::Finished {
            success: true,
            stderr: String::new(),
        }
    }

    #[tokio::test]
    async fn a_missing_formatter_is_recorded_and_skipped_and_the_check_still_runs() {
        let (report, log) = run(
            &["src/a.rs"],
            &["rustfmt", "{files}"],
            FormatRun::Missing,
            BuildState::Pass,
        )
        .await;
        assert!(!report.fmt.ran);
        assert!(!report.fmt.ok);
        assert_eq!(report.fmt.files, vec!["src/a.rs"]);
        assert!(report.fmt.stderr_tail.contains("rustfmt"));
        assert_eq!(report.build, BuildState::Pass);
        assert_eq!(log.last().unwrap(), "check");
    }

    #[tokio::test]
    async fn a_failing_formatter_is_tolerated_and_its_stderr_tail_recorded() {
        let stderr = format!("{}\nerror: expected item", "noise\n".repeat(1000));
        let (report, _) = run(
            &["src/a.rs"],
            &["rustfmt", "{files}"],
            FormatRun::Finished {
                success: false,
                stderr,
            },
            BuildState::Pass,
        )
        .await;
        assert!(report.fmt.ran);
        assert!(!report.fmt.ok);
        assert!(report.fmt.stderr_tail.ends_with("error: expected item"));
        assert!(report.fmt.stderr_tail.chars().count() <= STDERR_TAIL_CHARS);
        assert_eq!(report.build, BuildState::Pass);
    }

    #[tokio::test]
    async fn a_formatter_timeout_is_tolerated() {
        let (report, _) = run(
            &["src/a.rs"],
            &["rustfmt", "{files}"],
            FormatRun::TimedOut,
            BuildState::Pass,
        )
        .await;
        assert!(report.fmt.ran);
        assert!(!report.fmt.ok);
        assert!(report.fmt.stderr_tail.contains("timed out"));
    }

    #[tokio::test]
    async fn the_success_path_reports_both_results() {
        let (report, _) = run(
            &["src/a.rs", "src/b.rs"],
            &["rustfmt", "--edition", "2021", "{files}"],
            ok_run(),
            BuildState::Pass,
        )
        .await;
        assert_eq!(
            report.fmt,
            FmtReport {
                ran: true,
                ok: true,
                files: argv(&["src/a.rs", "src/b.rs"]),
                stderr_tail: String::new(),
            }
        );
        assert_eq!(report.build, BuildState::Pass);
    }

    #[tokio::test]
    async fn the_formatter_runs_before_the_final_check_with_files_expanded() {
        let (_, log) = run(
            &["src/a.rs", "src/b.rs"],
            &["rustfmt", "{files}"],
            ok_run(),
            BuildState::Pass,
        )
        .await;
        assert_eq!(log, vec!["fmt rustfmt src/a.rs src/b.rs", "check"]);
    }

    #[tokio::test]
    async fn files_are_appended_when_the_template_has_no_placeholder() {
        let (_, log) = run(&["src/a.rs"], &["rustfmt"], ok_run(), BuildState::Pass).await;
        assert_eq!(log[0], "fmt rustfmt src/a.rs");
    }

    #[tokio::test]
    async fn no_touched_files_or_no_template_skips_the_formatter_but_not_the_check() {
        let (report, log) = run(&[], &["rustfmt", "{files}"], ok_run(), BuildState::Pass).await;
        assert!(!report.fmt.ran);
        assert_eq!(log, vec!["check"]);
        let (report, log) = run(&["src/a.rs"], &[], ok_run(), BuildState::Pass).await;
        assert!(!report.fmt.ran);
        assert_eq!(log, vec!["check"]);
    }

    #[tokio::test]
    async fn a_failing_check_is_returned_as_the_build_state() {
        let state = BuildState::Fail {
            errors: vec![Diagnostic {
                file: "src/a.rs".to_string(),
                line: 1,
                col: 1,
                severity: Severity::Error,
                code: None,
                message: "boom".to_string(),
            }],
        };
        let (report, _) = run(&["src/a.rs"], &["rustfmt"], ok_run(), state.clone()).await;
        assert_eq!(report.build, state);
    }

    #[tokio::test]
    async fn a_check_that_exceeds_its_timeout_is_timed_out() {
        let log: Log = Arc::default();
        let format = FakeFormat {
            log: log.clone(),
            result: ok_run(),
        };
        let check = FakeCheck {
            log,
            state: BuildState::Pass,
            delay: Some(Duration::from_secs(60)),
        };
        let report = run_post_run(PostRunSpec {
            touched_rs: vec![],
            format_argv: vec![],
            format_runner: &format,
            format_timeout: Duration::from_secs(1),
            check_runner: &check,
            check_timeout: Duration::from_millis(20),
        })
        .await;
        assert_eq!(report.build, BuildState::TimedOut);
    }

    #[tokio::test]
    async fn the_command_runner_reports_a_missing_binary_and_real_exit_codes() {
        let runner = CommandFormatRunner {
            cwd: std::env::temp_dir(),
        };
        let missing = runner
            .run(
                &argv(&["definitely-not-a-formatter-xyz"]),
                Duration::from_secs(5),
            )
            .await;
        assert_eq!(missing, FormatRun::Missing);
        let failed = runner
            .run(
                &argv(&["sh", "-c", "echo bad >&2; exit 3"]),
                Duration::from_secs(5),
            )
            .await;
        assert_eq!(
            failed,
            FormatRun::Finished {
                success: false,
                stderr: "bad\n".to_string()
            }
        );
        let passed = runner.run(&argv(&["true"]), Duration::from_secs(5)).await;
        assert!(matches!(passed, FormatRun::Finished { success: true, .. }));
        let slow = runner
            .run(&argv(&["sleep", "30"]), Duration::from_millis(50))
            .await;
        assert_eq!(slow, FormatRun::TimedOut);
    }
}
