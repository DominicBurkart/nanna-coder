use super::broker::{ArmedBroker, Broker, BrokerError};
use super::rpc::{serve_listener, LineReader, ReadLimits, ServeEnd};
use super::steer::{Coalescer, PiSteerDoorbell, SteerHandle};
use super::{
    launch_plan, BrokerPolicy, CheckedPlan, PiAdapter, PiExecutor, PiLaunch, PiRunOutcome,
    PlanError, RingAuditSink, BROKER_SOCKET_CONTAINER_PATH,
};
use crate::agent::{AgentError, AgentRunResult, AgentState};
use crate::container::{detect_runtime, ContainerRuntime};
use crate::entities::context::types::ToolCallRecord;
use crate::hub::doorbell::doorbell_text;
use crate::hub::lineage::SessionId;
use crate::task::TaskId;
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::Instant;

#[doc = "Environment variable naming the operator-provisioned podman network the agent container joins. Unset means no network."]
pub const PI_NETWORK_ENV: &str = "NANNA_PI_NETWORK";

const CONTAINER_USER: &str = "65534:65534";
const AGENT_DIR_ENV: &str = "PI_CODING_AGENT_DIR";
const HOME_ENV: &str = "HOME";
const WORK_TMPFS_OPTIONS: &str = "rw,noexec,nosuid,size=64m,uid=65534,gid=65534";
const RESERVED_NETWORKS: [&str; 8] = [
    "host",
    "none",
    "bridge",
    "private",
    "slirp4netns",
    "pasta",
    "default",
    "podman",
];

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && name.chars().all(is_name_char)
}

#[doc = "A refused network name."]
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PiNetworkError {
    #[doc = "The name is empty, starts with `-` or holds characters outside `[A-Za-z0-9_.-]`."]
    #[error("invalid podman network name `{0}`")]
    Invalid(String),
    #[doc = "The name selects the host network, no network or a built-in mode, none of which is an operator-provisioned network."]
    #[error("podman network `{0}` is a built-in mode, not an operator-provisioned network")]
    Reserved(String),
}

#[doc = r#"The network the agent container joins. `None` is the default and gives the container no network. `Named` is an operator-provisioned podman network chosen explicitly; nanna does not create it and cannot verify that it reaches only the model gateway.

```
use harness::harness_adapter::PiNetwork;

assert_eq!(PiNetwork::default(), PiNetwork::None);
assert!(PiNetwork::named("model-net").is_ok());
assert!(PiNetwork::named("host").is_err());
assert!(PiNetwork::named("-x").is_err());
```"#]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PiNetwork {
    #[doc = "No network."]
    #[default]
    None,
    #[doc = "A named, operator-provisioned network."]
    Named(String),
}

impl PiNetwork {
    #[doc = "Validate a network name. Reserved built-in modes are refused."]
    pub fn named(name: impl Into<String>) -> Result<Self, PiNetworkError> {
        let name = name.into();
        if !is_plain_name(&name) {
            return Err(PiNetworkError::Invalid(name));
        }
        if RESERVED_NETWORKS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(PiNetworkError::Reserved(name));
        }
        Ok(Self::Named(name))
    }

    fn arg(&self) -> String {
        match self {
            PiNetwork::None => "--network=none".to_string(),
            PiNetwork::Named(name) => format!("--network={name}"),
        }
    }
}

#[doc = r#"Runs pi in a rootless-podman container created from the digest-pinned agent image: read-only root, all capabilities dropped, `no-new-privileges`, a non-root user, no pull, memory and pids limits, a writable volume only for pi's agent directory, and the broker socket as the only bind mount. The workspace is never mounted. The host serves the broker socket after the runtime's tool list has been verified against the plan.

```
use harness::harness_adapter::{PiNetwork, PodmanPiExecutor};

let isolated = PodmanPiExecutor::isolated();
assert_eq!(format!("{isolated:?}"), format!("{:?}", PodmanPiExecutor::new(PiNetwork::None)));
```"#]
#[derive(Debug, Clone)]
pub struct PodmanPiExecutor {
    network: PiNetwork,
    socket_root: Option<PathBuf>,
    doorbells: Option<Arc<PiSteerDoorbell>>,
}

impl PodmanPiExecutor {
    #[doc = "An executor whose containers have no network."]
    pub fn isolated() -> Self {
        Self::new(PiNetwork::None)
    }

