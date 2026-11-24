use super::binding::BoundWorkspace;
use super::executor::{
    clip_text, copy_args, ensure_private_dir, plan_archive, remove_args, start_args, tar_header,
    Step, Tracker, MAX_CALL_RESULT_BYTES, MAX_SUMMARY_BYTES,
};
use super::testing::{allowed_for, base_registry, calls, fail_with, ok_with, scoped, Behavior};
use super::tests::{gateway, Card, SOCKET};
use super::*;
use crate::agent::AgentError;
use crate::test_support::FakePodman;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const TASK: &str = "t-test";
const NAME: &str = "nanna-pi-t-test";

fn spec_of(tools: &[(&str, EffectClass, Behavior)]) -> Vec<CapabilitySpec> {
    tools
        .iter()
        .map(|(n, e, _)| {
            CapabilitySpec::new(
                *n,
                format!("{n} tool"),
                serde_json::json!({"type": "object"}),
                *e,
            )
        })
        .collect()
}

struct Rig {
    agent: ResolvedAgent,
    identity: AgentIdentity,
    repo: String,
    registry: Option<crate::tools::ToolRegistry>,
    counters: super::testing::Counters,
    route: PiRoute,
    progress: Arc<AtomicUsize>,
}

fn rig(tools: Vec<(&str, EffectClass, Behavior)>) -> Rig {
    let card = Card::new("\"*\"", "workspace");
    let allowed = allowed_for(&card.identity());
    let grant = scoped(allowed.identity(), &spec_of(&tools));
    let agent =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    let (registry, counters) = base_registry(tools);
    let route = PiRoute::new(
        format!("nanna-pi@sha256:{}", "a".repeat(64)),
        "/run/nanna/broker",
        PiLimits {
            memory_mb: 512,
            pids: 64,
        },
        Arc::new(UnwiredPiExecutor),
    )
    .unwrap();
    Rig {
        identity: allowed.identity().clone(),
        repo: allowed.repo().to_string(),
        registry: Some(registry.scoped_for(allowed.identity())),
        agent,
        counters,
        route,
        progress: Arc::new(AtomicUsize::new(0)),
    }
}

impl Rig {
    fn plan(&self) -> CheckedPlan {
        launch_plan(&PiAdapter, &self.agent).unwrap()
    }

    fn launch<'a>(&'a mut self, task: &'a str, name: &'a str, deadline: Duration) -> PiLaunch<'a> {
        let bound = BoundWorkspace::for_test(
            self.registry.take().unwrap(),
            &self.identity,
            &self.repo,
            &self.agent,
        );
        PiLaunch::new(
            task,
            name,
            &self.agent,
            bound,
            &self.route,
            Arc::clone(&self.progress),
            deadline,
        )
    }
}

fn two_tools() -> Vec<(&'static str, EffectClass, Behavior)> {
    vec![
        (
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!("contents")),
        ),
        ("search", EffectClass::None, ok_with(serde_json::json!(2))),
    ]
}

fn escalating() -> Vec<(&'static str, EffectClass, Behavior)> {
    use crate::action_auditor::ActionDenied;
    use crate::auditor::{Reason, ReasonCode};
    use crate::tools::ToolError;
    vec![(
        "t_escalate",
        EffectClass::None,
        fail_with(|| {
            ToolError::ActionDenied(ActionDenied::Escalate {
                reasons: vec![Reason::new(ReasonCode::Other, "needs a human")],
            })
        }),
    )]
}

fn event(value: serde_json::Value) -> String {
    value.to_string()
}

fn prompt_ok() -> String {
    event(serde_json::json!({"id":"task","type":"response","command":"prompt","success":true}))
}

fn system_start(tools: &[&str]) -> String {
    let added: Vec<_> = tools
        .iter()
        .map(|n| serde_json::json!({"name": n}))
        .collect();
    event(
        serde_json::json!({"type":"message_start","message":{"role":"system","toolsAdded":added}}),
    )
}

fn turn_start() -> String {
    event(serde_json::json!({"type":"turn_start"}))
}

fn tool_start(id: &str, name: &str) -> String {
    event(
        serde_json::json!({"type":"tool_execution_start","toolCallId":id,"toolName":name,"args":{"path":"a"}}),
    )
}

fn tool_end(id: &str, name: &str, text: &str) -> String {
    event(
        serde_json::json!({"type":"tool_execution_end","toolCallId":id,"toolName":name,"result":{"content":[{"type":"text","text":text}]},"isError":false}),
    )
}

fn agent_end(stop: &str, text: &str, error: Option<&str>) -> String {
    let mut message = serde_json::json!({"role":"assistant","content":[{"type":"text","text":text}],"stopReason":stop});
    if let Some(error) = error {
        message["errorMessage"] = serde_json::json!(error);
    }
    event(
        serde_json::json!({"type":"agent_end","messages":[{"role":"user","content":"x"},message]}),
    )
}

fn settled(aborted: bool) -> String {
    event(serde_json::json!({"type":"agent_settled","aborted":aborted}))
}

const SCRIPT: &str = r#"
case "$1" in
create) echo $$ > "$d/create.pid"; [ -f "$d/hang-create" ] && exec sleep 30; [ -f "$d/fail-create" ] && exit 1;;
cp) echo $$ > "$d/cp.pid"; [ -f "$d/hang-cp" ] && exec sleep 30; cat > "$d/cp.tar";;
rm) [ -f "$d/hang-rm" ] && exec sleep 60;;
start)
  echo $$ > "$d/start.pid"
  read -r prompt
  printf '%s\n' "$prompt" > "$d/prompt.txt"
  [ -f "$d/hang" ] && exec sleep 30
  cat "$d/part1.jsonl"
  while [ ! -f "$d/go" ]; do sleep 0.02; done
  cat "$d/part2.jsonl";;
esac
"#;

fn write_parts(fake: &FakePodman, part1: &[String], part2: &[String]) {
    std::fs::write(fake.dir().join("part1.jsonl"), part1.join("\n") + "\n").unwrap();
    std::fs::write(fake.dir().join("part2.jsonl"), part2.join("\n") + "\n").unwrap();
}

