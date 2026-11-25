use super::types::{Composition, Role, Stack};
use crate::effects::EffectClass;
use crate::identity::{
    AgentIdentity, DevLoop, IdentityError, IdentitySection, LimitsSection, ScopeSection,
    SystemPrompt, ToolPattern,
};
use std::path::Path;
use thiserror::Error;

pub const DEFAULT_AGENT_MODEL: &str = "gemma4:e4b";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginSource {
    Curated,
    Host,
    Repo,
}

impl PluginSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            PluginSource::Curated => "curated",
            PluginSource::Host => "host",
            PluginSource::Repo => "repo",
        }
    }
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("unknown template variable `{0}`")]
    UnknownVariable(String),
    #[error("template variable `{0}` is a list and may only fill a whole list item")]
    ListInText(String),
    #[error("malformed template: {0}")]
    MalformedTemplate(String),
    #[error("role `{role}` lists verb `{verb}` which stack `{stack}` does not define")]
    UnknownVerb {
        role: String,
        stack: String,
        verb: String,
    },
    #[error("verb `{verb}` has effect {verb_effect}, above the agent ceiling {ceiling}")]
    VerbExceedsCeiling {
        verb: String,
        verb_effect: EffectClass,
        ceiling: EffectClass,
    },
    #[error("agent `{0}` has no limits: set them on the role or the composition")]
    MissingLimits(String),
    #[error("agent `{agent}`: unknown dev loop `{value}`")]
    UnknownLoop { agent: String, value: String },
    #[error("{file}: {message}")]
    Io { file: String, message: String },
    #[error("{file}: {message}")]
    Parse { file: String, message: String },
    #[error("{kind} `{name}` is already defined by {first}")]
    Duplicate {
        kind: &'static str,
        name: String,
        first: String,
    },
    #[error("{kind} `{name}` is not defined")]
    Missing { kind: &'static str, name: String },
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Policy(#[from] super::policy::PolicyViolation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedTool {
    pub name: String,
    pub effect: EffectClass,
    pub network: bool,
    pub verb: Option<RenderedVerb>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedVerb {
    pub verb: String,
    pub argv: Vec<String>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLimits {
    pub max_iterations: u32,
    pub max_wall_clock_secs: u64,
    pub max_concurrent: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedAgent {
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub dev_loop: DevLoop,
    pub model: String,
    pub repos: Vec<String>,
    pub write_paths: Vec<String>,
    pub read_paths: Vec<String>,
    pub protected: Vec<String>,
    pub max_effect: EffectClass,
    pub tools: Vec<RenderedTool>,
    pub limits: RenderedLimits,
    pub source: PluginSource,
    pub plugin_version: String,
}

enum Piece {
    Text(String),
    Var(String),
}

enum Resolved<'a> {
    Text(String),
    List(&'a [String]),
}

fn parse(template: &str) -> Result<Vec<Piece>, PluginError> {
    let mut pieces = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        if start > 0 {
            pieces.push(Piece::Text(rest[..start].to_string()));
        }
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or_else(|| {
            PluginError::MalformedTemplate(format!("unterminated `{{{{` in `{template}`"))
        })?;
        let name = after[..end].trim();
        if name.is_empty() {
            return Err(PluginError::MalformedTemplate(format!(
                "empty placeholder in `{template}`"
            )));
        }
        pieces.push(Piece::Var(name.to_string()));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        pieces.push(Piece::Text(rest.to_string()));
    }
    Ok(pieces)
}

fn resolve<'a>(stack: &'a Stack, name: &str) -> Result<Resolved<'a>, PluginError> {
    let unknown = || PluginError::UnknownVariable(name.to_string());
    let field = name.strip_prefix("stack.").ok_or_else(unknown)?;
    if let Some(verb) = field.strip_prefix("verbs.") {
        let spec = stack.verbs.get(verb).ok_or_else(unknown)?;
        return Ok(Resolved::Text(spec.argv.join(" ")));
    }
    match field {
        "language" => Ok(Resolved::Text(stack.language.clone())),
        "build_system" => Ok(Resolved::Text(stack.build_system.clone())),
        "package_manager" => Ok(Resolved::Text(stack.package_manager.clone())),
        "source_globs" => Ok(Resolved::List(&stack.source_globs)),
        "test_globs" => Ok(Resolved::List(&stack.test_globs)),
        "protected" => Ok(Resolved::List(&stack.protected)),
        "detect" => Ok(Resolved::List(&stack.detect)),
        _ => Err(unknown()),
    }
}

fn render_pieces(pieces: Vec<Piece>, stack: &Stack) -> Result<String, PluginError> {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(text) => out.push_str(&text),
            Piece::Var(name) => match resolve(stack, &name)? {
                Resolved::Text(text) => out.push_str(&text),
                Resolved::List(_) => return Err(PluginError::ListInText(name)),
            },
        }
    }
    Ok(out)
}

