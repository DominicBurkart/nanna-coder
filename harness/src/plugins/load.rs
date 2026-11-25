use super::curated;
use super::policy::apply_policy;
use super::render::{render_composition, PluginError, PluginSource, RenderedAgent};
use super::types::{Composition, CompositionFile, HostPolicy, Role, RoleFile, Stack, StackFile};
use crate::identity::{AgentIdentity, IdentityCatalog};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const REPO_PLUGINS_DIR: &str = ".nanna/plugins";

const MAX_PLUGIN_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<T> {
    pub value: T,
    pub source: PluginSource,
    pub version: String,
    pub origin: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSet {
    pub stacks: BTreeMap<String, Entry<Stack>>,
    pub roles: BTreeMap<String, Entry<Role>>,
    pub agents: BTreeMap<String, Entry<Composition>>,
}

fn fnv(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("{hash:016x}")
}

fn version_of(source: PluginSource, text: &str) -> String {
    match source {
        PluginSource::Curated => curated::VERSION.to_string(),
        _ => fnv(&[text]),
    }
}

fn parse_error(origin: &str, e: toml::de::Error) -> PluginError {
    PluginError::Parse {
        file: origin.to_string(),
        message: e.to_string(),
    }
}

fn valid_name(origin: &str, name: &str) -> Result<(), PluginError> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    if ok {
        Ok(())
    } else {
        Err(PluginError::Parse {
            file: origin.to_string(),
            message: format!(
                "`{name}` is not a valid plugin name: ASCII letters, digits, `-`, `_`"
            ),
        })
    }
}

fn insert<T>(
    map: &mut BTreeMap<String, Entry<T>>,
    kind: &'static str,
    name: String,
    entry: Entry<T>,
) -> Result<(), PluginError> {
    if let Some(first) = map.get(&name) {
        return Err(PluginError::Duplicate {
            kind,
            name,
            first: first.origin.clone(),
        });
    }
    map.insert(name, entry);
    Ok(())
}

impl PluginSet {
    pub fn add_stack(
        &mut self,
        name: &str,
        text: &str,
        source: PluginSource,
        origin: &str,
    ) -> Result<(), PluginError> {
        valid_name(origin, name)?;
        let file: StackFile = toml::from_str(text).map_err(|e| parse_error(origin, e))?;
        let entry = Entry {
            value: file.stack,
            source,
            version: version_of(source, text),
            origin: origin.to_string(),
        };
        insert(&mut self.stacks, "stack", name.to_string(), entry)
    }

    pub fn add_role(
        &mut self,
        text: &str,
        source: PluginSource,
        origin: &str,
    ) -> Result<(), PluginError> {
        let file: RoleFile = toml::from_str(text).map_err(|e| parse_error(origin, e))?;
        valid_name(origin, &file.role.name)?;
        let entry = Entry {
            source,
            version: version_of(source, text),
            origin: origin.to_string(),
            value: file.role,
        };
        insert(&mut self.roles, "role", entry.value.name.clone(), entry)
    }

    pub fn add_agent(
        &mut self,
        text: &str,
        source: PluginSource,
        origin: &str,
    ) -> Result<(), PluginError> {
        let file: CompositionFile = toml::from_str(text).map_err(|e| parse_error(origin, e))?;
        valid_name(origin, &file.agent.name)?;
        let entry = Entry {
            source,
            version: version_of(source, text),
            origin: origin.to_string(),
            value: file.agent,
        };
        insert(&mut self.agents, "agent", entry.value.name.clone(), entry)
    }