fn private_root() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn executor(root: &Path) -> PodmanPiExecutor {
    PodmanPiExecutor::isolated().with_socket_root(root)
}

async fn call_broker(socket: &Path, line: &str) -> String {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\n');
    call_broker_bytes(socket, &bytes).await
}

async fn call_broker_bytes(socket: &Path, bytes: &[u8]) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = None;
    for _ in 0..1500 {
        match tokio::net::UnixStream::connect(socket).await {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    let Some(mut stream) = stream else {
        return String::new();
    };
    let mut reply = String::new();
    let sent = async {
        stream.write_all(bytes).await?;
        stream.read_to_string(&mut reply).await
    }
    .await;
    if sent.is_err() {
        reply.clear();
    }
    reply
}

fn is_state_error(outcome: &PiRunOutcome, needle: &str) -> bool {
    let matched = matches!(&outcome.result, Err(AgentError::StateError { message, .. }) if message.contains(needle));
    if !matched {
        eprintln!(
            "expected a state error containing {needle:?}, got {:?}",
            outcome.result
        );
    }
    matched
}

fn removed(fake: &FakePodman) -> bool {
    fake.calls().contains(&format!("rm -f --volumes {NAME}"))
}

#[test]
fn the_create_argv_is_exact() {
    let rig = rig(two_tools());
    let plan = rig.plan();
    let executor = PodmanPiExecutor::isolated();
    let actual = executor
        .create_args(
            &plan,
            NAME,
            PiLimits {
                memory_mb: 512,
                pids: 64,
            },
            Path::new("/run/user/1000/nanna/broker/t-test.sock"),
        )
        .unwrap();
    let expected: Vec<&str> = vec![
        "create",
        "--name",
        "nanna-pi-t-test",
        "--pull=never",
        "--read-only",
        "--user",
        "65534:65534",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--network=none",
        "--memory=512m",
        "--pids-limit=64",
        "--interactive",
        "--workdir",
        "/nanna/work",
        "--mount",
        "type=volume,destination=/nanna/pi,U=true",
        "--tmpfs",
        "/nanna/work:rw,noexec,nosuid,size=64m,uid=65534,gid=65534",
        "--mount",
        "type=bind,source=/run/user/1000/nanna/broker/t-test.sock,destination=/nanna/broker.sock",
        "--env",
        "PI_CODING_AGENT_DIR=/nanna/pi",
        "--env",
        "HOME=/nanna/work",
        "--env",
        "NANNA_BROKER_SOCKET=/nanna/broker.sock",
        "--env",
        "NANNA_CAPABILITIES=/nanna/pi/capabilities.json",
        "nanna-agent:pi",
        "pi",
        "--mode",
        "rpc",
        "--no-session",
        "--provider",
        "nanna",
        "--model",
        "gemma4:e4b",
        "--no-builtin-tools",
        "--no-extensions",
        "--extension",
        "/nanna/pi/extensions/nanna-capabilities.ts",
        "--no-skills",
        "--no-prompt-templates",
        "--no-context-files",
        "--no-mcp",
        "--no-approve",
        "--offline",
    ];
    assert_eq!(actual.len(), expected.len());
    for (index, (got, want)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(got, want, "argv element {index}");
    }
}

#[test]
fn a_named_network_replaces_none_and_nothing_else() {
    let rig = rig(two_tools());
    let plan = rig.plan();
    let limits = PiLimits::default();
    let socket = Path::new("/run/nanna/s.sock");
    let none = PodmanPiExecutor::isolated()
        .create_args(&plan, NAME, limits, socket)
        .unwrap();
    let named = PodmanPiExecutor::new(PiNetwork::named("model-net").unwrap())
        .create_args(&plan, NAME, limits, socket)
        .unwrap();
    let differing: Vec<_> = none.iter().zip(&named).filter(|(a, b)| a != b).collect();
    assert_eq!(
        differing,
        vec![(
            &"--network=none".to_string(),
            &"--network=model-net".to_string()
        )]
    );
    assert!(none.contains(&"--memory=2048m".to_string()));
    assert!(none.contains(&"--pids-limit=512".to_string()));
    for forbidden in ["--privileged", "--network=host", "--pid=host", "--device"] {
        assert!(!none.iter().any(|a| a.starts_with(forbidden)));
    }
    assert_eq!(none.iter().filter(|a| *a == "--mount").count(), 2);
}

#[test]
fn a_socket_path_that_could_inject_mount_options_is_refused() {
    let rig = rig(two_tools());
    for bad in ["/s/a,b.sock", "/s/a=b.sock", "/s/a\nb.sock"] {
        let refused = PodmanPiExecutor::isolated()
            .create_args(&rig.plan(), NAME, PiLimits::default(), Path::new(bad))
            .unwrap_err();
        assert!(refused.contains("cannot be mounted"), "{bad}");
    }
}

#[test]
fn network_names_are_validated() {
    assert_eq!(PiNetwork::default(), PiNetwork::None);
    assert!(PiNetwork::named("model-net_1.x").is_ok());
    for bad in ["", "-x", "a b", "a:b", "a/b", "a,b"] {
        assert_eq!(
            PiNetwork::named(bad),
            Err(PiNetworkError::Invalid(bad.to_string())),
            "{bad}"
        );
    }
    for reserved in [
        "host",
        "podman",
        "PODMAN",
        "NONE",
        "bridge",
        "private",
        "slirp4netns",
        "pasta",
        "default",
    ] {
        assert_eq!(
            PiNetwork::named(reserved),
            Err(PiNetworkError::Reserved(reserved.to_string()))
        );
    }
    assert!(PiNetworkError::Invalid("x".into())
        .to_string()
        .contains("invalid"));
    assert!(PiNetworkError::Reserved("x".into())
        .to_string()
        .contains("built-in"));
}

#[test]
fn the_network_comes_from_the_environment_and_defaults_to_none() {
    let none = PodmanPiExecutor::from_lookup(&|_| None).unwrap();
    assert_eq!(
        format!("{none:?}"),
        format!("{:?}", PodmanPiExecutor::isolated())
    );
    let lookup = |key: &str| (key == PI_NETWORK_ENV).then(|| OsString::from("model-net"));
    let named = PodmanPiExecutor::from_lookup(&lookup).unwrap();
    assert!(format!("{named:?}").contains("model-net"));
    assert!(PodmanPiExecutor::from_lookup(&|_| Some(OsString::from("host"))).is_err());
}

#[test]
fn the_host_socket_follows_the_root_only_when_one_is_set() {
    let plan_socket = Path::new("/run/nanna/broker/t.sock");
    assert_eq!(
        PodmanPiExecutor::isolated().host_socket(plan_socket),
        plan_socket
    );
    assert_eq!(
        PodmanPiExecutor::isolated()
            .with_socket_root("/x/y")
            .host_socket(plan_socket),
        PathBuf::from("/x/y/t.sock")
    );
    assert_eq!(
        PodmanPiExecutor::isolated()
            .with_socket_root("/x/y")
            .host_socket(Path::new("/")),
        PathBuf::from("/")
    );
}

#[test]
fn the_other_podman_invocations_are_exact() {
    assert_eq!(
        start_args(NAME),
        ["start", "--attach", "--interactive", "nanna-pi-t-test"]
    );
    assert_eq!(copy_args(NAME), ["cp", "-", "nanna-pi-t-test:/"]);
    assert_eq!(
        remove_args(NAME),
        ["rm", "-f", "--volumes", "nanna-pi-t-test"]
    );
}

fn tar_entries(bytes: &[u8]) -> Vec<(String, u8, usize, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 512 <= bytes.len() && bytes[at] != 0 {
        let block = &bytes[at..at + 512];
        let end = block[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let name = String::from_utf8(block[..end].to_vec()).unwrap();
        let size =
            usize::from_str_radix(std::str::from_utf8(&block[124..135]).unwrap(), 8).unwrap();
        let mut copy = block.to_vec();
        let stored =
            u32::from_str_radix(std::str::from_utf8(&block[148..154]).unwrap(), 8).unwrap();
        copy[148..156].fill(b' ');
        assert_eq!(stored, copy.iter().map(|b| u32::from(*b)).sum::<u32>());
        assert_eq!(&block[257..263], b"ustar\0");
        assert_eq!(&block[108..115], b"0177776");
        assert_eq!(&block[116..123], b"0177776");
        let mode = String::from_utf8(block[100..107].to_vec()).unwrap();
        let body = String::from_utf8(bytes[at + 512..at + 512 + size].to_vec()).unwrap();
        out.push((name, block[156], size, format!("{mode}|{body}")));
        at += 512 + size.div_ceil(512) * 512;
    }
    assert!(bytes[at..].iter().all(|b| *b == 0));
    assert_eq!(bytes.len() - at, 1024);
    out
}

#[test]
fn the_plan_files_are_archived_with_their_directories() {
    let rig = rig(two_tools());
    let plan = rig.plan();
    let bytes = plan_archive(&plan).unwrap();
    assert_eq!(bytes.len() % 512, 0);
    let entries = tar_entries(&bytes);
    let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(
        names,
        [
            "nanna/",
            "nanna/pi/",
            "nanna/pi/extensions/",
            "nanna/pi/models.json",
            "nanna/pi/SYSTEM.md",
            "nanna/pi/capabilities.json",
            "nanna/pi/extensions/nanna-capabilities.ts",
        ]
    );
    for entry in &entries[..3] {
        assert_eq!((entry.1, entry.2), (b'5', 0));
        assert!(entry.3.starts_with("0000755|"));
    }
    for (entry, file) in entries[3..].iter().zip(&plan.files) {
        assert_eq!(entry.1, b'0');
        assert_eq!(entry.3, format!("0000644|{}", file.contents));
    }
}

#[test]
fn an_archive_name_over_one_hundred_bytes_is_refused() {
    assert!(tar_header(&"a".repeat(100), 0, 0o644, b'0').is_ok());
    assert!(tar_header(&"a".repeat(101), 0, 0o644, b'0')
        .unwrap_err()
        .contains("too long"));
}

#[test]
fn the_socket_directory_must_be_private_and_ours() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let fresh = root.path().join("a/b");
    ensure_private_dir(&fresh).unwrap();
    let mode = std::fs::metadata(&fresh).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    ensure_private_dir(&fresh).unwrap();
    std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ensure_private_dir(&fresh).is_err());
    std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o770)).unwrap();
    assert!(ensure_private_dir(&fresh).is_err());
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&fresh, &link).unwrap();
    assert!(ensure_private_dir(&link).is_err());
    let file = root.path().join("file");
    std::fs::write(&file, "x").unwrap();
    assert!(ensure_private_dir(&file).is_err());
}

