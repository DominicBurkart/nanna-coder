//! The read-only view an auditor gets of the world.

use super::AuditError;
use crate::effects::EffectClass;
use crate::identity::{AgentIdentity, IdentityCatalog, IdentityError};
use crate::onboarding::profile::ProjectProfile;
use std::fmt::Write as _;
use std::sync::Arc;

/// Everything an auditor may consult: the identity catalog, a summary of the
/// repository profile and the auditor's own identity.
///
/// The auditor identity must be inert: `scope.max_effect = "none"` and no
/// tools. [`AuditContext::new`] refuses anything else, so an auditor can
/// never be spawned as an agent that acts.
///
/// ```
/// use harness::auditor::AuditContext;
/// use harness::identity::{AgentIdentity, IdentityCatalog};
///
/// let auditor = AgentIdentity::from_toml_str(r#"
/// [identity]
/// name = "auditor"
/// description = "Reviews spawns."
/// loop = "inner"
/// model = "gemma4:e4b"
/// system_prompt = { inline = "Find the flaw." }
///
/// [scope]
/// repos = []
/// paths = []
/// max_effect = "none"
/// tools = []
///
/// [limits]
/// max_iterations = 1
/// max_wall_clock_secs = 60
/// max_concurrent = 1
/// "#, "auditor.toml").unwrap();
///
/// let context = AuditContext::new(IdentityCatalog::default(), auditor).unwrap();
/// assert!(context.catalog().is_empty());
/// assert_eq!(context.auditor().name(), "auditor");
/// assert!(context.repo_profile().is_none());
/// ```
#[derive(Debug, Clone)]
pub struct AuditContext {
    catalog: Arc<IdentityCatalog>,
    auditor: AgentIdentity,
    auditor_prompt: String,
    repo_profile: Option<String>,
}

impl AuditContext {
    /// Build a context over `catalog` for `auditor`, which must be inert and
    /// whose system prompt must resolve.
    pub fn new(
        catalog: impl Into<Arc<IdentityCatalog>>,
        auditor: AgentIdentity,
    ) -> Result<Self, AuditError> {
        let name = auditor.name().to_string();
        let ceiling = auditor.scope.max_effect;
        let tool_count = auditor.scope.tools.len();
        if ceiling != EffectClass::None {
            let reason = format!("scope.max_effect is `{ceiling}`, expected `none`");
            return Err(AuditError::AuditorNotInert { name, reason });
        }
        if tool_count != 0 {
            let reason = format!("scope.tools grants {tool_count} tool pattern(s), expected none");
            return Err(AuditError::AuditorNotInert { name, reason });
        }
        let auditor_prompt = auditor.system_prompt_text()?;
        Ok(Self {
            catalog: catalog.into(),
            auditor,
            auditor_prompt,
            repo_profile: None,
        })
    }

    /// Attach a one-paragraph description of the repository.
    pub fn with_repo_profile(mut self, summary: impl Into<String>) -> Self {
        self.repo_profile = Some(summary.into());
        self
    }

    /// Attach the summary of an onboarding profile.
    pub fn with_project_profile(self, profile: &ProjectProfile) -> Self {
        self.with_repo_profile(Self::summarize_profile(profile))
    }

    /// Render an onboarding profile as the one-paragraph summary an auditor sees.
    pub fn summarize_profile(profile: &ProjectProfile) -> String {
        let mut out = format!(
            "project `{}` built with {:?}",
            profile.project_name, profile.build_system
        );
        if let Some(version) = &profile.rust_version {
            let _ = write!(out, " (rust {version})");
        }
        let tools: Vec<String> = profile
            .tools
            .iter()
            .map(|tool| format!("{} = `{}`", tool.name, tool.command))
            .collect();
        if !tools.is_empty() {
            let _ = write!(out, "; tools: {}", tools.join(", "));
        }
        if !profile.nix_packages.is_empty() {
            let _ = write!(out, "; nix packages: {}", profile.nix_packages.join(", "));
        }
        out
    }