    pub fn load_dir(&mut self, root: &Path, source: PluginSource) -> Result<(), PluginError> {
        match fs::symlink_metadata(root) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(refuse(root, "plugin directories may not be symlinks"))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_error(root, &e)),
        }
        for path in toml_files(&root.join("stacks"))? {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let text = read_untrusted(&path)?;
            self.add_stack(&name, &text, source, &path.display().to_string())?;
        }
        for path in toml_files(&root.join("roles"))? {
            let text = read_untrusted(&path)?;
            self.add_role(&text, source, &path.display().to_string())?;
        }
        for path in toml_files(&root.join("agents"))? {
            let text = read_untrusted(&path)?;
            self.add_agent(&text, source, &path.display().to_string())?;
        }
        Ok(())
    }

    pub fn extend_definitions(&mut self, other: &PluginSet) -> Result<(), PluginError> {
        for (name, entry) in &other.stacks {
            insert(&mut self.stacks, "stack", name.clone(), entry.clone())?;
        }
        for (name, entry) in &other.roles {
            insert(&mut self.roles, "role", name.clone(), entry.clone())?;
        }
        Ok(())
    }

    pub fn detect(&self, repo_root: &Path) -> Vec<&str> {
        self.stacks
            .iter()
            .filter(|(_, entry)| {
                entry
                    .value
                    .detect
                    .iter()
                    .any(|marker| repo_root.join(marker).exists())
            })
            .map(|(name, _)| name.as_str())
            .collect()
    }

    pub fn render_entry(&self, comp: &Entry<Composition>) -> Result<RenderedAgent, PluginError> {
        let missing = |kind, name: &str| PluginError::Missing {
            kind,
            name: name.to_string(),
        };
        let role = self
            .roles
            .get(&comp.value.role)
            .ok_or_else(|| missing("role", &comp.value.role))?;
        let stack = self
            .stacks
            .get(&comp.value.stack)
            .ok_or_else(|| missing("stack", &comp.value.stack))?;
        let rendered = render_composition(&role.value, &stack.value, &comp.value)?;
        let source = comp.source.max(role.source).max(stack.source);
        let version = if source == PluginSource::Curated {
            curated::VERSION.to_string()
        } else {
            fnv(&[&comp.version, &role.version, &stack.version])
        };
        Ok(rendered.with_provenance(source, version))
    }

    pub fn render(&self, name: &str) -> Result<RenderedAgent, PluginError> {
        let comp = self.agents.get(name).ok_or(PluginError::Missing {
            kind: "agent",
            name: name.to_string(),
        })?;
        self.render_entry(comp)
    }
}

fn io_error(path: &Path, e: &std::io::Error) -> PluginError {
    PluginError::Io {
        file: path.display().to_string(),
        message: e.to_string(),
    }
}

fn refuse(path: &Path, message: &str) -> PluginError {
    PluginError::Parse {
        file: path.display().to_string(),
        message: message.to_string(),
    }
}

