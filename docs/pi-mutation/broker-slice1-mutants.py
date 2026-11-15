import re
import subprocess

DIR = "harness/src/harness_adapter/"
B, K, M, P = DIR + "binding.rs", DIR + "broker.rs", DIR + "mod.rs", DIR + "pi.rs"
G = "harness/tests/broker_bind_structural.rs"
T = ["--lib", "harness_adapter"]
S = ["--test", "broker_bind_structural"]

MUTANTS = [
    (B, "agent.name() == identity.name()", "true", T),
    (B, "scope.repos == identity.scope.repos", "true", T),
    (B, "scope.paths == identity.scope.paths", "true", T),
    (B, "scope.read_paths == identity.scope.read_paths", "true", T),
    (B, "scope.tools == tool_patterns", "true", T),
    (B, "agent.max_effect() == identity.scope.max_effect", "true", T),
    (B, "agent.limits() == identity.limits", "true", T),
    (B, "agent.model() == identity.identity.model", "true", T),
    (B, "agent.audited_repo() == audited_repo", "true", T),
    (B, "&& prompt_matches", "&& true", T),
    (B, "&& capabilities_allowed", "&& true", T),
    (B, ".all(|c| identity.allows_tool", ".any(|c| identity.allows_tool", T),
    (B, "identity.allows_tool(&c.name) && identity.allows_effect(c.effect)", "identity.allows_tool(&c.name)", T),
    (B, "identity.allows_tool(&c.name) && identity.allows_effect(c.effect)", "identity.allows_effect(c.effect)", T),
    (B, ".is_ok_and(|text| text == agent.system_prompt())", ".map_or(true, |text| text == agent.system_prompt())", T),
    (B, "!granted.contains(&name.as_str())", "false", T),
    (B, "registry.effect_class_of(&capability.name) == Some(capability.effect)", "true", T),
    (B, "if !same {", "if same {", T),
    (B, "offered.sort();", "", T),
    (B, "let unaudited = matches!(bound, BoundIdentity::Unaudited(_));", "let unaudited = false;", T),
    (B, "if !agent_matches_identity(self, identity, audited_repo) {", "if false {", T),
    (B, "if !names_repo(audited_repo, source) {", "if false {", T),
    (B, "check_repo(identity, source)?;", "let _ = check_repo(identity, source);", T),
    (B, "layers_match(identity, workspace, &layers)?;", "let _ = layers_match(identity, workspace, &layers);", T),
    (B, "registry_matches(self, &registry)?;", "let _ = registry_matches(self, &registry);", T),
    (B, "model_facing_registry(workspace, Some(identity), layers)?", "model_facing_registry(workspace, None, layers)?", T),
    (B, 'subject.task_id.0 == workspace.task_id', "true", T),
    (B, "subject.max_effect == identity.scope.max_effect", "true", T),
    (B, "subject.paths == identity.scope.paths", "true", T),
    (B, "subject.repo == subject_repo(&workspace.source_repo)", "true", T),
    (B, ".map(|c| (c.name.clone(), c.effect))", ".map(|c| (c.name.clone(), EffectClass::None))", T),
    (B, "self.capabilities == capability_snapshot(agent)", "true", T),
    (K, "effect <= self.max_effect", "effect < self.max_effect", T),
    (K, "Self::new(EffectClass::Workspace)", "Self::new(EffectClass::Repository)", T),
    (K, "capacity: capacity.max(1)", "capacity: capacity", T),
    (K, "state.0.len() >= self.capacity", "state.0.len() > self.capacity", T),
    (K, "state.1 += 1;", "state.1 += 0;", T),
    (K, "state.0.pop_front();", "", T),
    (K, "if !bound.agent_matches(agent) {", "if false {", T),
    (K, "if !bound.capabilities_match(agent) {", "if false {", T),
    (K, "if policy.admits(capability.effect) {", "if true {", T),
    (K, '("agent", plan.agent == core.agent)', '("agent", true)', T),
    (K, '("audited repository", plan.audited_repo == core.audited_repo)', '("audited repository", true)', T),
    (K, '("model", plan.model == core.model)', '("model", true)', T),
    (K, '("image", plan.image == core.image)', '("image", true)', T),
    (K, '("limits", plan.limits == core.limits)', '("limits", true)', T),
    (K, '("scope", plan.scope == core.scope)', '("scope", true)', T),
    (K, '("broker socket", plan.broker_socket == core.broker_socket)', '("broker socket", true)', T),
    (K, "plan.exposed_capabilities == core.capabilities,", "true,", T),
    (K, ".filter(|name| !core.served.contains(name.as_str()))", ".filter(|name| core.served.contains(name.as_str()))", T),
    (K, "verify_runtime_tools(plan, reported)?;", "let _ = verify_runtime_tools(plan, reported);", T),
    (K, "started: core.started.fetch_add(1, Ordering::SeqCst)", "started: core.started.fetch_add(0, Ordering::SeqCst)", T),
    (K, "args_bytes: args_text.len()", "args_bytes: 0", T),
    (K, "args_fingerprint: content_hash(&args_text)", "args_fingerprint: String::new()", T),
    (K, "record.completed = self.core.completed.fetch_add(1, Ordering::SeqCst);", "record.completed = self.core.completed.fetch_add(0, Ordering::SeqCst);", T),
    (K, "outcome: AuditOutcome::Aborted,\n            }),", "outcome: AuditOutcome::Ok,\n            }),", T),
    (K, "if self.is_halted() {", "if false {", T),
    (K, "if !core.served.contains(&request.tool) {", "if false {", T),
    (K, "core.halted.store(true, Ordering::SeqCst);", "core.halted.store(false, Ordering::SeqCst);", T),
    (K, "if outcome.is_halt() {", "if false {", T),
    (K, ".len() > MAX_OUTPUT_BYTES =>", ".len() >= MAX_OUTPUT_BYTES =>", T),
    (K, "line.len() > MAX_REQUEST_BYTES", "line.len() >= MAX_REQUEST_BYTES", T),
    (K, "MAX_LOGGED_NAME: usize = 128", "MAX_LOGGED_NAME: usize = 127", T),
    (K, "if next > MAX_LOGGED_NAME", "if next >= MAX_LOGGED_NAME", T),
    (K, "name.escape_debug().collect()", "name.chars().collect()", T),
    (K, "logged_name(&request.tool)),", "request.tool),", T),
    (K, "(ToolErrorKind::ActionBlocked, error)", "(ToolErrorKind::ExecutionFailed, error)", T),
    (K, "(ToolErrorKind::BudgetRefused, error)", "(ToolErrorKind::ExecutionFailed, error)", T),
    (K, "(ToolErrorKind::ScopeDenied, error)", "(ToolErrorKind::PathSecurity, error)", T),
    (K, "(ToolErrorKind::ProtectedPath, error)", "(ToolErrorKind::ScopeDenied, error)", T),
    (K, 'const HALT_NOTICE: &str = "the task was halted by the host";', 'const HALT_NOTICE: &str = "halted";', T),
    (K, 'const HALT_ERROR_TYPE: &str = "StateError";', 'const HALT_ERROR_TYPE: &str = "Other";', T),
    (K, "self.finish(AuditOutcome::Aborted);", "", T),
    (K, "BrokerOutcome::Halt(_) => AuditOutcome::Halt,", "BrokerOutcome::Halt(_) => AuditOutcome::Ok,", T),
    (K, 'format!("bad request: {error}")', 'format!("bad request")', T),
    (K, "self.core.registry.action_reviews()", "Vec::new()", T),
    (K, "tool: logged_name(tool),", "tool: String::new(),", T),
    (K, 'format!("production hold {escalation_id} on {repo} halted the task")', 'String::new()', T),
    (P, "agent: agent.name().to_string(),\n            model:", "agent: String::new(),\n            model:", T),
    (P, "model: agent.model().to_string(),", "model: String::new(),", T),
    (M, "                tools: identity\n                    .scope\n                    .tools\n                    .iter()\n                    .map(ToString::to_string)\n                    .collect(),", "                tools: Vec::new(),", T),
    (G, 'syn::Visibility::Restricted(restricted) if restricted.path.is_ident("crate") => {', "syn::Visibility::Restricted(restricted) if restricted.path.is_ident(\"super\") => {", S),
    (G, "if name == BIND {\n            self.binds", "if name == \"never\" {\n            self.binds", S),
    (G, "PRODUCTION_BIND_CALLERS_IN_THIS_SLICE: usize = 0", "PRODUCTION_BIND_CALLERS_IN_THIS_SLICE: usize = 1", S),
    (G, 'if CONSTRUCTORS.contains(&name) {', 'if false {', S),
]

