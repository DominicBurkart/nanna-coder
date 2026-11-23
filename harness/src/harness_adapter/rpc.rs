use super::{Endpoint, ResolveError, MAX_REQUEST_BYTES};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[doc = "Name podman gives the host inside a container."]
pub const RELAY_HOST: &str = "host.containers.internal";
#[doc = "Largest line of the runtime's event stream."]
pub const MAX_RPC_LINE_BYTES: usize = 32 * 1024 * 1024;
#[doc = "Most bytes read from the runtime's event stream."]
pub const MAX_RPC_TOTAL_BYTES: usize = 512 * 1024 * 1024;
#[doc = "Largest reply line, the extension's reply cap."]
pub const MAX_REPLY_BYTES: usize = 1_048_576;
#[doc = "Per-read timeout for a broker request."]
pub const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
#[doc = "A framing or transport failure; every one fails closed."]
pub enum RpcError {
    #[error("line longer than {limit} bytes")]
    LineTooLong { limit: usize },
    #[error("more than {limit} bytes in total")]
    TotalTooLong { limit: usize },
    #[error("read timed out")]
    Timeout,
    #[error("stream ended inside a line")]
    Truncated,
    #[error("line is not valid UTF-8")]
    NotUtf8,
    #[error("reply contains a line feed")]
    ReplyHasNewline,
    #[error("reply longer than {limit} bytes")]
    ReplyTooLarge { limit: usize },
    #[error("the reader already failed")]
    Failed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc = "Caps for a [`LineReader`]."]
pub struct ReadLimits {
    pub max_line: usize,
    pub max_total: usize,
    pub read_timeout: Duration,
}

impl ReadLimits {
    #[doc = r#"Limits for one broker request: a line of at most `MAX_REQUEST_BYTES`, one line in total.

```
use harness::harness_adapter::{ReadLimits, MAX_REQUEST_BYTES, REQUEST_READ_TIMEOUT};

let limits = ReadLimits::for_requests();
assert_eq!(limits.max_line, MAX_REQUEST_BYTES);
assert_eq!(limits.max_total, MAX_REQUEST_BYTES + 1);
assert_eq!(limits.read_timeout, REQUEST_READ_TIMEOUT);
```"#]
    pub const fn for_requests() -> Self {
        Self {
            max_line: MAX_REQUEST_BYTES,
            max_total: MAX_REQUEST_BYTES + 1,
            read_timeout: REQUEST_READ_TIMEOUT,
        }
    }

    #[doc = r#"Limits for the runtime's event stream.

```
use harness::harness_adapter::{ReadLimits, MAX_RPC_LINE_BYTES, MAX_RPC_TOTAL_BYTES};
use std::time::Duration;

let limits = ReadLimits::for_events(Duration::from_secs(3));
assert_eq!(limits.max_line, MAX_RPC_LINE_BYTES);
assert_eq!(limits.max_total, MAX_RPC_TOTAL_BYTES);
assert_eq!(limits.read_timeout, Duration::from_secs(3));
```"#]
    pub const fn for_events(read_timeout: Duration) -> Self {
        Self {
            max_line: MAX_RPC_LINE_BYTES,
            max_total: MAX_RPC_TOTAL_BYTES,
            read_timeout,
        }
    }
}

const CHUNK_BYTES: usize = 8192;

#[doc = r#"A reader of LF-terminated lines with every read capped. Only `\n` ends a line: a lone CR, U+2028, U+2029 and U+0085 stay inside the line. A line over `max_line`, more than `max_total` bytes in all, a silent peer, a partial final line or invalid UTF-8 is an error, and after any error the reader stays failed.

```
# #[tokio::main]
# async fn main() {
use harness::harness_adapter::{LineReader, ReadLimits};
use std::time::Duration;

let limits = ReadLimits { max_line: 8, max_total: 64, read_timeout: Duration::from_secs(1) };
let mut reader = LineReader::new("a\rb\u{2028}c\nnext\n".as_bytes(), limits);
assert_eq!(reader.next_line().await.unwrap().as_deref(), Some("a\rb\u{2028}c"));
assert_eq!(reader.next_line().await.unwrap().as_deref(), Some("next"));
assert!(reader.next_line().await.unwrap().is_none());
# }
```"#]
pub struct LineReader<R> {
    inner: R,
    pending: Vec<u8>,
    total: usize,
    limits: ReadLimits,
    failed: bool,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    #[doc = "Wrap `inner` with `limits`."]
    pub fn new(inner: R, limits: ReadLimits) -> Self {
        Self {
            inner,
            pending: Vec::new(),
            total: 0,
            limits,
            failed: false,
        }
    }

    #[doc = "The next line without its LF, `None` at a clean end of stream."]
    pub async fn next_line(&mut self) -> Result<Option<String>, RpcError> {
        if self.failed {
            return Err(RpcError::Failed);
        }
        let result = self.read_line().await;
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    async fn read_line(&mut self) -> Result<Option<String>, RpcError> {
        let mut chunk = [0u8; CHUNK_BYTES];
        loop {
            if let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                return self.take_line(end).map(Some);
            }
            if self.pending.len() > self.limits.max_line {
                return Err(RpcError::LineTooLong {
                    limit: self.limits.max_line,
                });
            }
            let read = tokio::time::timeout(self.limits.read_timeout, self.inner.read(&mut chunk))
                .await
                .map_err(|_| RpcError::Timeout)??;
            if read == 0 {
                return if self.pending.is_empty() {
                    Ok(None)
                } else {
                    Err(RpcError::Truncated)
                };
            }
            self.pending.extend_from_slice(&chunk[..read]);
        }
    }

