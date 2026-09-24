//! Detection of the full-stack Rust profile: a cargo workspace holding an
//! actix-web backend binary, a dioxus web frontend built with trunk, shared
//! library crates and an optional sqlx/Postgres database.
//!
//! Detection is purely filesystem based. It reads the root `Cargo.toml`, every
//! workspace member manifest, the optional `CHECKS` endpoint manifest and the
//! frontend's `Trunk.toml`; it never runs cargo or touches the network.

use super::OnboardingError;
use std::path::{Path, PathBuf};

/// Endpoint manifest at the workspace root, one absolute path per line.
pub const CHECKS_FILE: &str = "CHECKS";

/// Health endpoint candidates tried when `CHECKS` does not declare one, in
/// order of preference.
pub const DEFAULT_HEALTH_PATHS: [&str; 2] = ["/health/v1", "/health"];

/// Rust target the frontend crate is compiled for.
pub const WASM_TARGET: &str = "wasm32-unknown-unknown";

const MIGRATIONS_DIR: &str = "migrations";

/// A workspace member: its package name and its directory relative to the
/// workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCrate {
    /// Package name from `[package].name`.
    pub name: String,
    /// Member directory relative to the workspace root, e.g. `api` or
    /// `crates/api`.
    pub path: PathBuf,
}

/// Database signals found in the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseUsage {
    /// Some member depends on `sqlx` with its `postgres` feature.
    pub sqlx_postgres: bool,
    /// Directory holding sqlx migrations, relative to the workspace root.
    pub migrations_dir: Option<PathBuf>,
}

/// Topology of a detected full-stack Rust workspace.
///
/// ```
/// use harness::onboarding::fullstack::FullStackRust;
/// use std::path::Path;
///
/// let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/fullstack");
/// let profile = FullStackRust::detect(&fixture).unwrap().expect("fixture is full-stack");
/// assert_eq!(profile.api.name, "api");
/// assert_eq!(profile.frontend.name, "ui");
/// assert_eq!(profile.health_path(), "/health/v1");
/// assert!(profile.has_database());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullStackRust {
    /// Every workspace member with a readable manifest, in workspace order.
    pub members: Vec<MemberCrate>,
    /// The actix-web binary crate.
    pub api: MemberCrate,
    /// The dioxus/wasm crate built with trunk.
    pub frontend: MemberCrate,
    /// Members that are neither the api nor the frontend.
    pub shared: Vec<MemberCrate>,
    /// Database signals, `None` when neither sqlx/postgres nor a
    /// `migrations/` directory is present.
    pub database: Option<DatabaseUsage>,
    /// Health endpoint candidates in order of preference: `/health*` entries
    /// from `CHECKS` first, then [`DEFAULT_HEALTH_PATHS`].
    pub health_paths: Vec<String>,
    /// `[[proxy]].backend` entries from the frontend's `Trunk.toml`, which
    /// describe how the dev server forwards requests to the backend.
    pub proxy_backends: Vec<String>,
}

impl FullStackRust {
    /// Detect the profile under `root`.
    ///
    /// Returns `Ok(None)` when `root` is not a cargo workspace, or when no
    /// member is an actix-web binary or no other member is a trunk frontend.
    /// Members listed in the workspace whose `Cargo.toml` does not exist are
    /// skipped, mirroring [`super::detect::scan_project`]. Malformed TOML in
    /// any manifest that does exist is an error.
    pub fn detect(root: &Path) -> Result<Option<Self>, OnboardingError> {
        let manifest = root.join("Cargo.toml");
        if !manifest.is_file() {
            return Ok(None);
        }
        let doc = parse_toml(&manifest)?;
        let Some(workspace) = doc.get("workspace") else {
            return Ok(None);
        };
        let patterns = string_array(workspace.get("members"));
        let ws_deps = workspace
            .get("dependencies")
            .and_then(toml::Value::as_table);

        let mut manifests = Vec::new();
        for dir in expand_member_patterns(root, &patterns) {
            let member_manifest = dir.join("Cargo.toml");
            if !member_manifest.is_file() {
                continue;
            }
            let member_doc = parse_toml(&member_manifest)?;
            manifests.push(MemberManifest::from_doc(root, &dir, &member_doc, ws_deps)?);
        }

        let Some(api) = manifests.iter().find(|m| m.is_actix_binary()) else {
            return Ok(None);
        };
        let Some(frontend) = manifests
            .iter()
            .find(|m| m.is_trunk_frontend() && m.krate != api.krate)
        else {
            return Ok(None);
        };

        let shared = manifests
            .iter()
            .filter(|m| m.krate != api.krate && m.krate != frontend.krate)
            .map(|m| m.krate.clone())
            .collect();
        let sqlx_postgres = manifests.iter().any(|m| m.sqlx_postgres);
        let migrations_dir = find_migrations_dir(root, &api.krate.path);
        let database = if sqlx_postgres || migrations_dir.is_some() {
            Some(DatabaseUsage {
                sqlx_postgres,
                migrations_dir,
            })
        } else {
            None
        };

        Ok(Some(Self {
            members: manifests.iter().map(|m| m.krate.clone()).collect(),
            api: api.krate.clone(),
            frontend: frontend.krate.clone(),
            shared,
            database,
            health_paths: health_paths(root)?,
            proxy_backends: frontend.proxy_backends.clone(),
        }))
    }