fn toml_files(dir: &Path) -> Result<Vec<PathBuf>, PluginError> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(refuse(dir, "plugin directories may not be symlinks"))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_error(dir, &e)),
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| io_error(dir, &e))? {
        let path = entry.map_err(|e| io_error(dir, &e))?.path();
        if path.extension().is_some_and(|ext| ext == "toml") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn read_untrusted(path: &Path) -> Result<String, PluginError> {
    let meta = fs::symlink_metadata(path).map_err(|e| io_error(path, &e))?;
    if meta.file_type().is_symlink() {
        return Err(refuse(path, "repo plugin files may not be symlinks"));
    }
    if !meta.is_file() {
        return Err(refuse(path, "repo plugin entry is not a regular file"));
    }
    if meta.len() > MAX_PLUGIN_BYTES {
        return Err(refuse(
            path,
            &format!("repo plugin file exceeds {MAX_PLUGIN_BYTES} bytes"),
        ));
    }
    fs::read_to_string(path).map_err(|e| io_error(path, &e))
}

pub fn provenance_of(identity: &AgentIdentity) -> (PluginSource, String) {
    let source = identity.source();
    if source.to_string_lossy().starts_with("curated:") {
        return (PluginSource::Curated, curated::VERSION.to_string());
    }
    if source.components().any(|c| c.as_os_str() == ".nanna") {
        return (PluginSource::Repo, "repo".to_string());
    }
    (PluginSource::Host, "host".to_string())
}

pub fn plugin_set(repo_root: Option<&Path>) -> Result<PluginSet, PluginError> {
    let mut set = curated::curated_set();
    if let Some(root) = repo_root {
        let mut repo = PluginSet::default();
        repo.load_dir(&root.join(REPO_PLUGINS_DIR), PluginSource::Repo)?;
        set.extend_definitions(&repo)?;
        for (name, entry) in repo.agents {
            set.agents.insert(name, entry);
        }
    }
    Ok(set)
}

pub fn describe_rendered(
    repo_root: Option<&Path>,
    name: &str,
    repos: &[String],
) -> Result<String, PluginError> {
    let rendered = plugin_set(repo_root)?
        .render(name)?
        .with_repos(repos.to_vec());
    let mut out = format!(
        "name: {}\nsource: {}\nplugin_version: {}\nloop: {}\nmodel: {}\ndescription: {}\nrepos: {}\n\n",
        rendered.name,
        rendered.source.as_str(),
        rendered.plugin_version,
        rendered.dev_loop,
        rendered.model,
        rendered.description,
        rendered.repos.join(", "),
    );
    out.push_str(&super::render::render_capabilities(&rendered));
    out.push_str("\n## System prompt\n");
    out.push_str(&rendered.system_prompt);
    out.push('\n');
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub identity: AgentIdentity,
    pub source: PluginSource,
    pub plugin_version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayeredCatalog {
    entries: BTreeMap<String, CatalogEntry>,
}

impl LayeredCatalog {
    pub fn get(&self, name: &str) -> Option<&CatalogEntry> {
        self.entries.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn into_identity_catalog(self) -> IdentityCatalog {
        IdentityCatalog::from_identities(self.entries.into_values().map(|e| e.identity))
    }

    pub fn render_table(&self) -> String {
        let header = ["NAME", "SOURCE", "VERSION", "MAX_EFFECT"];
        let rows: Vec<[String; 4]> = self
            .iter()
            .map(|e| {
                [
                    e.identity.name().to_string(),
                    e.source.as_str().to_string(),
                    e.plugin_version.clone(),
                    e.identity.scope.max_effect.to_string(),
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
        let mut line = |cells: [&str; 4]| {
            let padded: Vec<String> = cells
                .iter()
                .zip(widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect();
            out.push_str(padded.join("  ").trim_end());
            out.push('\n');
        };
        line(header);
        for row in &rows {
            line([&row[0], &row[1], &row[2], &row[3]]);
        }
        out
    }
}

pub fn build_catalog(
    host: Option<IdentityCatalog>,
    repo_root: Option<&Path>,
    repos: &[String],
    model: Option<&str>,
    policy: &HostPolicy,
) -> Result<LayeredCatalog, PluginError> {
    let set = curated::curated_set();
    let mut entries: BTreeMap<String, CatalogEntry> = BTreeMap::new();
    for (name, comp) in &set.agents {
        let mut rendered = set.render_entry(comp)?.with_repos(repos.to_vec());
        if let Some(model) = model {
            rendered = rendered.with_model(model);
        }
        entries.insert(
            name.clone(),
            CatalogEntry {
                identity: rendered.to_identity(format!("curated:{name}"))?,
                source: PluginSource::Curated,
                plugin_version: rendered.plugin_version.clone(),
            },
        );
    }
    if let Some(host) = host {
        for identity in host.iter() {
            entries.insert(
                identity.name().to_string(),
                CatalogEntry {
                    identity: identity.clone(),
                    source: PluginSource::Host,
                    plugin_version: "host".to_string(),
                },
            );
        }
    }
    let Some(root) = repo_root else {
        return Ok(LayeredCatalog { entries });
    };

    let mut repo_set = PluginSet::default();
    repo_set.load_dir(&root.join(REPO_PLUGINS_DIR), PluginSource::Repo)?;
    let mut merged = set;
    merged.extend_definitions(&repo_set)?;
    for (name, comp) in &repo_set.agents {
        let mut rendered = merged.render_entry(comp)?;
        apply_policy(&rendered, policy)?;
        match entries.get(name) {
            Some(base) => {
                rendered = rendered
                    .with_repos(base.identity.scope.repos.clone())
                    .with_model(base.identity.identity.model.clone());
                let identity = rendered.to_identity(&comp.origin)?;
                identity.narrows(&base.identity)?;
                entries.insert(
                    name.clone(),
                    CatalogEntry {
                        identity,
                        source: PluginSource::Repo,
                        plugin_version: rendered.plugin_version.clone(),
                    },
                );
            }
            None => {
                rendered = rendered.with_repos(repos.to_vec());
                if let Some(model) = model {
                    rendered = rendered.with_model(model);
                }
                entries.insert(
                    name.clone(),
                    CatalogEntry {
                        identity: rendered.to_identity(&comp.origin)?,
                        source: PluginSource::Repo,
                        plugin_version: rendered.plugin_version.clone(),
                    },
                );
            }
        }
    }

    let before = entries.clone();
    let layered = IdentityCatalog::from_identities(entries.into_values().map(|e| e.identity))
        .with_repo_overrides(root)?;
    let mut entries = BTreeMap::new();
    for identity in layered.iter() {
        let name = identity.name().to_string();
        let entry = match before.get(&name) {
            Some(prior) if prior.identity == *identity => prior.clone(),
            _ => CatalogEntry {
                identity: identity.clone(),
                source: PluginSource::Repo,
                plugin_version: "repo".to_string(),
            },
        };
        entries.insert(name, entry);
    }
    Ok(LayeredCatalog { entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectClass;
    use tempfile::TempDir;

    const GO_STACK: &str = r#"
[stack]
language = "go"
package_manager = "go"
build_system = "go"
detect = ["go.mod"]
source_globs = ["**/*.go"]
test_globs = ["**/*_test.go"]

[stack.verbs.check]
argv = ["go", "vet", "./..."]
effect = "workspace"

[stack.verbs.test]
argv = ["go", "test", "./..."]
effect = "workspace"

[stack.verbs.format]
argv = ["gofmt", "-l", "-w", "{{files}}"]
effect = "workspace"
"#;

    const GO_AGENT: &str = r#"
[agent]
name = "go-implementer"
role = "implementer"
stack = "go"
"#;

    fn permissive() -> HostPolicy {
        HostPolicy {
            max_effect: EffectClass::Workspace,
            network: false,
            allowed_verbs: vec!["check".into(), "test".into(), "format".into()],
        }
    }

    fn repo_with(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in files {
            let path = dir.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        dir
    }

    fn catalog(repo: &Path, host: Option<IdentityCatalog>) -> Result<LayeredCatalog, PluginError> {
        build_catalog(
            host,
            Some(repo),
            &["github.com/example/repo".to_string()],
            None,
            &permissive(),
        )
    }

    #[test]
    fn the_curated_layer_alone_provides_rust_implementer() {
        let built = build_catalog(None, None, &[], None, &HostPolicy::default()).unwrap();
        let entry = built.get("rust-implementer").unwrap();
        assert_eq!(entry.source, PluginSource::Curated);
        assert_eq!(entry.plugin_version, curated::VERSION);
    }

    #[test]
    fn a_repo_adds_a_stack_and_an_agent() {
        let repo = repo_with(&[
            (".nanna/plugins/stacks/go.toml", GO_STACK),
            (".nanna/plugins/agents/go-implementer.toml", GO_AGENT),
        ]);
        let built = catalog(repo.path(), None).unwrap();
        let entry = built.get("go-implementer").unwrap();
        assert_eq!(entry.source, PluginSource::Repo);
        assert!(entry.identity.allows_tool("check"));
        assert_eq!(entry.plugin_version.len(), 16);
        assert_eq!(
            built.get("rust-implementer").unwrap().source,
            PluginSource::Curated
        );
    }

    #[test]
    fn the_default_policy_blocks_a_repo_stack_that_executes() {
        let repo = repo_with(&[
            (".nanna/plugins/stacks/go.toml", GO_STACK),
            (".nanna/plugins/agents/go-implementer.toml", GO_AGENT),
        ]);
        let err =
            build_catalog(None, Some(repo.path()), &[], None, &HostPolicy::default()).unwrap_err();
        assert!(matches!(err, PluginError::Policy(_)), "{err}");
    }

    #[test]
    fn a_repo_stack_may_not_redefine_a_curated_stack() {
        let repo = repo_with(&[(".nanna/plugins/stacks/rust-cargo.toml", GO_STACK)]);
        let err = catalog(repo.path(), None).unwrap_err();
        assert!(
            matches!(err, PluginError::Duplicate { kind: "stack", .. }),
            "{err}"
        );
    }

    #[test]
    fn a_repo_role_may_not_redefine_a_curated_role() {
        let role = include_str!("../../../plugins/roles/reviewer.toml");
        let repo = repo_with(&[(".nanna/plugins/roles/reviewer.toml", role)]);
        let err = catalog(repo.path(), None).unwrap_err();
        assert!(
            matches!(err, PluginError::Duplicate { kind: "role", .. }),
            "{err}"
        );
    }

    #[test]
    fn a_repo_agent_with_the_name_of_a_host_card_must_narrow_it() {
        let host = tempfile::tempdir().unwrap();
        fs::write(
            host.path().join("go-implementer.toml"),
            r#"
[identity]
name = "go-implementer"
description = "host card"
loop = "inner"
model = "gemma4:e4b"
system_prompt = { inline = "host prompt" }

[scope]
repos = ["github.com/example/repo"]
paths = ["**/*.go", "**/*_test.go"]
max_effect = "workspace"
tools = ["read_file", "write_file", "replace", "grep", "list_directory", "finish", "check", "test", "format"]

[limits]
max_iterations = 200
max_wall_clock_secs = 43200
max_concurrent = 4
"#,
        )
        .unwrap();
        let host = IdentityCatalog::load(host.path()).unwrap();
        let repo = repo_with(&[
            (".nanna/plugins/stacks/go.toml", GO_STACK),
            (".nanna/plugins/agents/go-implementer.toml", GO_AGENT),
        ]);
        let built = catalog(repo.path(), Some(host)).unwrap();
        let entry = built.get("go-implementer").unwrap();
        assert_eq!(entry.source, PluginSource::Repo);
        assert_eq!(entry.identity.scope.repos, ["github.com/example/repo"]);
        assert!(entry
            .identity
            .system_prompt_text()
            .unwrap()
            .contains("## Capabilities"));
    }

    #[test]
    fn a_repo_agent_that_widens_a_host_card_is_rejected() {
        let host = tempfile::tempdir().unwrap();
        fs::write(
            host.path().join("go-implementer.toml"),
            r#"
[identity]
name = "go-implementer"
description = "host card"
loop = "inner"
model = "gemma4:e4b"
system_prompt = { inline = "host prompt" }

[scope]
repos = ["github.com/example/repo"]
paths = ["docs/**"]
max_effect = "workspace"
tools = ["read_file"]

[limits]
max_iterations = 1
max_wall_clock_secs = 1
max_concurrent = 1
"#,
        )
        .unwrap();
        let host = IdentityCatalog::load(host.path()).unwrap();
        let repo = repo_with(&[
            (".nanna/plugins/stacks/go.toml", GO_STACK),
            (".nanna/plugins/agents/go-implementer.toml", GO_AGENT),
        ]);
        let err = catalog(repo.path(), Some(host)).unwrap_err();
        assert!(err.to_string().contains("widens"), "{err}");
    }

    #[test]
    fn unknown_keys_in_repo_plugins_are_rejected() {
        let repo = repo_with(&[(
            ".nanna/plugins/stacks/go.toml",
            &GO_STACK.replace("language = \"go\"", "language = \"go\"\nlanguag = 1"),
        )]);
        let err = catalog(repo.path(), None).unwrap_err();
        assert!(matches!(err, PluginError::Parse { .. }), "{err}");
    }

    #[test]
    fn oversized_repo_plugin_files_are_rejected() {
        let big = format!("{GO_STACK}\n# {}\n", "x".repeat(70 * 1024));
        let repo = repo_with(&[(".nanna/plugins/stacks/go.toml", &big)]);
        let err = catalog(repo.path(), None).unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_repo_plugin_files_and_directories_are_rejected() {
        let repo = repo_with(&[("real/go.toml", GO_STACK)]);
        let stacks = repo.path().join(".nanna/plugins/stacks");
        fs::create_dir_all(&stacks).unwrap();
        std::os::unix::fs::symlink(repo.path().join("real/go.toml"), stacks.join("go.toml"))
            .unwrap();
        let err = catalog(repo.path(), None).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");

        let repo = repo_with(&[("real/go.toml", GO_STACK)]);
        let plugins = repo.path().join(".nanna/plugins");
        fs::create_dir_all(&plugins).unwrap();
        std::os::unix::fs::symlink(repo.path().join("real"), plugins.join("stacks")).unwrap();
        let err = catalog(repo.path(), None).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
    }

    #[test]
    fn an_agent_naming_a_missing_stack_is_an_error() {
        let repo = repo_with(&[(".nanna/plugins/agents/go-implementer.toml", GO_AGENT)]);
        let err = catalog(repo.path(), None).unwrap_err();
        assert!(
            matches!(err, PluginError::Missing { kind: "stack", .. }),
            "{err}"
        );
    }

    #[test]
    fn a_stack_is_detected_by_its_markers() {
        let repo = repo_with(&[
            ("go.mod", "module x\n"),
            (".nanna/plugins/stacks/go.toml", GO_STACK),
        ]);
        let mut set = curated::curated_set();
        let mut repo_set = PluginSet::default();
        repo_set
            .load_dir(&repo.path().join(REPO_PLUGINS_DIR), PluginSource::Repo)
            .unwrap();
        set.extend_definitions(&repo_set).unwrap();
        assert_eq!(set.detect(repo.path()), ["go"]);

        let rust = repo_with(&[("Cargo.toml", "")]);
        assert_eq!(set.detect(rust.path()), ["rust-cargo"]);
        let empty = repo_with(&[]);
        assert!(set.detect(empty.path()).is_empty());
    }

    #[test]
    fn a_missing_plugin_directory_means_no_repo_plugins() {
        let repo = repo_with(&[]);
        let built = catalog(repo.path(), None).unwrap();
        assert!(built.get("rust-implementer").is_some());
    }

    #[test]
    fn the_table_lists_source_and_version() {
        let built = build_catalog(None, None, &[], None, &HostPolicy::default()).unwrap();
        let table = built.render_table();
        assert!(table.starts_with("NAME"));
        assert!(table.contains("rust-implementer"));
        assert!(table.contains("curated"));
    }

    #[test]
    fn provenance_is_read_from_the_identity_source_path() {
        let built = build_catalog(None, None, &[], None, &HostPolicy::default()).unwrap();
        let curated = &built.get("rust-implementer").unwrap().identity;
        assert_eq!(
            provenance_of(curated),
            (PluginSource::Curated, curated::VERSION.to_string())
        );
        let repo = repo_with(&[
            (".nanna/plugins/stacks/go.toml", GO_STACK),
            (".nanna/plugins/agents/go-implementer.toml", GO_AGENT),
        ]);
        let built = catalog(repo.path(), None).unwrap();
        let local = &built.get("go-implementer").unwrap().identity;
        assert_eq!(provenance_of(local).0, PluginSource::Repo);
        let host = tempfile::tempdir().unwrap();
        let path = host.path().join("a.toml");
        fs::write(
            &path,
            "[identity]\nname = \"a\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\nsystem_prompt = { inline = \"p\" }\n[scope]\nrepos = []\npaths = []\nmax_effect = \"none\"\ntools = []\n[limits]\nmax_iterations = 1\nmax_wall_clock_secs = 1\nmax_concurrent = 1\n",
        )
        .unwrap();
        let identity = AgentIdentity::load(&path).unwrap();
        assert_eq!(provenance_of(&identity).0, PluginSource::Host);
    }

    #[test]
    fn describing_a_rendered_agent_lists_capabilities_and_prompt() {
        let repo = repo_with(&[
            (".nanna/plugins/stacks/go.toml", GO_STACK),
            (".nanna/plugins/agents/go-implementer.toml", GO_AGENT),
        ]);
        let text = describe_rendered(
            Some(repo.path()),
            "go-implementer",
            &["github.com/example/repo".to_string()],
        )
        .unwrap();
        assert!(text.starts_with("name: go-implementer\nsource: repo\n"));
        assert!(text.contains("argv=`go vet ./...`"));
        assert!(text.contains("## System prompt"));
        let err = describe_rendered(None, "nobody", &[]).unwrap_err();
        assert!(matches!(err, PluginError::Missing { kind: "agent", .. }));
    }

    #[test]
    fn a_layered_catalog_converts_to_an_identity_catalog() {
        let built = build_catalog(None, None, &[], None, &HostPolicy::default()).unwrap();
        let len = built.len();
        let plain = built.into_identity_catalog();
        assert_eq!(plain.len(), len);
        assert!(plain.get("rust-implementer").is_some());
    }
}
