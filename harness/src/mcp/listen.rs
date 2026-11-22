use super::NannaMcpServer;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;

pub const MAX_CONNECTIONS: usize = 64;
const ACCEPT_BACKOFF: Duration = Duration::from_millis(200);

#[derive(Debug, thiserror::Error)]
pub enum ListenError {
    #[error("{path} is already served by a running process")]
    InUse { path: PathBuf },
    #[error("{path} exists and is not a socket; refusing to replace it")]
    NotASocket { path: PathBuf },
    #[error("socket path {path} has no parent directory")]
    NoParent { path: PathBuf },
    #[error("socket io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> ListenError + '_ {
    move |source| ListenError::Io {
        path: path.to_path_buf(),
        source,
    }
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn clear_stale(path: &Path) -> Result<(), ListenError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_at(path)(e)),
    };
    if !meta.file_type().is_socket() {
        return Err(ListenError::NotASocket {
            path: path.to_path_buf(),
        });
    }
    match UnixStream::connect(path).await {
        Ok(_) => Err(ListenError::InUse {
            path: path.to_path_buf(),
        }),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path).map_err(io_at(path))
        }
        Err(e) => Err(io_at(path)(e)),
    }
}

pub async fn bind_private(path: &Path) -> Result<UnixListener, ListenError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| ListenError::NoParent {
            path: path.to_path_buf(),
        })?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(io_at(parent))?;
    clear_stale(path).await?;
    let stage = parent.join(format!(".mcp-bind-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&stage)
        .map_err(io_at(&stage))?;
    let staged = stage.join("s");
    let placed = (|| -> io::Result<UnixListener> {
        let listener = UnixListener::bind(&staged)?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&staged, path)?;
        Ok(listener)
    })();
    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_dir(&stage);
    placed.map_err(io_at(path))
}

pub async fn serve_unix(server: Arc<NannaMcpServer>, path: &Path) -> Result<(), ListenError> {
    let listener = bind_private(path).await?;
    let _guard = SocketGuard(path.to_path_buf());
    tracing::info!(socket = %path.display(), "Listening for MCP clients");
    accept_loop(server, listener).await;
    Ok(())
}

