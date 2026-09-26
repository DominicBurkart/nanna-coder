//! The `AgentIdentity` schema: TOML parsing, field-level validation and
//! system-prompt resolution.

use super::{DevLoop, IdentityError, ToolPattern};
use crate::effects::EffectClass;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// A validated agent identity together with the file it was parsed from.
///
/// Field names mirror the TOML tables; see the [module docs](super) for the
/// file format. Serialising an identity yields TOML that parses back to an
/// equal value, which [`AgentIdentity::to_toml_string`] relies on.
///
/// ```
/// use harness::effects::EffectClass;
/// use harness::identity::{AgentIdentity, DevLoop};
///
/// let toml = r#"
/// [identity]
/// name = "rust-implementer"
/// description = "Implements a scoped issue in a Rust workspace and opens a draft PR."
/// loop = "inner"
/// model = "gemma4:e4b"
/// system_prompt = { inline = "You implement one issue at a time." }
///
/// [scope]
/// repos = ["github.com/example/repo"]
/// paths = ["api/**", "shared/**"]
/// max_effect = "repository"
/// tools = ["read_file", "write_file", "search", "cargo_*", "git_*"]
///
/// [limits]
/// max_iterations = 200
/// max_wall_clock_secs = 3600
/// max_concurrent = 4
/// "#;
///
/// let identity = AgentIdentity::from_toml_str(toml, "rust-implementer.toml").unwrap();
/// assert_eq!(identity.name(), "rust-implementer");
/// assert_eq!(identity.identity.dev_loop, DevLoop::Inner);
/// assert_eq!(identity.scope.max_effect, EffectClass::Repository);
/// assert!(identity.allows_tool("cargo_check"));
/// assert!(!identity.allows_tool("github_pr_status"));
/// assert!(identity.allows_effect(EffectClass::Workspace));
/// assert!(!identity.allows_effect(EffectClass::Ci));
/// assert_eq!(identity.system_prompt_text().unwrap(), "You implement one issue at a time.");
///
/// let invalid = toml.replace("loop = \"inner\"", "loop = \"sideways\"");
/// let err = AgentIdentity::from_toml_str(&invalid, "rust-implementer.toml").unwrap_err();
/// assert!(err.to_string().contains("identity.loop"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(into = "RawIdentity")]
pub struct AgentIdentity {
    /// The `[identity]` table.
    pub identity: IdentitySection,
    /// The `[scope]` table.
    pub scope: ScopeSection,
    /// The `[limits]` table.
    pub limits: LimitsSection,
    source: PathBuf,
}

/// The `[identity]` table: who the agent is and what it runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentitySection {
    /// Catalog key; ASCII letters, digits, `-` and `_` only.
    pub name: String,
    /// One-line summary the orchestrator uses when choosing a card.
    pub description: String,
    /// SDLC stage the identity acts in (`loop` in TOML).
    pub dev_loop: DevLoop,
    /// Model name or provider/model reference.
    pub model: String,
    /// Inline prompt text or a path relative to the identity file.
    pub system_prompt: SystemPrompt,
}

/// The `system_prompt` value: a bare string is a path relative to the
/// identity file, `{ inline = "..." }` is literal prompt text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemPrompt {
    /// Literal prompt text.
    Inline(String),
    /// Relative path to a prompt file inside the identity directory.
    Path(PathBuf),
}

/// The `[scope]` table: what the agent may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSection {
    /// Repositories the identity may be spawned against.
    pub repos: Vec<String>,
    /// Writable globs relative to the worktree root.
    pub paths: Vec<String>,
    /// Readable globs; `None` leaves reads unrestricted inside the worktree.
    pub read_paths: Option<Vec<String>>,
    /// Highest [`EffectClass`] any tool call may reach.
    pub max_effect: EffectClass,
    /// Tool-name patterns the identity may call.
    pub tools: Vec<ToolPattern>,
}

