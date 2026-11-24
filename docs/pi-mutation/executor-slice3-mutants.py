import re
import subprocess
import sys

DIR = "harness/src/harness_adapter/"
E, R, RT = DIR + "executor.rs", DIR + "rpc.rs", DIR + "route.rs"
MN = "harness/src/main.rs"
B = ["--bin", "nanna"]
T = ["--lib", "harness_adapter"]

EXIT_OK_MUTANT = (
    "Ended::Halted(reason) => failure(reason.message(), calls),",
    "Ended::Halted(_) => PiRunOutcome { result: Ok(AgentRunResult { final_state: AgentState::Completed, iterations: turns, task_completed: true, result_summary: String::new(), tool_calls_made: calls, conversation_snapshot: vec![], token_usage: None, denials: vec![], action_audit: action_audit.clone() }), action_audit },",
)

MUTANTS = [
    (E, '"--pull=never".to_string(),', '"--pull=missing".to_string(),', T),
    (E, '            "--read-only".to_string(),\n', "", T),
    (E, 'const CONTAINER_USER: &str = "65534:65534";', 'const CONTAINER_USER: &str = "0:0";', T),
    (E, '"--cap-drop=ALL".to_string(),', '"--cap-add=ALL".to_string(),', T),
    (E, '            "--security-opt=no-new-privileges".to_string(),\n', "", T),
    (E, 'PiNetwork::None => "--network=none".to_string(),', 'PiNetwork::None => "--network=host".to_string(),', T),
    (E, "Self::new(PiNetwork::None)", 'Self::new(PiNetwork::Named("bridge".to_string()))', T),
    (E, '    "host",\n    "none",', '    "hostx",\n    "none",', T),
    (E, "if RESERVED_NETWORKS.contains(&name.to_ascii_lowercase().as_str()) {", "if false {", T),
    (E, "RESERVED_NETWORKS.contains(&name.to_ascii_lowercase().as_str())", "RESERVED_NETWORKS.contains(&name.as_str())", T),
    (E, "&& !name.starts_with('-')", "&& true", T),
    (E, "if socket.contains([',', '=', '\\n']) {", "if false {", T),
    (E, '            format!("--memory={}m", limits.memory_mb),', '            format!("--memory={}m", limits.memory_mb + 1),', T),
    (E, '            format!("--pids-limit={}", limits.pids),', '            format!("--pids-limit={}", limits.pids + 1),', T),
    (E, "        super::route::off_runtime(|| {\n            super::route::bounded_status(\n                self.command,\n                &remove_args(&self.name),\n                super::route::REMOVE_LIMIT,\n            );\n        });", "        let _ = &self.name;", T),
    (E, "        drop(guard);\n        drop(socket_guard);", "        std::mem::forget(guard);\n        drop(socket_guard);", T),
    (E, '["rm", "-f", "--volumes", name]', '["rm", "--volumes", name]', T),
    (E, "_ = tokio::time::sleep_until(deadline) => {", "_ = std::future::pending::<()>() => {", T),
    (E, "let deadline = Instant::now() + launch.deadline();", "let deadline = Instant::now() + launch.deadline() * 1000;", T),
    (E, "judge(&what, tokio::time::timeout_at(deadline, work).await.ok())", "judge(&what, Some(work.await))", T),
    (E, "        if self.tools.is_none() {\n            return Step::Fail(\"a tool ran before", "        if false {\n            return Step::Fail(\"a tool ran before", T),
    (E, "Some(first) if *first == set => Step::Continue,", "Some(_) => Step::Continue,", T),
    (E, "Err(e) => break Ended::Failed(format!(\"lockdown not verified: {e}\")),", "Err(_) => {}", T),
    (E, "match broker.arm(&self.plan, &names) {", "match broker.arm(&self.plan, &[]) {", T),
    (E, "ServeEnd::Halted(reason) => break Ended::Halted(reason),", "ServeEnd::Halted(_) => break Ended::Settled,", T),
    (E,) + EXIT_OK_MUTANT + (T,),
    (E, "if self.turns > self.max_turns {", "if self.turns >= self.max_turns {", T),
    (E, "self.progress.fetch_add(1, Ordering::SeqCst);", "self.progress.fetch_add(0, Ordering::SeqCst);", T),
    (E, 'last.get("stopReason").and_then(Value::as_str) == Some("error");', 'last.get("stopReason").and_then(Value::as_str) == Some("errorx");', T),
    (E, 'if prompt && event.get("success").and_then(Value::as_bool) != Some(true) {', "if false {", T),
    (E, 'if event.get("aborted").and_then(Value::as_bool) == Some(false) {', "if true {", T),
    (E, "let action_audit = armed.map(ArmedBroker::action_audit).unwrap_or_default();", "let action_audit: Vec<_> = Vec::new();", T),
    (E, ".mode(0o700)", ".mode(0o755)", T),
    (E, "if name != super::pi_container_name(launch.task_id()) {", "if false {", T),
    (E, "if broker.task_id().0 != launch_task_id {", "if false {", T),
    (E, "block[124..136].copy_from_slice(&octal(size, 12));", "block[124..136].copy_from_slice(&octal(size + 1, 12));", T),
    (E, '["cp", "-", &format!("{name}:/")]', '["cp", "-", &format!("{name}:/tmp")]', T),
    (E, '["start", "--attach", "--interactive", name]', '["start", "--interactive", name]', T),
    (E, "            .stderr(Stdio::null())\n            .kill_on_drop(true)\n            .spawn()", "            .stderr(Stdio::null())\n            .kill_on_drop(false)\n            .spawn()", T),
    (E, "pub(super) const MAX_SUMMARY_BYTES: usize = 64 * 1024;", "pub(super) const MAX_SUMMARY_BYTES: usize = 64 * 1024 * 1024;", T),
    (E, "result_summary: clip_text(text, MAX_SUMMARY_BYTES),", "result_summary: text,", T),
    (E, "while !text.is_char_boundary(end) {", "while false {", T),
    (R, "if self.pending.len() > self.limits.max_line {", "if false {", T),
    (R, "if end > self.limits.max_line {", "if end > self.limits.max_line + 1 {", T),
    (R, "if self.total + end + 1 > self.limits.max_total {", "if self.total + end > self.limits.max_total {", T),
    (R, ".position(|b| *b == b'\\n')", ".position(|b| *b == b'\\n' || *b == b'\\r')", T),
    (R, "if self.failed {", "if false {", T),
    (R, "            self.failed = true;\n", "", T),
    (R, "return if self.pending.is_empty() {", "return if true {", T),
    (R, "String::from_utf8(line).map_err(|_| RpcError::NotUtf8)", "Ok(String::from_utf8_lossy(&line).into_owned())", T),
    (R, "if reply.contains('\\n') {", "if false {", T),
    (R, '        .replace(\'\\u{2028}\', "\\\\u2028")\n', "", T),
    (R, '.replace(\'\\u{2029}\', "\\\\u2029");', ";", T),
    (R, "if text.len() > max {", "if text.len() > max + 1 {", T),
    (R, "if text.len() > max {", "if false {", T),
    (R, "pub const MAX_REPLY_BYTES: usize = 1_048_576;", "pub const MAX_REPLY_BYTES: usize = 1_048_576 * 64;", T),
    (R, "return ServeEnd::Halted(reason);", "return ServeEnd::Failed(String::new());", T),
    (R, "(_, Some(reason)) => Ok(Some(reason)),", "(_, Some(_)) => Ok(None),", T),
    (R, 'RpcError::NotUtf8 => Some("bad request: not valid UTF-8"),', "RpcError::NotUtf8 => None,", T),
    (R, 'pub const RELAY_HOST: &str = "host.containers.internal";', 'pub const RELAY_HOST: &str = "host.internal";', T),
    (RT, "Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {", "Ok(_) if false => {", T),
    (RT, "Ok(None) if started.elapsed() < limit => {", "Ok(None) => {", T),
    (RT, "                let _ = child.kill();\n", "", T),
    (RT, "                let args = [\"rm\", \"-f\", self.name.as_str()].map(String::from);\n                bounded_status(runtime.command(), &args, REMOVE_LIMIT);", "let _ = runtime;", T),
    (E, '    "default",\n    "podman",\n];', '    "default",\n    "podmanx",\n];', T),
    (E, "is_dir && owner == effective && mode & 0o077 == 0", "owner == effective && mode & 0o077 == 0", T),
    (E, "is_dir && owner == effective && mode & 0o077 == 0", "is_dir && mode & 0o077 == 0", T),
    (E, "is_dir && owner == effective && mode & 0o077 == 0", "is_dir && owner == effective", T),
    (E, "is_dir && owner == effective && mode & 0o077 == 0", "is_dir && owner == effective && mode & 0o007 == 0", T),
    (E, "            ContainerRuntime::Podman => ContainerRuntime::Podman,", "            ContainerRuntime::Podman | ContainerRuntime::Docker => ContainerRuntime::Podman,", T),
    (E, "std::fs::Permissions::from_mode(0o666)", "std::fs::Permissions::from_mode(0o600)", T),
    (E, "std::fs::Permissions::from_mode(0o666)", "std::fs::Permissions::from_mode(0o777)", T),
    (E, "        .stderr(Stdio::null())\n        .kill_on_drop(true);", "        .stderr(Stdio::null())\n        .kill_on_drop(false);", T),
    (E, 'Some(Err(e)) => Err(format!("podman {what}: {e}")),', "Some(Err(_)) => Ok(()),", T),
    (E, "Some(Ok(status)) if status.success() => Ok(()),", "Some(Ok(_)) => Ok(()),", T),
    (E, 'None => Err(format!("podman {what} exceeded the wall-clock limit")),', "None => Ok(()),", T),
    (E, "if !matches!(tokio::time::timeout_at(deadline, sent).await, Ok(Ok(()))) {", "if !matches!(Ok::<_, tokio::time::error::Elapsed>(sent.await), Ok(Ok(()))) {", T),
    (E, '            writer.write_all(b"\\n").await?;', "", T),
    (E, "pub(super) const MAX_CALL_RESULT_BYTES: usize = 16 * 1024;", "pub(super) const MAX_CALL_RESULT_BYTES: usize = 17 * 1024;", T),
    (E, "pub(super) const MAX_SUMMARY_BYTES: usize = 64 * 1024;", "pub(super) const MAX_SUMMARY_BYTES: usize = 65 * 1024;", T),
    (E, "Ended::Halted(reason) => failure(reason.message(), calls),", "Ended::Halted(reason) => failure(reason.message(), vec![]),", T),
    (E, "                super::route::REMOVE_LIMIT,", "                std::time::Duration::from_millis(1),", T),
    (E, "                super::route::REMOVE_LIMIT,", "                std::time::Duration::from_secs(100000),", T),
    (R, "tokio::time::timeout(self.limits.read_timeout, self.inner.read(&mut chunk))", "tokio::time::timeout(self.limits.read_timeout * 2, self.inner.read(&mut chunk))", T),
    (R, "    tokio::time::timeout(timeout, async {", "    tokio::time::timeout(timeout * 2, async {", T),
    (R, "let written = reply_line(&mut write_half, &reply, limits.read_timeout).await;", "let written = reply_line(&mut write_half, &reply, limits.read_timeout * 100).await;", T),
    (R, "let _ = reply_line(&mut write_half, &text, limits.read_timeout).await;", "let _ = reply_line(&mut write_half, &text, limits.read_timeout * 100).await;", T),
    (R, "pub const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);", "pub const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(11);", T),
    (RT, "pub(super) const REMOVE_LIMIT: Duration = Duration::from_secs(10);", "pub(super) const REMOVE_LIMIT: Duration = Duration::from_secs(1);", T),
    (RT, "bounded_status(runtime.command(), &args, REMOVE_LIMIT);", "bounded_status(runtime.command(), &args, Duration::from_secs(100000));", T),
    (E, "!name.is_empty() && !name.starts_with('-') && name.chars().all(is_name_char)", "!name.starts_with('-') && name.chars().all(is_name_char)", T),
    (E, "!name.is_empty() && !name.starts_with('-') && name.chars().all(is_name_char)", "!name.is_empty() && name.chars().all(is_name_char)", T),
    (E, "!name.is_empty() && !name.starts_with('-') && name.chars().all(is_name_char)", "!name.is_empty() && !name.starts_with('-')", T),
    (E, 'Self(format!("launch plan refused: {error}"))', 'Self(format!("refused: {error}"))', T),
    (E, 'Self(format!("broker refused: {error}"))', 'Self(format!("refused: {error}"))', T),
    (E, 'Self(format!("runtime detection failed: {error}"))', 'Self(format!("failed: {error}"))', T),
    (E, '.expect("podman start has a piped stdin")', '.expect("")', T),
    (R, 'RpcError::NotUtf8 => Some("bad request: not valid UTF-8"),', "RpcError::NotUtf8 => None,", T),
    (R, "RpcError::LineTooLong { .. } | RpcError::TotalTooLong { .. } => Some(\"request too large\"),", "RpcError::LineTooLong { .. } => Some(\"request too large\"),", T),
    (R, "        halt: false,\n    };\n    serde_json::to_string(&response)", "        halt: true,\n    };\n    serde_json::to_string(&response)", T),
    (R, "        kind: Some(super::ToolErrorKind::BadRequest),\n        halt: false,\n    };\n    serde_json::to_string(&response)", "        kind: Some(super::ToolErrorKind::ExecutionFailed),\n        halt: false,\n    };\n    serde_json::to_string(&response)", T),
    (R, 'ServeEnd::Failed(format!("broker socket accept failed: {error}"))', 'ServeEnd::Failed(format!("accept failed: {error}"))', T),
    (E, "match tokio::task::spawn_blocking(detect_runtime).await? {", "match Ok::<_, tokio::task::JoinError>(detect_runtime())? {", T),
    (MN, "pi_route_with(enabled, lookup, &warn_notice)", "pi_route_with(enabled, lookup, &|_| {})", B),
    (MN, 'fn warn_notice(message: &str) {\n    warn!("{message}");\n}', "fn warn_notice(message: &str) {\n    let _ = message;\n}", B),
    (MN, 'warn!("{message}");', 'info!("{message}");', B),
    (MN, "    notify(PI_ROUTE_INERT_NOTICE);\n", "", B),
]


