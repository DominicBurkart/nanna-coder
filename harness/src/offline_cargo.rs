use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

use crate::container::ReadOnlyMount;
use crate::sidecar::RunOutput;

pub const CONTAINER_VENDOR_DIR: &str = "/opt/nanna-offline/vendor";

pub const CONTAINER_CARGO_CONFIG: &str = "/.cargo/config.toml";

const REPO_CONFIG_FILES: [&str; 2] = [".cargo/config.toml", ".cargo/config"];

const OVERRIDING_KEYS: [&str; 3] = ["source", "patch", "include"];

#[derive(Debug, Error)]
pub enum OfflineCargoError {
    #[error("repository cargo config {file} declares `{key}`, which would override the offline dependency set")]
    RepoOverride { file: String, key: String },
    #[error("cannot read repository cargo config {file}: {reason}")]
    UnreadableRepoConfig { file: String, reason: String },
    #[error("cargo vendor failed: {0}")]
    VendorFailed(String),
    #[error("cargo vendor produced an unusable source configuration: {0}")]
    BadVendorConfig(String),
    #[error("cannot stage offline dependencies: {0}")]
    Io(#[from] std::io::Error),
}

pub trait Vendorer: Send + Sync {
    fn vendor(&self, manifest: &Path, vendor_dir: &Path, cwd: &Path) -> std::io::Result<RunOutput>;
}

/// ```
/// use harness::offline_cargo::{CargoVendor, Vendorer};
///
/// let dir = tempfile::tempdir().unwrap();
/// let out = CargoVendor
///     .vendor(&dir.path().join("Cargo.toml"), &dir.path().join("v"), dir.path())
///     .unwrap();
/// assert!(!out.success);
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct CargoVendor;

impl Vendorer for CargoVendor {
    fn vendor(&self, manifest: &Path, vendor_dir: &Path, cwd: &Path) -> std::io::Result<RunOutput> {
        let output = Command::new("cargo")
            .arg("vendor")
            .arg("--locked")
            .arg("--manifest-path")
            .arg(manifest)
            .arg(vendor_dir)
            .current_dir(cwd)
            .output()?;
        Ok(RunOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// ```
/// use harness::offline_cargo::render_config;
///
/// let printed = "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n\
///                [source.vendored-sources]\ndirectory = \"../host/vendor\"\n";
/// let config = render_config(printed).unwrap();
/// assert!(config.contains("directory = \"/opt/nanna-offline/vendor\""));
/// assert!(config.contains("offline = true"));
/// assert!(render_config("nothing = 1").is_err());
/// ```
pub fn render_config(vendor_stdout: &str) -> Result<String, OfflineCargoError> {
    let mut table: toml::Table = toml::from_str(vendor_stdout)
        .map_err(|e| OfflineCargoError::BadVendorConfig(e.to_string()))?;
    let sources = table
        .get_mut("source")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| OfflineCargoError::BadVendorConfig("no [source] table".to_string()))?;
    let mut rewritten = 0;
    for (_, source) in sources.iter_mut() {
        if let Some(entry) = source.as_table_mut() {
            if entry.contains_key("directory") {
                entry.insert(
                    "directory".to_string(),
                    toml::Value::String(CONTAINER_VENDOR_DIR.to_string()),
                );
                rewritten += 1;
            }
        }
    }
    if rewritten == 0 {
        return Err(OfflineCargoError::BadVendorConfig(
            "no directory source".to_string(),
        ));
    }
    let mut net = toml::Table::new();
    net.insert("offline".to_string(), toml::Value::Boolean(true));
    table.insert("net".to_string(), toml::Value::Table(net));
    toml::to_string(&table).map_err(|e| OfflineCargoError::BadVendorConfig(e.to_string()))
}

/// ```
/// use harness::offline_cargo::check_repo_config;
///
/// let repo = tempfile::tempdir().unwrap();
/// assert!(check_repo_config(repo.path()).is_ok());
/// std::fs::create_dir(repo.path().join(".cargo")).unwrap();
/// std::fs::write(repo.path().join(".cargo/config.toml"), "[source.x]\n").unwrap();
/// assert!(check_repo_config(repo.path()).is_err());
/// ```
pub fn check_repo_config(workspace: &Path) -> Result<(), OfflineCargoError> {
    for file in REPO_CONFIG_FILES {
        let text = match std::fs::read_to_string(workspace.join(file)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(OfflineCargoError::UnreadableRepoConfig {
                    file: file.to_string(),
                    reason: e.to_string(),
                })
            }
        };
        let table: toml::Table =
            toml::from_str(&text).map_err(|e| OfflineCargoError::UnreadableRepoConfig {
                file: file.to_string(),
                reason: e.to_string(),
            })?;
        if let Some(key) = OVERRIDING_KEYS.iter().find(|k| table.contains_key(**k)) {
            return Err(OfflineCargoError::RepoOverride {
                file: file.to_string(),
                key: (*key).to_string(),
            });
        }
    }
    Ok(())
}

/// ```
/// use harness::offline_cargo::{OfflineCargo, Vendorer};
/// use harness::sidecar::RunOutput;
/// use std::path::Path;
///
/// struct NoDeps;
/// impl Vendorer for NoDeps {
///     fn vendor(&self, _: &Path, _: &Path, _: &Path) -> std::io::Result<RunOutput> {
///         Ok(RunOutput { success: true, stdout: String::new(), stderr: String::new() })
///     }
/// }
///
/// let repo = tempfile::tempdir().unwrap();
/// let stage = tempfile::tempdir().unwrap();
/// std::fs::write(repo.path().join("Cargo.toml"), "[package]\n").unwrap();
/// let offline = OfflineCargo::prepare(repo.path(), &stage.path().join("s"), &NoDeps).unwrap();
/// assert!(offline.is_none());
/// ```
#[derive(Debug)]
pub struct OfflineCargo {
    stage: PathBuf,
    mounts: Vec<ReadOnlyMount>,
    env: Vec<(String, String)>,
}

impl OfflineCargo {
    pub fn prepare(
        workspace: &Path,
        stage: &Path,
        vendorer: &dyn Vendorer,
    ) -> Result<Option<OfflineCargo>, OfflineCargoError> {
        let manifest = workspace.join("Cargo.toml");
        if !manifest.is_file() {
            return Ok(None);
        }
        check_repo_config(workspace)?;
        let staged = Self::vendor_into(&manifest, stage, vendorer);
        if !matches!(staged, Ok(Some(_))) {
            let _ = std::fs::remove_dir_all(stage);
        }
        staged
    }

