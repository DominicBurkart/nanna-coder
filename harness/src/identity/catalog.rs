//! A directory of identity files, with repo-local overrides.

use super::{AgentIdentity, IdentityError};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Environment variable naming Nanna's configuration directory; identities
/// live in its `agents` subdirectory.
pub const CONFIG_DIR_ENV: &str = "NANNA_CONFIG_DIR";

/// Subdirectory of the configuration directory that holds identity files.
pub const AGENTS_SUBDIR: &str = "agents";

/// Repo-relative directory whose identities override the global catalog.
pub const REPO_AGENTS_DIR: &str = ".nanna/agents";

/// Every identity found in a catalog directory, keyed by name.
///
/// [`IdentityCatalog::load`] reads each `*.toml` file directly under the
/// directory (prompt files may live in subdirectories);
/// [`IdentityCatalog::with_repo_overrides`] then layers a repository's
/// `.nanna/agents/` on top, where each file must name a global identity and
/// may only narrow it (see [`AgentIdentity::narrows`]).
///
/// ```
/// use harness::identity::IdentityCatalog;
/// use std::fs;
///
/// let dir = tempfile::tempdir().unwrap();
/// fs::write(dir.path().join("deployer.toml"), r#"
/// [identity]
/// name = "deployer"
/// description = "Rolls a merged change out to the sandbox environment."
/// loop = "outer"
/// model = "gemma4:e4b"
/// system_prompt = { inline = "Deploy only inside an open window." }
///
/// [scope]
/// repos = ["github.com/example/repo"]
/// paths = []
/// max_effect = "sandbox"
/// tools = ["read_file", "run_command"]
///
/// [limits]
/// max_iterations = 50
/// max_wall_clock_secs = 900
/// max_concurrent = 1
/// "#).unwrap();
///
/// let catalog = IdentityCatalog::load(dir.path()).unwrap();
/// assert_eq!(catalog.names().collect::<Vec<_>>(), vec!["deployer"]);
/// let deployer = catalog.get("deployer").unwrap();
/// assert!(deployer.allows_tool("run_command"));
/// assert!(catalog.get("missing").is_none());
///
/// let repo = tempfile::tempdir().unwrap();
/// let catalog = catalog.with_repo_overrides(repo.path()).unwrap();
/// assert_eq!(catalog.len(), 1);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityCatalog {
    identities: BTreeMap<String, AgentIdentity>,
}

impl IdentityCatalog {
    /// The global catalog directory derived from the environment:
    /// `$NANNA_CONFIG_DIR/agents`, else `$XDG_CONFIG_HOME/nanna/agents`,
    /// else `$HOME/.config/nanna/agents`. `None` when none of those is set.
    pub fn default_global_dir() -> Option<PathBuf> {
        Self::global_dir_from(|key| std::env::var_os(key))
    }

