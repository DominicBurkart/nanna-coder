//! A minimal Chrome DevTools Protocol client over Chromium's
//! `--remote-debugging-pipe`.
//!
//! Chromium reads protocol messages from file descriptor 3 and writes them to
//! file descriptor 4, each message a JSON object terminated by a NUL byte.
//! Started through `sh -c 'exec chromium ... 3<&0 4>&1'` inside the dev
//! container with `<runtime> exec -i`, those descriptors become the child's
//! stdin and stdout on the host, so no debugging port, websocket library or
//! extra binary is involved. [`CdpSession`] numbers requests, matches
//! responses and keeps every console error it sees on the way.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use thiserror::Error;

/// Byte that terminates every message on the pipe.
pub const MESSAGE_TERMINATOR: u8 = 0;

/// Errors from driving the browser.
#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("could not spawn `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("browser pipe closed during {method}: {source}")]
    Pipe {
        method: String,
        #[source]
        source: std::io::Error,
    },
    #[error("browser sent a message that is not JSON: {raw}")]
    Malformed { raw: String },
    #[error("{method} failed: {message}")]
    Protocol { method: String, message: String },
    #[error("{method} returned no `{field}`: {result}")]
    MissingField {
        method: String,
        field: String,
        result: Value,
    },
    #[error("navigation to {url} failed: {error}")]
    Navigation { url: String, error: String },
    #[error("script failed: {message}")]
    Script { message: String },
    #[error("screenshot data is not base64: {0}")]
    Base64(String),
    #[error("could not write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// A console error observed while a scenario ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleError {
    /// `console` for `console.error`, `exception` for uncaught exceptions,
    /// otherwise the DevTools log source (`network`, `security`, ...).
    pub source: String,
    pub text: String,
    /// Script or resource the error refers to, when known.
    pub url: Option<String>,
}

/// Sends and receives NUL-terminated protocol messages.
pub trait CdpTransport: Send {
    fn send(&mut self, message: &str) -> std::io::Result<()>;
    /// The next message without its terminator; an error at end of stream.
    fn receive(&mut self) -> std::io::Result<String>;
}

/// [`CdpTransport`] over the stdin and stdout of a child process.
pub struct PipeTransport {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl PipeTransport {
    /// Spawn `program args...` with piped stdin and stdout; stderr is
    /// discarded because Chromium logs freely there.
    pub fn spawn(program: &str, args: &[String]) -> std::io::Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("child has no stdout"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

impl CdpTransport for PipeTransport {
    fn send(&mut self, message: &str) -> std::io::Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(std::io::Error::other("child stdin already closed"));
        };
        stdin.write_all(message.as_bytes())?;
        stdin.write_all(&[MESSAGE_TERMINATOR])?;
        stdin.flush()
    }

    fn receive(&mut self) -> std::io::Result<String> {
        let mut buf = Vec::new();
        let read = self.stdout.read_until(MESSAGE_TERMINATOR, &mut buf)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "browser closed the pipe",
            ));
        }
        if buf.last() == Some(&MESSAGE_TERMINATOR) {
            buf.pop();
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

impl Drop for PipeTransport {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let mut rest = Vec::new();
        let _ = self.stdout.read_to_end(&mut rest);
        let _ = self.child.wait();
    }
}

/// Creates transports; the production spawner starts real processes and
/// tests substitute scripted ones.
pub trait TransportSpawner: Send + Sync {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Box<dyn CdpTransport>>;
}

/// [`TransportSpawner`] backed by [`PipeTransport`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessSpawner;

impl TransportSpawner for ProcessSpawner {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Box<dyn CdpTransport>> {
        Ok(Box::new(PipeTransport::spawn(program, args)?))
    }
}

/// One protocol conversation, attached to a page target once
/// [`CdpSession::attach_new_page`] has run.
pub struct CdpSession {
    transport: Box<dyn CdpTransport>,
    next_id: u64,
    session_id: Option<String>,
    console_errors: Vec<ConsoleError>,
}

impl CdpSession {
    pub fn new(transport: Box<dyn CdpTransport>) -> Self {
        Self {
            transport,
            next_id: 0,
            session_id: None,
            console_errors: Vec::new(),
        }
    }