pub fn render_text(template: &str, stack: &Stack) -> Result<String, PluginError> {
    render_pieces(parse(template)?, stack)
}

pub fn render_list(items: &[String], stack: &Stack) -> Result<Vec<String>, PluginError> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |value: String| {
        if !out.contains(&value) {
            out.push(value);
        }
    };
    for item in items {
        let pieces = parse(item)?;
        if let [Piece::Var(name)] = pieces.as_slice() {
            match resolve(stack, name)? {
                Resolved::List(values) => values.iter().cloned().for_each(&mut push),
                Resolved::Text(text) => push(text),
            }
            continue;
        }
        push(render_pieces(pieces, stack)?);
    }
    Ok(out)
}

fn builtin_effect(tool: &str) -> Option<EffectClass> {
    match tool {
        "read_file" | "list_directory" | "grep" | "search" | "finish" => Some(EffectClass::None),
        "write_file" | "replace" | "edit_file" => Some(EffectClass::Workspace),
        _ => None,
    }
}

fn reads_files(tool: &str) -> bool {
    matches!(tool, "read_file" | "list_directory" | "grep" | "search")
}

fn writes_files(tool: &str) -> bool {
    matches!(tool, "write_file" | "replace" | "edit_file")
}

fn within(tool: &str, effect: EffectClass, ceiling: EffectClass) -> Result<(), PluginError> {
    if effect > ceiling {
        return Err(PluginError::VerbExceedsCeiling {
            verb: tool.to_string(),
            verb_effect: effect,
            ceiling,
        });
    }
    Ok(())
}