    /// [`IdentityCatalog::default_global_dir`] over an arbitrary environment lookup.
    pub fn global_dir_from(lookup: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
        let non_empty = |key: &str| lookup(key).filter(|value| !value.is_empty());
        if let Some(config) = non_empty(CONFIG_DIR_ENV) {
            return Some(PathBuf::from(config).join(AGENTS_SUBDIR));
        }
        if let Some(xdg) = non_empty("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(xdg).join("nanna").join(AGENTS_SUBDIR));
        }
        let home = non_empty("HOME")?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("nanna")
                .join(AGENTS_SUBDIR),
        )
    }

    /// Load the global catalog from [`IdentityCatalog::default_global_dir`].
    pub fn load_default() -> Result<Self, IdentityError> {
        Self::load(Self::default_global_dir().ok_or(IdentityError::NoConfigDir)?)
    }

    /// Load every `*.toml` file directly under `dir`. A missing directory is
    /// an [`IdentityError::Io`]; two files declaring the same name are an
    /// [`IdentityError::DuplicateName`].
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, IdentityError> {
        let mut catalog = Self::default();
        for identity in load_dir(dir.as_ref())? {
            catalog.insert_unique(identity)?;
        }
        Ok(catalog)
    }

    /// Layer `<repo_root>/.nanna/agents/*.toml` on top of this catalog. Each
    /// file must name an identity already in the catalog and narrow it; a
    /// missing directory means the repository has no overrides.
    pub fn with_repo_overrides(
        mut self,
        repo_root: impl AsRef<Path>,
    ) -> Result<Self, IdentityError> {
        let dir = repo_root.as_ref().join(REPO_AGENTS_DIR);
        if !dir.is_dir() {
            return Ok(self);
        }
        let mut overrides = Self::default();
        for local in load_dir(&dir)? {
            let name = local.name();
            let base = self.identities.get(name).ok_or_else(|| no_base(&local))?;
            local.narrows(base)?;
            overrides.insert_unique(local)?;
        }
        self.identities.extend(overrides.identities);
        Ok(self)
    }

    fn insert_unique(&mut self, identity: AgentIdentity) -> Result<(), IdentityError> {
        if let Some(first) = self.identities.get(identity.name()) {
            return Err(IdentityError::DuplicateName {
                name: identity.name().to_string(),
                first: first.source().to_path_buf(),
                second: identity.source().to_path_buf(),
            });
        }
        self.identities
            .insert(identity.name().to_string(), identity);
        Ok(())
    }

    /// The identity called `name`, if any.
    pub fn get(&self, name: &str) -> Option<&AgentIdentity> {
        self.identities.get(name)
    }

    /// Identity names in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.identities.keys().map(String::as_str)
    }

    /// Identities in name order.
    pub fn iter(&self) -> impl Iterator<Item = &AgentIdentity> {
        self.identities.values()
    }

    /// Number of identities.
    pub fn len(&self) -> usize {
        self.identities.len()
    }

    /// Whether the catalog holds no identities.
    pub fn is_empty(&self) -> bool {
        self.identities.is_empty()
    }

    /// A column-aligned listing (name, loop, model, max_effect) for the CLI.
    pub fn render_table(&self) -> String {
        let header = ["NAME", "LOOP", "MODEL", "MAX_EFFECT"];
        let rows: Vec<[String; 4]> = self
            .iter()
            .map(|id| {
                [
                    id.name().to_string(),
                    id.identity.dev_loop.to_string(),
                    id.identity.model.clone(),
                    id.scope.max_effect.to_string(),
                ]
            })
            .collect();
        let mut widths = header.map(str::len);
        for row in &rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.len());
            }
        }
        let mut out = String::new();
        let line = |out: &mut String, cells: [&str; 4]| {
            let padded: Vec<String> = cells
                .iter()
                .zip(widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect();
            let _ = writeln!(out, "{}", padded.join("  ").trim_end());
        };
        line(&mut out, header);
        for row in &rows {
            line(&mut out, [&row[0], &row[1], &row[2], &row[3]]);
        }
        out
    }
}

fn no_base(local: &AgentIdentity) -> IdentityError {
    IdentityError::NoBaseIdentity {
        name: local.name().to_string(),
        file: local.source().to_path_buf(),
    }
}

fn load_dir(dir: &Path) -> Result<Vec<AgentIdentity>, IdentityError> {
    let io = |source| IdentityError::Io {
        file: dir.to_path_buf(),
        source,
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "toml") {
            files.push(path);
        }
    }
    files.sort();
    files.iter().map(AgentIdentity::load).collect()
}