fn tracker(max: usize) -> Tracker {
    Tracker::new(max, Arc::new(AtomicUsize::new(0)))
}

fn step_name(step: &Step) -> String {
    match step {
        Step::Continue => "continue".into(),
        Step::Handshake(names) => format!("handshake {names:?}"),
        Step::Settled => "settled".into(),
        Step::Fail(m) => format!("fail {m}"),
        Step::TurnLimit => "turn-limit".into(),
    }
}

#[test]
fn the_tracker_follows_a_clean_run() {
    let mut t = tracker(2);
    let mut feed = |line: String| step_name(&t.feed(&line));
    assert_eq!(feed(String::new()), "continue");
    assert_eq!(feed(prompt_ok()), "continue");
    assert_eq!(feed(turn_start()), "continue");
    assert_eq!(
        feed(system_start(&["read_file"])),
        "handshake [\"read_file\"]"
    );
    assert_eq!(feed(system_start(&["read_file"])), "continue");
    assert_eq!(
        feed(event(
            serde_json::json!({"type":"message_start","message":{"role":"user","toolsAdded":[]}})
        )),
        "continue"
    );
    assert_eq!(
        feed(event(
            serde_json::json!({"type":"message_start","message":{"role":"user"}})
        )),
        "continue"
    );
    assert_eq!(feed(tool_start("c1", "read_file")), "continue");
    assert_eq!(feed(tool_end("c1", "read_file", "hi")), "continue");
    assert_eq!(feed(tool_end("c9", "late", "x")), "continue");
    assert_eq!(
        feed(event(
            serde_json::json!({"type":"response","command":"abort","success":false})
        )),
        "continue"
    );
    assert_eq!(
        feed(event(serde_json::json!({"type":"something_new"}))),
        "continue"
    );
    assert_eq!(feed(agent_end("stop", "done", None)), "continue");
    assert_eq!(feed(settled(false)), "settled");
    assert_eq!(t.calls.len(), 2);
    assert_eq!(t.calls[0].tool_name, "read_file");
    assert_eq!(t.calls[0].result, "hi");
    assert_eq!(t.calls[0].arguments, serde_json::json!({"path":"a"}));
    assert_eq!(t.calls[1].tool_name, "late");
    assert_eq!(t.calls[1].arguments, serde_json::Value::Null);
    assert_eq!(t.progress.load(Ordering::SeqCst), 1);
}