    /// ```
    /// use harness::auditor::AuditContext;
    /// use harness::identity::IdentityCatalog;
    /// let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/identities");
    /// let catalog = IdentityCatalog::load(format!("{fixtures}/global")).unwrap();
    /// let auditor = catalog.get("auditor").unwrap().clone();
    /// let context = AuditContext::new(catalog, auditor).unwrap();
    /// let layered = context.with_repo_overrides(format!("{fixtures}/repo")).unwrap();
    /// assert_eq!(layered.catalog().get("rust-implementer").unwrap().scope.paths, ["api/**"]);
    /// assert_eq!(context.catalog().get("rust-implementer").unwrap().scope.paths, ["api/**", "shared/**"]);
    /// ```
    pub fn with_repo_overrides(
        &self,
        repo_root: impl AsRef<std::path::Path>,
    ) -> Result<Self, IdentityError> {
        let catalog = IdentityCatalog::clone(&self.catalog).with_repo_overrides(repo_root)?;
        Ok(Self {
            catalog: Arc::new(catalog),
            ..self.clone()
        })
    }

    /// The identities the planner may spawn.
    pub fn catalog(&self) -> &IdentityCatalog {
        &self.catalog
    }

    /// The auditor's own identity.
    pub fn auditor(&self) -> &AgentIdentity {
        &self.auditor
    }

    /// The auditor identity's resolved system prompt.
    pub fn auditor_prompt(&self) -> &str {
        &self.auditor_prompt
    }