    /// The preferred health endpoint path.
    pub fn health_path(&self) -> &str {
        &self.health_paths[0]
    }

    /// Whether the workspace uses a database.
    pub fn has_database(&self) -> bool {
        self.database.is_some()
    }
}

/// Resolve the `[workspace].members` patterns of the manifest at `root`
/// into absolute member directories, in declaration order with glob matches
/// sorted. Returns an empty list when `root` has no readable workspace
/// manifest.
pub fn workspace_member_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(content) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return Vec::new();
    };
    let Ok(doc) = content.parse::<toml::Value>() else {
        return Vec::new();
    };
    let patterns = string_array(doc.get("workspace").and_then(|w| w.get("members")));
    expand_member_patterns(root, &patterns)
}

fn expand_member_patterns(root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for pattern in patterns {
        if pattern.contains(['*', '?', '[']) {
            let full = root.join(pattern).to_string_lossy().into_owned();
            let matches = glob::glob(&full).into_iter().flatten().flatten();
            dirs.extend(matches.filter(|p| p.is_dir()));
        } else {
            dirs.push(root.join(pattern));
        }
    }
    dirs
}

struct MemberManifest {
    krate: MemberCrate,
    has_binary: bool,
    cdylib: bool,
    actix_web: bool,
    dioxus_web: bool,
    sqlx_postgres: bool,
    has_trunk_input: bool,
    proxy_backends: Vec<String>,
}

impl MemberManifest {
    fn from_doc(
        root: &Path,
        dir: &Path,
        doc: &toml::Value,
        ws_deps: Option<&toml::map::Map<String, toml::Value>>,
    ) -> Result<Self, OnboardingError> {
        let dir_name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
        let name = doc
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .or(dir_name)
            .ok_or_else(|| {
                OnboardingError::ParseError(format!("{}: no package name", dir.display()))
            })?;
        let path = dir.strip_prefix(root).unwrap_or(dir).to_path_buf();
        let crate_types = string_array(doc.get("lib").and_then(|l| l.get("crate-type")));
        let trunk_toml = dir.join("Trunk.toml");
        let proxy_backends = if trunk_toml.is_file() {
            trunk_proxy_backends(&parse_toml(&trunk_toml)?)
        } else {
            Vec::new()
        };
        Ok(Self {
            krate: MemberCrate { name, path },
            has_binary: doc.get("bin").is_some() || dir.join("src/main.rs").is_file(),
            cdylib: crate_types.iter().any(|t| t == "cdylib"),
            actix_web: dep_features(doc, ws_deps, "actix-web").is_some(),
            dioxus_web: has_feature(dep_features(doc, ws_deps, "dioxus"), "web"),
            sqlx_postgres: has_feature(dep_features(doc, ws_deps, "sqlx"), "postgres"),
            has_trunk_input: trunk_toml.is_file() || dir.join("index.html").is_file(),
            proxy_backends,
        })
    }

    fn is_actix_binary(&self) -> bool {
        self.actix_web && self.has_binary
    }

    fn is_trunk_frontend(&self) -> bool {
        (self.cdylib || self.dioxus_web) && self.has_trunk_input
    }
}

fn parse_toml(path: &Path) -> Result<toml::Value, OnboardingError> {
    let content = std::fs::read_to_string(path).map_err(OnboardingError::Io)?;
    content
        .parse()
        .map_err(|e| OnboardingError::ParseError(format!("invalid {}: {}", path.display(), e)))
}