#[test]
fn the_tracker_fails_closed_on_every_bad_signal() {
    let cases: Vec<(String, &str)> = vec![
        ("not json".into(), "not JSON"),
        (
            event(
                serde_json::json!({"type":"response","command":"prompt","success":false,"error":"EROFS"}),
            ),
            "rejected the prompt: EROFS",
        ),
        (
            event(serde_json::json!({"type":"response","command":"prompt"})),
            "no reason given",
        ),
        (settled(true), "aborted"),
        (
            event(serde_json::json!({"type":"agent_settled"})),
            "aborted",
        ),
        (
            tool_start("c", "x"),
            "before the runtime's tools were verified",
        ),
        (
            event(
                serde_json::json!({"type":"message_start","message":{"role":"system","toolsAdded":[{"x":1}]}}),
            ),
            "malformed tool list",
        ),
        (
            event(
                serde_json::json!({"type":"message_start","message":{"role":"system","toolsAdded":"x"}}),
            ),
            "malformed tool list",
        ),
    ];
    for (line, needle) in cases {
        let step = step_name(&tracker(2).feed(&line));
        assert!(
            step.starts_with("fail") && step.contains(needle),
            "{line}: {step}"
        );
    }
    let mut t = tracker(2);
    t.feed(&system_start(&["a"]));
    let step = step_name(&t.feed(&system_start(&["a", "b"])));
    assert!(step.contains("changed after the handshake"), "{step}");
}

#[test]
fn the_turn_limit_trips_on_the_turn_after_the_cap() {
    let mut t = tracker(2);
    assert_eq!(step_name(&t.feed(&turn_start())), "continue");
    assert_eq!(step_name(&t.feed(&turn_start())), "continue");
    assert_eq!(step_name(&t.feed(&turn_start())), "turn-limit");
}

#[test]
fn agent_end_records_the_last_assistant_message() {
    let mut t = tracker(2);
    t.feed(&event(
        serde_json::json!({"type":"agent_end","messages":[{"role":"user","content":"x"}]}),
    ));
    assert!(t
        .last_end
        .as_ref()
        .unwrap()
        .error
        .as_ref()
        .unwrap()
        .contains("without an assistant"));
    t.feed(&agent_end("error", "", Some("500: boom")));
    assert_eq!(
        t.last_end.as_ref().unwrap().error.as_deref(),
        Some("500: boom")
    );
    t.feed(&event(serde_json::json!({"type":"agent_end","messages":[{"role":"assistant","content":"plain","stopReason":"error"}]})));
    let last = t.last_end.as_ref().unwrap();
    assert_eq!(last.error.as_deref(), Some("the model call failed"));
    assert_eq!(last.text, "plain");
    t.feed(&agent_end("stop", "a", None));
    t.feed(&event(serde_json::json!({"type":"agent_end","messages":[{"role":"assistant","content":7,"stopReason":"stop"}]})));
    let last = t.last_end.as_ref().unwrap();
    assert!(last.error.is_none() && last.text.is_empty());
}

#[tokio::test]
async fn a_full_run_verifies_serves_and_maps_the_result() {
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    let root = private_root();
    write_parts(
        &fake,
        &[
            prompt_ok(),
            turn_start(),
            system_start(&["read_file", "search"]),
        ],
        &[
            tool_start("c1", "read_file"),
            tool_end("c1", "read_file", "contents"),
            agent_end("stop", "all done", None),
            settled(false),
        ],
    );
    let mut rig = rig(two_tools());
    let expected_prompt = rig.plan().stdin[0].clone();
    let socket = root.path().join("broker.sock");
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(20));
    let (outcome, reply) = tokio::join!(exec.run(launch), async {
        let reply = call_broker(&socket, r#"{"tool":"read_file","args":{}}"#).await;
        std::fs::write(fake.dir().join("go"), "").unwrap();
        reply
    });
    assert!(
        reply.contains("\"ok\":true") && reply.contains("contents"),
        "{reply}"
    );
    let result = outcome.result.unwrap();
    assert_eq!(result.result_summary, "all done");
    assert!(result.task_completed);
    assert_eq!(result.iterations, 1);
    assert_eq!(result.tool_calls_made.len(), 1);
    assert_eq!(result.tool_calls_made[0].result, "contents");
    assert_eq!(calls(&rig.counters, "read_file"), 1);
    assert_eq!(rig.progress.load(Ordering::SeqCst), 1);
    assert!(outcome.action_audit.is_empty());
    let log = fake.calls();
    assert_eq!(log[0], "--version");
    assert!(log[1].starts_with(&format!("create --name {NAME} --pull=never --read-only ")));
    assert_eq!(log[2], format!("cp - {NAME}:/"));
    assert_eq!(log[3], format!("start --attach --interactive {NAME}"));
    assert_eq!(log[4], format!("rm -f --volumes {NAME}"));
    assert_eq!(log.len(), 5);
    let prompt = std::fs::read_to_string(fake.dir().join("prompt.txt")).unwrap();
    assert_eq!(prompt.trim_end(), expected_prompt);
    let tar = std::fs::read(fake.dir().join("cp.tar")).unwrap();
    assert_eq!(tar, plan_archive(&rig.plan()).unwrap());
    assert!(!socket.exists());
}

