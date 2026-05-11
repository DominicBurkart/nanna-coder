//! EXPERIMENTAL structured logging for the harness.
//!
//! This module installs the global tracing subscriber used by the harness CLI.
//! It always installs a human-readable `fmt` layer writing to stderr (preserving
//! the current UX). When a log file path is supplied, an additional JSON layer is
//! attached that writes one JSON object per event via a non-blocking appender.
//!
//! The JSON layer emits ISO 8601 / RFC 3339 UTC timestamps and attaches three
//! default identifying fields to every event that this module emits via its
//! helpers (`session_start`, `env_snapshot`): `container`, `service`, and
//! `version`. Downstream log sinks can key on these to correlate events across
//! processes and deployments.
//!
//! This is a first-phase ("PR1 of 6") scaffolding module. Subsequent PRs will
//! extend it with agent-loop, tool-dispatch, and outcome events. The schema is
//! intentionally designed so that new event classes can be added by emitting
//! additional `tracing::info!(event = "...")` calls without reshaping existing
//! records.

use std::path::{Path, PathBuf};

use tracing::info;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::{
    fmt::{self, time::UtcTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Layer,
};

/// Resolve the container identifier for this process.
///
/// Prefers `$HOSTNAME` (which podman/docker set inside containers), falls back
/// to `$CONTAINER_NAME`, and finally to `"host"` when neither is present.
pub fn resolve_container_id() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("CONTAINER_NAME").ok())
        .unwrap_or_else(|| "host".to_string())
}

/// Service name embedded in every JSON event.
pub const SERVICE_NAME: &str = "nanna-coder";

/// Crate version embedded in every JSON event.
pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Initialize the global tracing subscriber.
///
/// Installs a human-readable `fmt` layer on stderr (mirroring the pre-existing
/// behavior) and, when `log_file` is `Some`, attaches an additional JSON layer
/// that writes to the named file via `tracing_appender::non_blocking`. The
/// non-blocking writer is configured with `lossy(false)` so backpressure blocks
/// the producer rather than dropping records silently — correctness matters
/// more than throughput for observability data.
///
/// Returns `Some(WorkerGuard)` when a log file is configured; the caller MUST
/// hold the guard until the process is ready to exit so buffered records flush.
/// Returns `None` when no log file is configured.
pub fn init(log_file: Option<&Path>, log_file_level: &str) -> std::io::Result<Option<WorkerGuard>> {
    // Stderr human-readable layer — always installed.
    let stderr_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(stderr_filter);

    let registry = tracing_subscriber::registry().with(stderr_layer);

    match log_file {
        Some(path) => {
            // Ensure parent directory exists so the appender doesn't fail silently.
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }

            let (dir, filename) = split_log_path(path);
            let file_appender = tracing_appender::rolling::never(dir, filename);
            let (non_blocking, guard) = NonBlockingBuilder::default()
                .lossy(false)
                .finish(file_appender);

            let file_filter =
                EnvFilter::try_new(log_file_level).unwrap_or_else(|_| EnvFilter::new("info"));

            let json_layer = fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false)
                .with_timer(UtcTime::rfc_3339())
                .with_writer(non_blocking)
                .with_filter(file_filter);

            registry.with(json_layer).init();
            Ok(Some(guard))
        }
        None => {
            registry.init();
            Ok(None)
        }
    }
}

fn split_log_path(path: &Path) -> (PathBuf, PathBuf) {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let filename = path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("nanna.log"));
    (dir, filename)
}

/// Emit a `session.start` event capturing the per-run correlation id and the
/// user prompt. Callers should invoke this once at the top of `run_agent` or
/// `run_mcp_server`. Later PRs will add span propagation so every downstream
/// event inherits the `session_id` automatically.
pub fn session_start(session_id: &str, user_prompt: &str) {
    info!(
        event = "session.start",
        session_id = %session_id,
        user_prompt = %user_prompt,
        container = %resolve_container_id(),
        service = SERVICE_NAME,
        version = SERVICE_VERSION,
    );
}