pub async fn accept_loop(server: Arc<NannaMcpServer>, listener: UnixListener) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                tracing::warn!(%error, "accept failed");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            tracing::warn!(
                max = MAX_CONNECTIONS,
                "Too many MCP connections; refusing one"
            );
            continue;
        };
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncReadExt};
            let (read, write) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(read);
            let mut first = String::new();
            if reader.read_line(&mut first).await.is_err() {
                return;
            }
            let client = super::attach::parse_preamble(&first);
            let replay = if client.is_some() {
                Vec::new()
            } else {
                first.into_bytes()
            };
            let client = client.unwrap_or_else(|| "untagged".to_string());
            tracing::info!(%client, "MCP client connected");
            let reader = std::io::Cursor::new(replay).chain(reader);
            if let Err(error) = server.serve(reader, write).await {
                tracing::warn!(%client, %error, "MCP connection ended with an error");
            }
            tracing::info!(%client, "MCP client disconnected");
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskManager;
    use async_trait::async_trait;
    use model::provider::{ModelProvider, ModelResult};
    use model::types::{ChatRequest, ChatResponse, ModelInfo};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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

    fn server_over(manager: Arc<TaskManager>) -> Arc<NannaMcpServer> {
        Arc::new(NannaMcpServer::new(
            manager,
            Arc::new(NoopProvider),
            crate::auditor::test_support::fixture_gate(),
            10,
        ))
    }

    struct Client {
        reader: BufReader<tokio::net::unix::OwnedReadHalf>,
        writer: tokio::net::unix::OwnedWriteHalf,
    }

    impl Client {
        async fn connect(path: &Path) -> Self {
            let (read, writer) = UnixStream::connect(path).await.unwrap().into_split();
            Self {
                reader: BufReader::new(read),
                writer,
            }
        }

        async fn call(&mut self, id: u64, method: &str) -> serde_json::Value {
            let line = format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\"}}\n");
            self.writer.write_all(line.as_bytes()).await.unwrap();
            let mut reply = String::new();
            tokio::time::timeout(Duration::from_secs(10), self.reader.read_line(&mut reply))
                .await
                .expect("no reply from the daemon")
                .unwrap();
            serde_json::from_str(&reply).unwrap()
        }
    }

    async fn listening(
        manager: Arc<TaskManager>,
    ) -> (tempfile::TempDir, PathBuf, tokio::task::JoinHandle<()>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("mcp.sock");
        let server = server_over(manager);
        let bound = bind_private(&path).await.unwrap();
        let handle = tokio::spawn(accept_loop(server, bound));
        (dir, path, handle)
    }

    #[tokio::test]
    async fn the_socket_is_private_and_its_directory_is_created() {
        let manager = Arc::new(TaskManager::new(0));
        let (_dir, path, _handle) = listening(manager).await;
        let socket = std::fs::symlink_metadata(&path).unwrap();
        assert!(socket.file_type().is_socket());
        assert_eq!(socket.permissions().mode() & 0o777, 0o600);
        let parent = std::fs::metadata(path.parent().unwrap()).unwrap();
        assert_eq!(parent.permissions().mode() & 0o777, 0o700);
    }

    #[tokio::test]
    async fn two_clients_at_once_see_the_same_task() {
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = Arc::new(NoopProvider);
        let id = manager
            .submit(
                "shared".to_string(),
                PathBuf::from("/nonexistent"),
                "HEAD".to_string(),
                "mock".to_string(),
                1,
                provider,
            )
            .await;
        let (_dir, path, _handle) = listening(Arc::clone(&manager)).await;
        let mut first = Client::connect(&path).await;
        let mut second = Client::connect(&path).await;
        for (client, n) in [(&mut first, 1u64), (&mut second, 2u64)] {
            let reply = client.call(n, "tasks/list").await;
            let tasks = reply["result"]["tasks"].as_array().unwrap();
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0]["taskId"], id.0);
        }
    }

    #[tokio::test]
    async fn a_client_that_disconnects_leaves_the_tasks_and_other_clients_alone() {
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = Arc::new(NoopProvider);
        let id = manager
            .submit(
                "durable".to_string(),
                PathBuf::from("/nonexistent"),
                "HEAD".to_string(),
                "mock".to_string(),
                1,
                provider,
            )
            .await;
        let (_dir, path, handle) = listening(Arc::clone(&manager)).await;
        let mut stays = Client::connect(&path).await;
        let mut leaves = Client::connect(&path).await;
        assert_eq!(leaves.call(1, "ping").await["id"], 1);
        drop(leaves);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(stays.call(2, "ping").await["id"], 2);
        assert!(!handle.is_finished());
        assert!(manager.poll(&id).await.is_some());
        let mut later = Client::connect(&path).await;
        let reply = later.call(3, "tasks/list").await;
        assert_eq!(reply["result"]["tasks"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn garbage_on_one_connection_does_not_stop_the_accept_loop() {
        let manager = Arc::new(TaskManager::new(0));
        let (_dir, path, handle) = listening(manager).await;
        let mut bad = UnixStream::connect(&path).await.unwrap();
        bad.write_all(&[0xff, 0xfe, b'\n']).await.unwrap();
        drop(bad);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!handle.is_finished());
        let mut good = Client::connect(&path).await;
        assert_eq!(good.call(1, "ping").await["id"], 1);
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let listener = bind_private(&path).await.unwrap();
        drop(listener);
    }

    #[tokio::test]
    async fn a_live_socket_is_never_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.sock");
        let _live = bind_private(&path).await.unwrap();
        let refused = bind_private(&path).await;
        assert!(matches!(refused, Err(ListenError::InUse { .. })));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn a_regular_file_or_symlink_at_the_socket_path_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.sock");
        std::fs::write(&file, "data").unwrap();
        assert!(matches!(
            bind_private(&file).await,
            Err(ListenError::NotASocket { .. })
        ));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "data");
        let link = dir.path().join("b.sock");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(matches!(
            bind_private(&link).await,
            Err(ListenError::NotASocket { .. })
        ));
        assert!(std::fs::symlink_metadata(&link).is_ok());
    }

    #[tokio::test]
    async fn a_bare_filename_without_a_parent_is_refused() {
        assert!(matches!(
            bind_private(Path::new("mcp.sock")).await,
            Err(ListenError::NoParent { .. })
        ));
    }

    #[tokio::test]
    async fn the_socket_file_is_removed_when_serving_stops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.sock");
        let server = server_over(Arc::new(TaskManager::new(0)));
        let serving = tokio::spawn({
            let path = path.clone();
            async move { serve_unix(server, &path).await }
        });
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(path.exists());
        serving.abort();
        let _ = serving.await;
        assert!(!path.exists());
    }
}