#[tokio::test]
async fn nothing_is_served_until_the_runtime_tools_are_verified() {
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    let root = private_root();
    write_parts(
        &fake,
        &[
            prompt_ok(),
            turn_start(),
            system_start(&["read_file", "search", "bash"]),
        ],
        &[],
    );
    let mut rig = rig(two_tools());
    let socket = root.path().join("broker.sock");
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(20));
    let (outcome, reply) = tokio::join!(exec.run(launch), async {
        tokio::time::timeout(
            Duration::from_secs(2),
            call_broker(&socket, r#"{"tool":"read_file","args":{}}"#),
        )
        .await
    });
    assert!(is_state_error(&outcome, "lockdown not verified"));
    assert!(reply.is_err() || reply.unwrap().is_empty());
    assert_eq!(calls(&rig.counters, "read_file"), 0);
    assert!(removed(&fake));
}

#[tokio::test]
async fn an_escalation_halts_the_task_and_kills_the_container() {
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    let root = private_root();
    write_parts(
        &fake,
        &[prompt_ok(), turn_start(), system_start(&["t_escalate"])],
        &[],
    );
    let mut rig = rig(escalating());
    let socket = root.path().join("broker.sock");
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(20));
    let (outcome, reply) = tokio::join!(
        exec.run(launch),
        call_broker(&socket, r#"{"tool":"t_escalate","args":{}}"#)
    );
    assert!(reply.contains("\"halt\":true"), "{reply}");
    assert!(is_state_error(
        &outcome,
        "action review escalated `t_escalate`"
    ));
    assert!(removed(&fake));
}

#[tokio::test]
async fn a_second_connection_after_the_first_is_served_in_order() {
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    let root = private_root();
    write_parts(
        &fake,
        &[
            prompt_ok(),
            turn_start(),
            system_start(&["read_file", "search"]),
        ],
        &[agent_end("stop", "ok", None), settled(false)],
    );
    let mut rig = rig(two_tools());
    let socket = root.path().join("broker.sock");
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(20));
    let (outcome, (first, second)) = tokio::join!(exec.run(launch), async {
        let pair = tokio::join!(
            call_broker(&socket, r#"{"tool":"read_file","args":{}}"#),
            call_broker(&socket, r#"{"tool":"search","args":{}}"#),
        );
        std::fs::write(fake.dir().join("go"), "").unwrap();
        pair
    });
    assert!(first.contains("\"ok\":true") && second.contains("\"ok\":true"));
    assert_eq!(calls(&rig.counters, "read_file"), 1);
    assert_eq!(calls(&rig.counters, "search"), 1);
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
}

async fn failing_run(
    fake_extra_files: &[&str],
    part1: Vec<String>,
    tools: Vec<(&'static str, EffectClass, Behavior)>,
    deadline: Duration,
) -> (PiRunOutcome, FakePodman, Rig) {
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    let root = private_root();
    write_parts(&fake, &part1, &[]);
    std::fs::write(fake.dir().join("go"), "").unwrap();
    for name in fake_extra_files {
        std::fs::write(fake.dir().join(name), "").unwrap();
    }
    let mut rig = rig(tools);
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, deadline);
    let outcome = exec.run(launch).await;
    (outcome, fake, rig)
}

#[tokio::test]
async fn the_deadline_ends_the_run_and_removes_the_container() {
    let (outcome, fake, _) =
        failing_run(&["hang"], vec![], two_tools(), Duration::from_millis(400)).await;
    assert!(is_state_error(&outcome, "wall-clock limit of 0s"));
    assert!(removed(&fake));
}

#[tokio::test]
async fn cancelling_the_run_mid_flight_still_removes_the_container() {
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    std::fs::write(fake.dir().join("hang"), "").unwrap();
    let root = private_root();
    let mut rig = rig(two_tools());
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(60));
    let cancelled = tokio::time::timeout(Duration::from_millis(500), exec.run(launch)).await;
    assert!(cancelled.is_err());
    assert!(fake.calls().iter().any(|c| c.starts_with("start --attach")));
    assert!(removed(&fake));
    assert!(!root.path().join("broker.sock").exists());
}

#[tokio::test]
async fn pi_failure_signals_map_to_state_errors() {
    let handshake = system_start(&["read_file", "search"]);
    let cases: Vec<(Vec<String>, &str)> = vec![
        (
            vec![event(
                serde_json::json!({"type":"response","command":"prompt","success":false,"error":"EROFS"}),
            )],
            "rejected the prompt",
        ),
        (
            vec![
                prompt_ok(),
                turn_start(),
                handshake.clone(),
                agent_end("error", "", Some("500: boom")),
                settled(false),
            ],
            "the model run failed: 500: boom",
        ),
        (
            vec![prompt_ok(), handshake.clone(), settled(false)],
            "without finishing a turn",
        ),
        (
            vec![prompt_ok(), handshake.clone()],
            "exited before the run settled",
        ),
        (vec![prompt_ok(), "garbage".to_string()], "not JSON"),
        (
            vec![tool_start("c", "read_file")],
            "before the runtime's tools were verified",
        ),
    ];
    for (part1, needle) in cases {
        let (outcome, fake, _) =
            failing_run(&[], part1, two_tools(), Duration::from_secs(20)).await;
        assert!(
            is_state_error(&outcome, needle),
            "{needle}: {:?}",
            outcome.result.as_ref().err()
        );
        assert!(removed(&fake), "{needle}");
    }
}