/// Emit an `env.snapshot` event capturing the workspace root and (when
/// available) the current git branch and HEAD commit. Later PRs will extend
/// this with resolved dependency versions and a `Cargo.lock` hash.
pub fn env_snapshot(workspace_root: &Path, git_branch: Option<&str>, git_head: Option<&str>) {
    info!(
        event = "env.snapshot",
        workspace_root = %workspace_root.display(),
        git_branch = git_branch.unwrap_or("unknown"),
        git_head = git_head.unwrap_or("unknown"),
        container = %resolve_container_id(),
        service = SERVICE_NAME,
        version = SERVICE_VERSION,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;
    use tracing::subscriber::with_default;
    use tracing_subscriber::fmt::MakeWriter;

    /// A `MakeWriter` that captures everything written into a shared buffer,
    /// so tests can inspect the emitted JSON.
    #[derive(Clone, Default)]
    struct BufferWriter(Arc<Mutex<Vec<u8>>>);

    impl BufferWriter {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl std::io::Write for BufferWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for BufferWriter {
        type Writer = BufferWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn resolve_container_id_is_never_empty() {
        // HOSTNAME is usually set in CI and dev shells; the fallback chain
        // guarantees the returned string is non-empty either way.
        let id = resolve_container_id();
        assert!(!id.is_empty());
    }

    #[test]
    fn split_log_path_uses_cwd_when_no_parent() {
        let (dir, file) = split_log_path(Path::new("foo.log"));
        assert_eq!(dir, PathBuf::from("."));
        assert_eq!(file, PathBuf::from("foo.log"));
    }

    #[test]
    fn split_log_path_uses_explicit_parent() {
        let (dir, file) = split_log_path(Path::new("/tmp/nested/out.jsonl"));
        assert_eq!(dir, PathBuf::from("/tmp/nested"));
        assert_eq!(file, PathBuf::from("out.jsonl"));
    }

    /// Validate the contract of the non-blocking builder the way `init` uses
    /// it. We cannot safely invoke `init` directly in unit tests because it
    /// installs a global subscriber; that path is covered by the integration
    /// tests in `tests/logging_integration.rs`.
    #[test]
    fn non_blocking_guard_is_returned_for_log_file_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nanna.log");
        let (dir_p, file_p) = split_log_path(&path);
        assert_eq!(dir_p, dir.path());
        assert_eq!(file_p, PathBuf::from("nanna.log"));

        let appender = tracing_appender::rolling::never(dir_p, file_p);
        let (_non_blocking, guard) = NonBlockingBuilder::default().lossy(false).finish(appender);
        // The guard is what `init` surfaces to the caller; holding it until
        // the process exits is how buffered records are flushed.
        drop(guard);
    }

    #[test]
    fn session_start_emits_required_json_fields() {
        let buf = BufferWriter::default();
        let layer = fmt::layer()
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .with_span_list(false)
            .with_timer(UtcTime::rfc_3339())
            .with_writer(buf.clone());

        let subscriber = tracing_subscriber::registry().with(layer);
        with_default(subscriber, || {
            session_start("11111111-1111-1111-1111-111111111111", "hello world");
        });

        let out = buf.contents();
        assert!(
            out.contains("\"session.start\""),
            "missing event tag: {}",
            out
        );
        assert!(out.contains("\"session_id\":\"11111111-1111-1111-1111-111111111111\""));
        assert!(out.contains("\"user_prompt\":\"hello world\""));
        assert!(out.contains("\"service\":\"nanna-coder\""));
        assert!(out.contains("\"version\":"));
        assert!(out.contains("\"container\":"));

        // Verify the top-level timestamp looks like RFC 3339 / ISO 8601.
        let ts_re = Regex::new(r#""timestamp":"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}"#).unwrap();
        assert!(ts_re.is_match(&out), "bad timestamp format: {}", out);
    }

    #[test]
    fn env_snapshot_emits_required_json_fields() {
        let buf = BufferWriter::default();
        let layer = fmt::layer()
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .with_span_list(false)
            .with_timer(UtcTime::rfc_3339())
            .with_writer(buf.clone());

        let subscriber = tracing_subscriber::registry().with(layer);
        with_default(subscriber, || {
            env_snapshot(Path::new("/tmp/ws"), Some("main"), Some("abcdef0"));
        });

        let out = buf.contents();
        assert!(
            out.contains("\"env.snapshot\""),
            "missing event tag: {}",
            out
        );
        assert!(out.contains("\"git_branch\":\"main\""));
        assert!(out.contains("\"git_head\":\"abcdef0\""));
        assert!(out.contains("\"workspace_root\":\"/tmp/ws\""));
        assert!(out.contains("\"service\":\"nanna-coder\""));
    }

    #[test]
    fn env_snapshot_handles_missing_git_info() {
        let buf = BufferWriter::default();
        let layer = fmt::layer()
            .json()
            .flatten_event(true)
            .with_writer(buf.clone());

        let subscriber = tracing_subscriber::registry().with(layer);
        with_default(subscriber, || {
            env_snapshot(Path::new("/tmp/ws"), None, None);
        });

        let out = buf.contents();
        assert!(out.contains("\"git_branch\":\"unknown\""));
        assert!(out.contains("\"git_head\":\"unknown\""));
    }
}