pub fn render_composition(
    role: &Role,
    stack: &Stack,
    comp: &Composition,
) -> Result<RenderedAgent, PluginError> {
    let ceiling = comp
        .max_effect
        .map_or(role.max_effect, |own| own.min(role.max_effect));
    let dev_loop = match &role.dev_loop {
        None => DevLoop::Inner,
        Some(value) => value
            .parse::<DevLoop>()
            .map_err(|_| PluginError::UnknownLoop {
                agent: comp.name.clone(),
                value: value.clone(),
            })?,
    };
    let limits = comp
        .limits
        .as_ref()
        .or(role.limits.as_ref())
        .ok_or_else(|| PluginError::MissingLimits(comp.name.clone()))?;

    let mut tools: Vec<RenderedTool> = Vec::new();
    for name in &role.tools {
        if tools.iter().any(|t| &t.name == name) {
            continue;
        }
        let effect = builtin_effect(name).unwrap_or(ceiling);
        within(name, effect, ceiling)?;
        tools.push(RenderedTool {
            name: name.clone(),
            effect,
            network: false,
            verb: None,
        });
    }
    let prefix = stack.tool_prefix.as_deref().unwrap_or("");
    for verb in &role.verbs {
        let spec = stack
            .verbs
            .get(verb)
            .ok_or_else(|| PluginError::UnknownVerb {
                role: role.name.clone(),
                stack: stack.language.clone(),
                verb: verb.clone(),
            })?;
        within(verb, spec.effect, ceiling)?;
        let name = format!("{prefix}{verb}");
        if tools.iter().any(|t| t.name == name) {
            continue;
        }
        tools.push(RenderedTool {
            name,
            effect: spec.effect,
            network: spec.network,
            verb: Some(RenderedVerb {
                verb: verb.clone(),
                argv: spec.argv.clone(),
                timeout_secs: spec.timeout_secs,
            }),
        });
    }

    let description = match &comp.description {
        Some(own) => render_text(own, stack)?,
        None => render_text(&role.description, stack)?,
    };
    Ok(RenderedAgent {
        name: comp.name.clone(),
        description,
        system_prompt: render_text(&role.prompt, stack)?,
        dev_loop,
        model: DEFAULT_AGENT_MODEL.to_string(),
        repos: Vec::new(),
        write_paths: render_list(&role.write_globs, stack)?,
        read_paths: render_list(&role.read_globs, stack)?,
        protected: stack.protected.clone(),
        max_effect: ceiling,
        tools,
        limits: RenderedLimits {
            max_iterations: limits.max_iterations,
            max_wall_clock_secs: limits.max_wall_clock_secs,
            max_concurrent: limits.max_concurrent,
        },
        source: PluginSource::Curated,
        plugin_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

fn globs(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values.join(", ")
    }
}

pub fn render_capabilities(agent: &RenderedAgent) -> String {
    let mut tools: Vec<&RenderedTool> = agent.tools.iter().collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let network = tools.iter().any(|t| t.network);
    let mut out = String::new();
    out.push_str("## Capabilities\n");
    out.push_str(&format!("agent: {}\n", agent.name));
    out.push_str(&format!("max_effect: {}\n", agent.max_effect));
    out.push_str(&format!("network: {network}\n"));
    out.push_str(&format!("read: {}\n", globs(&agent.read_paths)));
    out.push_str(&format!("write: {}\n", globs(&agent.write_paths)));
    out.push_str(&format!("protected: {}\n", globs(&agent.protected)));
    out.push_str("tools:\n");
    for tool in tools {
        let reads = if reads_files(&tool.name) {
            globs(&agent.read_paths)
        } else {
            "-".to_string()
        };
        let writes = if writes_files(&tool.name) {
            globs(&agent.write_paths)
        } else {
            "-".to_string()
        };
        out.push_str(&format!(
            "- {} effect={} network={} reads=[{}] writes=[{}]",
            tool.name, tool.effect, tool.network, reads, writes
        ));
        if let Some(verb) = &tool.verb {
            out.push_str(&format!(
                " verb={} timeout_secs={} argv=`{}`",
                verb.verb,
                verb.timeout_secs,
                verb.argv.join(" ")
            ));
        }
        out.push('\n');
    }
    out
}

impl RenderedAgent {
    pub fn with_repos(mut self, repos: Vec<String>) -> Self {
        self.repos = repos;
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_provenance(
        mut self,
        source: PluginSource,
        plugin_version: impl Into<String>,
    ) -> Self {
        self.source = source;
        self.plugin_version = plugin_version.into();
        self
    }

    pub fn to_identity(&self, file: impl AsRef<Path>) -> Result<AgentIdentity, PluginError> {
        let file = file.as_ref();
        let mut tools = Vec::with_capacity(self.tools.len());
        for (index, tool) in self.tools.iter().enumerate() {
            let pattern =
                ToolPattern::new(&tool.name).map_err(|e| IdentityError::InvalidField {
                    file: file.to_path_buf(),
                    field: format!("scope.tools[{index}]"),
                    reason: e.to_string(),
                })?;
            tools.push(pattern);
        }
        let read_paths = match self.read_paths.as_slice() {
            [only] if only == "**" => None,
            other => Some(other.to_vec()),
        };
        let prompt = format!("{}\n\n{}", self.system_prompt, render_capabilities(self));
        Ok(AgentIdentity::new(
            IdentitySection {
                name: self.name.clone(),
                description: self.description.clone(),
                dev_loop: self.dev_loop,
                model: self.model.clone(),
                system_prompt: SystemPrompt::Inline(prompt),
            },
            ScopeSection {
                repos: self.repos.clone(),
                paths: self.write_paths.clone(),
                read_paths,
                max_effect: self.max_effect,
                tools,
            },
            LimitsSection {
                max_iterations: self.limits.max_iterations as usize,
                max_wall_clock_secs: self.limits.max_wall_clock_secs,
                max_concurrent: self.limits.max_concurrent as usize,
                ..LimitsSection::default()
            },
            file,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::types::{CompositionFile, RoleFile, StackFile};

    const RUST_STACK: &str = r#"
[stack]
language = "rust"
package_manager = "cargo"
build_system = "cargo"
detect = ["Cargo.toml"]
source_globs = ["**/*.rs"]
test_globs = ["tests/**"]
protected = ["Cargo.lock"]
tool_prefix = "cargo_"

[stack.verbs.check]
argv = ["cargo", "check", "--message-format=json", "{{package}}"]
effect = "workspace"

[stack.verbs.test]
argv = ["cargo", "test"]
effect = "workspace"

[stack.verbs.publish]
argv = ["cargo", "publish"]
effect = "repository"
network = true
"#;

    const ROLE: &str = r#"
[role]
name = "implementer"
description = "Implements one change in a {{stack.language}} repository"
prompt = "You change {{stack.language}} code with {{stack.build_system}} via {{stack.package_manager}}. Check: {{stack.verbs.check}}."
tools = ["read_file", "write_file", "replace", "grep", "list_directory", "finish"]
verbs = ["check", "test"]
max_effect = "workspace"
write_globs = ["{{stack.source_globs}}", "{{stack.test_globs}}"]

[role.limits]
max_iterations = 200
max_wall_clock_secs = 43200
max_concurrent = 4
"#;

    const COMP: &str = r#"
[agent]
name = "rust-implementer"
role = "implementer"
stack = "rust-cargo"
"#;

    fn stack() -> Stack {
        toml::from_str::<StackFile>(RUST_STACK).unwrap().stack
    }
    fn role() -> Role {
        toml::from_str::<RoleFile>(ROLE).unwrap().role
    }
    fn comp() -> Composition {
        toml::from_str::<CompositionFile>(COMP).unwrap().agent
    }
    fn rendered() -> RenderedAgent {
        render_composition(&role(), &stack(), &comp()).unwrap()
    }

    #[test]
    fn string_variables_substitute_text() {
        let out = render_text(
            "{{stack.language}}/{{ stack.build_system }}/{{stack.package_manager}}",
            &stack(),
        )
        .unwrap();
        assert_eq!(out, "rust/cargo/cargo");
    }

    #[test]
    fn a_verb_variable_substitutes_its_argv_joined_by_spaces() {
        let out = render_text("{{stack.verbs.test}}", &stack()).unwrap();
        assert_eq!(out, "cargo test");
    }

    #[test]
    fn an_unknown_variable_is_an_error_naming_it() {
        let err = render_text("x {{stack.nonsense}}", &stack()).unwrap_err();
        assert!(matches!(&err, PluginError::UnknownVariable(v) if v == "stack.nonsense"));
        assert!(err.to_string().contains("stack.nonsense"));
        let err = render_text("{{stack.verbs.absent}}", &stack()).unwrap_err();
        assert!(err.to_string().contains("stack.verbs.absent"));
        let err = render_text("{{files}}", &stack()).unwrap_err();
        assert!(err.to_string().contains("files"));
    }

    #[test]
    fn lists_expand_in_list_position_into_multiple_items() {
        let mut s = stack();
        s.source_globs = vec!["src/**".into(), "build.rs".into()];
        let out = render_list(&["{{stack.source_globs}}".into(), "docs/**".into()], &s).unwrap();
        assert_eq!(out, ["src/**", "build.rs", "docs/**"]);
    }

    #[test]
    fn a_string_variable_in_list_position_is_one_item() {
        let out = render_list(&["{{stack.language}}/**".into()], &stack()).unwrap();
        assert_eq!(out, ["rust/**"]);
    }

    #[test]
    fn a_list_inside_text_is_rejected() {
        let err = render_text("globs: {{stack.source_globs}}", &stack()).unwrap_err();
        assert!(matches!(&err, PluginError::ListInText(v) if v == "stack.source_globs"));
        let err = render_list(&["pre-{{stack.source_globs}}".into()], &stack()).unwrap_err();
        assert!(matches!(err, PluginError::ListInText(_)));
    }

    #[test]
    fn rendered_values_are_never_retemplated() {
        let mut s = stack();
        s.language = "{{stack.build_system}}".into();
        assert_eq!(
            render_text("{{stack.language}}", &s).unwrap(),
            "{{stack.build_system}}"
        );
    }

    #[test]
    fn an_unterminated_or_empty_placeholder_is_malformed() {
        assert!(matches!(
            render_text("{{stack.language", &stack()),
            Err(PluginError::MalformedTemplate(_))
        ));
        assert!(matches!(
            render_text("{{}}", &stack()),
            Err(PluginError::MalformedTemplate(_))
        ));
    }

    #[test]
    fn text_without_placeholders_is_unchanged() {
        assert_eq!(
            render_text("plain } { text", &stack()).unwrap(),
            "plain } { text"
        );
    }

    #[test]
    fn a_composition_renders_name_prompt_scope_and_limits() {
        let agent = rendered();
        assert_eq!(agent.name, "rust-implementer");
        assert_eq!(
            agent.description,
            "Implements one change in a rust repository"
        );
        assert_eq!(
            agent.system_prompt,
            "You change rust code with cargo via cargo. Check: cargo check --message-format=json {{package}}."
        );
        assert_eq!(agent.write_paths, ["**/*.rs", "tests/**"]);
        assert_eq!(agent.read_paths, ["**"]);
        assert_eq!(agent.max_effect, EffectClass::Workspace);
        assert_eq!(agent.limits.max_iterations, 200);
        assert_eq!(agent.limits.max_wall_clock_secs, 43200);
        assert_eq!(agent.limits.max_concurrent, 4);
        assert_eq!(agent.protected, ["Cargo.lock"]);
        assert!(agent.repos.is_empty());
    }

    #[test]
    fn tools_are_the_role_tools_plus_prefixed_stack_verbs() {
        let agent = rendered();
        let names: Vec<&str> = agent.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "read_file",
                "write_file",
                "replace",
                "grep",
                "list_directory",
                "finish",
                "cargo_check",
                "cargo_test"
            ]
        );
        let check = agent
            .tools
            .iter()
            .find(|t| t.name == "cargo_check")
            .unwrap();
        assert_eq!(check.verb.as_ref().unwrap().verb, "check");
        assert_eq!(check.effect, EffectClass::Workspace);
    }

    #[test]
    fn verb_names_are_unprefixed_without_a_tool_prefix() {
        let mut s = stack();
        s.tool_prefix = None;
        let agent = render_composition(&role(), &s, &comp()).unwrap();
        assert!(agent.tools.iter().any(|t| t.name == "check"));
    }

    #[test]
    fn a_role_verb_missing_from_the_stack_is_an_error() {
        let mut r = role();
        r.verbs.push("deploy".into());
        let err = render_composition(&r, &stack(), &comp()).unwrap_err();
        assert!(matches!(&err, PluginError::UnknownVerb { verb, .. } if verb == "deploy"));
    }

    #[test]
    fn a_verb_above_the_ceiling_is_an_error() {
        let mut r = role();
        r.verbs.push("publish".into());
        let err = render_composition(&r, &stack(), &comp()).unwrap_err();
        assert!(matches!(&err, PluginError::VerbExceedsCeiling { verb, .. } if verb == "publish"));
    }

    #[test]
    fn max_effect_is_the_minimum_of_role_and_composition() {
        let mut c = comp();
        c.max_effect = Some(EffectClass::None);
        let err = render_composition(&role(), &stack(), &c).unwrap_err();
        assert!(matches!(err, PluginError::VerbExceedsCeiling { .. }));
        let mut r = role();
        r.verbs.clear();
        r.tools.retain(|t| t != "write_file" && t != "replace");
        let agent = render_composition(&r, &stack(), &c).unwrap();
        assert_eq!(agent.max_effect, EffectClass::None);
        c.max_effect = Some(EffectClass::Production);
        let agent = render_composition(&role(), &stack(), &c).unwrap();
        assert_eq!(agent.max_effect, EffectClass::Workspace);
    }

    #[test]
    fn composition_limits_and_description_override_the_role() {
        let mut c = comp();
        c.description = Some("custom".into());
        c.limits = Some(crate::plugins::types::RoleLimits {
            max_iterations: 5,
            max_wall_clock_secs: 60,
            max_concurrent: 1,
            max_inactivity_secs: None,
        });
        let agent = render_composition(&role(), &stack(), &c).unwrap();
        assert_eq!(agent.description, "custom");
        assert_eq!(agent.limits.max_iterations, 5);
    }

    #[test]
    fn no_limits_anywhere_is_an_error() {
        let mut r = role();
        r.limits = None;
        let err = render_composition(&r, &stack(), &comp()).unwrap_err();
        assert!(matches!(err, PluginError::MissingLimits(_)));
    }

    #[test]
    fn the_role_loop_defaults_to_inner_and_must_parse() {
        assert_eq!(rendered().dev_loop, DevLoop::Inner);
        let mut r = role();
        r.dev_loop = Some("middle".into());
        assert_eq!(
            render_composition(&r, &stack(), &comp()).unwrap().dev_loop,
            DevLoop::Middle
        );
        r.dev_loop = Some("sideways".into());
        assert!(matches!(
            render_composition(&r, &stack(), &comp()),
            Err(PluginError::UnknownLoop { .. })
        ));
    }

    #[test]
    fn the_capability_block_is_byte_stable_and_lists_every_tool() {
        let agent = rendered();
        let block = render_capabilities(&agent);
        assert_eq!(block, render_capabilities(&agent.clone()));
        for name in [
            "read_file",
            "write_file",
            "cargo_check",
            "cargo_test",
            "finish",
        ] {
            assert!(block.contains(name), "{block}");
        }
        assert!(
            block.contains("cargo check --message-format=json {{package}}"),
            "{block}"
        );
        assert!(block.contains("**/*.rs, tests/**"), "{block}");
        assert!(block.contains("max_effect: workspace"), "{block}");
        assert!(block.contains("network=false"), "{block}");
    }

    #[test]
    fn the_capability_block_is_independent_of_tool_declaration_order() {
        let a = rendered();
        let mut b = a.clone();
        b.tools.reverse();
        assert_eq!(render_capabilities(&a), render_capabilities(&b));
    }

    #[test]
    fn a_rendered_agent_becomes_a_validated_identity() {
        let agent = rendered().with_repos(vec!["github.com/example/repo".into()]);
        let identity = agent.to_identity("curated:rust-implementer").unwrap();
        assert_eq!(identity.name(), "rust-implementer");
        assert_eq!(identity.scope.max_effect, EffectClass::Workspace);
        assert_eq!(identity.scope.repos, ["github.com/example/repo"]);
        assert_eq!(identity.scope.paths, ["**/*.rs", "tests/**"]);
        assert!(identity.scope.read_paths.is_none());
        assert!(identity.allows_tool("cargo_check"));
        assert!(!identity.allows_tool("cargo_publish"));
        assert!(!identity.allows_tool("run_command"));
        assert_eq!(identity.limits.max_iterations, 200);
        let prompt = identity.system_prompt_text().unwrap();
        assert!(prompt.starts_with("You change rust code"));
        assert!(prompt.contains(&render_capabilities(&agent)));
    }

    #[test]
    fn a_restricted_read_set_is_kept() {
        let mut agent = rendered();
        agent.read_paths = vec!["src/**".into()];
        let identity = agent.to_identity("x").unwrap();
        assert_eq!(identity.scope.read_paths, Some(vec!["src/**".to_string()]));
    }

    #[test]
    fn an_invalid_name_fails_identity_validation() {
        let mut agent = rendered();
        agent.name = "bad name".into();
        assert!(matches!(
            agent.to_identity("x"),
            Err(PluginError::Identity(_))
        ));
    }
}