    #[doc = "An executor whose containers join `network`."]
    pub fn new(network: PiNetwork) -> Self {
        Self {
            network,
            socket_root: None,
            doorbells: None,
        }
    }

    /// Register every run's session with `bell`, so a ring for that session steers the running
    /// pi turn. Without it nothing is registered and a ring is a no-op.
    ///
    /// ```
    /// use harness::harness_adapter::{PiSteerDoorbell, PodmanPiExecutor};
    /// use std::sync::Arc;
    ///
    /// let bell = Arc::new(PiSteerDoorbell::new());
    /// let executor = PodmanPiExecutor::isolated().with_doorbells(bell);
    /// assert!(format!("{executor:?}").contains("PiSteerDoorbell"));
    /// ```
    pub fn with_doorbells(mut self, bell: Arc<PiSteerDoorbell>) -> Self {
        self.doorbells = Some(bell);
        self
    }

    #[doc = r#"Read the network from the environment: unset means none, set means a named network.

```
use harness::harness_adapter::{PodmanPiExecutor, PI_NETWORK_ENV};
use std::ffi::OsString;

let none = PodmanPiExecutor::from_lookup(&|_| None).unwrap();
let named = PodmanPiExecutor::from_lookup(&|key| (key == PI_NETWORK_ENV).then(|| OsString::from("model-net"))).unwrap();
assert_ne!(format!("{none:?}"), format!("{named:?}"));
assert!(PodmanPiExecutor::from_lookup(&|_| Some(OsString::from("host"))).is_err());
```"#]
    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<OsString>) -> Result<Self, PiNetworkError> {
        match lookup(PI_NETWORK_ENV) {
            None => Ok(Self::isolated()),
            Some(raw) => Ok(Self::new(PiNetwork::named(
                raw.to_string_lossy().into_owned(),
            )?)),
        }
    }

    #[doc = r#"Bind the broker socket under `root` instead of the plan's directory. The container still sees it at the fixed in-container path.

```
use harness::harness_adapter::PodmanPiExecutor;

let executor = PodmanPiExecutor::isolated().with_socket_root("/run/user/1000/nanna/sockets");
assert!(format!("{executor:?}").contains("sockets"));
```"#]
    pub fn with_socket_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.socket_root = Some(root.into());
        self
    }

    pub(super) fn host_socket(&self, plan_socket: &Path) -> PathBuf {
        match (&self.socket_root, plan_socket.file_name()) {
            (Some(root), Some(file)) => root.join(file),
            _ => plan_socket.to_path_buf(),
        }
    }

    pub(super) fn create_args(
        &self,
        plan: &CheckedPlan,
        name: &str,
        limits: super::PiLimits,
        host_socket: &Path,
    ) -> Result<Vec<String>, String> {
        let socket = host_socket.to_string_lossy();
        if socket.contains([',', '=', '\n']) {
            return Err(format!("broker socket path `{socket}` cannot be mounted"));
        }
        let env_value = |key: &str| {
            plan.env
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| format!("the plan has no {key}"))
        };
        let agent_dir = env_value(AGENT_DIR_ENV)?;
        let home = env_value(HOME_ENV)?;
        let mut args: Vec<String> = [
            "create".to_string(),
            "--name".to_string(),
            name.to_string(),
            "--pull=never".to_string(),
            "--read-only".to_string(),
            "--user".to_string(),
            CONTAINER_USER.to_string(),
            "--cap-drop=ALL".to_string(),
            "--security-opt=no-new-privileges".to_string(),
            self.network.arg(),
            format!("--memory={}m", limits.memory_mb),
            format!("--pids-limit={}", limits.pids),
            "--interactive".to_string(),
            "--workdir".to_string(),
            home.clone(),
            "--mount".to_string(),
            format!("type=volume,destination={agent_dir},U=true"),
            "--tmpfs".to_string(),
            format!("{home}:{WORK_TMPFS_OPTIONS}"),
            "--mount".to_string(),
            format!("type=bind,source={socket},destination={BROKER_SOCKET_CONTAINER_PATH}"),
        ]
        .to_vec();
        for (key, value) in &plan.env {
            args.push("--env".to_string());
            args.push(format!("{key}={value}"));
        }
        args.push(plan.image.clone());
        args.extend(plan.argv.iter().cloned());
        Ok(args)
    }
}