/// The `[limits]` table: resource ceilings per spawned agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSection {
    /// Maximum agent-loop iterations.
    pub max_iterations: usize,
    /// Maximum wall-clock seconds per run.
    pub max_wall_clock_secs: u64,
    /// Maximum concurrently running agents with this identity.
    pub max_concurrent: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIdentity {
    identity: RawIdentitySection,
    scope: RawScopeSection,
    limits: LimitsSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIdentitySection {
    name: String,
    description: String,
    #[serde(rename = "loop")]
    dev_loop: String,
    model: String,
    system_prompt: RawSystemPrompt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum RawSystemPrompt {
    Path(String),
    PathTable { path: String },
    Inline { inline: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScopeSection {
    repos: Vec<String>,
    paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    read_paths: Option<Vec<String>>,
    max_effect: String,
    tools: Vec<String>,
}

impl From<AgentIdentity> for RawIdentity {
    fn from(identity: AgentIdentity) -> Self {
        let system_prompt = match identity.identity.system_prompt {
            SystemPrompt::Inline(inline) => RawSystemPrompt::Inline { inline },
            SystemPrompt::Path(path) => RawSystemPrompt::Path(path.to_string_lossy().into_owned()),
        };
        RawIdentity {
            identity: RawIdentitySection {
                name: identity.identity.name,
                description: identity.identity.description,
                dev_loop: identity.identity.dev_loop.as_str().to_string(),
                model: identity.identity.model,
                system_prompt,
            },
            scope: RawScopeSection {
                repos: identity.scope.repos,
                paths: identity.scope.paths,
                read_paths: identity.scope.read_paths,
                max_effect: identity.scope.max_effect.as_str().to_string(),
                tools: identity
                    .scope
                    .tools
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            },
            limits: identity.limits,
        }
    }
}

struct Validator<'a> {
    file: &'a Path,
}

impl Validator<'_> {
    fn invalid(&self, field: impl Into<String>, reason: impl Into<String>) -> IdentityError {
        IdentityError::InvalidField {
            file: self.file.to_path_buf(),
            field: field.into(),
            reason: reason.into(),
        }
    }

    fn non_empty(&self, field: &str, value: &str) -> Result<(), IdentityError> {
        if value.trim().is_empty() {
            return Err(self.invalid(field, "must not be empty"));
        }
        Ok(())
    }

    fn name(&self, value: &str) -> Result<(), IdentityError> {
        self.non_empty("identity.name", value)?;
        let allowed = |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_');
        match value.chars().find(|ch| !allowed(*ch)) {
            Some(ch) => Err(self.invalid(
                "identity.name",
                format!("contains {ch:?}; allowed: ASCII letters, digits, `-`, `_`"),
            )),
            None => Ok(()),
        }
    }

    fn relative_inside(&self, field: &str, value: &str) -> Result<(), IdentityError> {
        self.non_empty(field, value)?;
        let path = Path::new(value);
        for component in path.components() {
            match component {
                Component::RootDir | Component::Prefix(_) => {
                    return Err(
                        self.invalid(field, format!("`{value}` is absolute; paths are relative"))
                    );
                }
                Component::ParentDir => {
                    return Err(self.invalid(
                        field,
                        format!("`{value}` contains `..`, which would escape the directory"),
                    ));
                }
                Component::Normal(_) | Component::CurDir => {}
            }
        }
        Ok(())
    }

    fn path_globs(&self, field: &str, values: &[String]) -> Result<(), IdentityError> {
        for (index, value) in values.iter().enumerate() {
            let field = format!("{field}[{index}]");
            self.relative_inside(&field, value)?;
            glob::Pattern::new(value).map_err(|e| {
                self.invalid(&field, format!("`{value}` is not a valid glob: {}", e.msg))
            })?;
        }
        Ok(())
    }

    fn repos(&self, values: &[String]) -> Result<(), IdentityError> {
        for (index, value) in values.iter().enumerate() {
            let field = format!("scope.repos[{index}]");
            self.non_empty(&field, value)?;
            if value.chars().any(char::is_whitespace) {
                return Err(self.invalid(field, format!("`{value}` contains whitespace")));
            }
        }
        Ok(())
    }

    fn tools(&self, values: &[String]) -> Result<Vec<ToolPattern>, IdentityError> {
        let mut patterns = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            let pattern = ToolPattern::new(value)
                .map_err(|e| self.invalid(format!("scope.tools[{index}]"), e.to_string()))?;
            patterns.push(pattern);
        }
        Ok(patterns)
    }

    fn system_prompt(&self, raw: RawSystemPrompt) -> Result<SystemPrompt, IdentityError> {
        const FIELD: &str = "identity.system_prompt";
        match raw {
            RawSystemPrompt::Inline { inline } => {
                self.non_empty(FIELD, &inline)?;
                Ok(SystemPrompt::Inline(inline))
            }
            RawSystemPrompt::Path(path) | RawSystemPrompt::PathTable { path } => {
                self.relative_inside(FIELD, &path)?;
                Ok(SystemPrompt::Path(PathBuf::from(path)))
            }
        }
    }

    fn positive(&self, field: &str, value: u64) -> Result<(), IdentityError> {
        if value == 0 {
            return Err(self.invalid(field, "must be greater than zero"));
        }
        Ok(())
    }

    fn validate(&self, raw: RawIdentity) -> Result<AgentIdentity, IdentityError> {
        self.name(&raw.identity.name)?;
        self.non_empty("identity.description", &raw.identity.description)?;
        let dev_loop = raw
            .identity
            .dev_loop
            .parse::<DevLoop>()
            .map_err(|e| self.invalid("identity.loop", e.to_string()))?;
        self.non_empty("identity.model", &raw.identity.model)?;
        let system_prompt = self.system_prompt(raw.identity.system_prompt)?;

        self.repos(&raw.scope.repos)?;
        self.path_globs("scope.paths", &raw.scope.paths)?;
        if let Some(read_paths) = &raw.scope.read_paths {
            self.path_globs("scope.read_paths", read_paths)?;
        }
        let max_effect = raw.scope.max_effect.parse::<EffectClass>().map_err(|e| {
            self.invalid(
                "scope.max_effect",
                format!("{e}; expected one of {}", effect_names()),
            )
        })?;
        let tools = self.tools(&raw.scope.tools)?;

        self.positive("limits.max_iterations", raw.limits.max_iterations as u64)?;
        self.positive("limits.max_wall_clock_secs", raw.limits.max_wall_clock_secs)?;
        self.positive("limits.max_concurrent", raw.limits.max_concurrent as u64)?;
        let source = self.file.to_path_buf();

        Ok(AgentIdentity {
            identity: IdentitySection {
                name: raw.identity.name,
                description: raw.identity.description,
                dev_loop,
                model: raw.identity.model,
                system_prompt,
            },
            scope: ScopeSection {
                repos: raw.scope.repos,
                paths: raw.scope.paths,
                read_paths: raw.scope.read_paths,
                max_effect,
                tools,
            },
            limits: raw.limits,
            source,
        })
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> IdentityError + '_ {
    move |source| IdentityError::Io {
        file: path.to_path_buf(),
        source,
    }
}

fn effect_names() -> String {
    EffectClass::ALL
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

impl AgentIdentity {
    /// Parse and validate identity TOML. `file` labels errors and anchors a
    /// relative `system_prompt` path; it does not have to exist.
    ///
    /// Field-level problems are reported as
    /// [`IdentityError::InvalidField`] naming the TOML key (`identity.loop`,
    /// `scope.tools[3]`, ...); malformed TOML and unknown keys are
    /// [`IdentityError::Parse`].
    pub fn from_toml_str(src: &str, file: impl AsRef<Path>) -> Result<Self, IdentityError> {
        let file = file.as_ref();
        let raw: RawIdentity = toml::from_str(src).map_err(|e| IdentityError::Parse {
            file: file.to_path_buf(),
            message: e.to_string(),
        })?;
        Validator { file }.validate(raw)
    }

    /// Read, parse and validate an identity file, and resolve its system
    /// prompt so that a missing prompt file or one outside the identity
    /// directory is rejected at load time.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, IdentityError> {
        let path = path.as_ref();
        let src = fs::read_to_string(path).map_err(io(path))?;
        let identity = Self::from_toml_str(&src, path)?;
        identity.system_prompt_text()?;
        Ok(identity)
    }

    /// Construct an identity from already-typed sections, running the same
    /// validation as [`AgentIdentity::from_toml_str`].
    pub fn new(
        identity: IdentitySection,
        scope: ScopeSection,
        limits: LimitsSection,
        file: impl AsRef<Path>,
    ) -> Result<Self, IdentityError> {
        let file = file.as_ref();
        let source = file.to_path_buf();
        let typed = AgentIdentity {
            identity,
            scope,
            limits,
            source,
        };
        Validator { file }.validate(RawIdentity::from(typed))
    }

    /// Catalog key (`identity.name`).
    pub fn name(&self) -> &str {
        &self.identity.name
    }

    /// The file this identity was parsed from, as given to the constructor.
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Directory that anchors a relative `system_prompt` path.
    pub fn source_dir(&self) -> &Path {
        match self.source.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        }
    }

    /// Whether `tool_name` matches any `scope.tools` pattern.
    pub fn allows_tool(&self, tool_name: &str) -> bool {
        self.scope
            .tools
            .iter()
            .any(|pattern| pattern.matches(tool_name))
    }

    /// Whether `class` is at or below `scope.max_effect`.
    pub fn allows_effect(&self, class: EffectClass) -> bool {
        class <= self.scope.max_effect
    }

    /// The system prompt text: inline text as written, or the contents of
    /// the prompt file. A file path must canonicalise to a location inside
    /// the identity directory, so symlinks cannot escape it either.
    pub fn system_prompt_text(&self) -> Result<String, IdentityError> {
        let relative = match &self.identity.system_prompt {
            SystemPrompt::Inline(text) => return Ok(text.clone()),
            SystemPrompt::Path(path) => path,
        };
        let dir = self
            .source_dir()
            .canonicalize()
            .map_err(io(self.source_dir()))?;
        let prompt_path = self.source_dir().join(relative);
        let resolved = prompt_path.canonicalize().map_err(io(&prompt_path))?;
        if !resolved.starts_with(&dir) {
            return Err(IdentityError::PromptOutsideDirectory {
                file: self.source.clone(),
                path: relative.clone(),
                dir,
            });
        }
        fs::read_to_string(&resolved).map_err(io(&resolved))
    }

    /// Serialise back to TOML that [`AgentIdentity::from_toml_str`] accepts.
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string(self)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    pub(crate) const EXAMPLE: &str = r#"
[identity]
name = "rust-implementer"
description = "Implements a scoped issue in a Rust workspace and opens a draft PR."
loop = "inner"
model = "gemma4:e4b"
system_prompt = "prompts/rust-implementer.md"

[scope]
repos = ["github.com/example/repo"]
paths = ["api/**", "shared/**"]
max_effect = "repository"
tools = ["read_file", "write_file", "search", "cargo_*", "git_*"]

[limits]
max_iterations = 200
max_wall_clock_secs = 3600
max_concurrent = 4
"#;

    pub(crate) fn example() -> AgentIdentity {
        AgentIdentity::from_toml_str(EXAMPLE, "agents/rust-implementer.toml").unwrap()
    }

    fn with(replace: &str, by: &str) -> Result<AgentIdentity, IdentityError> {
        assert!(EXAMPLE.contains(replace), "{replace} not in EXAMPLE");
        AgentIdentity::from_toml_str(
            &EXAMPLE.replace(replace, by),
            "agents/rust-implementer.toml",
        )
    }

    fn invalid_field(result: Result<AgentIdentity, IdentityError>) -> (String, String) {
        match result {
            Err(IdentityError::InvalidField {
                file,
                field,
                reason,
            }) => {
                assert_eq!(file, PathBuf::from("agents/rust-implementer.toml"));
                (field, reason)
            }
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }

    #[test]
    fn parses_the_issue_example() {
        let identity = example();
        assert_eq!(identity.name(), "rust-implementer");
        assert_eq!(
            identity.identity.description,
            "Implements a scoped issue in a Rust workspace and opens a draft PR."
        );
        assert_eq!(identity.identity.dev_loop, DevLoop::Inner);
        assert_eq!(identity.identity.model, "gemma4:e4b");
        assert_eq!(
            identity.identity.system_prompt,
            SystemPrompt::Path(PathBuf::from("prompts/rust-implementer.md"))
        );
        assert_eq!(identity.scope.repos, vec!["github.com/example/repo"]);
        assert_eq!(identity.scope.paths, vec!["api/**", "shared/**"]);
        assert_eq!(identity.scope.read_paths, None);
        assert_eq!(identity.scope.max_effect, EffectClass::Repository);
        let tools: Vec<&str> = identity
            .scope
            .tools
            .iter()
            .map(ToolPattern::as_str)
            .collect();
        assert_eq!(
            tools,
            vec!["read_file", "write_file", "search", "cargo_*", "git_*"]
        );
        assert_eq!(
            identity.limits,
            LimitsSection {
                max_iterations: 200,
                max_wall_clock_secs: 3600,
                max_concurrent: 4
            }
        );
        assert_eq!(identity.source(), Path::new("agents/rust-implementer.toml"));
        assert_eq!(identity.source_dir(), Path::new("agents"));
    }

    #[test]
    fn source_dir_of_a_bare_file_name_is_the_current_directory() {
        let identity = AgentIdentity::from_toml_str(EXAMPLE, "rust-implementer.toml").unwrap();
        assert_eq!(identity.source_dir(), Path::new("."));
    }

    #[test]
    fn allows_tool_follows_scope_patterns() {
        let identity = example();
        for allowed in [
            "read_file",
            "write_file",
            "search",
            "cargo_check",
            "cargo_deny",
            "git_status",
            "git_diff",
        ] {
            assert!(identity.allows_tool(allowed), "{allowed}");
        }
        for denied in [
            "run_command",
            "github_pr_status",
            "list_directory",
            "cargo",
            "echo",
        ] {
            assert!(!identity.allows_tool(denied), "{denied}");
        }
    }

    #[test]
    fn allows_effect_is_bounded_by_max_effect() {
        let identity = example();
        for class in EffectClass::ALL {
            assert_eq!(
                identity.allows_effect(class),
                class <= EffectClass::Repository,
                "{class}"
            );
        }
    }

    #[test]
    fn inline_and_table_forms_of_system_prompt_are_accepted() {
        let inline = with(
            "system_prompt = \"prompts/rust-implementer.md\"",
            "system_prompt = { inline = \"Be brief.\" }",
        )
        .unwrap();
        assert_eq!(
            inline.identity.system_prompt,
            SystemPrompt::Inline("Be brief.".to_string())
        );
        assert_eq!(inline.system_prompt_text().unwrap(), "Be brief.");
        let table = with(
            "system_prompt = \"prompts/rust-implementer.md\"",
            "system_prompt = { path = \"p.md\" }",
        )
        .unwrap();
        assert_eq!(
            table.identity.system_prompt,
            SystemPrompt::Path(PathBuf::from("p.md"))
        );
    }

    #[test]
    fn read_paths_are_optional_and_validated() {
        let identity = with(
            "paths = [\"api/**\", \"shared/**\"]",
            "paths = [\"api/**\"]\nread_paths = [\"docs/**\"]",
        )
        .unwrap();
        assert_eq!(identity.scope.read_paths, Some(vec!["docs/**".to_string()]));
        let (field, reason) = invalid_field(with(
            "paths = [\"api/**\", \"shared/**\"]",
            "paths = [\"api/**\"]\nread_paths = [\"docs/**\", \"../secrets\"]",
        ));
        assert_eq!(field, "scope.read_paths[1]");
        assert!(reason.contains(".."), "{reason}");
    }

    #[test]
    fn malformed_toml_and_unknown_keys_are_parse_errors() {
        for src in [
            "not toml at all = = =",
            &format!("{EXAMPLE}\n[extra]\nx = 1"),
            &EXAMPLE.replace("max_concurrent", "max_concurrency"),
            &EXAMPLE.replace("[limits]", "[limitz]"),
        ] {
            match AgentIdentity::from_toml_str(src, "agents/x.toml") {
                Err(IdentityError::Parse { file, message }) => {
                    assert_eq!(file, PathBuf::from("agents/x.toml"));
                    assert!(!message.is_empty());
                }
                other => panic!("expected Parse error, got {other:?}"),
            }
        }
        let err = AgentIdentity::from_toml_str("", "agents/x.toml").unwrap_err();
        assert!(err.to_string().starts_with("agents/x.toml: "), "{err}");
    }

    #[test]
    fn unsupported_system_prompt_shape_is_a_parse_error() {
        let result = with(
            "system_prompt = \"prompts/rust-implementer.md\"",
            "system_prompt = 42",
        );
        assert!(
            matches!(result, Err(IdentityError::Parse { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn unknown_loop_names_the_field() {
        let (field, reason) = invalid_field(with("loop = \"inner\"", "loop = \"sideways\""));
        assert_eq!(field, "identity.loop");
        assert_eq!(
            reason,
            "unknown loop `sideways`; expected one of inner, middle, outer"
        );
    }

    #[test]
    fn unknown_max_effect_names_the_field() {
        let (field, reason) = invalid_field(with(
            "max_effect = \"repository\"",
            "max_effect = \"galaxy\"",
        ));
        assert_eq!(field, "scope.max_effect");
        assert_eq!(reason, "unknown effect class: galaxy; expected one of none, workspace, repository, ci, sandbox, production");
    }

    #[test]
    fn empty_or_malformed_names_are_rejected() {
        let (field, reason) = invalid_field(with("name = \"rust-implementer\"", "name = \"\""));
        assert_eq!(
            (field.as_str(), reason.as_str()),
            ("identity.name", "must not be empty")
        );
        let (field, reason) = invalid_field(with("name = \"rust-implementer\"", "name = \"   \""));
        assert_eq!(
            (field.as_str(), reason.as_str()),
            ("identity.name", "must not be empty")
        );
        let (field, reason) = invalid_field(with(
            "name = \"rust-implementer\"",
            "name = \"rust implementer\"",
        ));
        assert_eq!(field, "identity.name");
        assert!(reason.contains("' '"), "{reason}");
        let (field, _) = invalid_field(with("name = \"rust-implementer\"", "name = \"a/b\""));
        assert_eq!(field, "identity.name");
    }

    #[test]
    fn empty_description_and_model_are_rejected() {
        let (field, _) = invalid_field(with(
            "description = \"Implements a scoped issue in a Rust workspace and opens a draft PR.\"",
            "description = \"\"",
        ));
        assert_eq!(field, "identity.description");
        let (field, _) = invalid_field(with("model = \"gemma4:e4b\"", "model = \" \""));
        assert_eq!(field, "identity.model");
    }

    #[test]
    fn system_prompt_paths_that_escape_the_directory_are_rejected() {
        for escaping in ["../other/prompt.md", "prompts/../../x.md", "/etc/passwd"] {
            let (field, reason) = invalid_field(with(
                "system_prompt = \"prompts/rust-implementer.md\"",
                &format!("system_prompt = \"{escaping}\""),
            ));
            assert_eq!(field, "identity.system_prompt", "{escaping}");
            assert!(reason.contains(escaping), "{reason}");
        }
        let (field, _) = invalid_field(with(
            "system_prompt = \"prompts/rust-implementer.md\"",
            "system_prompt = \"\"",
        ));
        assert_eq!(field, "identity.system_prompt");
        let (field, _) = invalid_field(with(
            "system_prompt = \"prompts/rust-implementer.md\"",
            "system_prompt = { inline = \"\" }",
        ));
        assert_eq!(field, "identity.system_prompt");
    }

    #[test]
    fn bad_path_globs_are_rejected_with_index() {
        let (field, reason) = invalid_field(with(
            "paths = [\"api/**\", \"shared/**\"]",
            "paths = [\"api/**\", \"shared/[\"]",
        ));
        assert_eq!(field, "scope.paths[1]");
        assert!(reason.contains("not a valid glob"), "{reason}");
        let (field, reason) = invalid_field(with(
            "paths = [\"api/**\", \"shared/**\"]",
            "paths = [\"/abs/**\"]",
        ));
        assert_eq!(field, "scope.paths[0]");
        assert!(reason.contains("absolute"), "{reason}");
        let (field, _) = invalid_field(with(
            "paths = [\"api/**\", \"shared/**\"]",
            "paths = [\"\"]",
        ));
        assert_eq!(field, "scope.paths[0]");
    }

    #[test]
    fn bad_repos_are_rejected_with_index() {
        let (field, _) = invalid_field(with(
            "repos = [\"github.com/example/repo\"]",
            "repos = [\"github.com/example/repo\", \"\"]",
        ));
        assert_eq!(field, "scope.repos[1]");
        let (field, reason) = invalid_field(with(
            "repos = [\"github.com/example/repo\"]",
            "repos = [\"github.com/example repo\"]",
        ));
        assert_eq!(field, "scope.repos[0]");
        assert!(reason.contains("whitespace"), "{reason}");
    }

    #[test]
    fn bad_tool_patterns_are_rejected_with_index() {
        let (field, reason) = invalid_field(with(
            "tools = [\"read_file\", \"write_file\", \"search\", \"cargo_*\", \"git_*\"]",
            "tools = [\"read_file\", \"cargo [\"]",
        ));
        assert_eq!(field, "scope.tools[1]");
        assert!(reason.contains("cargo ["), "{reason}");
        let (field, reason) = invalid_field(with(
            "tools = [\"read_file\", \"write_file\", \"search\", \"cargo_*\", \"git_*\"]",
            "tools = [\"\"]",
        ));
        assert_eq!(field, "scope.tools[0]");
        assert_eq!(reason, "tool pattern is empty");
    }

    #[test]
    fn zero_limits_are_rejected() {
        for (key, field) in [
            ("max_iterations = 200", "limits.max_iterations"),
            ("max_wall_clock_secs = 3600", "limits.max_wall_clock_secs"),
            ("max_concurrent = 4", "limits.max_concurrent"),
        ] {
            let zeroed = format!("{} = 0", key.split(' ').next().unwrap());
            let (got, reason) = invalid_field(with(key, &zeroed));
            assert_eq!(got, field);
            assert_eq!(reason, "must be greater than zero");
        }
        assert!(matches!(
            with("max_concurrent = 4", "max_concurrent = -1"),
            Err(IdentityError::Parse { .. })
        ));
    }

    #[test]
    fn error_display_names_file_field_and_reason() {
        let err = with("loop = \"inner\"", "loop = \"x\"").unwrap_err();
        assert_eq!(err.to_string(), "agents/rust-implementer.toml: invalid `identity.loop`: unknown loop `x`; expected one of inner, middle, outer");
    }

    #[test]
    fn new_runs_validation() {
        let example = example();
        let rebuilt = AgentIdentity::new(
            example.identity.clone(),
            example.scope.clone(),
            example.limits,
            example.source(),
        )
        .unwrap();
        assert_eq!(rebuilt, example);
        let mut limits = example.limits;
        limits.max_concurrent = 0;
        let err = AgentIdentity::new(
            example.identity.clone(),
            example.scope.clone(),
            limits,
            "x.toml",
        )
        .unwrap_err();
        assert!(
            matches!(err, IdentityError::InvalidField { ref field, .. } if field == "limits.max_concurrent"),
            "{err}"
        );
    }

    #[test]
    fn to_toml_string_round_trips_the_example() {
        let identity = example();
        let toml = identity.to_toml_string().unwrap();
        assert!(toml.contains("loop = \"inner\""), "{toml}");
        assert!(toml.contains("max_effect = \"repository\""), "{toml}");
        assert!(!toml.contains("read_paths"), "{toml}");
        let back = AgentIdentity::from_toml_str(&toml, identity.source()).unwrap();
        assert_eq!(back, identity);
    }

    #[test]
    fn load_reads_file_and_resolves_prompt() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("prompts")).unwrap();
        fs::write(
            dir.path().join("prompts/rust-implementer.md"),
            "# Implementer\n",
        )
        .unwrap();
        let file = dir.path().join("rust-implementer.toml");
        fs::write(&file, EXAMPLE).unwrap();
        let identity = AgentIdentity::load(&file).unwrap();
        assert_eq!(identity.source(), file);
        assert_eq!(identity.system_prompt_text().unwrap(), "# Implementer\n");
    }

    #[test]
    fn load_reports_missing_identity_file_and_missing_prompt_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.toml");
        match AgentIdentity::load(&missing) {
            Err(IdentityError::Io { file, source }) => {
                assert_eq!(file, missing);
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io error, got {other:?}"),
        }
        let file = dir.path().join("rust-implementer.toml");
        fs::write(&file, EXAMPLE).unwrap();
        match AgentIdentity::load(&file) {
            Err(IdentityError::Io { file, .. }) => {
                assert_eq!(file, dir.path().join("prompts/rust-implementer.md"))
            }
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    #[test]
    fn load_propagates_validation_errors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.toml");
        fs::write(&file, EXAMPLE.replace("loop = \"inner\"", "loop = \"x\"")).unwrap();
        let err = AgentIdentity::load(&file).unwrap_err();
        assert!(
            matches!(err, IdentityError::InvalidField { ref field, .. } if field == "identity.loop"),
            "{err}"
        );
    }

    #[test]
    fn prompt_text_fails_when_identity_directory_is_missing() {
        let identity =
            AgentIdentity::from_toml_str(EXAMPLE, "/nonexistent-dir-for-identity-test/x.toml")
                .unwrap();
        match identity.system_prompt_text() {
            Err(IdentityError::Io { file, .. }) => {
                assert_eq!(file, PathBuf::from("/nonexistent-dir-for-identity-test"))
            }
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_prompt_outside_directory_is_rejected() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.md"), "secret").unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("prompts")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.md"),
            dir.path().join("prompts/rust-implementer.md"),
        )
        .unwrap();
        let file = dir.path().join("rust-implementer.toml");
        fs::write(&file, EXAMPLE).unwrap();
        match AgentIdentity::load(&file) {
            Err(IdentityError::PromptOutsideDirectory {
                file: reported,
                path,
                dir: reported_dir,
            }) => {
                assert_eq!(reported, file);
                assert_eq!(path, PathBuf::from("prompts/rust-implementer.md"));
                assert_eq!(reported_dir, dir.path().canonicalize().unwrap());
            }
            other => panic!("expected PromptOutsideDirectory, got {other:?}"),
        }
    }

    pub(crate) fn name_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9][a-zA-Z0-9_-]{0,20}"
    }

    pub(crate) fn text_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9 ,.!?'\"\\\\:/#\n\t-]{1,60}".prop_filter("non-blank", |s| !s.trim().is_empty())
    }

    pub(crate) fn path_glob_strategy() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_.-]{0,8}(/([a-z0-9][a-z0-9_.?-]{0,7}|\\*|\\*\\*|[a-z]{1,4}\\*)){0,3}"
    }

    pub(crate) fn repo_strategy() -> impl Strategy<Value = String> {
        "github\\.com/[a-z][a-z0-9-]{0,10}/[a-z][a-z0-9-]{0,10}"
    }

    pub(crate) fn tool_pattern_strategy() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_]{0,10}\\*?"
    }

    pub(crate) fn unique_list(
        item: impl Strategy<Value = String>,
        max: usize,
    ) -> impl Strategy<Value = Vec<String>> {
        prop::collection::btree_set(item, 0..=max)
            .prop_map(|set: BTreeSet<String>| set.into_iter().collect())
    }

    pub(crate) fn any_identity() -> impl Strategy<Value = AgentIdentity> {
        let system_prompt = prop_oneof![
            text_strategy().prop_map(SystemPrompt::Inline),
            path_glob_strategy().prop_map(|p| SystemPrompt::Path(PathBuf::from(p)))
        ];
        let identity = (
            name_strategy(),
            text_strategy(),
            prop::sample::select(DevLoop::ALL.to_vec()),
            "[a-z0-9:./-]{1,20}",
            system_prompt,
        )
            .prop_map(|(name, description, dev_loop, model, system_prompt)| {
                IdentitySection {
                    name,
                    description,
                    dev_loop,
                    model,
                    system_prompt,
                }
            });
        let scope = (
            unique_list(repo_strategy(), 3),
            unique_list(path_glob_strategy(), 4),
            prop::option::of(unique_list(path_glob_strategy(), 4)),
            prop::sample::select(EffectClass::ALL.to_vec()),
            unique_list(tool_pattern_strategy(), 5),
        )
            .prop_map(
                |(repos, paths, read_paths, max_effect, tools)| ScopeSection {
                    repos,
                    paths,
                    read_paths,
                    max_effect,
                    tools: tools.iter().map(|t| ToolPattern::new(t).unwrap()).collect(),
                },
            );
        let limits = (1usize..=10_000, 1u64..=1_000_000, 1usize..=64).prop_map(
            |(max_iterations, max_wall_clock_secs, max_concurrent)| LimitsSection {
                max_iterations,
                max_wall_clock_secs,
                max_concurrent,
            },
        );
        (identity, scope, limits).prop_map(|(identity, scope, limits)| {
            AgentIdentity::new(identity, scope, limits, "agents/generated.toml").unwrap()
        })
    }

    proptest! {
        #[test]
        fn any_valid_identity_round_trips_through_toml(identity in any_identity()) {
            let toml = identity.to_toml_string().unwrap();
            let back = AgentIdentity::from_toml_str(&toml, identity.source()).unwrap();
            prop_assert_eq!(back, identity);
        }

        #[test]
        fn allows_effect_agrees_with_ordering(identity in any_identity(), class in prop::sample::select(EffectClass::ALL.to_vec())) {
            prop_assert_eq!(identity.allows_effect(class), class <= identity.scope.max_effect);
        }

        #[test]
        fn allows_tool_agrees_with_pattern_matching(identity in any_identity(), tool in "[a-z][a-z0-9_]{0,12}") {
            let expected = identity.scope.tools.iter().any(|p| p.matches(&tool));
            prop_assert_eq!(identity.allows_tool(&tool), expected);
        }
    }
}
