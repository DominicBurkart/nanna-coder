use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::AgentIdentity;
use crate::plugins::types::{Stack, VerbSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Light,
    Heavy { sandbox_image: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    OfflinePrivateNamespaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilitySpec {
    pub name: &'static str,
    pub tools: &'static [&'static str],
    pub toolchain: &'static [&'static str],
    pub isolation: Isolation,
    pub tier: Tier,
    pub workspace_signal: Option<&'static str>,
    pub offline_cargo_home: bool,
}

pub const BUILD: CapabilitySpec = CapabilitySpec {
    name: "build",
    tools: &["cargo_check", "cargo_test", "cargo_build"],
    toolchain: &["cargo", "rustc", "cc"],
    isolation: Isolation::OfflinePrivateNamespaces,
    tier: Tier::Light,
    workspace_signal: Some("Cargo.toml"),
    offline_cargo_home: true,
};

pub const CAPABILITIES: &[CapabilitySpec] = &[BUILD];

pub fn capability_for_tool(tool: &str) -> Option<&'static CapabilitySpec> {
    CAPABILITIES.iter().find(|spec| spec.tools.contains(&tool))
}

pub fn wants_exec_tools(identity: &AgentIdentity) -> bool {
    CAPABILITIES
        .iter()
        .any(|spec| !requested_tools(spec, identity).is_empty())
}

pub fn is_exec_tool(tool: &str) -> bool {
    capability_for_tool(tool).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsUnavailable {
    pub capability: String,
    pub tools: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct Environment {
    pub path_dirs: Vec<PathBuf>,
    pub cargo_home: Option<PathBuf>,
    pub cargo_home_read_only: bool,
    pub offline: bool,
    pub is_root: bool,
    pub helper: Option<PathBuf>,
    pub isolation: Result<(), String>,
    pub hidden: Vec<PathBuf>,
}

pub const HELPER_NAME: &str = "nanna-sandbox";
pub const DEFAULT_HIDDEN: &[&str] = &["/repos", "/etc/nanna", "/var/lib/nanna"];

impl Environment {
    pub fn detect() -> Self {
        let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
        let cargo_home_read_only = cargo_home.as_deref().is_some_and(mount_is_read_only);
        let offline = std::env::var("CARGO_NET_OFFLINE").is_ok_and(|v| v == "true");
        let is_root = unsafe { libc::geteuid() } == 0;
        let helper = locate_helper(&path_dirs);
        let hidden = match std::env::var("NANNA_SANDBOX_HIDE") {
            Ok(list) => list
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
            Err(_) => DEFAULT_HIDDEN.iter().map(PathBuf::from).collect(),
        };
        let isolation = match &helper {
            Some(helper) => crate::exec_sandbox::probe(helper),
            None => Err(format!(
                "{HELPER_NAME} is not installed next to nanna or on PATH"
            )),
        };
        Self {
            path_dirs,
            cargo_home,
            cargo_home_read_only,
            offline,
            is_root,
            helper,
            isolation,
            hidden,
        }
    }

    pub fn find_program(&self, name: &str) -> Option<PathBuf> {
        self.path_dirs
            .iter()
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    }
}

fn locate_helper(path_dirs: &[PathBuf]) -> Option<PathBuf> {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(HELPER_NAME)));
    sibling
        .into_iter()
        .chain(path_dirs.iter().map(|dir| dir.join(HELPER_NAME)))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

pub fn mount_is_read_only(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return false;
    }
    stat.f_flag & libc::ST_RDONLY != 0
}

pub fn refusal(spec: &CapabilitySpec, workspace_root: &Path, env: &Environment) -> Option<String> {
    refuse(
        &Needs {
            name: spec.name,
            tier: spec.tier,
            workspace_signal: spec.workspace_signal,
            toolchain: spec.toolchain.to_vec(),
            offline_cargo_home: spec.offline_cargo_home,
            isolation: spec.isolation,
        },
        workspace_root,
        env,
    )
}

struct Needs<'a> {
    name: &'a str,
    tier: Tier,
    workspace_signal: Option<&'a str>,
    toolchain: Vec<&'a str>,
    offline_cargo_home: bool,
    isolation: Isolation,
}

