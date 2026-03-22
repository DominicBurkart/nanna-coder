//! Evaluation case format for SWE-bench-style testing.
//!
//! Each eval case pairs a `task.toml` definition with a fixture repository.
//! The `task.toml` describes what the agent should do and how to validate the result.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// A complete evaluation case loaded from a `task.toml` file.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EvalCase {
    pub case: CaseInfo,
    pub task: TaskSpec,
    #[serde(default)]
    pub expected: ExpectedResult,
    #[serde(default)]
    pub metadata: CaseMetadata,
}

/// Identity and description of an eval case.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CaseInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// The task specification given to the agent.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TaskSpec {
    pub prompt: String,
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "rust".to_string()
}

/// Machine-checkable validation criteria for the result.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ExpectedResult {
    #[serde(default)]
    pub files_changed: Vec<String>,
    #[serde(default = "default_true")]
    pub build_must_pass: bool,
    #[serde(default)]
    pub tests_must_pass: bool,
    #[serde(default)]
    pub required_symbols: Vec<String>,
}

impl Default for ExpectedResult {
    fn default() -> Self {
        Self {
            files_changed: Vec::new(),
            build_must_pass: true,
            tests_must_pass: false,
            required_symbols: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Optional organizational metadata.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CaseMetadata {
    #[serde(default = "default_difficulty")]
    pub difficulty: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

impl Default for CaseMetadata {
    fn default() -> Self {
        Self {
            difficulty: default_difficulty(),
            tags: Vec::new(),
            timeout_secs: default_timeout(),
        }
    }
}

fn default_difficulty() -> String {
    "medium".to_string()
}

fn default_timeout() -> u64 {
    300
}

/// Errors that can occur when loading eval cases.
#[derive(Debug, thiserror::Error)]
pub enum EvalCaseError {
    #[error("failed to read {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("failed to parse TOML: {0}")]
    Parse(#[from] toml::de::Error),
}

impl EvalCase {
    /// Load an `EvalCase` from a `task.toml` file path.
    pub fn from_toml_file(path: &Path) -> Result<Self, EvalCaseError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| EvalCaseError::Io(path.to_path_buf(), e))?;
        Self::from_toml_str(&content)
    }

    /// Parse an `EvalCase` from a TOML string.
    pub fn from_toml_str(content: &str) -> Result<Self, EvalCaseError> {
        toml::from_str(content).map_err(EvalCaseError::Parse)
    }

    /// Resolve the repo directory path relative to a `task.toml` path.
    pub fn repo_path(task_toml_path: &Path) -> PathBuf {
        task_toml_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("repo")
    }

    /// Discover all eval cases under a directory, sorted by directory name.
    pub fn discover(cases_dir: &Path) -> Result<Vec<(Self, PathBuf)>, EvalCaseError> {
        let mut cases = Vec::new();
        if !cases_dir.is_dir() {
            return Ok(cases);
        }
        let mut entries: Vec<_> = std::fs::read_dir(cases_dir)
            .map_err(|e| EvalCaseError::Io(cases_dir.to_path_buf(), e))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let task_toml = entry.path().join("task.toml");
            if task_toml.is_file() {
                let eval_case = Self::from_toml_file(&task_toml)?;
                cases.push((eval_case, entry.path()));
            }
        }
        Ok(cases)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_minimal() {
        let toml_str = r#"
[case]
id = "test-001"
name = "Test case"
description = "A minimal test"

[task]
prompt = "Do something"
"#;
        let case = EvalCase::from_toml_str(toml_str).unwrap();
        assert_eq!(case.case.id, "test-001");
        assert_eq!(case.case.name, "Test case");
        assert_eq!(case.task.prompt, "Do something");
        assert_eq!(case.task.language, "rust");
        assert!(case.expected.build_must_pass);
        assert!(!case.expected.tests_must_pass);
        assert_eq!(case.metadata.difficulty, "medium");
        assert_eq!(case.metadata.timeout_secs, 300);
    }

    #[test]
    fn test_deserialize_full() {
        let toml_str = r#"
[case]
id = "full-001"
name = "Full case"
description = "All fields set"

[task]
prompt = "Add a function"
language = "python"

[expected]
files_changed = ["src/lib.rs"]
build_must_pass = true
tests_must_pass = true
required_symbols = ["greet"]

[metadata]
difficulty = "hard"
tags = ["function", "addition"]
timeout_secs = 60
"#;
        let case = EvalCase::from_toml_str(toml_str).unwrap();
        assert_eq!(case.task.language, "python");
        assert!(case.expected.tests_must_pass);
        assert_eq!(case.expected.required_symbols, vec!["greet"]);
        assert_eq!(case.metadata.difficulty, "hard");
        assert_eq!(case.metadata.tags, vec!["function", "addition"]);
        assert_eq!(case.metadata.timeout_secs, 60);
    }

    #[test]
    fn test_invalid_toml() {
        let result = EvalCase::from_toml_str("not valid {{{{");
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_required_field() {
        let toml_str = r#"
[case]
id = "incomplete"
name = "Incomplete"
description = "Missing task section"
"#;
        let result = EvalCase::from_toml_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_repo_path() {
        let path = Path::new("/evals/cases/happy-path-001/task.toml");
        let repo = EvalCase::repo_path(path);
        assert_eq!(repo, Path::new("/evals/cases/happy-path-001/repo"));
    }
}