    fn vendor_into(
        manifest: &Path,
        stage: &Path,
        vendorer: &dyn Vendorer,
    ) -> Result<Option<OfflineCargo>, OfflineCargoError> {
        let _ = std::fs::remove_dir_all(stage);
        let cwd = stage.join("cwd");
        std::fs::create_dir_all(&cwd)?;
        let vendor_dir = stage.join("vendor");
        let output = vendorer.vendor(manifest, &vendor_dir, &cwd)?;
        if !output.success {
            return Err(OfflineCargoError::VendorFailed(
                output.stderr.trim().to_string(),
            ));
        }
        if output.stdout.trim().is_empty() {
            return Ok(None);
        }
        let config = render_config(&output.stdout)?;
        if !vendor_dir.is_dir() {
            return Err(OfflineCargoError::BadVendorConfig(
                "vendor directory was not created".to_string(),
            ));
        }
        let config_path = stage.join("config.toml");
        std::fs::write(&config_path, config)?;
        Ok(Some(OfflineCargo {
            stage: stage.to_path_buf(),
            mounts: vec![
                ReadOnlyMount::relabeled(vendor_dir, CONTAINER_VENDOR_DIR),
                ReadOnlyMount::relabeled(config_path, CONTAINER_CARGO_CONFIG),
            ],
            env: vec![("CARGO_NET_OFFLINE".to_string(), "true".to_string())],
        }))
    }

    pub fn mounts(&self) -> &[ReadOnlyMount] {
        &self.mounts
    }

    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }

    pub fn stage(&self) -> &Path {
        &self.stage
    }

    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.stage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Mutex;