#[tokio::test]
async fn too_many_turns_is_a_max_iterations_error() {
    let mut part1 = vec![prompt_ok(), system_start(&["read_file", "search"])];
    part1.extend((0..8).map(|_| turn_start()));
    let (outcome, fake, _) = failing_run(&[], part1, two_tools(), Duration::from_secs(20)).await;
    match outcome.result {
        Err(AgentError::MaxIterationsExceeded {
            iterations_completed,
            ..
        }) => {
            assert_eq!(iterations_completed, 7);
        }
        other => panic!("{other:?}"),
    }
    assert!(removed(&fake));
}

#[tokio::test]
async fn a_failing_create_is_reported_and_cleaned_up() {
    let (outcome, fake, _) = failing_run(
        &["fail-create"],
        vec![],
        two_tools(),
        Duration::from_secs(20),
    )
    .await;
    assert!(is_state_error(&outcome, "podman create failed"));
    assert!(removed(&fake));
    assert!(!fake.calls().iter().any(|c| c.starts_with("cp ")));
}

#[tokio::test]
async fn an_unspawnable_start_is_reported() {
    let fake = FakePodman::install_scripted_async("case \"$1\" in start) exit 3;; esac").await;
    let root = private_root();
    let mut rig = rig(two_tools());
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(20));
    let outcome = exec.run(launch).await;
    assert!(
        is_state_error(&outcome, "exited before the run settled")
            || is_state_error(&outcome, "writing the prompt failed")
    );
    assert!(removed(&fake));
}

#[tokio::test]
async fn launches_that_do_not_match_the_task_are_refused_before_podman_runs() {
    let fake = FakePodman::install_scripted_async("").await;
    let root = private_root();
    let exec = executor(root.path());
    let mut rig = rig(two_tools());
    let launch = rig.launch(TASK, "nanna-pi-other", Duration::from_secs(5));
    assert!(is_state_error(
        &exec.run(launch).await,
        "is not the task's name"
    ));
    let mut rig = rig2();
    let launch = rig.launch("t-other", "nanna-pi-t-other", Duration::from_secs(5));
    assert!(is_state_error(
        &exec.run(launch).await,
        "bound to another task"
    ));
    assert!(fake.calls().is_empty());
}

fn rig2() -> Rig {
    rig(two_tools())
}

#[tokio::test]
async fn an_existing_socket_path_is_never_replaced() {
    let fake = FakePodman::install_scripted_async("").await;
    let root = private_root();
    let socket = root.path().join("broker.sock");
    std::fs::write(&socket, "mine").unwrap();
    let mut rig = rig(two_tools());
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(5));
    assert!(is_state_error(&exec.run(launch).await, "broker socket"));
    assert_eq!(std::fs::read_to_string(&socket).unwrap(), "mine");
    assert!(!fake.calls().iter().any(|c| c.starts_with("create")));
}

#[tokio::test]
async fn a_missing_container_runtime_refuses_the_launch() {
    let _fake = FakePodman::install_without_runtime_async().await;
    let root = private_root();
    let mut rig = rig(two_tools());
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(5));
    let outcome = exec.run(launch).await;
    assert!(is_state_error(&outcome, "no container runtime"));
}

#[tokio::test]
async fn a_socket_dir_that_cannot_be_made_private_refuses_the_launch() {
    use std::os::unix::fs::PermissionsExt;
    let _fake = FakePodman::install_scripted_async("").await;
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut rig = rig(two_tools());
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(5));
    assert!(is_state_error(&exec.run(launch).await, "private directory"));
}

#[test]
fn the_unwired_executor_stays_available_and_distinct() {
    assert_eq!(format!("{UnwiredPiExecutor:?}"), "UnwiredPiExecutor");
    assert!(format!("{:?}", PodmanPiExecutor::isolated()).contains("PodmanPiExecutor"));
}

#[test]
fn clipping_keeps_short_text_and_cuts_long_text_on_a_char_boundary() {
    assert_eq!(clip_text("short".to_string(), 10), "short");
    assert_eq!(clip_text("exactly".to_string(), 7), "exactly");
    let long = "é".repeat(10);
    let clipped = clip_text(long, 5);
    assert_eq!(clipped, "éé\n[clipped]");
}

#[test]
fn a_huge_tool_result_is_clipped_in_the_recorded_call() {
    let mut tracker = tracker(5);
    let _ = tracker.feed(&system_start(&["read_file"]));
    let _ = tracker.feed(&tool_start("c1", "read_file"));
    let huge = "x".repeat(MAX_CALL_RESULT_BYTES * 4);
    let _ = tracker.feed(&tool_end("c1", "read_file", &huge));
    assert_eq!(tracker.calls.len(), 1);
    assert!(tracker.calls[0].result.len() <= MAX_CALL_RESULT_BYTES + 16);
    assert!(tracker.calls[0].result.ends_with("[clipped]"));
}

#[tokio::test]
async fn a_huge_final_message_is_clipped_in_the_result_summary() {
    let huge = "y".repeat(MAX_SUMMARY_BYTES * 3);
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    let root = private_root();
    write_parts(
        &fake,
        &[
            prompt_ok(),
            system_start(&["read_file", "search"]),
            turn_start(),
            agent_end("stop", &huge, None),
            settled(false),
        ],
        &[],
    );
    std::fs::write(fake.dir().join("go"), "").unwrap();
    let mut rig = rig(two_tools());
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(20));
    let outcome = exec.run(launch).await;
    let result = outcome.result.unwrap();
    assert!(result.result_summary.len() <= MAX_SUMMARY_BYTES + 16);
    assert!(result.result_summary.ends_with("[clipped]"));
    assert!(removed(&fake));
}

#[tokio::test]
async fn a_create_or_copy_that_hangs_is_cut_off_at_the_deadline_and_cleaned_up() {
    for marker in ["hang-create", "hang-cp"] {
        let (outcome, fake, _) =
            failing_run(&[marker], vec![], two_tools(), Duration::from_millis(500)).await;
        assert!(
            is_state_error(&outcome, "exceeded the wall-clock limit"),
            "{marker}"
        );
        assert!(removed(&fake), "{marker}");
        let pid_file = marker.replace("hang-", "") + ".pid";
        assert!(
            process_gone(&fake.dir().join(&pid_file)).await,
            "the timed-out {marker} client outlived the run"
        );
    }
}