fn refuse(needs: &Needs<'_>, workspace_root: &Path, env: &Environment) -> Option<String> {
    if let Tier::Heavy { sandbox_image } = needs.tier {
        return Some(format!(
            "capability {} needs the dedicated sandbox image {sandbox_image}, which the default harness image does not carry",
            needs.name
        ));
    }
    if let Some(signal) = needs.workspace_signal {
        if !workspace_root.join(signal).exists() {
            return Some(format!("the workspace has no {signal}"));
        }
    }
    for program in &needs.toolchain {
        if env.find_program(program).is_none() {
            return Some(format!(
                "toolchain program {program} is not in the harness image"
            ));
        }
    }
    if needs.offline_cargo_home {
        if !env.offline {
            return Some("CARGO_NET_OFFLINE=true is not set for the harness".to_string());
        }
        match &env.cargo_home {
            None => return Some("CARGO_HOME is not set for the harness".to_string()),
            Some(home) if !home.join("config.toml").is_file() => {
                return Some(format!(
                    "CARGO_HOME {} has no config.toml pointing at vendored dependencies",
                    home.display()
                ))
            }
            Some(_) if !env.cargo_home_read_only => {
                return Some("CARGO_HOME is not a read-only mount".to_string())
            }
            Some(_) => {}
        }
    }
    match needs.isolation {
        Isolation::OfflinePrivateNamespaces => {
            if !env.is_root {
                return Some(
                    "the sandbox drops to an unprivileged uid and the harness is not root"
                        .to_string(),
                );
            }
            if let Err(why) = &env.isolation {
                return Some(format!("sandbox isolation is unavailable: {why}"));
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackCapability {
    pub name: String,
    pub tools: Vec<String>,
    pub toolchain: Vec<String>,
    pub workspace_signal: Option<String>,
    pub offline_cargo_home: bool,
    pub isolation: Isolation,
    pub tier: Tier,
}

fn is_cargo_stack(stack: &Stack) -> bool {
    stack.build_system == "cargo" || stack.package_manager == "cargo"
}

pub fn tool_names(stack: &Stack, verb: &str) -> Vec<String> {
    let mut names = vec![verb.to_string()];
    if is_cargo_stack(stack) {
        names.push(format!("cargo_{verb}"));
    }
    names
}

pub fn verb_for_tool<'a>(stack: &'a Stack, tool: &str) -> Option<(&'a str, &'a VerbSpec)> {
    stack
        .verbs
        .iter()
        .find(|(verb, _)| tool_names(stack, verb).iter().any(|name| name == tool))
        .map(|(verb, spec)| (verb.as_str(), spec))
}

pub fn capability_for_stack(stack: &Stack) -> StackCapability {
    let mut toolchain: Vec<String> = Vec::new();
    if is_cargo_stack(stack) {
        toolchain.extend(["cargo", "rustc", "cc"].map(String::from));
    }
    StackCapability {
        name: stack.build_system.clone(),
        tools: stack
            .verbs
            .keys()
            .flat_map(|verb| tool_names(stack, verb))
            .collect(),
        toolchain,
        workspace_signal: stack.detect.first().cloned(),
        offline_cargo_home: is_cargo_stack(stack),
        isolation: Isolation::OfflinePrivateNamespaces,
        tier: Tier::Light,
    }
}

pub fn stack_refusal(
    capability: &StackCapability,
    workspace_root: &Path,
    env: &Environment,
) -> Option<String> {
    refuse(
        &Needs {
            name: &capability.name,
            tier: capability.tier,
            workspace_signal: capability.workspace_signal.as_deref(),
            toolchain: capability.toolchain.iter().map(String::as_str).collect(),
            offline_cargo_home: capability.offline_cargo_home,
            isolation: capability.isolation,
        },
        workspace_root,
        env,
    )
}

#[derive(Debug, Default)]
pub struct StackPlan {
    pub granted: Vec<String>,
    pub unavailable: Vec<ToolsUnavailable>,
}

pub fn plan_for_stack(
    identity: &AgentIdentity,
    stack: &Stack,
    workspace_root: &Path,
    env: &Environment,
    allow_network: bool,
) -> StackPlan {
    let mut plan = StackPlan::default();
    let capability = capability_for_stack(stack);
    let requested = |verb: &str| -> Vec<String> {
        tool_names(stack, verb)
            .into_iter()
            .filter(|name| {
                identity
                    .scope
                    .tools
                    .iter()
                    .any(|pattern| pattern.as_str() == name)
            })
            .collect()
    };
    let wanted: Vec<(&VerbSpec, Vec<String>)> = stack
        .verbs
        .iter()
        .map(|(verb, spec)| (spec, requested(verb)))
        .filter(|(_, names)| !names.is_empty())
        .collect();
    if wanted.is_empty() {
        return plan;
    }
    if let Some(reason) = stack_refusal(&capability, workspace_root, env) {
        plan.unavailable.push(ToolsUnavailable {
            capability: capability.name,
            tools: wanted.iter().flat_map(|(_, names)| names.clone()).collect(),
            reason,
        });
        return plan;
    }
    let mut blocked = Vec::new();
    for (spec, names) in wanted {
        let missing = spec
            .argv
            .first()
            .filter(|program| env.find_program(program).is_none());
        if let Some(program) = missing {
            plan.unavailable.push(ToolsUnavailable {
                capability: capability.name.clone(),
                tools: names,
                reason: format!("toolchain program {program} is not in the harness image"),
            });
        } else if spec.network && !allow_network {
            blocked.extend(names);
        } else {
            plan.granted.extend(names);
        }
    }
    if !blocked.is_empty() {
        plan.unavailable.push(ToolsUnavailable {
            capability: capability.name,
            tools: blocked,
            reason: "the verb needs network access and the host does not allow network".to_string(),
        });
    }
    plan
}

pub fn requested_tools<'a>(
    spec: &'a CapabilitySpec,
    identity: &AgentIdentity,
) -> Vec<&'a &'static str> {
    spec.tools
        .iter()
        .filter(|tool| {
            identity
                .scope
                .tools
                .iter()
                .any(|pattern| pattern.as_str() == **tool)
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct Plan {
    pub granted: Vec<&'static str>,
    pub unavailable: Vec<ToolsUnavailable>,
}

pub fn plan(identity: &AgentIdentity, workspace_root: &Path, env: &Environment) -> Plan {
    let mut plan = Plan::default();
    for spec in CAPABILITIES {
        let requested = requested_tools(spec, identity);
        if requested.is_empty() {
            continue;
        }
        match refusal(spec, workspace_root, env) {
            None => plan.granted.extend(requested.into_iter().copied()),
            Some(reason) => plan.unavailable.push(ToolsUnavailable {
                capability: spec.name.to_string(),
                tools: requested.into_iter().map(|t| t.to_string()).collect(),
                reason,
            }),
        }
    }
    plan
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn identity_with(tools: &str) -> AgentIdentity {
        let src = format!(
            "[identity]\nname = \"card\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\nsystem_prompt = {{ inline = \"p\" }}\n\n[scope]\nrepos = [\"github.com/o/n\"]\npaths = [\"**\"]\nmax_effect = \"workspace\"\ntools = [{tools}]\n\n[limits]\nmax_iterations = 10\nmax_wall_clock_secs = 60\nmax_concurrent = 1\n"
        );
        AgentIdentity::from_toml_str(&src, "agents/card.toml").unwrap()
    }

    pub(crate) fn ready_environment(bin: &Path, home: &Path) -> Environment {
        Environment {
            path_dirs: vec![bin.to_path_buf()],
            cargo_home: Some(home.to_path_buf()),
            cargo_home_read_only: true,
            offline: true,
            is_root: true,
            helper: Some(bin.join(HELPER_NAME)),
            isolation: Ok(()),
            hidden: vec![PathBuf::from("/repos")],
        }
    }

    pub(crate) struct Fixture {
        pub bin: tempfile::TempDir,
        pub home: tempfile::TempDir,
        pub repo: tempfile::TempDir,
    }

    pub(crate) fn fixture() -> Fixture {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        for program in ["cargo", "rustc", "cc"] {
            let path = bin.path().join(program);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[net]\noffline = true\n").unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        Fixture { bin, home, repo }
    }

    #[test]
    fn a_card_naming_cargo_tools_exactly_is_granted_them_when_everything_is_in_place() {
        let f = fixture();
        let env = ready_environment(f.bin.path(), f.home.path());
        let identity = identity_with("\"read_file\", \"cargo_check\", \"cargo_test\"");
        let plan = plan(&identity, f.repo.path(), &env);
        assert_eq!(plan.granted, vec!["cargo_check", "cargo_test"]);
        assert!(plan.unavailable.is_empty());
    }

    #[test]
    fn cards_without_the_exact_tool_names_never_receive_exec_tools() {
        let f = fixture();
        let env = ready_environment(f.bin.path(), f.home.path());
        for tools in [
            "\"read_file\", \"search\", \"list_directory\", \"git_diff\"",
            "\"cargo_*\"",
            "\"*\"",
            "\"cargo_[a-z]*\"",
        ] {
            let plan = plan(&identity_with(tools), f.repo.path(), &env);
            assert!(plan.granted.is_empty(), "{tools}");
            assert!(plan.unavailable.is_empty(), "{tools}");
        }
    }

    #[test]
    fn every_missing_prerequisite_refuses_with_a_reason() {
        let f = fixture();
        let identity = identity_with("\"cargo_check\"");
        type Break = Box<dyn Fn(&mut Environment)>;
        let cases: Vec<(&str, Break)> = vec![
            ("CARGO_NET_OFFLINE", Box::new(|e| e.offline = false)),
            ("CARGO_HOME is not set", Box::new(|e| e.cargo_home = None)),
            ("read-only", Box::new(|e| e.cargo_home_read_only = false)),
            ("not root", Box::new(|e| e.is_root = false)),
            (
                "isolation is unavailable",
                Box::new(|e| e.isolation = Err("denied".to_string())),
            ),
            ("toolchain program cargo", Box::new(|e| e.path_dirs.clear())),
        ];
        for (needle, break_it) in cases {
            let mut env = ready_environment(f.bin.path(), f.home.path());
            break_it(&mut env);
            let plan = plan(&identity, f.repo.path(), &env);
            assert!(plan.granted.is_empty(), "{needle}");
            assert_eq!(plan.unavailable.len(), 1, "{needle}");
            assert!(
                plan.unavailable[0].reason.contains(needle),
                "{needle}: {}",
                plan.unavailable[0].reason
            );
            assert_eq!(plan.unavailable[0].tools, vec!["cargo_check"]);
        }
    }

    #[test]
    fn a_workspace_without_cargo_toml_refuses() {
        let f = fixture();
        let env = ready_environment(f.bin.path(), f.home.path());
        let empty = tempfile::tempdir().unwrap();
        let plan = plan(&identity_with("\"cargo_build\""), empty.path(), &env);
        assert!(plan.granted.is_empty());
        assert!(plan.unavailable[0].reason.contains("Cargo.toml"));
    }

    #[test]
    fn a_cargo_home_without_vendored_config_refuses() {
        let f = fixture();
        let env = ready_environment(f.bin.path(), f.home.path());
        std::fs::remove_file(f.home.path().join("config.toml")).unwrap();
        let plan = plan(&identity_with("\"cargo_check\""), f.repo.path(), &env);
        assert!(plan.unavailable[0].reason.contains("config.toml"));
    }

    #[test]
    fn heavy_tier_capabilities_are_refused_by_the_default_image() {
        let f = fixture();
        let env = ready_environment(f.bin.path(), f.home.path());
        let heavy = CapabilitySpec {
            name: "formal",
            tier: Tier::Heavy {
                sandbox_image: "nanna-sandbox-formal",
            },
            ..BUILD
        };
        let reason = refusal(&heavy, f.repo.path(), &env).unwrap();
        assert!(reason.contains("nanna-sandbox-formal"));
    }

    #[test]
    fn the_catalog_maps_each_tool_to_exactly_one_capability() {
        let mut seen = std::collections::HashSet::new();
        for spec in CAPABILITIES {
            for tool in spec.tools {
                assert!(seen.insert(*tool), "{tool} appears twice");
                assert_eq!(capability_for_tool(tool).unwrap().name, spec.name);
            }
        }
        assert!(!is_exec_tool("read_file"));
    }

    #[test]
    fn detect_reports_what_this_process_has() {
        let env = Environment::detect();
        assert!(!env.path_dirs.is_empty());
    }

    #[test]
    fn a_writable_directory_is_not_a_read_only_mount() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!mount_is_read_only(dir.path()));
        assert!(!mount_is_read_only(Path::new("/definitely/not/here")));
    }

    const RUST_STACK: &str = r#"
[stack]
language = "rust"
package_manager = "cargo"
build_system = "cargo"
detect = ["Cargo.toml"]
source_globs = ["**/*.rs"]

[stack.verbs.check]
argv = ["cargo", "check", "--message-format=json", "{{package}}"]
effect = "workspace"
diagnostics = "cargo_json"

[stack.verbs.check.params.package]
kind = "string"

[stack.verbs.test]
argv = ["cargo", "test"]
effect = "workspace"

[stack.verbs.build]
argv = ["cargo", "build"]
effect = "workspace"

[stack.verbs.format]
argv = ["rustfmt", "--check"]
effect = "workspace"
"#;

    const GO_STACK: &str = r#"
[stack]
language = "go"
package_manager = "go"
build_system = "go"
detect = ["go.mod"]
source_globs = ["**/*.go"]

[stack.verbs.check]
argv = ["go", "vet", "./..."]
effect = "workspace"

[stack.verbs.fetch]
argv = ["go", "mod", "download"]
effect = "workspace"
network = true
"#;

    fn stack(src: &str) -> crate::plugins::types::Stack {
        toml::from_str::<crate::plugins::types::StackFile>(src)
            .unwrap()
            .stack
    }

    fn go_fixture() -> Fixture {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        let path = bin.path().join("go");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("go.mod"), "module x\n").unwrap();
        Fixture { bin, home, repo }
    }

    fn go_environment(f: &Fixture) -> Environment {
        Environment {
            cargo_home: None,
            cargo_home_read_only: false,
            offline: false,
            ..ready_environment(f.bin.path(), f.home.path())
        }
    }

    #[test]
    fn verb_names_are_bare_and_cargo_prefixed_only_for_cargo_stacks() {
        let rust = stack(RUST_STACK);
        assert_eq!(tool_names(&rust, "check"), vec!["check", "cargo_check"]);
        let go = stack(GO_STACK);
        assert_eq!(tool_names(&go, "check"), vec!["check"]);
    }

    #[test]
    fn a_tool_name_resolves_back_to_its_verb() {
        let rust = stack(RUST_STACK);
        for name in ["check", "cargo_check"] {
            assert_eq!(verb_for_tool(&rust, name).unwrap().0, "check", "{name}");
        }
        assert!(verb_for_tool(&rust, "cargo_ghost").is_none());
        assert!(verb_for_tool(&rust, "read_file").is_none());
        let go = stack(GO_STACK);
        assert!(verb_for_tool(&go, "cargo_check").is_none());
        assert_eq!(verb_for_tool(&go, "check").unwrap().0, "check");
    }

    #[test]
    fn a_cargo_stack_keeps_the_legacy_tool_names_and_the_exact_name_rule() {
        let f = fixture();
        let env = ready_environment(f.bin.path(), f.home.path());
        let rust = stack(RUST_STACK);
        let granted = plan_for_stack(
            &identity_with("\"read_file\", \"cargo_check\", \"cargo_test\""),
            &rust,
            f.repo.path(),
            &env,
            false,
        );
        assert_eq!(granted.granted, vec!["cargo_check", "cargo_test"]);
        let bare = plan_for_stack(
            &identity_with("\"check\", \"build\""),
            &rust,
            f.repo.path(),
            &env,
            false,
        );
        assert_eq!(bare.granted, vec!["build", "check"]);
        for tools in ["\"cargo_*\"", "\"*\"", "\"che*\"", "\"read_file\""] {
            let plan = plan_for_stack(&identity_with(tools), &rust, f.repo.path(), &env, false);
            assert!(plan.granted.is_empty(), "{tools}");
            assert!(plan.unavailable.is_empty(), "{tools}");
        }
    }

    #[test]
    fn a_cargo_stack_still_refuses_on_every_cargo_prerequisite() {
        let f = fixture();
        let rust = stack(RUST_STACK);
        let identity = identity_with("\"cargo_check\"");
        type Break = Box<dyn Fn(&mut Environment)>;
        let cases: Vec<(&str, Break)> = vec![
            ("CARGO_NET_OFFLINE", Box::new(|e| e.offline = false)),
            ("CARGO_HOME is not set", Box::new(|e| e.cargo_home = None)),
            ("read-only", Box::new(|e| e.cargo_home_read_only = false)),
            ("not root", Box::new(|e| e.is_root = false)),
            (
                "isolation is unavailable",
                Box::new(|e| e.isolation = Err("denied".to_string())),
            ),
            ("toolchain program cargo", Box::new(|e| e.path_dirs.clear())),
        ];
        for (needle, break_it) in cases {
            let mut env = ready_environment(f.bin.path(), f.home.path());
            break_it(&mut env);
            let plan = plan_for_stack(&identity, &rust, f.repo.path(), &env, false);
            assert!(plan.granted.is_empty(), "{needle}");
            assert_eq!(plan.unavailable.len(), 1, "{needle}");
            assert!(plan.unavailable[0].reason.contains(needle), "{needle}");
            assert_eq!(plan.unavailable[0].tools, vec!["cargo_check"]);
        }
    }

    #[test]
    fn a_go_stack_needs_no_cargo_environment() {
        let f = go_fixture();
        let go = stack(GO_STACK);
        let env = go_environment(&f);
        let plan = plan_for_stack(&identity_with("\"check\""), &go, f.repo.path(), &env, false);
        assert_eq!(plan.granted, vec!["check"]);
        assert!(plan.unavailable.is_empty());
    }

    #[test]
    fn a_go_stack_refuses_without_its_toolchain_or_workspace_signal() {
        let f = go_fixture();
        let go = stack(GO_STACK);
        let identity = identity_with("\"check\"");
        let mut env = go_environment(&f);
        env.path_dirs.clear();
        let plan = plan_for_stack(&identity, &go, f.repo.path(), &env, false);
        assert!(plan.unavailable[0].reason.contains("toolchain program go"));
        let env = go_environment(&f);
        let empty = tempfile::tempdir().unwrap();
        let plan = plan_for_stack(&identity, &go, empty.path(), &env, false);
        assert!(plan.unavailable[0].reason.contains("go.mod"));
    }

    #[test]
    fn a_missing_verb_program_refuses_only_the_verbs_that_need_it() {
        let f = fixture();
        let env = ready_environment(f.bin.path(), f.home.path());
        let rust = stack(RUST_STACK);
        let plan = plan_for_stack(
            &identity_with("\"cargo_check\", \"format\""),
            &rust,
            f.repo.path(),
            &env,
            false,
        );
        assert_eq!(plan.granted, vec!["cargo_check"]);
        assert_eq!(plan.unavailable.len(), 1);
        assert_eq!(plan.unavailable[0].tools, vec!["format"]);
        assert!(plan.unavailable[0]
            .reason
            .contains("toolchain program rustfmt"));
    }

    #[test]
    fn network_verbs_need_the_caller_to_allow_network() {
        let f = go_fixture();
        let go = stack(GO_STACK);
        let env = go_environment(&f);
        let identity = identity_with("\"check\", \"fetch\"");
        let denied = plan_for_stack(&identity, &go, f.repo.path(), &env, false);
        assert_eq!(denied.granted, vec!["check"]);
        assert_eq!(denied.unavailable.len(), 1);
        assert_eq!(denied.unavailable[0].tools, vec!["fetch"]);
        assert!(denied.unavailable[0].reason.contains("network"));
        let allowed = plan_for_stack(&identity, &go, f.repo.path(), &env, true);
        assert_eq!(allowed.granted, vec!["check", "fetch"]);
        assert!(allowed.unavailable.is_empty());
    }

    #[test]
    fn stack_capabilities_name_tools_uniquely_and_derive_the_toolchain() {
        let rust = capability_for_stack(&stack(RUST_STACK));
        assert_eq!(rust.tools.len(), 8);
        let unique: std::collections::HashSet<&String> = rust.tools.iter().collect();
        assert_eq!(unique.len(), rust.tools.len());
        for program in ["cargo", "rustc", "cc"] {
            assert!(rust.toolchain.iter().any(|p| p == program), "{program}");
        }
        assert!(rust.offline_cargo_home);
        assert_eq!(rust.workspace_signal.as_deref(), Some("Cargo.toml"));
        let go = capability_for_stack(&stack(GO_STACK));
        assert!(go.toolchain.is_empty());
        assert!(!go.offline_cargo_home);
        assert_eq!(go.workspace_signal.as_deref(), Some("go.mod"));
    }
}
