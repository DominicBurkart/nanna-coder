use crate::effects::EffectClass;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn default_timeout_secs() -> u64 {
    300
}

fn default_read_globs() -> Vec<String> {
    vec!["**".to_string()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Role,
    Stack,
    Bundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub kind: PluginKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsParser {
    #[default]
    None,
    CargoJson,
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    #[default]
    String,
    Path,
    PathList,
    Enum,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamSpec {
    #[serde(default)]
    pub kind: ParamKind,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerbSpec {
    pub argv: Vec<String>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamSpec>,
    pub effect: EffectClass,
    #[serde(default)]
    pub network: bool,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub diagnostics: DiagnosticsParser,
    #[serde(default)]
    pub diagnostics_pattern: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stack {
    pub language: String,
    pub package_manager: String,
    pub build_system: String,
    #[serde(default)]
    pub detect: Vec<String>,
    #[serde(default)]
    pub source_globs: Vec<String>,
    #[serde(default)]
    pub test_globs: Vec<String>,
    #[serde(default)]
    pub protected: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub verbs: BTreeMap<String, VerbSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackFile {
    pub stack: Stack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleLimits {
    pub max_iterations: u32,
    pub max_wall_clock_secs: u64,
    pub max_concurrent: u32,
    #[serde(default)]
    pub max_inactivity_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub name: String,
    pub description: String,
    pub prompt: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub verbs: Vec<String>,
    pub max_effect: EffectClass,
    #[serde(default = "default_read_globs")]
    pub read_globs: Vec<String>,
    #[serde(default)]
    pub write_globs: Vec<String>,
    #[serde(default)]
    pub limits: Option<RoleLimits>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleFile {
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Composition {
    pub name: String,
    pub role: String,
    pub stack: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub max_effect: Option<EffectClass>,
    #[serde(default)]
    pub limits: Option<RoleLimits>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionFile {
    pub agent: Composition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPolicy {
    pub max_effect: EffectClass,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub allowed_verbs: Vec<String>,
}

impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            max_effect: EffectClass::None,
            network: false,
            allowed_verbs: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectClass;

    const RUST_STACK: &str = r#"
[stack]
language = "rust"
package_manager = "cargo"
build_system = "cargo"
detect = ["Cargo.toml"]
source_globs = ["**/*.rs"]
test_globs = ["tests/**/*.rs"]
protected = ["Cargo.lock"]

[stack.env]
CARGO_NET_OFFLINE = "true"

[stack.verbs.check]
argv = ["cargo", "check", "--message-format=json", "{{package}}"]
effect = "workspace"
network = false
timeout_secs = 300
diagnostics = "cargo_json"

[stack.verbs.check.params.package]
kind = "string"
description = "Package to check"
required = false

[stack.verbs.format]
argv = ["rustfmt", "--edition", "{{edition}}", "{{files}}"]
effect = "workspace"
"#;

    const IMPLEMENTER_ROLE: &str = r#"
[role]
name = "implementer"
description = "Implements one change in a {{stack.language}} repository"
prompt = "You change {{stack.language}} code. Build with {{stack.build_system}}."
tools = ["read_file", "write_file", "replace", "grep", "list_directory", "finish"]
verbs = ["check"]
max_effect = "workspace"
write_globs = ["{{stack.source_globs}}"]

[role.limits]
max_iterations = 200
max_wall_clock_secs = 43200
max_concurrent = 4
"#;

    const COMPOSITION: &str = r#"
[agent]
name = "rust-implementer"
role = "implementer"
stack = "rust-cargo"
"#;

    #[test]
    fn a_stack_parses_its_markers_globs_verbs_and_env() {
        let file: StackFile = toml::from_str(RUST_STACK).unwrap();
        let stack = file.stack;

        assert_eq!(stack.language, "rust");
        assert_eq!(stack.package_manager, "cargo");
        assert_eq!(stack.detect, ["Cargo.toml"]);
        assert_eq!(stack.source_globs, ["**/*.rs"]);
        assert_eq!(stack.protected, ["Cargo.lock"]);
        assert_eq!(stack.env["CARGO_NET_OFFLINE"], "true");
        let check = &stack.verbs["check"];
        assert_eq!(check.argv[0], "cargo");
        assert_eq!(check.effect, EffectClass::Workspace);
        assert!(!check.network);
        assert_eq!(check.timeout_secs, 300);
        assert_eq!(check.diagnostics, DiagnosticsParser::CargoJson);
        assert!(!check.params["package"].required);
    }

    #[test]
    fn a_verb_defaults_to_no_network_a_five_minute_cap_and_no_parser() {
        let file: StackFile = toml::from_str(RUST_STACK).unwrap();
        let format = &file.stack.verbs["format"];

        assert!(!format.network);
        assert_eq!(format.timeout_secs, 300);
        assert_eq!(format.diagnostics, DiagnosticsParser::None);
        assert!(format.params.is_empty());
    }

    #[test]
    fn a_role_parses_its_prompt_tools_verbs_and_limits() {
        let file: RoleFile = toml::from_str(IMPLEMENTER_ROLE).unwrap();
        let role = file.role;

        assert_eq!(role.name, "implementer");
        assert!(role.prompt.contains("{{stack.language}}"));
        assert_eq!(role.tools.len(), 6);
        assert_eq!(role.verbs, ["check"]);
        assert_eq!(role.max_effect, EffectClass::Workspace);
        assert_eq!(role.write_globs, ["{{stack.source_globs}}"]);
        assert_eq!(role.read_globs, ["**"]);
        let limits = role.limits.unwrap();
        assert_eq!(limits.max_wall_clock_secs, 43200);
        assert_eq!(limits.max_inactivity_secs, None);
    }

    #[test]
    fn a_composition_names_a_role_and_a_stack() {
        let file: CompositionFile = toml::from_str(COMPOSITION).unwrap();

        assert_eq!(file.agent.name, "rust-implementer");
        assert_eq!(file.agent.role, "implementer");
        assert_eq!(file.agent.stack, "rust-cargo");
        assert!(file.agent.max_effect.is_none());
    }

    #[test]
    fn unknown_keys_are_rejected_in_every_plugin_file() {
        let stack = RUST_STACK.replace("language = \"rust\"", "language = \"rust\"\nlanguag = 1");
        let role =
            IMPLEMENTER_ROLE.replace("verbs = [\"check\"]", "verbs = [\"check\"]\nextra = 1");
        let composition =
            COMPOSITION.replace("stack = \"rust-cargo\"", "stack = \"rust-cargo\"\nx = 1");

        assert!(toml::from_str::<StackFile>(&stack).is_err());
        assert!(toml::from_str::<RoleFile>(&role).is_err());
        assert!(toml::from_str::<CompositionFile>(&composition).is_err());
    }

    #[test]
    fn a_manifest_names_its_kind() {
        let manifest: PluginManifest =
            toml::from_str("name = \"rust\"\nversion = \"1.0.0\"\nkind = \"bundle\"").unwrap();

        assert_eq!(manifest.kind, PluginKind::Bundle);
        assert!(toml::from_str::<PluginManifest>(
            "name = \"x\"\nversion = \"1\"\nkind = \"nonsense\""
        )
        .is_err());
    }

    #[test]
    fn a_host_policy_caps_effect_network_and_verbs() {
        let policy: HostPolicy = toml::from_str(
            "max_effect = \"workspace\"\nnetwork = false\nallowed_verbs = [\"check\", \"test\", \"format\"]",
        )
        .unwrap();

        assert_eq!(policy.max_effect, EffectClass::Workspace);
        assert!(!policy.network);
        assert_eq!(policy.allowed_verbs, ["check", "test", "format"]);
    }

    #[test]
    fn a_host_policy_defaults_to_the_narrowest_grant() {
        let policy = HostPolicy::default();

        assert_eq!(policy.max_effect, EffectClass::None);
        assert!(!policy.network);
        assert!(policy.allowed_verbs.is_empty());
    }

    #[test]
    fn diagnostics_parsers_use_snake_case_names() {
        let parsed: Vec<DiagnosticsParser> =
            serde_json::from_str("[\"none\", \"cargo_json\", \"regex\"]").unwrap();

        assert_eq!(
            parsed,
            [
                DiagnosticsParser::None,
                DiagnosticsParser::CargoJson,
                DiagnosticsParser::Regex
            ]
        );
    }
}