    fn take_line(&mut self, end: usize) -> Result<String, RpcError> {
        if end > self.limits.max_line {
            return Err(RpcError::LineTooLong {
                limit: self.limits.max_line,
            });
        }
        if self.total + end + 1 > self.limits.max_total {
            return Err(RpcError::TotalTooLong {
                limit: self.limits.max_total,
            });
        }
        self.total += end + 1;
        let rest = self.pending.split_off(end + 1);
        let mut line = std::mem::replace(&mut self.pending, rest);
        line.pop();
        String::from_utf8(line).map_err(|_| RpcError::NotUtf8)
    }
}

#[doc = r#"Write one reply line: U+2028 and U+2029 are escaped, a reply holding a line feed or longer than `max` (with its LF) is refused, and the write is bounded by `timeout`.

```
# #[tokio::main]
# async fn main() {
use harness::harness_adapter::write_reply;
use std::time::Duration;

let mut out = Vec::new();
write_reply(&mut out, "{\"a\":\"\u{2028}\"}", 64, Duration::from_secs(1)).await.unwrap();
assert_eq!(out, b"{\"a\":\"\\u2028\"}\n");
assert!(write_reply(&mut out, "a\nb", 64, Duration::from_secs(1)).await.is_err());
# }
```"#]
pub async fn write_reply<W: AsyncWrite + Unpin>(
    writer: &mut W,
    reply: &str,
    max: usize,
    timeout: Duration,
) -> Result<(), RpcError> {
    if reply.contains('\n') {
        return Err(RpcError::ReplyHasNewline);
    }
    let mut text = reply
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    text.push('\n');
    if text.len() > max {
        return Err(RpcError::ReplyTooLarge { limit: max });
    }
    tokio::time::timeout(timeout, async {
        writer.write_all(text.as_bytes()).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| RpcError::Timeout)??;
    Ok(())
}

impl Endpoint {
    #[doc = r#"The endpoint a runtime inside the container uses to reach a relay on the host (podman's `host.containers.internal`). A mechanism only: the broker itself is reached over the mounted unix socket.

```
use harness::harness_adapter::Endpoint;

let relay = Endpoint::in_container_relay(8080).unwrap();
assert_eq!(relay.host(), "host.containers.internal");
assert_eq!(relay.port(), 8080);
assert!(Endpoint::in_container_relay(0).is_err());
```"#]
    pub fn in_container_relay(port: u16) -> Result<Self, ResolveError> {
        Endpoint::new(RELAY_HOST, port)
    }
}

#[cfg(unix)]
pub(super) enum ServeEnd {
    Halted(super::HaltReason),
    Failed(String),
}

#[cfg(unix)]
pub(super) async fn serve_connection<S>(
    stream: S,
    broker: &super::broker::ArmedBroker,
    limits: ReadLimits,
) -> Result<Option<super::HaltReason>, RpcError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = LineReader::new(read_half, limits);
    match reader.next_line().await {
        Ok(None) => Ok(None),
        Ok(Some(line)) => {
            let (reply, halt) = broker.handle_line_with_halt(&line).await;
            let written = write_reply(
                &mut write_half,
                &reply,
                MAX_REPLY_BYTES,
                limits.read_timeout,
            )
            .await;
            match (written, halt) {
                (_, Some(reason)) => Ok(Some(reason)),
                (Ok(()), None) => Ok(None),
                (Err(error), None) => Err(error),
            }
        }
        Err(error) => {
            let message = match error {
                RpcError::LineTooLong { .. } | RpcError::TotalTooLong { .. } => {
                    Some("request too large")
                }
                RpcError::NotUtf8 => Some("bad request: not valid UTF-8"),
                _ => None,
            };
            if let Some(message) = message {
                let reply = serde_json::to_string(&super::BrokerResponse {
                    ok: false,
                    output: serde_json::json!(message),
                    kind: Some(super::ToolErrorKind::BadRequest),
                    halt: false,
                })
                .expect("broker responses are plain JSON");
                let _ = write_reply(
                    &mut write_half,
                    &reply,
                    MAX_REPLY_BYTES,
                    limits.read_timeout,
                )
                .await;
            }
            Err(error)
        }
    }
}

#[cfg(unix)]
pub(super) async fn serve_listener(
    listener: tokio::net::UnixListener,
    broker: std::sync::Arc<super::broker::ArmedBroker>,
    limits: ReadLimits,
) -> ServeEnd {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => return ServeEnd::Failed(format!("broker socket accept failed: {error}")),
        };
        if let Ok(Some(reason)) = serve_connection(stream, &broker, limits).await {
            return ServeEnd::Halted(reason);
        }
    }
}