pub(super) fn steer_line(count: usize) -> String {
    serde_json::json!({"type": "steer", "message": doorbell_text(count)}).to_string()
}

async fn next_steer(steer: &mut Option<SteerHandle>) -> usize {
    match steer.as_mut() {
        Some(handle) => handle.next().await,
        None => std::future::pending().await,
    }
}

pub(super) fn start_args(name: &str) -> Vec<String> {
    ["start", "--attach", "--interactive", name]
        .map(String::from)
        .to_vec()
}

pub(super) fn copy_args(name: &str) -> Vec<String> {
    ["cp", "-", &format!("{name}:/")].map(String::from).to_vec()
}

pub(super) fn remove_args(name: &str) -> Vec<String> {
    ["rm", "-f", "--volumes", name].map(String::from).to_vec()
}

struct RemoveGuard {
    command: &'static str,
    name: String,
}

impl RemoveGuard {
    fn arm(runtime: &ContainerRuntime, name: &str) -> Self {
        Self {
            command: runtime.command(),
            name: name.to_string(),
        }
    }
}

impl Drop for RemoveGuard {
    fn drop(&mut self) {
        super::route::off_runtime(|| {
            super::route::bounded_status(
                self.command,
                &remove_args(&self.name),
                super::route::REMOVE_LIMIT,
            );
        });
    }
}

pub(super) struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(super) fn is_private(owner: u32, effective: u32, mode: u32, is_dir: bool) -> bool {
    is_dir && owner == effective && mode & 0o077 == 0
}

pub(super) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !is_private(
        meta.uid(),
        unsafe { libc::geteuid() },
        meta.mode(),
        meta.is_dir(),
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a private directory owned by this user",
                dir.display()
            ),
        ));
    }
    Ok(())
}

pub(super) fn bind_socket(path: &Path) -> std::io::Result<(tokio::net::UnixListener, SocketGuard)> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
    ensure_private_dir(parent)?;
    let address = std::os::unix::net::SocketAddr::from_pathname(path)?;
    let std_listener = std::os::unix::net::UnixListener::bind_addr(&address)?;
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(std_listener)?;
    let guard = SocketGuard(path.to_path_buf());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok((listener, guard))
}

const TAR_BLOCK: usize = 512;

fn octal(value: u64, width: usize) -> Vec<u8> {
    let mut text = format!("{value:0w$o}", w = width - 1).into_bytes();
    text.push(0);
    text
}

pub(super) fn tar_header(
    name: &str,
    size: u64,
    mode: u64,
    kind: u8,
) -> Result<[u8; TAR_BLOCK], String> {
    if name.len() > 100 {
        return Err(format!("`{name}` is too long for an archive entry"));
    }
    let mut block = [0u8; TAR_BLOCK];
    block[..name.len()].copy_from_slice(name.as_bytes());
    block[100..108].copy_from_slice(&octal(mode, 8));
    block[108..116].copy_from_slice(&octal(65534, 8));
    block[116..124].copy_from_slice(&octal(65534, 8));
    block[124..136].copy_from_slice(&octal(size, 12));
    block[136..148].copy_from_slice(&octal(0, 12));
    block[148..156].fill(b' ');
    block[156] = kind;
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    let sum: u32 = block.iter().map(|b| u32::from(*b)).sum();
    let mut check = format!("{sum:06o}").into_bytes();
    check.push(0);
    check.push(b' ');
    block[148..156].copy_from_slice(&check);
    Ok(block)
}

pub(super) fn plan_archive(plan: &CheckedPlan) -> Result<Vec<u8>, String> {
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for file in &plan.files {
        let relative = file
            .path
            .to_string_lossy()
            .trim_start_matches('/')
            .to_string();
        let mut prefix = String::new();
        let parts: Vec<&str> = relative.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            prefix.push_str(part);
            prefix.push('/');
            dirs.insert(prefix.clone());
        }
    }
    let mut out = Vec::new();
    for dir in &dirs {
        out.extend_from_slice(&tar_header(dir, 0, 0o755, b'5')?);
    }
    for file in &plan.files {
        let relative = file
            .path
            .to_string_lossy()
            .trim_start_matches('/')
            .to_string();
        let bytes = file.contents.as_bytes();
        out.extend_from_slice(&tar_header(&relative, bytes.len() as u64, 0o644, b'0')?);
        out.extend_from_slice(bytes);
        out.resize(out.len().div_ceil(TAR_BLOCK) * TAR_BLOCK, 0);
    }
    out.resize(out.len() + 2 * TAR_BLOCK, 0);
    Ok(out)
}