    /// The page session id, once attached.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Send `method` and wait for its response, recording events that
    /// arrive in between. Methods sent before attaching go to the browser
    /// target, later ones to the page session.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, BrowserError> {
        self.next_id += 1;
        let id = self.next_id;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = &self.session_id {
            message["sessionId"] = Value::String(session.clone());
        }
        self.transport
            .send(&message.to_string())
            .map_err(|source| pipe_error(method, source))?;
        self.await_response(method, id)
    }

    /// Read protocol messages until the response to `id` arrives, recording
    /// every event seen along the way.
    fn await_response(&mut self, method: &str, id: u64) -> Result<Value, BrowserError> {
        let raw = self
            .transport
            .receive()
            .map_err(|source| pipe_error(method, source))?;
        let msg: Value =
            serde_json::from_str(&raw).map_err(|_| BrowserError::Malformed { raw: raw.clone() })?;
        if msg.get("id").and_then(Value::as_u64) == Some(id) {
            return response(method, msg);
        }
        self.record_event(&msg);
        self.await_response(method, id)
    }

    /// Create a blank page target, attach to it and enable the page,
    /// runtime and log domains.
    pub fn attach_new_page(&mut self) -> Result<(), BrowserError> {
        let created = self.call("Target.createTarget", json!({ "url": "about:blank" }))?;
        let target_id = string_field("Target.createTarget", &created, "targetId")?;
        let params = json!({ "targetId": target_id, "flatten": true });
        let attached = self.call("Target.attachToTarget", params)?;
        let session_id = string_field("Target.attachToTarget", &attached, "sessionId")?;
        self.session_id = Some(session_id);
        for domain in ["Page.enable", "Runtime.enable", "Log.enable"] {
            self.call(domain, json!({}))?;
        }
        Ok(())
    }

    /// Console errors seen so far, leaving the session empty.
    pub fn drain_console_errors(&mut self) -> Vec<ConsoleError> {
        std::mem::take(&mut self.console_errors)
    }

    fn record_event(&mut self, msg: &Value) {
        if let Some(error) = console_error_from_event(msg) {
            self.console_errors.push(error);
        }
    }
}

fn pipe_error(method: &str, source: std::io::Error) -> BrowserError {
    BrowserError::Pipe {
        method: method.to_string(),
        source,
    }
}

fn response(method: &str, msg: Value) -> Result<Value, BrowserError> {
    if let Some(error) = msg.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_string();
        return Err(BrowserError::Protocol {
            method: method.to_string(),
            message,
        });
    }
    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
}

/// `field` of a protocol result as a string.
pub fn string_field(method: &str, result: &Value, field: &str) -> Result<String, BrowserError> {
    let value = result.get(field).and_then(Value::as_str);
    value
        .map(str::to_string)
        .ok_or_else(|| BrowserError::MissingField {
            method: method.to_string(),
            field: field.to_string(),
            result: result.clone(),
        })
}