INJECTED = [
    ("a new bind_workspace caller in tools.rs", "harness/src/tools.rs", "pub struct EchoTool;", "#[allow(dead_code)]\nfn zz_bypass(a: &crate::harness_adapter::ResolvedAgent, i: &crate::identity::AgentIdentity, w: &crate::workspace::TaskWorkspace, l: crate::task::ModelFacingLayers) {\n    let _ = a.bind_workspace(i, \"r\", w, l);\n}\n\npub struct EchoTool;", S),
    ("a new registry constructor call in pr_tools.rs", "harness/src/pr_tools.rs", "fn git_cmd(workspace_root: &Path) -> Command {", "#[allow(dead_code)]\nfn zz_bypass(p: &Path) { let _ = crate::tools::create_tool_registry(p); }\n\nfn git_cmd(workspace_root: &Path) -> Command {", S),
    ("ModelFacingLayers made pub", "harness/src/task.rs", "pub(crate) struct ModelFacingLayers {", "pub struct ModelFacingLayers {", S),
]


def run(args):
    result = subprocess.run(["cargo", "test", "-p", "harness"] + args, capture_output=True, text=True)
    out = re.sub(r"\x1b\[[0-9;]*m", "", result.stdout + result.stderr)
    return result.returncode, out


def verdict(code, out):
    if "error[" in out or "could not compile" in out:
        return "COMPILE ERROR"
    return "KILLED" if code != 0 else "SURVIVED"


def main():
    lines = []
    for number, (path, old, new, args) in enumerate(MUTANTS, 1):
        original = open(path).read()
        if original.count(old) != 1:
            lines.append(f"{number} {path.split('/')[-1]} {old[:60]!r}: NOT APPLIED")
            continue
        try:
            open(path, "w").write(original.replace(old, new, 1))
            code, out = run(args)
        finally:
            open(path, "w").write(original)
        lines.append(f"{number} {path.split('/')[-1]} {old[:60]!r} -> {new[:40]!r}: {verdict(code, out)}")
        print(lines[-1], flush=True)
    for name, path, old, new, args in INJECTED:
        original = open(path).read()
        try:
            open(path, "w").write(original.replace(old, new, 1))
            code, out = run(args)
        finally:
            open(path, "w").write(original)
        lines.append(f"{name}: {verdict(code, out)}")
        print(lines[-1], flush=True)
    open("docs/pi-mutation/broker-slice1-results.txt", "w").write("\n".join(lines) + "\n")


main()