pub(super) enum Step {
    Continue,
    Handshake(Vec<String>),
    Settled,
    Fail(String),
    TurnLimit,
}

pub(super) struct Final {
    pub(super) error: Option<String>,
    pub(super) text: String,
}

pub(super) struct Tracker {
    pub(super) max_turns: usize,
    pub(super) progress: Arc<AtomicUsize>,
    pub(super) turns: usize,
    pub(super) tools: Option<BTreeSet<String>>,
    pub(super) pending: Vec<(String, String, Value)>,
    pub(super) calls: Vec<ToolCallRecord>,
    pub(super) last_end: Option<Final>,
}

pub(super) const MAX_SUMMARY_BYTES: usize = 64 * 1024;
pub(super) const MAX_CALL_RESULT_BYTES: usize = 16 * 1024;
const CLIPPED: &str = "\n[clipped]";

pub(super) fn clip_text(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(CLIPPED);
    text
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

impl Tracker {
    pub(super) fn new(max_turns: usize, progress: Arc<AtomicUsize>) -> Self {
        Self {
            max_turns,
            progress,
            turns: 0,
            tools: None,
            pending: Vec::new(),
            calls: Vec::new(),
            last_end: None,
        }
    }

    pub(super) fn feed(&mut self, line: &str) -> Step {
        if line.is_empty() {
            return Step::Continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return Step::Fail("the runtime wrote a line that is not JSON".to_string());
        };
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "response" => self.response(&event),
            "message_start" => self.message_start(&event),
            "turn_start" => {
                self.turns += 1;
                self.progress.fetch_add(1, Ordering::SeqCst);
                if self.turns > self.max_turns {
                    Step::TurnLimit
                } else {
                    Step::Continue
                }
            }
            "tool_execution_start" => self.tool_start(&event),
            "tool_execution_end" => self.tool_end(&event),
            "agent_end" => {
                self.agent_end(&event);
                Step::Continue
            }
            "agent_settled" => {
                if event.get("aborted").and_then(Value::as_bool) == Some(false) {
                    Step::Settled
                } else {
                    Step::Fail("the runtime aborted the run".to_string())
                }
            }
            _ => Step::Continue,
        }
    }

    fn response(&self, event: &Value) -> Step {
        let prompt = event.get("command").and_then(Value::as_str) == Some("prompt");
        if prompt && event.get("success").and_then(Value::as_bool) != Some(true) {
            let why = event
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("no reason given");
            return Step::Fail(format!("the runtime rejected the prompt: {why}"));
        }
        Step::Continue
    }

    fn message_start(&mut self, event: &Value) -> Step {
        let message = &event["message"];
        let Some(added) = message.get("toolsAdded") else {
            return Step::Continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("system") {
            return Step::Continue;
        }
        let names: Option<Vec<String>> = added.as_array().and_then(|tools| {
            tools
                .iter()
                .map(|tool| tool.get("name").and_then(Value::as_str).map(String::from))
                .collect()
        });
        let Some(names) = names else {
            return Step::Fail("the runtime reported a malformed tool list".to_string());
        };
        let set: BTreeSet<String> = names.iter().cloned().collect();
        match &self.tools {
            None => {
                self.tools = Some(set);
                Step::Handshake(names)
            }
            Some(first) if *first == set => Step::Continue,
            Some(_) => {
                Step::Fail("the runtime's tool list changed after the handshake".to_string())
            }
        }
    }

    fn tool_start(&mut self, event: &Value) -> Step {
        if self.tools.is_none() {
            return Step::Fail("a tool ran before the runtime's tools were verified".to_string());
        }
        let field = |key: &str| {
            event
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        self.pending.push((
            field("toolCallId"),
            field("toolName"),
            event["args"].clone(),
        ));
        Step::Continue
    }

    fn tool_end(&mut self, event: &Value) -> Step {
        let id = event
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("");
        let (name, arguments) = match self
            .pending
            .iter()
            .position(|(pending, _, _)| pending == id)
        {
            Some(index) => {
                let (_, name, arguments) = self.pending.remove(index);
                (name, arguments)
            }
            None => (
                event
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                Value::Null,
            ),
        };
        self.calls.push(ToolCallRecord {
            tool_name: name,
            arguments,
            call_id: id.to_string(),
            result: clip_text(text_of(&event["result"]["content"]), MAX_CALL_RESULT_BYTES),
            effect: None,
        });
        Step::Continue
    }

    fn agent_end(&mut self, event: &Value) {
        let last = event
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| {
                messages
                    .iter()
                    .rev()
                    .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
            });
        let Some(last) = last else {
            self.last_end = Some(Final {
                error: Some("the run ended without an assistant message".to_string()),
                text: String::new(),
            });
            return;
        };
        let failed = last.get("stopReason").and_then(Value::as_str) == Some("error");
        let error = failed.then(|| {
            last.get("errorMessage")
                .and_then(Value::as_str)
                .unwrap_or("the model call failed")
                .to_string()
        });
        self.last_end = Some(Final {
            error,
            text: text_of(&last["content"]),
        });
    }
}

fn state_error(message: String, turns: usize, calls: Vec<ToolCallRecord>) -> AgentError {
    AgentError::StateError {
        message,
        iterations_completed: turns,
        tool_calls_made: calls,
        conversation_snapshot: vec![],
        last_agent_state: AgentState::Error("pi run failed".to_string()),
    }
}

fn refusal(message: String) -> PiRunOutcome {
    PiRunOutcome {
        result: Err(state_error(message, 0, vec![])),
        action_audit: Vec::new(),
    }
}

async fn run_runtime(
    runtime: &ContainerRuntime,
    args: Vec<String>,
    stdin: Option<Vec<u8>>,
    deadline: Instant,
) -> Result<(), String> {
    let what = args.first().cloned().unwrap_or_default();
    let mut command = Command::new(runtime.command());
    command
        .args(&args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|e| format!("podman {what}: {e}"))?;
    let work = async {
        if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(&bytes).await?;
            pipe.shutdown().await?;
        }
        child.wait().await
    };
    judge(&what, tokio::time::timeout_at(deadline, work).await.ok())
}

