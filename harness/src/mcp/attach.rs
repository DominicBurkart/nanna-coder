use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;

pub const DEFAULT_SOCKET: &str = "/run/nanna/mcp.sock";
pub const CLIENT_KEY: &str = "nanna_client";
pub const MAX_CLIENT_TAG: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    #[error("cannot reach the nanna daemon at {path}: {source}")]
    Unreachable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the nanna daemon closed the connection while the client was still connected")]
    DaemonClosed,
    #[error("attach io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn client_tag(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(MAX_CLIENT_TAG)
        .collect();
    if cleaned.is_empty() {
        "unnamed".to_string()
    } else {
        cleaned
    }
}

pub fn preamble(client: &str) -> String {
    format!("{{\"{CLIENT_KEY}\":\"{}\"}}\n", client_tag(client))
}

pub fn parse_preamble(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object.get(CLIENT_KEY)?.as_str().map(client_tag)
}

pub async fn attach<I, O>(
    socket: &Path,
    client: &str,
    mut input: I,
    mut output: O,
) -> Result<(), AttachError>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|source| AttachError::Unreachable {
            path: socket.to_path_buf(),
            source,
        })?;
    let (mut from_socket, mut to_socket) = stream.into_split();
    if to_socket
        .write_all(preamble(client).as_bytes())
        .await
        .is_err()
    {
        return Err(AttachError::DaemonClosed);
    }
    let stdin_closed = AtomicBool::new(false);
    let up = async {
        let copied = tokio::io::copy(&mut input, &mut to_socket).await;
        stdin_closed.store(true, Ordering::SeqCst);
        let _ = to_socket.shutdown().await;
        copied
    };
    let down = async {
        let copied = tokio::io::copy(&mut from_socket, &mut output).await;
        let _ = output.flush().await;
        copied
    };
    tokio::pin!(up, down);
    tokio::select! {
        closed = &mut down => {
            if let Err(error) = closed {
                return Err(match error.kind() {
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe => {
                        AttachError::DaemonClosed
                    }
                    _ => AttachError::Io(error),
                });
            }
            if stdin_closed.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(AttachError::DaemonClosed)
            }
        }
        sent = &mut up => {
            if sent.is_err() {
                return Err(AttachError::DaemonClosed);
            }
            down.await?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::listen::{accept_loop, bind_private};
    use crate::mcp::NannaMcpServer;
    use crate::task::TaskManager;
    use async_trait::async_trait;
    use model::provider::{ModelProvider, ModelResult};
    use model::types::{ChatRequest, ChatResponse, ModelInfo};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct NoopProvider;

    #[async_trait]
    impl ModelProvider for NoopProvider {
        async fn chat(&self, _: ChatRequest) -> ModelResult<ChatResponse> {
            unimplemented!()
        }
        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }
        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }
        fn provider_name(&self) -> &'static str {
            "noop"
        }
    }

    async fn daemon(dir: &Path) -> (PathBuf, tokio::task::JoinHandle<()>) {
        let path = dir.join("mcp.sock");
        let server = Arc::new(NannaMcpServer::new(
            Arc::new(TaskManager::new(0)),
            Arc::new(NoopProvider),
            crate::auditor::test_support::fixture_gate(),
            10,
        ));
        let listener = bind_private(&path).await.unwrap();
        (path, tokio::spawn(accept_loop(server, listener)))
    }

    const PING: &str = "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";

    #[test]
    fn client_tags_are_reduced_to_plain_characters() {
        assert_eq!(client_tag("lumo"), "lumo");
        assert_eq!(client_tag("a b\"c\n{}"), "abc");
        assert_eq!(client_tag(""), "unnamed");
        assert_eq!(client_tag(&"x".repeat(500)).len(), MAX_CLIENT_TAG);
    }

    #[test]
    fn the_preamble_round_trips_and_nothing_else_parses_as_one() {
        assert_eq!(parse_preamble(&preamble("lumo")).as_deref(), Some("lumo"));
        assert_eq!(parse_preamble(PING), None);
        assert_eq!(parse_preamble("not json"), None);
        assert_eq!(parse_preamble("{\"nanna_client\":\"a\",\"id\":1}"), None);
        assert_eq!(parse_preamble("{\"nanna_client\":5}"), None);
    }

    #[tokio::test]
    async fn stdin_closing_drains_the_reply_and_exits_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let (path, _daemon) = daemon(dir.path()).await;
        let (mut feed, input) = tokio::io::duplex(4096);
        let (output, mut collected) = tokio::io::duplex(4096);
        feed.write_all(PING.as_bytes()).await.unwrap();
        drop(feed);
        attach(&path, "lumo", input, output).await.unwrap();
        let mut text = String::new();
        collected.read_to_string(&mut text).await.unwrap();
        let reply: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(reply["id"], 7);
        assert_eq!(text.lines().count(), 1);
        assert!(!text.contains(CLIENT_KEY));
    }

    #[tokio::test]
    async fn an_unreachable_daemon_is_reported_with_the_socket_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.sock");
        let (_feed, input) = tokio::io::duplex(64);
        let (output, _collected) = tokio::io::duplex(64);
        let error = attach(&path, "lumo", input, output).await.unwrap_err();
        assert!(matches!(error, AttachError::Unreachable { .. }));
        assert!(error.to_string().contains("absent.sock"));
    }

    #[tokio::test]
    async fn the_daemon_closing_while_stdin_is_open_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let hangup = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
        });
        let (_feed, input) = tokio::io::duplex(64);
        let (output, _collected) = tokio::io::duplex(64);
        let result = attach(&path, "lumo", input, output).await;
        hangup.await.unwrap();
        assert!(matches!(result, Err(AttachError::DaemonClosed)));
    }
}