#[cfg(test)]
mod tests {
    use super::super::schema::tests::EXAMPLE;
    use super::*;
    use crate::effects::EffectClass;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, contents: &str) -> PathBuf {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn identity_toml(name: &str, dev_loop: &str, max_effect: &str, tools: &str) -> String {
        EXAMPLE
            .replace("name = \"rust-implementer\"", &format!("name = \"{name}\""))
            .replace("loop = \"inner\"", &format!("loop = \"{dev_loop}\""))
            .replace(
                "max_effect = \"repository\"",
                &format!("max_effect = \"{max_effect}\""),
            )
            .replace(
                "tools = [\"read_file\", \"write_file\", \"search\", \"cargo_*\", \"git_*\"]",
                &format!("tools = [{tools}]"),
            )
            .replace(
                "system_prompt = \"prompts/rust-implementer.md\"",
                "system_prompt = { inline = \"Prompt.\" }",
            )
    }

    fn global_catalog() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "rust-implementer.toml", EXAMPLE);
        write(dir.path(), "prompts/rust-implementer.md", "# Implementer\n");
        write(
            dir.path(),
            "pr-shepherd.toml",
            &identity_toml(
                "pr-shepherd",
                "middle",
                "ci",
                "\"read_file\", \"git_*\", \"github_pr_status\"",
            ),
        );
        write(
            dir.path(),
            "deployer.toml",
            &identity_toml(
                "deployer",
                "outer",
                "sandbox",
                "\"read_file\", \"run_command\"",
            ),
        );
        write(dir.path(), "README.md", "not an identity");
        write(
            dir.path(),
            "prompts/ignored.toml",
            "this file is in a subdirectory and is not loaded",
        );
        dir
    }

    #[test]
    fn loads_every_toml_directly_under_the_directory_in_name_order() {
        let dir = global_catalog();
        let catalog = IdentityCatalog::load(dir.path()).unwrap();
        assert_eq!(
            catalog.names().collect::<Vec<_>>(),
            vec!["deployer", "pr-shepherd", "rust-implementer"]
        );
        assert_eq!(catalog.len(), 3);
        assert!(!catalog.is_empty());
        assert_eq!(
            catalog
                .iter()
                .map(|id| id.identity.dev_loop.to_string())
                .collect::<Vec<_>>(),
            vec!["outer", "middle", "inner"]
        );
        let implementer = catalog.get("rust-implementer").unwrap();
        assert_eq!(
            implementer.source(),
            dir.path().join("rust-implementer.toml")
        );
        assert_eq!(implementer.system_prompt_text().unwrap(), "# Implementer\n");
        assert_eq!(
            catalog.get("deployer").unwrap().scope.max_effect,
            EffectClass::Sandbox
        );
        assert!(catalog.get("nobody").is_none());
    }

    #[test]
    fn empty_directory_yields_empty_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = IdentityCatalog::load(dir.path()).unwrap();
        assert!(catalog.is_empty());
        assert_eq!(catalog, IdentityCatalog::default());
        assert_eq!(catalog.render_table(), "NAME  LOOP  MODEL  MAX_EFFECT\n");
    }

    #[test]
    fn missing_directory_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        match IdentityCatalog::load(&missing) {
            Err(IdentityError::Io { file, source }) => {
                assert_eq!(file, missing);
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    #[test]
    fn invalid_file_fails_the_whole_load() {
        let dir = global_catalog();
        write(
            dir.path(),
            "broken.toml",
            &EXAMPLE.replace("loop = \"inner\"", "loop = \"x\""),
        );
        let err = IdentityCatalog::load(dir.path()).unwrap_err();
        assert!(
            matches!(err, IdentityError::InvalidField { ref field, .. } if field == "identity.loop"),
            "{err}"
        );
    }

    #[test]
    fn duplicate_names_across_files_are_rejected() {
        let dir = global_catalog();
        write(
            dir.path(),
            "zz-copy.toml",
            &identity_toml("deployer", "outer", "sandbox", "\"read_file\""),
        );
        match IdentityCatalog::load(dir.path()) {
            Err(IdentityError::DuplicateName {
                name,
                first,
                second,
            }) => {
                assert_eq!(name, "deployer");
                assert_eq!(first, dir.path().join("deployer.toml"));
                assert_eq!(second, dir.path().join("zz-copy.toml"));
            }
            other => panic!("expected DuplicateName, got {other:?}"),
        }
    }

    #[test]
    fn repo_without_overrides_leaves_catalog_unchanged() {
        let dir = global_catalog();
        let repo = tempfile::tempdir().unwrap();
        let catalog = IdentityCatalog::load(dir.path()).unwrap();
        let layered = catalog.clone().with_repo_overrides(repo.path()).unwrap();
        assert_eq!(layered, catalog);
    }

    #[test]
    fn repo_overrides_replace_their_base_when_they_narrow() {
        let dir = global_catalog();
        let repo = tempfile::tempdir().unwrap();
        let local = write(
            repo.path(),
            ".nanna/agents/deployer.toml",
            &identity_toml("deployer", "outer", "workspace", "\"read_file\""),
        );
        let catalog = IdentityCatalog::load(dir.path())
            .unwrap()
            .with_repo_overrides(repo.path())
            .unwrap();
        assert_eq!(catalog.len(), 3);
        let deployer = catalog.get("deployer").unwrap();
        assert_eq!(deployer.source(), local);
        assert_eq!(deployer.scope.max_effect, EffectClass::Workspace);
        assert!(!deployer.allows_tool("run_command"));
        assert_eq!(
            catalog.get("pr-shepherd").unwrap().source(),
            dir.path().join("pr-shepherd.toml")
        );
    }

    #[test]
    fn repo_override_that_widens_is_rejected() {
        let dir = global_catalog();
        let repo = tempfile::tempdir().unwrap();
        write(
            repo.path(),
            ".nanna/agents/deployer.toml",
            &identity_toml("deployer", "outer", "production", "\"read_file\""),
        );
        let err = IdentityCatalog::load(dir.path())
            .unwrap()
            .with_repo_overrides(repo.path())
            .unwrap_err();
        assert!(
            matches!(err, IdentityError::WidensScope { ref name, ref field, .. } if name == "deployer" && field == "scope.max_effect"),
            "{err}"
        );
    }

    #[test]
    fn repo_override_without_global_base_is_rejected() {
        let dir = global_catalog();
        let repo = tempfile::tempdir().unwrap();
        let local = write(
            repo.path(),
            ".nanna/agents/newcomer.toml",
            &identity_toml("newcomer", "inner", "none", "\"read_file\""),
        );
        let err = IdentityCatalog::load(dir.path())
            .unwrap()
            .with_repo_overrides(repo.path())
            .unwrap_err();
        match err {
            IdentityError::NoBaseIdentity { name, file } => {
                assert_eq!(name, "newcomer");
                assert_eq!(file, local);
            }
            other => panic!("expected NoBaseIdentity, got {other:?}"),
        }
    }

    #[test]
    fn repo_override_directory_errors_propagate() {
        let dir = global_catalog();
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), ".nanna/agents/deployer.toml", "not = = toml");
        let err = IdentityCatalog::load(dir.path())
            .unwrap()
            .with_repo_overrides(repo.path())
            .unwrap_err();
        assert!(matches!(err, IdentityError::Parse { .. }), "{err}");
        write(
            repo.path(),
            ".nanna/agents/deployer.toml",
            &identity_toml("deployer", "outer", "workspace", "\"read_file\""),
        );
        write(
            repo.path(),
            ".nanna/agents/deployer-again.toml",
            &identity_toml("deployer", "outer", "workspace", "\"read_file\""),
        );
        let err = IdentityCatalog::load(dir.path())
            .unwrap()
            .with_repo_overrides(repo.path())
            .unwrap_err();
        assert!(
            matches!(err, IdentityError::DuplicateName { ref name, .. } if name == "deployer"),
            "{err}"
        );
    }

    #[test]
    fn render_table_aligns_columns() {
        let dir = global_catalog();
        let catalog = IdentityCatalog::load(dir.path()).unwrap();
        let table = catalog.render_table();
        let expected = "NAME              LOOP    MODEL       MAX_EFFECT\n\
                        deployer          outer   gemma4:e4b  sandbox\n\
                        pr-shepherd       middle  gemma4:e4b  ci\n\
                        rust-implementer  inner   gemma4:e4b  repository\n";
        assert_eq!(table, expected);
    }

    #[test]
    fn global_dir_prefers_config_dir_then_xdg_then_home() {
        let env = |vars: &[(&str, &str)]| {
            let vars: Vec<(String, String)> = vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            move |key: &str| {
                vars.iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        let all = env(&[
            ("NANNA_CONFIG_DIR", "/cfg"),
            ("XDG_CONFIG_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(
            IdentityCatalog::global_dir_from(all),
            Some(PathBuf::from("/cfg/agents"))
        );
        let xdg = env(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]);
        assert_eq!(
            IdentityCatalog::global_dir_from(xdg),
            Some(PathBuf::from("/xdg/nanna/agents"))
        );
        let home = env(&[("NANNA_CONFIG_DIR", ""), ("HOME", "/home/u")]);
        assert_eq!(
            IdentityCatalog::global_dir_from(home),
            Some(PathBuf::from("/home/u/.config/nanna/agents"))
        );
        assert_eq!(IdentityCatalog::global_dir_from(env(&[])), None);
        assert_eq!(IdentityCatalog::global_dir_from(env(&[("HOME", "")])), None);
    }

    fn with_env<T>(key: &str, value: Option<&Path>, body: impl FnOnce() -> T) -> T {
        let previous = std::env::var_os(key);
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        let result = body();
        match previous {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        result
    }

    #[test]
    #[serial_test::serial(nanna_config_dir_env)]
    fn load_default_reads_the_config_dir_from_the_environment() {
        let config = tempfile::tempdir().unwrap();
        let agents = config.path().join(AGENTS_SUBDIR);
        fs::create_dir(&agents).unwrap();
        write(
            &agents,
            "deployer.toml",
            &identity_toml("deployer", "outer", "sandbox", "\"read_file\""),
        );
        let catalog = with_env(
            CONFIG_DIR_ENV,
            Some(config.path()),
            IdentityCatalog::load_default,
        )
        .unwrap();
        assert_eq!(catalog.names().collect::<Vec<_>>(), vec!["deployer"]);
        let derived = with_env(
            CONFIG_DIR_ENV,
            Some(config.path()),
            IdentityCatalog::default_global_dir,
        );
        assert_eq!(derived, Some(agents));
    }

    #[test]
    #[serial_test::serial(nanna_config_dir_env)]
    fn load_default_reports_a_missing_config_dir() {
        let config = tempfile::tempdir().unwrap();
        let err = with_env(
            CONFIG_DIR_ENV,
            Some(&config.path().join("absent")),
            IdentityCatalog::load_default,
        )
        .unwrap_err();
        assert!(matches!(err, IdentityError::Io { .. }), "{err}");
    }
}