pub(super) fn judge(
    what: &str,
    outcome: Option<std::io::Result<std::process::ExitStatus>>,
) -> Result<(), String> {
    match outcome {
        None => Err(format!("podman {what} exceeded the wall-clock limit")),
        Some(Err(e)) => Err(format!("podman {what}: {e}")),
        Some(Ok(status)) if status.success() => Ok(()),
        Some(Ok(status)) => Err(format!("podman {what} failed: {status}")),
    }
}

pub(super) async fn send_lines<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    lines: &[String],
    deadline: Instant,
) -> bool {
    for line in lines {
        let sent = async {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await
        };
        if !matches!(tokio::time::timeout_at(deadline, sent).await, Ok(Ok(()))) {
            return false;
        }
    }
    true
}

pub(super) struct Refused(pub(super) String);

impl From<String> for Refused {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<PlanError> for Refused {
    fn from(error: PlanError) -> Self {
        Self(format!("launch plan refused: {error}"))
    }
}

impl From<BrokerError> for Refused {
    fn from(error: BrokerError) -> Self {
        Self(format!("broker refused: {error}"))
    }
}

impl From<tokio::task::JoinError> for Refused {
    fn from(error: tokio::task::JoinError) -> Self {
        Self(format!("runtime detection failed: {error}"))
    }
}

struct Started {
    session: Session,
    runtime: ContainerRuntime,
    name: String,
    create: Vec<String>,
    archive: Vec<u8>,
    broker: Broker,
    listener: tokio::net::UnixListener,
    deadline: Instant,
    guard: RemoveGuard,
    socket_guard: SocketGuard,
}

impl Started {
    async fn go(self) -> PiRunOutcome {
        let Started {
            session,
            runtime,
            name,
            create,
            archive,
            broker,
            listener,
            deadline,
            guard,
            socket_guard,
        } = self;
        let outcome = session
            .drive(&runtime, &name, create, archive, broker, listener, deadline)
            .await;
        drop(guard);
        drop(socket_guard);
        outcome
    }
}

impl PodmanPiExecutor {
    async fn start(&self, launch: PiLaunch<'_>) -> Result<Started, Refused> {
        let deadline = Instant::now() + launch.deadline();
        let name = launch.container_name().to_string();
        let launch_task_id = launch.task_id().to_string();
        if name != super::pi_container_name(launch.task_id()) {
            return Err(Refused(format!(
                "container name `{name}` is not the task's name"
            )));
        }
        let agent = launch.agent().clone();
        let limits = launch.route().limits();
        let progress = Arc::clone(launch.progress());
        let wall = launch.deadline();
        let plan = launch_plan(&PiAdapter, &agent)?;
        let socket = self.host_socket(&plan.broker_socket);
        let create = self.create_args(&plan, &name, limits, &socket)?;
        let archive = plan_archive(&plan)?;
        let binding = launch.into_binding();
        let policy = BrokerPolicy::default();
        let sink = Arc::new(RingAuditSink::default());
        let broker = Broker::for_agent(&agent, binding, policy, sink)?;
        if broker.task_id().0 != launch_task_id {
            return Err(Refused("the broker was bound to another task".to_string()));
        }
        let runtime = match tokio::task::spawn_blocking(detect_runtime).await? {
            ContainerRuntime::Podman => ContainerRuntime::Podman,
            ContainerRuntime::Docker => {
                return Err(Refused(
                    "pi runs only under podman; docker is not accepted".to_string(),
                ))
            }
            ContainerRuntime::None => {
                return Err(Refused("no container runtime is available".to_string()))
            }
        };
        let (listener, socket_guard) = bind_socket(&socket)
            .map_err(|e| Refused(format!("broker socket {}: {e}", socket.display())))?;
        let guard = RemoveGuard::arm(&runtime, &name);
        let max_turns = agent.limits().max_iterations;
        let session_id = SessionId::from_task(TaskId(launch_task_id));
        let steer = self
            .doorbells
            .as_ref()
            .map(|bell| bell.register(session_id));
        let session = Session {
            plan,
            tracker: Tracker::new(max_turns, progress),
            wall,
            steer,
        };
        Ok(Started {
            session,
            runtime,
            name,
            create,
            archive,
            broker,
            listener,
            deadline,
            guard,
            socket_guard,
        })
    }
}

#[async_trait::async_trait]
impl PiExecutor for PodmanPiExecutor {
    async fn run(&self, launch: PiLaunch<'_>) -> PiRunOutcome {
        match self.start(launch).await {
            Ok(started) => started.go().await,
            Err(Refused(message)) => refusal(message),
        }
    }
}

struct Session {
    plan: CheckedPlan,
    tracker: Tracker,
    wall: std::time::Duration,
    steer: Option<SteerHandle>,
}

enum Ended {
    Settled,
    Failed(String),
    TurnLimit,
    Halted(super::HaltReason),
}

type Serving = std::pin::Pin<Box<dyn std::future::Future<Output = ServeEnd> + Send>>;

impl Session {
    #[allow(clippy::too_many_arguments)]
    async fn drive(
        mut self,
        runtime: &ContainerRuntime,
        name: &str,
        create: Vec<String>,
        archive: Vec<u8>,
        broker: Broker,
        listener: tokio::net::UnixListener,
        deadline: Instant,
    ) -> PiRunOutcome {
        let prepared = async {
            run_runtime(runtime, create, None, deadline).await?;
            run_runtime(runtime, copy_args(name), Some(archive), deadline).await
        }
        .await;
        if let Err(message) = prepared {
            return self.finish(Ended::Failed(message), None);
        }
        let mut child = match Command::new(runtime.command())
            .args(start_args(name))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => return self.finish(Ended::Failed(format!("podman start: {e}")), None),
        };
        let mut stdin = child.stdin.take().expect("podman start has a piped stdin");
        let stdout = child
            .stdout
            .take()
            .expect("podman start has a piped stdout");
        if !send_lines(&mut stdin, &self.plan.stdin, deadline).await {
            return self.finish(Ended::Failed("writing the prompt failed".to_string()), None);
        }
        let mut events = LineReader::new(stdout, ReadLimits::for_events(self.wall));
        let mut listener = Some(listener);
        let mut unarmed = Some(broker);
        let mut serving: Option<Serving> = None;
        let mut armed: Option<Arc<ArmedBroker>> = None;
        let mut steer = self.steer.take();
        let mut coalescer = Coalescer::default();
        let mut due: Option<usize> = None;
        let ended = loop {
            tokio::select! {
                line = events.next_line() => match line {
                    Ok(Some(line)) => {
                    let before = self.tracker.turns;
                    let step = self.tracker.feed(&line);
                    if self.tracker.turns != before {
                        due = coalescer.turn_start();
                    }
                    match step {
                        Step::Continue => {}
                        Step::Settled => break Ended::Settled,
                        Step::Fail(message) => break Ended::Failed(message),
                        Step::TurnLimit => break Ended::TurnLimit,
                        Step::Handshake(names) => {
                            let (Some(broker), Some(listener)) = (unarmed.take(), listener.take()) else {
                                break Ended::Failed("a second handshake was attempted".to_string());
                            };
                            match broker.arm(&self.plan, &names) {
                                Ok(ready) => {
                                    let ready = Arc::new(ready);
                                    armed = Some(Arc::clone(&ready));
                                    due = coalescer.arm();
                                    serving = Some(Box::pin(serve_listener(
                                        listener,
                                        ready,
                                        ReadLimits::for_requests(),
                                    )));
                                }
                                Err(e) => break Ended::Failed(format!("lockdown not verified: {e}")),
                            }
                        }
                    }
                    }
                    Ok(None) => break Ended::Failed("the runtime exited before the run settled".to_string()),
                    Err(e) => break Ended::Failed(format!("the runtime's event stream failed: {e}")),
                },
                count = next_steer(&mut steer) => {
                    due = coalescer.on_ring(count);
                }
                end = async {
                    match serving.as_mut() {
                        Some(serving) => serving.await,
                        None => std::future::pending().await,
                    }
                } => match end {
                    ServeEnd::Halted(reason) => break Ended::Halted(reason),
                    ServeEnd::Failed(message) => break Ended::Failed(message),
                },
                _ = tokio::time::sleep_until(deadline) => {
                    break Ended::Failed(format!(
                        "the pi run exceeded its wall-clock limit of {}s",
                        self.wall.as_secs()
                    ));
                }
            }
            if let Some(count) = due.take() {
                let line = [steer_line(count)];
                if !send_lines(&mut stdin, &line, deadline).await {
                    break Ended::Failed("writing the steer failed".to_string());
                }
            }
        };
        drop(steer);
        drop(serving);
        drop(stdin);
        drop(child);
        self.finish(ended, armed.as_deref())
    }