    const PRINTED: &str = "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n\
        [source.\"git+https://example.com/x\"]\ngit = \"https://example.com/x\"\n\
        replace-with = \"vendored-sources\"\n\n\
        [source.vendored-sources]\ndirectory = \"../host/vendor\"\n";

    struct FakeVendorer {
        stdout: String,
        success: bool,
        create_vendor_dir: bool,
        calls: Mutex<Vec<(PathBuf, PathBuf, PathBuf)>>,
    }

    impl FakeVendorer {
        fn printing(stdout: &str) -> Self {
            Self {
                stdout: stdout.to_string(),
                success: true,
                create_vendor_dir: true,
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl Vendorer for FakeVendorer {
        fn vendor(
            &self,
            manifest: &Path,
            vendor_dir: &Path,
            cwd: &Path,
        ) -> std::io::Result<RunOutput> {
            self.calls.lock().unwrap().push((
                manifest.to_path_buf(),
                vendor_dir.to_path_buf(),
                cwd.to_path_buf(),
            ));
            if self.create_vendor_dir {
                std::fs::create_dir_all(vendor_dir.join("dep"))?;
            }
            Ok(RunOutput {
                success: self.success,
                stdout: self.stdout.clone(),
                stderr: if self.success {
                    String::new()
                } else {
                    "  no lock file\n".to_string()
                },
            })
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        repo: PathBuf,
        stage: PathBuf,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let stage = root.path().join("stage");
        Fixture {
            _root: root,
            repo,
            stage,
        }
    }

    fn write_config(repo: &Path, file: &str, text: &str) {
        let path = repo.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn render_config_points_every_directory_source_at_the_container_path() {
        let rendered = render_config(PRINTED).unwrap();
        let table: toml::Table = toml::from_str(&rendered).unwrap();
        let sources = table["source"].as_table().unwrap();
        assert_eq!(
            sources["vendored-sources"]["directory"].as_str(),
            Some(CONTAINER_VENDOR_DIR)
        );
        assert_eq!(
            sources["crates-io"]["replace-with"].as_str(),
            Some("vendored-sources")
        );
        assert_eq!(
            sources["git+https://example.com/x"]["git"].as_str(),
            Some("https://example.com/x")
        );
        assert_eq!(table["net"]["offline"].as_bool(), Some(true));
        assert!(!rendered.contains("../host/vendor"));
    }

    #[test]
    fn render_config_rejects_unusable_output() {
        for bad in [
            "not = [toml",
            "x = 1",
            "[source]\n",
            "[source.crates-io]\nreplace-with = \"x\"\n",
            "source = 3",
        ] {
            assert!(
                matches!(
                    render_config(bad),
                    Err(OfflineCargoError::BadVendorConfig(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn repo_config_may_set_unrelated_keys() {
        let f = fixture();
        write_config(
            &f.repo,
            ".cargo/config.toml",
            "[alias]\nxtask = \"run -p xtask\"\n[build]\ntarget-dir = \"t\"\n",
        );
        assert!(check_repo_config(&f.repo).is_ok());
    }

    #[test]
    fn repo_config_overriding_sources_is_refused_in_either_file() {
        for file in [".cargo/config.toml", ".cargo/config"] {
            for key in OVERRIDING_KEYS {
                let f = fixture();
                write_config(&f.repo, file, &format!("[{key}.x]\na = 1\n"));
                match check_repo_config(&f.repo) {
                    Err(OfflineCargoError::RepoOverride { file: got, key: k }) => {
                        assert_eq!(got, file);
                        assert_eq!(k, key);
                    }
                    other => panic!("{file} {key}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn unreadable_repo_config_fails_closed() {
        let f = fixture();
        write_config(&f.repo, ".cargo/config.toml", "[broken");
        assert!(matches!(
            check_repo_config(&f.repo),
            Err(OfflineCargoError::UnreadableRepoConfig { .. })
        ));
        let g = fixture();
        std::fs::create_dir_all(g.repo.join(".cargo/config.toml")).unwrap();
        assert!(matches!(
            check_repo_config(&g.repo),
            Err(OfflineCargoError::UnreadableRepoConfig { .. })
        ));
    }

    #[test]
    fn a_repo_without_a_manifest_needs_nothing() {
        let f = fixture();
        std::fs::remove_file(f.repo.join("Cargo.toml")).unwrap();
        let vendorer = FakeVendorer::printing(PRINTED);
        assert!(OfflineCargo::prepare(&f.repo, &f.stage, &vendorer)
            .unwrap()
            .is_none());
        assert!(vendorer.calls.lock().unwrap().is_empty());
        assert!(!f.stage.exists());
    }

    #[test]
    fn a_repo_with_no_dependencies_needs_nothing_and_leaves_no_stage() {
        let f = fixture();
        let vendorer = FakeVendorer::printing("");
        assert!(OfflineCargo::prepare(&f.repo, &f.stage, &vendorer)
            .unwrap()
            .is_none());
        assert!(!f.stage.exists());
    }

    #[test]
    fn prepare_stages_the_vendor_dir_and_config_and_returns_read_only_mounts() {
        let f = fixture();
        let vendorer = FakeVendorer::printing(PRINTED);
        let offline = OfflineCargo::prepare(&f.repo, &f.stage, &vendorer)
            .unwrap()
            .unwrap();

        let calls = vendorer.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (manifest, vendor_dir, cwd) = &calls[0];
        assert_eq!(manifest, &f.repo.join("Cargo.toml"));
        assert_eq!(vendor_dir, &f.stage.join("vendor"));
        assert_eq!(cwd, &f.stage.join("cwd"));
        assert!(!cwd.starts_with(&f.repo));

        assert_eq!(offline.stage(), f.stage);
        assert_eq!(
            offline.env(),
            [("CARGO_NET_OFFLINE".to_string(), "true".to_string())]
        );
        let args: Vec<String> = offline.mounts().iter().map(|m| m.run_arg()).collect();
        assert_eq!(
            args,
            [
                format!(
                    "-v={}:{CONTAINER_VENDOR_DIR}:ro,z",
                    f.stage.join("vendor").display()
                ),
                format!(
                    "-v={}:{CONTAINER_CARGO_CONFIG}:ro,z",
                    f.stage.join("config.toml").display()
                ),
            ]
        );
        let written = std::fs::read_to_string(f.stage.join("config.toml")).unwrap();
        assert!(written.contains(CONTAINER_VENDOR_DIR));

        offline.cleanup();
        assert!(!f.stage.exists());
        offline.cleanup();
    }

    #[test]
    fn prepare_replaces_a_stale_stage() {
        let f = fixture();
        std::fs::create_dir_all(&f.stage).unwrap();
        std::fs::write(f.stage.join("stale"), "x").unwrap();
        let vendorer = FakeVendorer::printing(PRINTED);
        OfflineCargo::prepare(&f.repo, &f.stage, &vendorer)
            .unwrap()
            .unwrap();
        assert!(!f.stage.join("stale").exists());
    }

    #[test]
    fn a_failed_vendor_fails_closed_with_the_reason_and_removes_the_stage() {
        let f = fixture();
        let mut vendorer = FakeVendorer::printing(PRINTED);
        vendorer.success = false;
        match OfflineCargo::prepare(&f.repo, &f.stage, &vendorer) {
            Err(OfflineCargoError::VendorFailed(reason)) => assert_eq!(reason, "no lock file"),
            other => panic!("{other:?}"),
        }
        assert!(!f.stage.exists());
    }

    #[test]
    fn a_missing_vendor_directory_or_bad_snippet_fails_closed() {
        let f = fixture();
        let mut vendorer = FakeVendorer::printing(PRINTED);
        vendorer.create_vendor_dir = false;
        assert!(matches!(
            OfflineCargo::prepare(&f.repo, &f.stage, &vendorer),
            Err(OfflineCargoError::BadVendorConfig(_))
        ));
        assert!(!f.stage.exists());

        let g = fixture();
        let vendorer = FakeVendorer::printing("garbage = [");
        assert!(matches!(
            OfflineCargo::prepare(&g.repo, &g.stage, &vendorer),
            Err(OfflineCargoError::BadVendorConfig(_))
        ));
    }

    #[test]
    fn a_repo_source_override_is_refused_before_anything_is_vendored() {
        let f = fixture();
        write_config(
            &f.repo,
            ".cargo/config.toml",
            "[source.evil]\ndirectory = \"/x\"\n",
        );
        let vendorer = FakeVendorer::printing(PRINTED);
        assert!(matches!(
            OfflineCargo::prepare(&f.repo, &f.stage, &vendorer),
            Err(OfflineCargoError::RepoOverride { .. })
        ));
        assert!(vendorer.calls.lock().unwrap().is_empty());
        assert!(!f.stage.exists());
    }

    #[test]
    fn an_uncreatable_stage_is_an_io_error() {
        let f = fixture();
        let blocked = f.repo.join("Cargo.toml").join("stage");
        let vendorer = FakeVendorer::printing(PRINTED);
        assert!(matches!(
            OfflineCargo::prepare(&f.repo, &blocked, &vendorer),
            Err(OfflineCargoError::Io(_))
        ));
    }

    #[test]
    fn a_vendorer_io_error_propagates() {
        struct Broken;
        impl Vendorer for Broken {
            fn vendor(&self, _: &Path, _: &Path, _: &Path) -> std::io::Result<RunOutput> {
                Err(std::io::Error::other("no cargo"))
            }
        }
        let f = fixture();
        assert!(matches!(
            OfflineCargo::prepare(&f.repo, &f.stage, &Broken),
            Err(OfflineCargoError::Io(_))
        ));
        assert!(!f.stage.exists());
    }

    #[test]
    fn real_cargo_vendor_fails_closed_without_a_lockfile() {
        let f = fixture();
        let out = CargoVendor
            .vendor(&f.repo.join("Cargo.toml"), &f.stage.join("v"), &f.repo)
            .unwrap();
        assert!(!out.success);
        assert!(out.stderr.contains("Cargo.toml") || out.stderr.contains("lock"));
    }

    #[test]
    fn real_cargo_vendor_prints_nothing_for_a_crate_without_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"nodeps\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
        let generated = Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(generated.status.success());
        let stage = root.path().join("stage");
        let offline = OfflineCargo::prepare(&repo, &stage, &CargoVendor).unwrap();
        assert!(offline.is_none());
        assert!(!stage.exists());
    }

    proptest! {
        #[test]
        fn every_directory_source_is_rewritten_and_others_are_kept(
            dirs in prop::collection::btree_map("[a-z][a-z-]{0,8}", "[ -~]{0,20}", 1..4),
            gits in prop::collection::btree_map("g[a-z]{1,6}", "https://[a-z]{1,8}\\.example/[a-z]{1,6}", 0..3),
        ) {
            let mut sources = toml::Table::new();
            for (name, dir) in &dirs {
                let mut entry = toml::Table::new();
                entry.insert("directory".into(), toml::Value::String(dir.clone()));
                sources.insert(format!("d-{name}"), toml::Value::Table(entry));
            }
            for (name, url) in &gits {
                let mut entry = toml::Table::new();
                entry.insert("git".into(), toml::Value::String(url.clone()));
                sources.insert(name.clone(), toml::Value::Table(entry));
            }
            let mut table = toml::Table::new();
            table.insert("source".into(), toml::Value::Table(sources));
            let rendered = render_config(&toml::to_string(&table).unwrap()).unwrap();
            let back: toml::Table = toml::from_str(&rendered).unwrap();
            let out = back["source"].as_table().unwrap();
            for name in dirs.keys() {
                prop_assert_eq!(out[&format!("d-{name}")]["directory"].as_str(), Some(CONTAINER_VENDOR_DIR));
            }
            for (name, url) in &gits {
                prop_assert_eq!(out[name]["git"].as_str(), Some(url.as_str()));
                prop_assert!(out[name].as_table().unwrap().get("directory").is_none());
            }
            prop_assert_eq!(back["net"]["offline"].as_bool(), Some(true));
        }

        #[test]
        fn render_config_never_panics(text in ".*") {
            let _ = render_config(&text);
        }
    }
}