fn string_array(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Features enabled on dependency `name`, or `None` when the member does not
/// depend on it. A `workspace = true` dependency unions the features declared
/// under `[workspace.dependencies]`.
fn dep_features(
    doc: &toml::Value,
    ws_deps: Option<&toml::map::Map<String, toml::Value>>,
    name: &str,
) -> Option<Vec<String>> {
    let dep = doc.get("dependencies")?.get(name)?;
    let mut features = string_array(dep.get("features"));
    let inherited = dep.get("workspace").and_then(toml::Value::as_bool) == Some(true);
    if inherited {
        features.extend(string_array(
            ws_deps
                .and_then(|t| t.get(name))
                .and_then(|d| d.get("features")),
        ));
    }
    Some(features)
}

fn has_feature(features: Option<Vec<String>>, feature: &str) -> bool {
    features.is_some_and(|f| f.iter().any(|x| x == feature))
}

fn trunk_proxy_backends(doc: &toml::Value) -> Vec<String> {
    doc.get("proxy")
        .and_then(toml::Value::as_array)
        .map(|proxies| {
            proxies
                .iter()
                .filter_map(|p| p.get("backend").and_then(toml::Value::as_str))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn find_migrations_dir(root: &Path, api_dir: &Path) -> Option<PathBuf> {
    let candidates = [PathBuf::from(MIGRATIONS_DIR), api_dir.join(MIGRATIONS_DIR)];
    candidates.into_iter().find(|rel| root.join(rel).is_dir())
}

fn health_paths(root: &Path) -> Result<Vec<String>, OnboardingError> {
    let mut paths: Vec<String> = Vec::new();
    let checks = root.join(CHECKS_FILE);
    if checks.is_file() {
        let content = std::fs::read_to_string(&checks).map_err(OnboardingError::Io)?;
        paths.extend(
            content
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("/health"))
                .map(String::from),
        );
    }
    for default in DEFAULT_HEALTH_PATHS {
        if !paths.iter().any(|p| p == default) {
            paths.push(default.to_string());
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/fullstack")
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    const API_MANIFEST: &str = r#"
[package]
name = "api"
version = "0.1.0"

[dependencies]
actix-web = "4"
"#;

    const UI_MANIFEST: &str = r#"
[package]
name = "ui"
version = "0.1.0"

[dependencies]
dioxus = { version = "0.7", features = ["web"] }
"#;

    fn minimal_workspace() -> TempDir {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"api\", \"ui\"]\n",
        );
        write(dir.path(), "api/Cargo.toml", API_MANIFEST);
        write(dir.path(), "api/src/main.rs", "fn main() {}");
        write(dir.path(), "ui/Cargo.toml", UI_MANIFEST);
        write(dir.path(), "ui/src/main.rs", "fn main() {}");
        write(
            dir.path(),
            "ui/Trunk.toml",
            "[build]\ntarget = \"index.html\"\n",
        );
        dir
    }

    #[test]
    fn fixture_is_detected_with_full_topology() {
        let profile = FullStackRust::detect(&fixture_root()).unwrap().unwrap();
        assert_eq!(profile.api.name, "api");
        assert_eq!(profile.api.path, PathBuf::from("api"));
        assert_eq!(profile.frontend.name, "ui");
        assert_eq!(profile.frontend.path, PathBuf::from("ui"));
        let shared: Vec<&str> = profile.shared.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(shared, vec!["shared"]);
        let members: Vec<&str> = profile.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(members, vec!["api", "shared", "ui"]);
        assert_eq!(
            profile.database,
            Some(DatabaseUsage {
                sqlx_postgres: true,
                migrations_dir: Some(PathBuf::from("migrations")),
            })
        );
        assert_eq!(profile.health_paths, vec!["/health/v1", "/health"]);
        assert_eq!(profile.health_path(), "/health/v1");
        assert!(profile.has_database());
        assert_eq!(
            profile.proxy_backends,
            vec![
                "http://127.0.0.1:8080/api/",
                "http://127.0.0.1:8080/health/"
            ]
        );
    }

    #[test]
    fn plain_library_crate_is_not_full_stack() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        );
        write(dir.path(), "src/lib.rs", "");
        assert_eq!(FullStackRust::detect(dir.path()).unwrap(), None);
    }

    #[test]
    fn directory_without_manifest_is_not_full_stack() {
        let dir = TempDir::new().unwrap();
        assert_eq!(FullStackRust::detect(dir.path()).unwrap(), None);
    }

    #[test]
    fn minimal_workspace_is_detected_without_database() {
        let dir = minimal_workspace();
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.api.name, "api");
        assert_eq!(profile.frontend.name, "ui");
        assert!(profile.shared.is_empty());
        assert_eq!(profile.database, None);
        assert!(!profile.has_database());
        assert_eq!(profile.health_paths, DEFAULT_HEALTH_PATHS);
        assert!(profile.proxy_backends.is_empty());
    }

    #[test]
    fn workspace_without_actix_binary_is_not_full_stack() {
        let dir = minimal_workspace();
        write(
            dir.path(),
            "api/Cargo.toml",
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"1\"\n",
        );
        assert_eq!(FullStackRust::detect(dir.path()).unwrap(), None);
    }

    #[test]
    fn actix_library_without_binary_is_not_the_api() {
        let dir = minimal_workspace();
        fs::remove_file(dir.path().join("api/src/main.rs")).unwrap();
        assert_eq!(FullStackRust::detect(dir.path()).unwrap(), None);
    }

    #[test]
    fn bin_section_counts_as_binary() {
        let dir = minimal_workspace();
        fs::remove_file(dir.path().join("api/src/main.rs")).unwrap();
        write(
            dir.path(),
            "api/Cargo.toml",
            &format!("{API_MANIFEST}\n[[bin]]\nname = \"api\"\npath = \"src/bin/api.rs\"\n"),
        );
        assert!(FullStackRust::detect(dir.path()).unwrap().is_some());
    }

    #[test]
    fn workspace_without_trunk_frontend_is_not_full_stack() {
        let dir = minimal_workspace();
        fs::remove_file(dir.path().join("ui/Trunk.toml")).unwrap();
        assert_eq!(FullStackRust::detect(dir.path()).unwrap(), None);
    }

    #[test]
    fn index_html_alone_marks_the_trunk_input() {
        let dir = minimal_workspace();
        fs::remove_file(dir.path().join("ui/Trunk.toml")).unwrap();
        write(dir.path(), "ui/index.html", "<html></html>");
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.frontend.name, "ui");
        assert!(profile.proxy_backends.is_empty());
    }

    #[test]
    fn cdylib_without_dioxus_web_feature_is_a_frontend() {
        let dir = minimal_workspace();
        write(
            dir.path(),
            "ui/Cargo.toml",
            "[package]\nname = \"ui\"\nversion = \"0.1.0\"\n\n[lib]\ncrate-type = [\"cdylib\", \"rlib\"]\n\n[dependencies]\nyew = \"0.21\"\n",
        );
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.frontend.name, "ui");
    }

    #[test]
    fn dioxus_without_web_feature_is_not_a_frontend() {
        let dir = minimal_workspace();
        write(
            dir.path(),
            "ui/Cargo.toml",
            "[package]\nname = \"ui\"\nversion = \"0.1.0\"\n\n[dependencies]\ndioxus = { version = \"0.7\", features = [\"desktop\"] }\n",
        );
        assert_eq!(FullStackRust::detect(dir.path()).unwrap(), None);
    }

    #[test]
    fn workspace_inherited_features_are_honoured() {
        let dir = minimal_workspace();
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"api\", \"ui\"]\n\n[workspace.dependencies]\ndioxus = { version = \"0.7\", features = [\"web\"] }\nsqlx = { version = \"0.8\", features = [\"postgres\"] }\n",
        );
        write(
            dir.path(),
            "ui/Cargo.toml",
            "[package]\nname = \"ui\"\nversion = \"0.1.0\"\n\n[dependencies]\ndioxus = { workspace = true }\n",
        );
        write(
            dir.path(),
            "api/Cargo.toml",
            &format!("{API_MANIFEST}sqlx = {{ workspace = true, features = [\"migrate\"] }}\n"),
        );
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.frontend.name, "ui");
        assert_eq!(
            profile.database,
            Some(DatabaseUsage {
                sqlx_postgres: true,
                migrations_dir: None,
            })
        );
    }

    #[test]
    fn migrations_directory_under_api_member_is_found() {
        let dir = minimal_workspace();
        fs::create_dir_all(dir.path().join("api/migrations")).unwrap();
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(
            profile.database,
            Some(DatabaseUsage {
                sqlx_postgres: false,
                migrations_dir: Some(PathBuf::from("api/migrations")),
            })
        );
    }

    #[test]
    fn root_migrations_directory_wins_over_member_directory() {
        let dir = minimal_workspace();
        fs::create_dir_all(dir.path().join("api/migrations")).unwrap();
        fs::create_dir_all(dir.path().join("migrations")).unwrap();
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(
            profile.database.unwrap().migrations_dir,
            Some(PathBuf::from("migrations"))
        );
    }

    #[test]
    fn checks_health_entries_precede_defaults() {
        let dir = minimal_workspace();
        write(
            dir.path(),
            "CHECKS",
            "/\n/healthz\n  /health/v2  \n\n/api/v1/x\n",
        );
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(
            profile.health_paths,
            vec!["/healthz", "/health/v2", "/health/v1", "/health"]
        );
        assert_eq!(profile.health_path(), "/healthz");
    }

    #[test]
    fn checks_default_entry_is_not_duplicated() {
        let dir = minimal_workspace();
        write(dir.path(), "CHECKS", "/health\n");
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.health_paths, vec!["/health", "/health/v1"]);
    }

    #[test]
    fn missing_member_manifest_is_skipped() {
        let dir = minimal_workspace();
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"ghost\", \"api\", \"ui\"]\n",
        );
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.members.len(), 2);
    }

    #[test]
    fn glob_members_are_expanded_in_sorted_order() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n",
        );
        write(dir.path(), "crates/web/Cargo.toml", UI_MANIFEST);
        write(dir.path(), "crates/web/index.html", "");
        write(dir.path(), "crates/server/Cargo.toml", API_MANIFEST);
        write(dir.path(), "crates/server/src/main.rs", "fn main() {}");
        write(dir.path(), "crates/README.md", "not a crate");
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.api.path, PathBuf::from("crates/server"));
        assert_eq!(profile.frontend.path, PathBuf::from("crates/web"));
        let members: Vec<&str> = profile.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(members, vec!["api", "ui"]);
    }

    #[test]
    fn first_candidate_in_workspace_order_is_chosen() {
        let dir = minimal_workspace();
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"api\", \"ui\", \"api2\", \"ui2\"]\n",
        );
        write(
            dir.path(),
            "api2/Cargo.toml",
            API_MANIFEST.replace("\"api\"", "\"api2\"").as_str(),
        );
        write(dir.path(), "api2/src/main.rs", "fn main() {}");
        write(
            dir.path(),
            "ui2/Cargo.toml",
            UI_MANIFEST.replace("\"ui\"", "\"ui2\"").as_str(),
        );
        write(dir.path(), "ui2/index.html", "");
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.api.name, "api");
        assert_eq!(profile.frontend.name, "ui");
        let shared: Vec<&str> = profile.shared.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(shared, vec!["api2", "ui2"]);
    }

    #[test]
    fn member_without_package_name_uses_directory_name() {
        let dir = minimal_workspace();
        write(
            dir.path(),
            "ui/Cargo.toml",
            "[package]\nversion = \"0.1.0\"\n\n[dependencies]\ndioxus = { version = \"0.7\", features = [\"web\"] }\n",
        );
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.frontend.name, "ui");
    }

    #[test]
    fn invalid_root_manifest_is_an_error() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "Cargo.toml", "[workspace\nmembers = 1");
        let err = FullStackRust::detect(dir.path()).unwrap_err();
        assert!(matches!(err, OnboardingError::ParseError(_)), "{err}");
    }

    #[test]
    fn invalid_member_manifest_is_an_error() {
        let dir = minimal_workspace();
        write(dir.path(), "ui/Cargo.toml", "not = = toml");
        let err = FullStackRust::detect(dir.path()).unwrap_err();
        assert!(err.to_string().contains("ui"), "{err}");
    }

    #[test]
    fn invalid_trunk_toml_is_an_error() {
        let dir = minimal_workspace();
        write(dir.path(), "ui/Trunk.toml", "[[proxy]\nbackend = ");
        let err = FullStackRust::detect(dir.path()).unwrap_err();
        assert!(err.to_string().contains("Trunk.toml"), "{err}");
    }

    #[test]
    fn trunk_proxy_backends_are_collected() {
        let dir = minimal_workspace();
        write(
            dir.path(),
            "ui/Trunk.toml",
            "[[proxy]]\nbackend = \"http://127.0.0.1:9000/api/\"\n\n[[proxy]]\nrewrite = \"/x\"\n",
        );
        let profile = FullStackRust::detect(dir.path()).unwrap().unwrap();
        assert_eq!(profile.proxy_backends, vec!["http://127.0.0.1:9000/api/"]);
    }

    #[test]
    fn workspace_member_dirs_resolves_literal_and_glob_patterns() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"api\", \"crates/*\"]\n",
        );
        fs::create_dir_all(dir.path().join("crates/b")).unwrap();
        fs::create_dir_all(dir.path().join("crates/a")).unwrap();
        let dirs = workspace_member_dirs(dir.path());
        assert_eq!(
            dirs,
            vec![
                dir.path().join("api"),
                dir.path().join("crates/a"),
                dir.path().join("crates/b"),
            ]
        );
    }

    #[test]
    fn workspace_member_dirs_is_empty_without_manifest_or_on_invalid_toml() {
        let dir = TempDir::new().unwrap();
        assert!(workspace_member_dirs(dir.path()).is_empty());
        write(dir.path(), "Cargo.toml", "[workspace\n");
        assert!(workspace_member_dirs(dir.path()).is_empty());
    }
}