    fn finish(self, ended: Ended, armed: Option<&ArmedBroker>) -> PiRunOutcome {
        let action_audit = armed.map(ArmedBroker::action_audit).unwrap_or_default();
        let Tracker {
            turns,
            calls,
            last_end,
            max_turns,
            ..
        } = self.tracker;
        let failure = |message: String, calls: Vec<ToolCallRecord>| PiRunOutcome {
            result: Err(state_error(message, turns, calls)),
            action_audit: action_audit.clone(),
        };
        match ended {
            Ended::Failed(message) => failure(message, calls),
            Ended::Halted(reason) => failure(reason.message(), calls),
            Ended::TurnLimit => PiRunOutcome {
                result: Err(AgentError::MaxIterationsExceeded {
                    iterations_completed: max_turns,
                    tool_calls_made: calls,
                    conversation_snapshot: vec![],
                    last_agent_state: AgentState::Error("turn limit reached".to_string()),
                }),
                action_audit,
            },
            Ended::Settled => match last_end {
                None => failure(
                    "the run settled without finishing a turn".to_string(),
                    calls,
                ),
                Some(Final {
                    error: Some(why), ..
                }) => failure(format!("the model run failed: {why}"), calls),
                Some(Final { text, .. }) => PiRunOutcome {
                    result: Ok(AgentRunResult {
                        final_state: AgentState::Completed,
                        iterations: turns,
                        task_completed: true,
                        result_summary: clip_text(text, MAX_SUMMARY_BYTES),
                        tool_calls_made: calls,
                        conversation_snapshot: vec![],
                        token_usage: None,
                        denials: vec![],
                        action_audit: action_audit.clone(),
                    }),
                    action_audit,
                },
            },
        }
    }
}