def run(args):
    try:
        result = subprocess.run(["cargo", "test", "-p", "harness"] + args, capture_output=True, text=True, timeout=900)
    except subprocess.TimeoutExpired:
        return None, ""
    out = re.sub(r"\x1b\[[0-9;]*m", "", result.stdout + result.stderr)
    return result.returncode, out


def verdict(code, out):
    if code is None:
        return "KILLED (hang)"
    if "error[" in out or "could not compile" in out:
        return "COMPILE ERROR"
    return "KILLED" if code != 0 else "SURVIVED"


def main():
    only = [int(a) for a in sys.argv[1:]]
    lines = []
    for number, (path, old, new, args) in enumerate(MUTANTS, 1):
        if only and number not in only:
            continue
        original = open(path).read()
        if original.count(old) != 1:
            lines.append(f"{number} {path.split('/')[-1]} {old[:60]!r}: NOT APPLIED")
            print(lines[-1], flush=True)
            continue
        try:
            open(path, "w").write(original.replace(old, new, 1))
            code, out = run(args)
        finally:
            open(path, "w").write(original)
            subprocess.run(["touch", path])
        lines.append(f"{number} {path.split('/')[-1]} {old[:60]!r} -> {new[:40]!r}: {verdict(code, out)}")
        print(lines[-1], flush=True)
    results = "docs/pi-mutation/executor-slice3-results.txt"
    if not only:
        open(results, "w").write("\n".join(lines) + "\n")
        return
    merged = {}
    try:
        for line in open(results).read().splitlines():
            merged[int(line.split(" ", 1)[0])] = line
    except FileNotFoundError:
        pass
    for line in lines:
        merged[int(line.split(" ", 1)[0])] = line
    open(results, "w").write("\n".join(merged[key] for key in sorted(merged)) + "\n")


main()