/// The console error an event describes: `Runtime.consoleAPICalled` of type
/// `error`, `Runtime.exceptionThrown`, or `Log.entryAdded` at level `error`.
///
/// ```
/// use harness::qa::cdp::console_error_from_event;
/// use serde_json::json;
///
/// let event = json!({
///     "method": "Log.entryAdded",
///     "params": { "entry": { "source": "network", "level": "error",
///                            "text": "Failed to load resource: 500", "url": "http://app/api" } }
/// });
/// let error = console_error_from_event(&event).unwrap();
/// assert_eq!(error.source, "network");
/// assert_eq!(error.url.as_deref(), Some("http://app/api"));
/// assert!(console_error_from_event(&json!({ "method": "Page.loadEventFired" })).is_none());
/// ```
pub fn console_error_from_event(msg: &Value) -> Option<ConsoleError> {
    let params = msg.get("params")?;
    match msg.get("method")?.as_str()? {
        "Runtime.consoleAPICalled" => {
            if params.get("type")?.as_str()? != "error" {
                return None;
            }
            let text = params
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .map(remote_object_text)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let url = params
                .pointer("/stackTrace/callFrames/0/url")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(ConsoleError {
                source: "console".to_string(),
                text,
                url,
            })
        }
        "Runtime.exceptionThrown" => {
            let details = params.get("exceptionDetails")?;
            let text = details
                .pointer("/exception/description")
                .or_else(|| details.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("uncaught exception")
                .to_string();
            let url = details
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(ConsoleError {
                source: "exception".to_string(),
                text,
                url,
            })
        }
        "Log.entryAdded" => {
            let entry = params.get("entry")?;
            if entry.get("level")?.as_str()? != "error" {
                return None;
            }
            Some(ConsoleError {
                source: entry
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("log")
                    .to_string(),
                text: entry
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                url: entry.get("url").and_then(Value::as_str).map(str::to_string),
            })
        }
        _ => None,
    }
}

fn remote_object_text(arg: &Value) -> String {
    if let Some(value) = arg.get("value") {
        return match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    }
    arg.get("description")
        .and_then(Value::as_str)
        .unwrap_or("[object]")
        .to_string()
}

/// Decode standard base64 (with `=` padding) as `Page.captureScreenshot`
/// returns it.
///
/// ```
/// use harness::qa::cdp::decode_base64;
///
/// assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
/// assert_eq!(decode_base64("").unwrap(), b"");
/// assert!(decode_base64("a").is_err());
/// ```
pub fn decode_base64(text: &str) -> Result<Vec<u8>, BrowserError> {
    let bytes: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let trimmed = bytes.iter().rev().take_while(|b| **b == b'=').count();
    if !bytes.len().is_multiple_of(4) || trimmed > 2 {
        return Err(BrowserError::Base64(format!(
            "length {} is not a multiple of 4 with at most two '=' characters",
            bytes.len()
        )));
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut acc: u32 = 0;
        let mut pad = 0;
        for &b in chunk {
            if pad > 0 && b != b'=' {
                return Err(BrowserError::Base64("'=' before the end".to_string()));
            }
            let sextet = if b == b'=' {
                pad += 1;
                0
            } else {
                sextet_of(b)?
            };
            acc = (acc << 6) | u32::from(sextet);
        }
        let decoded = acc.to_be_bytes();
        out.extend_from_slice(&decoded[1..4 - pad]);
    }
    Ok(out)
}

fn sextet_of(b: u8) -> Result<u8, BrowserError> {
    match b {
        b'A'..=b'Z' => Ok(b - b'A'),
        b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        other => Err(BrowserError::Base64(format!(
            "unexpected character {:?}",
            char::from(other)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    pub(crate) struct ScriptedTransport {
        pub sent: Arc<Mutex<Vec<Value>>>,
        pub incoming: VecDeque<String>,
    }

    impl ScriptedTransport {
        pub(crate) fn new(incoming: &[Value]) -> (Self, Arc<Mutex<Vec<Value>>>) {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let transport = Self {
                sent: Arc::clone(&sent),
                incoming: incoming.iter().map(Value::to_string).collect(),
            };
            (transport, sent)
        }
    }

    impl CdpTransport for ScriptedTransport {
        fn send(&mut self, message: &str) -> std::io::Result<()> {
            let value: Value = serde_json::from_str(message).unwrap();
            self.sent.lock().unwrap().push(value);
            Ok(())
        }

        fn receive(&mut self) -> std::io::Result<String> {
            self.incoming
                .pop_front()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"))
        }
    }

    fn encode_base64(data: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let mut acc = 0u32;
            for i in 0..3 {
                acc = (acc << 8) | u32::from(*chunk.get(i).unwrap_or(&0));
            }
            for i in 0..4 {
                if i <= chunk.len() {
                    let idx = ((acc >> (18 - 6 * i)) & 63) as usize;
                    out.push(char::from(ALPHABET[idx]));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn pipe_transport_round_trips_nul_terminated_messages_through_cat() {
        let mut transport = PipeTransport::spawn("cat", &[]).unwrap();
        transport.send("{\"id\":1}").unwrap();
        transport.send("second").unwrap();
        assert_eq!(transport.receive().unwrap(), "{\"id\":1}");
        assert_eq!(transport.receive().unwrap(), "second");
        drop(transport.stdin.take());
        let err = transport.receive().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
        let err = transport.send("late").unwrap_err();
        assert!(err.to_string().contains("already closed"));
    }

    #[test]
    fn pipe_transport_reads_a_final_unterminated_message() {
        let mut transport = PipeTransport::spawn("printf", &["tail".to_string()]).unwrap();
        assert_eq!(transport.receive().unwrap(), "tail");
        assert!(transport.receive().is_err());
    }

    #[test]
    fn process_spawner_reports_missing_programs() {
        let err = ProcessSpawner
            .spawn("/nonexistent/browser", &[])
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(ProcessSpawner.spawn("true", &[]).is_ok());
    }

    #[test]
    fn session_matches_responses_by_id_and_records_events_in_between() {
        let (transport, sent) = ScriptedTransport::new(&[
            json!({ "method": "Runtime.consoleAPICalled", "params": { "type": "log", "args": [{ "type": "string", "value": "hi" }] } }),
            json!({ "method": "Runtime.consoleAPICalled", "params": { "type": "error", "args": [{ "type": "string", "value": "bad" }, { "type": "number", "value": 7 }], "stackTrace": { "callFrames": [{ "url": "http://app/main.js" }] } } }),
            json!({ "id": 1, "result": { "value": 1 } }),
        ]);
        let mut session = CdpSession::new(Box::new(transport));
        assert!(session.session_id().is_none());
        let result = session
            .call("Runtime.evaluate", json!({ "expression": "1" }))
            .unwrap();
        assert_eq!(result, json!({ "value": 1 }));
        let sent = sent.lock().unwrap().clone();
        assert_eq!(
            sent,
            vec![json!({ "id": 1, "method": "Runtime.evaluate", "params": { "expression": "1" } })]
        );
        let errors = session.drain_console_errors();
        assert_eq!(
            errors,
            vec![ConsoleError {
                source: "console".to_string(),
                text: "bad 7".to_string(),
                url: Some("http://app/main.js".to_string()),
            }]
        );
        assert!(session.drain_console_errors().is_empty());
    }

    #[test]
    fn session_attaches_to_a_new_page_and_scopes_later_calls() {
        let (transport, sent) = ScriptedTransport::new(&[
            json!({ "id": 1, "result": { "targetId": "T1" } }),
            json!({ "id": 2, "result": { "sessionId": "S1" } }),
            json!({ "id": 3, "result": {} }),
            json!({ "id": 4, "result": {} }),
            json!({ "id": 5, "result": {} }),
            json!({ "id": 6, "sessionId": "S1", "result": { "frameId": "F" } }),
        ]);
        let mut session = CdpSession::new(Box::new(transport));
        session.attach_new_page().unwrap();
        assert_eq!(session.session_id(), Some("S1"));
        session
            .call("Page.navigate", json!({ "url": "http://app/" }))
            .unwrap();
        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent[0]["method"], "Target.createTarget");
        assert_eq!(sent[0].get("sessionId"), None);
        assert_eq!(
            sent[1]["params"],
            json!({ "targetId": "T1", "flatten": true })
        );
        assert_eq!(sent[2]["method"], "Page.enable");
        assert_eq!(sent[2]["sessionId"], "S1");
        assert_eq!(sent[3]["method"], "Runtime.enable");
        assert_eq!(sent[4]["method"], "Log.enable");
        assert_eq!(sent[5]["method"], "Page.navigate");
        assert_eq!(sent[5]["sessionId"], "S1");
    }

    #[test]
    fn session_surfaces_protocol_errors_missing_fields_eof_and_garbage() {
        let (transport, _) = ScriptedTransport::new(&[
            json!({ "id": 1, "error": { "code": -32601, "message": "'Nope' wasn't found" } }),
            json!({ "id": 2, "error": { "code": -1 } }),
            json!({ "id": 3, "result": { "other": 1 } }),
        ]);
        let mut session = CdpSession::new(Box::new(transport));
        let err = session.call("Nope", json!({})).unwrap_err();
        assert_eq!(err.to_string(), "Nope failed: 'Nope' wasn't found");
        let err = session.call("Nope", json!({})).unwrap_err();
        assert_eq!(err.to_string(), "Nope failed: unknown error");
        let err = session.attach_new_page().unwrap_err();
        assert!(matches!(err, BrowserError::MissingField { .. }), "{err}");
        assert!(
            err.to_string()
                .starts_with("Target.createTarget returned no `targetId`"),
            "{err}"
        );
        let err = session.call("Runtime.evaluate", json!({})).unwrap_err();
        assert!(matches!(err, BrowserError::Pipe { .. }), "{err}");
        assert!(
            err.to_string()
                .contains("browser pipe closed during Runtime.evaluate"),
            "{err}"
        );

        let mut garbage = ScriptedTransport::new(&[]).0;
        garbage.incoming.push_back("not json".to_string());
        let mut session = CdpSession::new(Box::new(garbage));
        let err = session.call("X", json!({})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "browser sent a message that is not JSON: not json"
        );
    }

    #[test]
    fn session_reports_send_failures_as_pipe_errors() {
        struct Broken;
        impl CdpTransport for Broken {
            fn send(&mut self, _: &str) -> std::io::Result<()> {
                Err(std::io::Error::other("gone"))
            }
            fn receive(&mut self) -> std::io::Result<String> {
                unreachable!("send fails first")
            }
        }
        let err = CdpSession::new(Box::new(Broken))
            .call("Page.enable", json!({}))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "browser pipe closed during Page.enable: gone"
        );
    }

    #[test]
    fn console_errors_are_extracted_from_the_three_event_kinds_only() {
        let thrown = json!({ "method": "Runtime.exceptionThrown", "params": { "exceptionDetails": { "text": "Uncaught", "url": "http://app/x.js", "exception": { "description": "TypeError: boom\n at x" } } } });
        let error = console_error_from_event(&thrown).unwrap();
        assert_eq!(error.source, "exception");
        assert_eq!(error.text, "TypeError: boom\n at x");
        assert_eq!(error.url.as_deref(), Some("http://app/x.js"));
        let bare = json!({ "method": "Runtime.exceptionThrown", "params": { "exceptionDetails": { "text": "Uncaught" } } });
        assert_eq!(console_error_from_event(&bare).unwrap().text, "Uncaught");
        let nothing =
            json!({ "method": "Runtime.exceptionThrown", "params": { "exceptionDetails": {} } });
        assert_eq!(
            console_error_from_event(&nothing).unwrap().text,
            "uncaught exception"
        );
        let warning = json!({ "method": "Log.entryAdded", "params": { "entry": { "source": "network", "level": "warning", "text": "slow" } } });
        assert!(console_error_from_event(&warning).is_none());
        let minimal =
            json!({ "method": "Log.entryAdded", "params": { "entry": { "level": "error" } } });
        let error = console_error_from_event(&minimal).unwrap();
        assert_eq!(error.source, "log");
        assert_eq!(error.text, "");
        assert_eq!(error.url, None);
        let objects = json!({ "method": "Runtime.consoleAPICalled", "params": { "type": "error", "args": [{ "type": "object", "description": "Error: x" }, { "type": "undefined" }, { "type": "object", "value": { "a": 1 } }] } });
        let error = console_error_from_event(&objects).unwrap();
        assert_eq!(error.text, "Error: x [object] {\"a\":1}");
        assert_eq!(error.url, None);
        assert!(
            console_error_from_event(&json!({ "method": "Runtime.consoleAPICalled" })).is_none()
        );
        assert!(console_error_from_event(
            &json!({ "method": "Runtime.consoleAPICalled", "params": {} })
        )
        .is_none());
        assert!(console_error_from_event(&json!({ "params": {} })).is_none());
        assert!(console_error_from_event(
            &json!({ "method": "Page.loadEventFired", "params": {} })
        )
        .is_none());
        assert!(console_error_from_event(
            &json!({ "method": "Runtime.exceptionThrown", "params": {} })
        )
        .is_none());
        assert!(
            console_error_from_event(&json!({ "method": "Log.entryAdded", "params": {} }))
                .is_none()
        );
        let json_round_trip: ConsoleError =
            serde_json::from_value(serde_json::to_value(&error).unwrap()).unwrap();
        assert_eq!(json_round_trip, error);
    }

    #[test]
    fn base64_decodes_padded_input_and_rejects_bad_input() {
        assert_eq!(decode_base64("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(decode_base64("aGk=").unwrap(), b"hi");
        assert_eq!(decode_base64("aGV5").unwrap(), b"hey");
        assert_eq!(decode_base64("iVBORw0KGgo=").unwrap(), b"\x89PNG\r\n\x1a\n");
        assert_eq!(decode_base64("aGVs\nbG8=\n").unwrap(), b"hello");
        assert_eq!(decode_base64("+/8=").unwrap(), [0xfb, 0xff]);
        for bad in ["abc", "a===", "ab$=", "====", "ab=c"] {
            let err = decode_base64(bad).unwrap_err();
            assert!(matches!(err, BrowserError::Base64(_)), "{bad}: {err}");
        }
        assert!(decode_base64("ab$=")
            .unwrap_err()
            .to_string()
            .contains("unexpected character '$'"));
    }

    proptest! {
        #[test]
        fn base64_round_trips_any_bytes(data in proptest::collection::vec(any::<u8>(), 0..64)) {
            let encoded = encode_base64(&data);
            prop_assert_eq!(decode_base64(&encoded).unwrap(), data);
        }
    }
}
