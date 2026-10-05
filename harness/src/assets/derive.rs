use super::{manifest_path_in, AssetError};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const PROPOSAL_FILE_NAME: &str = "effects.proposed.toml";

const SKIPPED_DIRS: [&str; 5] = [".git", "target", "node_modules", "dist", ".nanna"];

const HEADER: &str = "# PROPOSAL derived from sqlx migrations and actix routes.\n# Nothing reads this file. Review it, fill in concerns, owners and reads,\n# then move it to .nanna/effects.toml yourself.\n\n";

const DEFAULT_CONCERNS: &str = "[concern.revenue]\nweight = 10\ndescription = \"Anything on the paid path.\"\n\n[concern.auth]\nweight = 9\n\n[concern.pii]\nweight = 8\n\n[concern.availability]\nweight = 6\n";

/// Derive a starter manifest from `sqlx` migrations and actix route handlers.
///
/// Tables come from `CREATE TABLE` statements in `.sql` files under a
/// `migrations` directory; endpoints come from `.route("/p", web::get()...)`
/// calls and `#[get("/p")]`-style attributes, owned by the file declaring
/// them. Concerns are left for the human to assign. The result always parses
/// as a manifest.
///
/// ```
/// use harness::assets::{propose, AssetGraph};
///
/// let repo = tempfile::tempdir().unwrap();
/// std::fs::create_dir_all(repo.path().join("migrations")).unwrap();
/// std::fs::write(repo.path().join("migrations/1_init.sql"), "CREATE TABLE IF NOT EXISTS greetings (id INT);").unwrap();
/// std::fs::create_dir_all(repo.path().join("api/src")).unwrap();
/// std::fs::write(repo.path().join("api/src/lib.rs"), "cfg.route(\"/health/v1\", web::get().to(health));").unwrap();
///
/// let graph = AssetGraph::parse(&propose(repo.path())).unwrap();
/// assert!(graph.asset("db.greetings").is_some());
/// assert_eq!(graph.owners_matching("api/src/lib.rs")[0].name, "http.GET /health/v1");
/// ```
pub fn propose(repo: &Path) -> String {
    let route_call =
        Regex::new(r#"\.route\(\s*"([^"]+)"\s*,\s*web::(get|post|put|delete|patch|head)\s*\("#)
            .expect("static regex");
    let route_attr =
        Regex::new(r#"#\[(get|post|put|delete|patch|head)\(\s*"([^"]+)""#).expect("static regex");
    let create_table = Regex::new(
        r#"(?i)create\s+table\s+(?:if\s+not\s+exists\s+)?((?:"[^"]+"|[\w]+)(?:\.(?:"[^"]+"|[\w]+))?)"#,
    )
    .expect("static regex");

    let mut files = Vec::new();
    collect_files(repo, repo, &mut files);
    files.sort();

    let mut tables = BTreeSet::new();
    let mut endpoints: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for rel in &files {
        let Ok(text) = std::fs::read_to_string(repo.join(rel)) else {
            continue;
        };
        let rel_str = rel_string(rel);
        match rel.extension().and_then(|ext| ext.to_str()) {
            Some("sql") if rel.components().any(|c| c.as_os_str() == "migrations") => {
                for capture in create_table.captures_iter(&text) {
                    tables.insert(capture[1].replace('"', ""));
                }
            }
            Some("rs") => {
                for capture in route_call.captures_iter(&text) {
                    endpoints
                        .entry(format!(
                            "http.{} {}",
                            capture[2].to_uppercase(),
                            &capture[1]
                        ))
                        .or_default()
                        .insert(rel_str.clone());
                }
                for capture in route_attr.captures_iter(&text) {
                    endpoints
                        .entry(format!(
                            "http.{} {}",
                            capture[1].to_uppercase(),
                            &capture[2]
                        ))
                        .or_default()
                        .insert(rel_str.clone());
                }
            }
            _ => {}
        }
    }

    let mut out = String::from(HEADER);
    out.push_str(DEFAULT_CONCERNS);
    for table in tables {
        out.push_str(&format!(
            "\n[asset.{}]\nkind = \"table\"\nconcerns = []\n",
            quote(&format!("db.{table}"))
        ));
    }
    for (name, owners) in endpoints {
        let owners: Vec<String> = owners.iter().map(|owner| quote(owner)).collect();
        out.push_str(&format!(
            "\n[asset.{}]\nkind = \"endpoint\"\nconcerns = []\nowners = [{}]\n",
            quote(&name),
            owners.join(", ")
        ));
    }
    out
}

/// Write [`propose`] to `<repo>/.nanna/effects.proposed.toml` and return that path.
///
/// Only a proposal is written: the manifest itself is never created, and
/// when a manifest already exists nothing is written at all.
///
/// ```
/// use harness::assets::{propose_in_repo, AssetError, AssetGraph};
///
/// let repo = tempfile::tempdir().unwrap();
/// let path = propose_in_repo(repo.path()).unwrap();
/// assert!(path.ends_with(".nanna/effects.proposed.toml"));
/// assert!(AssetGraph::load_from_repo(repo.path()).is_err());
/// assert!(matches!(propose_in_repo(repo.path()), Err(AssetError::AlreadyExists { .. })));
/// ```
pub fn propose_in_repo(repo: &Path) -> Result<PathBuf, AssetError> {
    let manifest = manifest_path_in(repo);
    if manifest.exists() {
        return Err(AssetError::ManifestExists { path: manifest });
    }
    let path = manifest.with_file_name(PROPOSAL_FILE_NAME);
    if path.exists() {
        return Err(AssetError::AlreadyExists { path });
    }
    let io = |path: &Path, source| AssetError::Io {
        path: path.to_path_buf(),
        source,
    };
    let dir = path.parent().expect("proposal path has a parent directory");
    std::fs::create_dir_all(dir).map_err(|source| io(dir, source))?;
    std::fs::write(&path, propose(repo)).map_err(|source| io(&path, source))?;
    Ok(path)
}

pub(super) fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let skipped = entry
                .file_name()
                .to_str()
                .is_some_and(|name| SKIPPED_DIRS.contains(&name));
            if !skipped {
                collect_files(root, &path, out);
            }
        } else if kind.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
    }
}