async fn process_gone(pid_file: &Path) -> bool {
    let pid = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .to_string();
    let stat = PathBuf::from(format!("/proc/{pid}/stat"));
    for _ in 0..100 {
        let state = std::fs::read_to_string(&stat).unwrap_or_default();
        if state.is_empty() || state.contains(") Z ") {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn cancelling_the_run_kills_the_attached_client_process() {
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    std::fs::write(fake.dir().join("hang"), "").unwrap();
    let root = private_root();
    let mut rig = rig(two_tools());
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(60));
    let cancelled = tokio::time::timeout(Duration::from_millis(700), exec.run(launch)).await;
    assert!(cancelled.is_err());
    let gone = process_gone(&fake.dir().join("start.pid")).await;
    assert!(gone, "the attached podman client outlived the run");
}

#[tokio::test]
async fn an_unreadable_or_oversized_request_gets_a_bad_request_reply_and_the_run_continues() {
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    let root = private_root();
    write_parts(
        &fake,
        &[
            prompt_ok(),
            turn_start(),
            system_start(&["read_file", "search"]),
        ],
        &[agent_end("stop", "ok", None), settled(false)],
    );
    let mut rig = rig(two_tools());
    let socket = root.path().join("broker.sock");
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(20));
    let mut oversized = vec![b'a'; MAX_REQUEST_BYTES + 10];
    oversized.push(b'\n');
    let (outcome, (invalid, large, good)) = tokio::join!(exec.run(launch), async {
        let replies = tokio::join!(
            call_broker_bytes(&socket, b"\xff\xfe\n"),
            call_broker_bytes(&socket, &oversized),
            call_broker(&socket, r#"{"tool":"read_file","args":{}}"#),
        );
        std::fs::write(fake.dir().join("go"), "").unwrap();
        replies
    });
    assert!(
        invalid.contains("bad request: not valid UTF-8"),
        "{invalid}"
    );
    assert!(large.contains("request too large"), "{large}");
    assert!(good.contains("\"ok\":true"), "{good}");
    assert_eq!(calls(&rig.counters, "read_file"), 1);
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
}

#[test]
fn only_a_directory_we_own_with_no_group_or_other_bits_is_private() {
    use super::executor::is_private;
    assert!(is_private(1000, 1000, 0o040700, true));
    assert!(is_private(1000, 1000, 0o040500, true));
    assert!(!is_private(1000, 1000, 0o040770, true));
    assert!(!is_private(1000, 1000, 0o040707, true));
    assert!(!is_private(1000, 1000, 0o040750, true));
    assert!(!is_private(1001, 1000, 0o040700, true));
    assert!(!is_private(1000, 1000, 0o100600, false));
    assert!(!is_private(1000, 1000, 0o120777, false));
}

#[test]
fn a_podman_exit_is_judged_by_deadline_error_and_status() {
    use super::executor::judge;
    use std::os::unix::process::ExitStatusExt;
    let ok = std::process::ExitStatus::from_raw(0);
    let bad = std::process::ExitStatus::from_raw(1 << 8);
    assert_eq!(judge("cp", Some(Ok(ok))), Ok(()));
    let late = judge("cp", None).unwrap_err();
    assert_eq!(late, "podman cp exceeded the wall-clock limit");
    let io = judge("cp", Some(Err(std::io::Error::other("pipe")))).unwrap_err();
    assert_eq!(io, "podman cp: pipe");
    let failed = judge("create", Some(Ok(bad))).unwrap_err();
    assert!(failed.starts_with("podman create failed: "), "{failed}");
}

#[tokio::test(start_paused = true)]
async fn the_prompt_write_gives_up_at_the_deadline_when_the_runtime_never_reads() {
    use super::executor::send_lines;
    let (mut writer, _reader) = tokio::io::duplex(8);
    let lines = vec!["x".repeat(4096)];
    let started = tokio::time::Instant::now();
    let deadline = started + Duration::from_secs(3);
    let sent = tokio::time::timeout(
        Duration::from_secs(600),
        send_lines(&mut writer, &lines, deadline),
    )
    .await;
    assert_eq!(sent, Ok(false));
    assert_eq!(started.elapsed(), Duration::from_secs(3));
}

#[tokio::test]
async fn every_prompt_line_is_written_with_its_line_feed() {
    use super::executor::send_lines;
    use tokio::io::AsyncReadExt;
    let (mut writer, mut reader) = tokio::io::duplex(1024);
    let lines = vec!["a".to_string(), "b".to_string()];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    assert!(send_lines(&mut writer, &lines, deadline).await);
    drop(writer);
    let mut text = String::new();
    reader.read_to_string(&mut text).await.unwrap();
    assert_eq!(text, "a\nb\n");
}

#[tokio::test]
async fn the_broker_socket_is_connectable_by_the_mapped_container_user() {
    use std::os::unix::fs::PermissionsExt;
    let root = private_root();
    let socket = root.path().join("s.sock");
    let (_listener, guard) = super::executor::bind_socket(&socket).unwrap();
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o666);
    drop(guard);
    assert!(!socket.exists());
}

#[test]
fn the_size_and_time_limits_are_pinned_to_their_values() {
    assert_eq!(MAX_CALL_RESULT_BYTES, 16 * 1024);
    assert_eq!(MAX_SUMMARY_BYTES, 64 * 1024);
    assert_eq!(REQUEST_READ_TIMEOUT, Duration::from_secs(10));
    assert_eq!(
        ReadLimits::for_requests().read_timeout,
        Duration::from_secs(10)
    );
    assert_eq!(MAX_REPLY_BYTES, 1_048_576);
}

#[tokio::test]
async fn docker_is_never_used_to_run_pi() {
    let fake = FakePodman::install_docker_only_async().await;
    let root = private_root();
    let mut rig = rig(two_tools());
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(5));
    let outcome = exec.run(launch).await;
    assert!(is_state_error(&outcome, "only under podman"));
    assert!(!fake.calls().iter().any(|c| c.starts_with("create")));
    assert!(!removed(&fake));
}

