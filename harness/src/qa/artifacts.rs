//! Where QA evidence is written: `<workspace>/.nanna-artifacts/qa`, with
//! endpoint reports as `endpoints-<n>.json` and browser runs as
//! `browser-<n>/` directories, numbered from 1 in order of creation.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Directory under the workspace root that holds harness artefacts; it is
/// kept out of the patch a task produces.
pub const ARTIFACT_DIR: &str = ".nanna-artifacts";
/// Subdirectory of [`ARTIFACT_DIR`] for QA evidence.
pub const QA_DIR: &str = "qa";
/// File name prefix of endpoint reports.
pub const ENDPOINT_PREFIX: &str = "endpoints-";
/// Directory name prefix of browser runs.
pub const BROWSER_PREFIX: &str = "browser-";
/// Report written inside a browser run directory.
pub const BROWSER_REPORT_FILE: &str = "report.json";

/// The QA artefact directory of one workspace.
///
/// ```
/// use harness::qa::artifacts::QaArtifacts;
/// use std::path::Path;
///
/// let artifacts = QaArtifacts::new(Path::new("/work/task"));
/// assert_eq!(artifacts.root(), Path::new("/work/task/.nanna-artifacts/qa"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QaArtifacts {
    root: PathBuf,
}

impl QaArtifacts {
    pub fn new(workspace_path: &Path) -> Self {
        Self {
            root: workspace_path.join(ARTIFACT_DIR).join(QA_DIR),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of the next endpoint report, `endpoints-<n>.json`; creates the
    /// directory.
    pub fn next_endpoint_report(&self) -> std::io::Result<PathBuf> {
        let n = self.next_index(ENDPOINT_PREFIX, ".json")?;
        Ok(self.root.join(format!("{ENDPOINT_PREFIX}{n}.json")))
    }

    /// The next browser run directory, `browser-<n>/`, created.
    pub fn next_browser_dir(&self) -> std::io::Result<PathBuf> {
        let n = self.next_index(BROWSER_PREFIX, "")?;
        let dir = self.root.join(format!("{BROWSER_PREFIX}{n}"));
        std::fs::create_dir(&dir)?;
        Ok(dir)
    }

    fn next_index(&self, prefix: &str, suffix: &str) -> std::io::Result<usize> {
        std::fs::create_dir_all(&self.root)?;
        let names: Vec<String> = std::fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        Ok(next_index(&names, prefix, suffix))
    }
}

/// One more than the highest `<prefix><n><suffix>` among `names`, or 1.
///
/// ```
/// use harness::qa::artifacts::next_index;
///
/// let names = ["endpoints-1.json", "endpoints-7.json", "endpoints-x.json", "browser-3"];
/// let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
/// assert_eq!(next_index(&names, "endpoints-", ".json"), 8);
/// assert_eq!(next_index(&names, "browser-", ""), 4);
/// assert_eq!(next_index(&[], "browser-", ""), 1);
/// ```
pub fn next_index(names: &[String], prefix: &str, suffix: &str) -> usize {
    names
        .iter()
        .filter_map(|name| index_of(name, prefix, suffix))
        .max()
        .map_or(1, |max| max + 1)
}

fn index_of(name: &str, prefix: &str, suffix: &str) -> Option<usize> {
    name.strip_prefix(prefix)?
        .strip_suffix(suffix)?
        .parse()
        .ok()
}

/// Write `value` as pretty JSON to `path`.
pub fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn reports_and_run_directories_are_numbered_in_creation_order() {
        let dir = TempDir::new().unwrap();
        let artifacts = QaArtifacts::new(dir.path());
        assert_eq!(
            artifacts.root(),
            dir.path().join(".nanna-artifacts").join("qa")
        );
        let first = artifacts.next_endpoint_report().unwrap();
        assert_eq!(first, artifacts.root().join("endpoints-1.json"));
        assert!(artifacts.root().is_dir());
        assert_eq!(
            artifacts.next_endpoint_report().unwrap(),
            first,
            "nothing written yet, so the index is unchanged"
        );
        write_json(&first, &json!({ "passed": 1 })).unwrap();
        assert_eq!(
            artifacts.next_endpoint_report().unwrap(),
            artifacts.root().join("endpoints-2.json")
        );
        let text = std::fs::read_to_string(&first).unwrap();
        assert_eq!(text, "{\n  \"passed\": 1\n}");
        std::fs::write(artifacts.root().join("endpoints-9.json"), "{}").unwrap();
        std::fs::write(artifacts.root().join("endpoints-9.txt"), "{}").unwrap();
        assert_eq!(
            artifacts.next_endpoint_report().unwrap(),
            artifacts.root().join("endpoints-10.json")
        );
        let run = artifacts.next_browser_dir().unwrap();
        assert_eq!(run, artifacts.root().join("browser-1"));
        assert!(run.is_dir());
        assert_eq!(
            artifacts.next_browser_dir().unwrap(),
            artifacts.root().join("browser-2")
        );
        assert_eq!(artifacts.clone(), artifacts);
        assert!(format!("{artifacts:?}").contains("qa"));
    }

    #[test]
    fn index_parsing_ignores_foreign_names() {
        assert_eq!(index_of("browser-12", "browser-", ""), Some(12));
        assert_eq!(index_of("browser-", "browser-", ""), None);
        assert_eq!(index_of("browser-a", "browser-", ""), None);
        assert_eq!(index_of("endpoints-3.json", "endpoints-", ".json"), Some(3));
        assert_eq!(index_of("endpoints-3", "endpoints-", ".json"), None);
        assert_eq!(index_of("other", "endpoints-", ".json"), None);
    }

    #[test]
    fn next_index_of_an_empty_slice_is_one() {
        assert_eq!(next_index(&[], "browser-", ""), 1);
        let names = vec!["browser-2".to_string(), "browser-9".to_string()];
        assert_eq!(next_index(&names, "browser-", ""), 10);
    }

    #[test]
    fn errors_surface_when_the_root_cannot_be_used() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        let artifacts = QaArtifacts::new(&file);
        assert!(artifacts.next_endpoint_report().is_err());
        assert!(artifacts.next_browser_dir().is_err());
        assert!(write_json(&dir.path().join("missing").join("r.json"), &json!(1)).is_err());
    }
}