fn rel_string(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{AssetGraph, AssetKind};

    fn write(repo: &Path, rel: &str, text: &str) {
        let path = repo.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn empty_repo_yields_concerns_only_and_parses() {
        let repo = tempfile::tempdir().unwrap();
        let graph = AssetGraph::parse(&propose(repo.path())).unwrap();
        assert_eq!(graph.assets().count(), 0);
        assert_eq!(graph.concerns().count(), 4);
    }

    #[test]
    fn tables_come_from_migrations_only() {
        let repo = tempfile::tempdir().unwrap();
        write(
            repo.path(),
            "migrations/1.sql",
            "create table users (id int);\nCREATE TABLE \"public\".\"orders\" (id int);",
        );
        write(
            repo.path(),
            "db/schema.sql",
            "CREATE TABLE ignored (id int);",
        );
        let graph = AssetGraph::parse(&propose(repo.path())).unwrap();
        let names: Vec<_> = graph.assets().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["db.public.orders", "db.users"]);
        assert!(graph.assets().all(|a| a.kind == AssetKind::Table));
    }

    #[test]
    fn routes_come_from_route_calls_and_attributes() {
        let repo = tempfile::tempdir().unwrap();
        write(
            repo.path(),
            "api/src/lib.rs",
            "cfg.route(\"/health/v1\", web::get().to(h)).route(\"/api/v1/x\", web::post().to(p));\n#[delete(\"/api/v1/x/{id}\")]\nasync fn d() {}",
        );
        write(
            repo.path(),
            "api/src/more.rs",
            "#[get(\"/health/v1\")]\nasync fn again() {}",
        );
        let graph = AssetGraph::parse(&propose(repo.path())).unwrap();
        let names: Vec<_> = graph.assets().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "http.DELETE /api/v1/x/{id}",
                "http.GET /health/v1",
                "http.POST /api/v1/x"
            ]
        );
        let health = graph.asset("http.GET /health/v1").unwrap();
        assert_eq!(health.owners, ["api/src/lib.rs", "api/src/more.rs"]);
    }

    #[test]
    fn build_output_and_vendored_dirs_are_skipped() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), "target/debug/x.rs", "#[get(\"/t\")]");
        write(
            repo.path(),
            "node_modules/a/migrations/1.sql",
            "CREATE TABLE nm (id int);",
        );
        let graph = AssetGraph::parse(&propose(repo.path())).unwrap();
        assert_eq!(graph.assets().count(), 0);
    }

    #[test]
    fn hostile_paths_are_quoted() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), "a.rs", "#[get(\"/a\\\"b\")]\nfn f() {}");
        let src = propose(repo.path());
        assert!(AssetGraph::parse(&src).is_ok());
    }

    #[test]
    fn proposal_is_written_but_never_adopted() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), "migrations/1.sql", "CREATE TABLE t (id int);");
        let path = propose_in_repo(repo.path()).unwrap();
        assert_eq!(path, repo.path().join(".nanna/effects.proposed.toml"));
        assert!(!repo.path().join(".nanna/effects.toml").exists());
        assert!(AssetGraph::load(&path).unwrap().asset("db.t").is_some());
        assert!(matches!(
            AssetGraph::load_from_repo(repo.path()),
            Err(AssetError::Io { .. })
        ));
    }

    #[test]
    fn existing_manifest_or_proposal_is_never_overwritten() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), ".nanna/effects.toml", "");
        assert!(matches!(
            propose_in_repo(repo.path()),
            Err(AssetError::ManifestExists { .. })
        ));
        let other = tempfile::tempdir().unwrap();
        write(other.path(), ".nanna/effects.proposed.toml", "keep");
        assert!(matches!(
            propose_in_repo(other.path()),
            Err(AssetError::AlreadyExists { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(other.path().join(".nanna/effects.proposed.toml")).unwrap(),
            "keep"
        );
    }
}