fn armed_broker(rig: &mut Rig) -> super::broker::ArmedBroker {
    let plan = rig.plan();
    let bound = BoundWorkspace::for_test(
        rig.registry.take().unwrap(),
        &rig.identity,
        &rig.repo,
        &rig.agent,
    );
    let broker = Broker::for_agent(
        &rig.agent,
        bound,
        BrokerPolicy::default(),
        Arc::new(RingAuditSink::default()),
    )
    .unwrap();
    broker
        .arm(&plan, &plan.exposed_capabilities.clone())
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_reply_the_client_never_reads_times_out_after_the_request_read_timeout() {
    use super::rpc::serve_connection;
    use tokio::io::AsyncWriteExt;
    let mut rig = rig(two_tools());
    let armed = armed_broker(&mut rig);
    let (mut client, server) = tokio::io::duplex(8);
    let writer = tokio::spawn(async move {
        client
            .write_all(b"{\"tool\":\"read_file\",\"args\":{}}\n")
            .await
            .unwrap();
        client
    });
    let limits = ReadLimits {
        read_timeout: Duration::from_secs(7),
        ..ReadLimits::for_requests()
    };
    let started = tokio::time::Instant::now();
    let served = serve_connection(server, &armed, limits).await;
    assert!(matches!(served, Err(RpcError::Timeout)), "{served:?}");
    assert_eq!(started.elapsed(), Duration::from_secs(7));
    drop(writer);
}

#[tokio::test]
async fn a_call_runs_the_action_gate_and_its_audit_survives_the_run() {
    use super::testing::{gated_registry_with_stub, layers_for, real_workspace, unique_task};
    use crate::action_auditor::ActionVerdict;
    use crate::auditor::{Reason, ReasonCode};
    let unique = unique_task("t-exec-gate");
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    let root = private_root();
    write_parts(
        &fake,
        &[prompt_ok(), turn_start(), system_start(&["run_command"])],
        &[agent_end("stop", "done", None), settled(false)],
    );
    let (mut workspace, _repo) = real_workspace(unique.as_str());
    let mut rig = rig(vec![(
        "run_command",
        EffectClass::Workspace,
        ok_with(serde_json::json!("ran")),
    )]);
    let layers = layers_for(
        &rig.identity,
        TASK,
        "example/repo",
        ActionVerdict::block(vec![Reason::new(ReasonCode::Other, "no")]),
    )
    .layers;
    let (registry, ran) =
        gated_registry_with_stub(&workspace, &rig.identity, layers, "run_command");
    rig.registry = Some(registry);
    let socket = root.path().join("broker.sock");
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(20));
    let (outcome, reply) = tokio::join!(exec.run(launch), async {
        let reply = call_broker(
            &socket,
            r#"{"tool":"run_command","args":{"command":"true"}}"#,
        )
        .await;
        std::fs::write(fake.dir().join("go"), "").unwrap();
        reply
    });
    assert!(reply.contains("\"ok\":false"), "{reply}");
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(outcome.action_audit.len(), 1);
    let result = outcome.result.unwrap();
    assert_eq!(result.action_audit.len(), 1);
    workspace.cleanup().unwrap();
}

#[tokio::test]
async fn a_halt_keeps_the_tool_calls_made_before_it() {
    let fake = FakePodman::install_scripted_async(SCRIPT).await;
    let root = private_root();
    write_parts(
        &fake,
        &[
            prompt_ok(),
            turn_start(),
            system_start(&["read_file", "search", "t_escalate"]),
            tool_start("c1", "read_file"),
            tool_end("c1", "read_file", "contents"),
        ],
        &[],
    );
    let mut tools = two_tools();
    tools.extend(escalating());
    let mut rig = rig(tools);
    let socket = root.path().join("broker.sock");
    let exec = executor(root.path());
    let launch = rig.launch(TASK, NAME, Duration::from_secs(20));
    let (outcome, reply) = tokio::join!(exec.run(launch), async {
        tokio::time::sleep(Duration::from_millis(600)).await;
        call_broker(&socket, r#"{"tool":"t_escalate","args":{}}"#).await
    });
    assert!(reply.contains("\"halt\":true"), "{reply}");
    match outcome.result {
        Err(AgentError::StateError {
            tool_calls_made, ..
        }) => assert_eq!(tool_calls_made.len(), 1),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_executor_guard_gives_a_hung_removal_the_removal_limit_and_then_kills_it() {
    let started = std::time::Instant::now();
    let (outcome, fake, _) = failing_run(
        &["fail-create", "hang-rm"],
        vec![],
        two_tools(),
        Duration::from_secs(20),
    )
    .await;
    let elapsed = started.elapsed();
    assert!(is_state_error(&outcome, "podman create failed"));
    assert!(removed(&fake));
    assert!(elapsed >= Duration::from_secs(9), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(40), "{elapsed:?}");
}

#[tokio::test(start_paused = true)]
async fn an_error_reply_the_client_never_reads_times_out_after_the_request_read_timeout() {
    use super::rpc::serve_connection;
    use tokio::io::AsyncWriteExt;
    let mut rig = rig(two_tools());
    let armed = armed_broker(&mut rig);
    let (mut client, server) = tokio::io::duplex(8);
    let writer = tokio::spawn(async move {
        client.write_all(b"\xff\xfe\n").await.unwrap();
        client
    });
    let limits = ReadLimits {
        read_timeout: Duration::from_secs(7),
        ..ReadLimits::for_requests()
    };
    let started = tokio::time::Instant::now();
    let served = serve_connection(server, &armed, limits).await;
    assert!(matches!(served, Err(RpcError::NotUtf8)), "{served:?}");
    assert_eq!(started.elapsed(), Duration::from_secs(7));
    drop(writer);
}