    /// The repository summary, if one was attached.
    pub fn repo_profile(&self) -> Option<&str> {
        self.repo_profile.as_deref()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::onboarding::profile::{BuildSystem, ToolCategory, ToolSpec};

    pub(crate) const AUDITOR_TOML: &str = r#"
[identity]
name = "auditor"
description = "Adversarially reviews every proposed agent spawn."
loop = "inner"
model = "gemma4:e4b"
system_prompt = { inline = "You are the auditor. Find the flaw in every spawn." }

[scope]
repos = []
paths = []
max_effect = "none"
tools = []

[limits]
max_iterations = 1
max_wall_clock_secs = 120
max_concurrent = 4
"#;

    pub(crate) fn auditor_identity() -> AgentIdentity {
        AgentIdentity::from_toml_str(AUDITOR_TOML, "agents/auditor.toml").unwrap()
    }

    pub(crate) fn context() -> AuditContext {
        AuditContext::new(IdentityCatalog::default(), auditor_identity()).unwrap()
    }

    const GLOBAL: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/identities/global"
    );
    const REPO: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/identities/repo"
    );

    fn fixture_context() -> AuditContext {
        let catalog = IdentityCatalog::load(GLOBAL).unwrap();
        let auditor = catalog.get("auditor").unwrap().clone();
        AuditContext::new(catalog, auditor).unwrap()
    }

    fn repo_with_override(name: &str, body: &str) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".nanna/agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
        repo
    }

    fn override_card(name: &str, paths: &str, effect: &str) -> String {
        format!(
            r#"
[identity]
name = "{name}"
description = "Override."
loop = "inner"
model = "gemma4:e4b"
system_prompt = {{ inline = "Override." }}

[scope]
repos = ["github.com/example/repo"]
paths = [{paths}]
max_effect = "{effect}"
tools = ["read_file"]

[limits]
max_iterations = 10
max_wall_clock_secs = 60
max_concurrent = 1
"#
        )
    }

    #[test]
    fn repo_overrides_narrow_the_catalog_copy_and_leave_the_original_alone() {
        let context = fixture_context();
        let layered = context.with_repo_overrides(REPO).unwrap();
        let narrowed = layered.catalog().get("rust-implementer").unwrap();
        assert_eq!(narrowed.scope.paths, ["api/**"]);
        let base = context.catalog().get("rust-implementer").unwrap();
        assert_eq!(base.scope.paths, ["api/**", "shared/**"]);
        assert_eq!(layered.auditor(), context.auditor());
    }

    #[test]
    fn a_repo_without_overrides_leaves_the_catalog_unchanged() {
        let context = fixture_context();
        let repo = tempfile::tempdir().unwrap();
        let layered = context.with_repo_overrides(repo.path()).unwrap();
        assert_eq!(layered.catalog(), context.catalog());
    }

    #[test]
    fn a_widening_override_is_refused() {
        let repo = repo_with_override(
            "rust-implementer.toml",
            &override_card(
                "rust-implementer",
                r#""api/**", "shared/**", "ui/**""#,
                "workspace",
            ),
        );
        let err = fixture_context()
            .with_repo_overrides(repo.path())
            .unwrap_err();
        assert!(matches!(err, IdentityError::WidensScope { .. }), "{err}");
    }

    #[test]
    fn an_override_without_a_global_base_is_refused() {
        let repo = repo_with_override("ghost.toml", &override_card("ghost", r#""api/**""#, "none"));
        let err = fixture_context()
            .with_repo_overrides(repo.path())
            .unwrap_err();
        assert!(matches!(err, IdentityError::NoBaseIdentity { .. }), "{err}");
    }

    #[test]
    fn an_unparseable_override_is_refused() {
        let repo = repo_with_override("rust-implementer.toml", "not = [valid");
        let err = fixture_context()
            .with_repo_overrides(repo.path())
            .unwrap_err();
        assert!(matches!(err, IdentityError::Parse { .. }), "{err}");
    }

    #[test]
    fn the_auditor_cannot_be_overridden() {
        let repo = repo_with_override("auditor.toml", &override_card("auditor", "", "none"));
        let err = fixture_context()
            .with_repo_overrides(repo.path())
            .unwrap_err();
        assert!(
            matches!(err, IdentityError::ProtectedIdentity { .. }),
            "{err}"
        );
    }

    #[test]
    fn inert_auditor_is_accepted() {
        let context = context().with_repo_profile("a rust monorepo");
        assert_eq!(context.auditor().name(), "auditor");
        assert_eq!(
            context.auditor_prompt(),
            "You are the auditor. Find the flaw in every spawn."
        );
        assert_eq!(context.repo_profile(), Some("a rust monorepo"));
        assert!(context.catalog().is_empty());
    }

    #[test]
    fn auditor_with_an_effect_ceiling_is_rejected() {
        let toml = AUDITOR_TOML.replace("max_effect = \"none\"", "max_effect = \"workspace\"");
        let auditor = AgentIdentity::from_toml_str(&toml, "auditor.toml").unwrap();
        let err = AuditContext::new(IdentityCatalog::default(), auditor).unwrap_err();
        assert_eq!(err.to_string(), "auditor identity `auditor` is not inert: scope.max_effect is `workspace`, expected `none`");
    }

    #[test]
    fn auditor_prompt_must_resolve() {
        let toml = AUDITOR_TOML.replace(
            "system_prompt = { inline = \"You are the auditor. Find the flaw in every spawn.\" }",
            "system_prompt = \"prompts/missing.md\"",
        );
        let dir = tempfile::tempdir().unwrap();
        let auditor = AgentIdentity::from_toml_str(&toml, dir.path().join("auditor.toml")).unwrap();
        let err = AuditContext::new(IdentityCatalog::default(), auditor).unwrap_err();
        assert!(matches!(err, AuditError::Identity(_)), "{err}");
        assert!(err.to_string().contains("missing.md"), "{err}");
    }

    #[test]
    fn auditor_with_tools_is_rejected() {
        let toml = AUDITOR_TOML.replace("tools = []", "tools = [\"read_file\", \"search\"]");
        let auditor = AgentIdentity::from_toml_str(&toml, "auditor.toml").unwrap();
        let err = AuditContext::new(IdentityCatalog::default(), auditor).unwrap_err();
        assert_eq!(err.to_string(), "auditor identity `auditor` is not inert: scope.tools grants 2 tool pattern(s), expected none");
    }

    #[test]
    fn profile_summary_lists_build_system_tools_and_packages() {
        let profile = ProjectProfile {
            project_name: "shop".to_string(),
            build_system: BuildSystem::Cargo,
            tools: vec![
                ToolSpec::new("build", "cargo build", "Build", ToolCategory::Build).unwrap(),
                ToolSpec::new("test", "cargo test", "Test", ToolCategory::Test).unwrap(),
            ],
            nix_packages: vec!["pkg-config".to_string()],
            rust_version: Some("1.84.0".to_string()),
            extra_env_vars: vec![],
            rust_targets: vec![],
        };
        let summary = AuditContext::summarize_profile(&profile);
        assert_eq!(summary, "project `shop` built with Cargo (rust 1.84.0); tools: build = `cargo build`, test = `cargo test`; nix packages: pkg-config");
        let context = context().with_project_profile(&profile);
        assert_eq!(context.repo_profile(), Some(summary.as_str()));
    }

    #[test]
    fn profile_summary_omits_empty_sections() {
        let profile = ProjectProfile {
            project_name: "bare".to_string(),
            build_system: BuildSystem::Cargo,
            tools: vec![],
            nix_packages: vec![],
            rust_version: None,
            extra_env_vars: vec![],
            rust_targets: vec![],
        };
        assert_eq!(
            AuditContext::summarize_profile(&profile),
            "project `bare` built with Cargo"
        );
    }
}
