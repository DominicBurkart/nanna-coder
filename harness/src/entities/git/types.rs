//! Git entity types
//!
//! Represents git repository state for version control awareness.
//! Full implementation tracked in issue #22.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// Git repository entity representing the state of a git repository
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepository {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    pub root_path: PathBuf,
    pub current_branch: String,
    pub head_commit: String,
    pub is_dirty: bool,
    pub staged_files: Vec<String>,
    pub modified_files: Vec<String>,
    pub untracked_files: Vec<String>,
}

#[async_trait]
impl Entity for GitRepository {
    fn metadata(&self) -> &EntityMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.metadata
    }

    fn to_json(&self) -> EntityResult<String> {
        serde_json::to_string(self)
            .map_err(|e| crate::entities::EntityError::SerializationError(e.to_string()))
    }
}

impl GitRepository {
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Git),
            root_path: PathBuf::new(),
            current_branch: String::new(),
            head_commit: String::new(),
            is_dirty: false,
            staged_files: Vec::new(),
            modified_files: Vec::new(),
            untracked_files: Vec::new(),
        }
    }

    pub fn detect(path: &std::path::Path) -> Option<Self> {
        let root = Self::find_git_root(path)?;

        let current_branch = Self::get_current_branch(&root).unwrap_or_default();
        let head_commit = Self::get_head_commit(&root).unwrap_or_default();
        let (staged, modified, untracked) = Self::get_status(&root).unwrap_or_default();

        let is_dirty = !staged.is_empty() || !modified.is_empty();

        Some(Self {
            metadata: EntityMetadata::new(EntityType::Git),
            root_path: root,
            current_branch,
            head_commit,
            is_dirty,
            staged_files: staged,
            modified_files: modified,
            untracked_files: untracked,
        })
    }

    fn find_git_root(path: &std::path::Path) -> Option<PathBuf> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(path)
            .output()
            .ok()?;

        if output.status.success() {
            let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Some(PathBuf::from(root))
        } else {
            None
        }
    }

    fn get_current_branch(root: &std::path::Path) -> Option<String> {
        let output = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(root)
            .output()
            .ok()?;

        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            None
        }
    }

    fn get_head_commit(root: &std::path::Path) -> Option<String> {
        let output = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(root)
            .output()
            .ok()?;

        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            None
        }
    }

    fn get_status(root: &std::path::Path) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(root)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let mut staged = Vec::new();
        let mut modified = Vec::new();
        let mut untracked = Vec::new();

        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if line.len() < 3 {
                continue;
            }

            let index_status = line.chars().next().unwrap_or(' ');
            let worktree_status = line.chars().nth(1).unwrap_or(' ');
            let file = line[3..].to_string();

            match index_status {
                'A' | 'M' | 'D' | 'R' | 'C' => staged.push(file.clone()),
                '?' => {}
                _ => {}
            }

            match worktree_status {
                'M' | 'D' => {
                    if !staged.contains(&file) {
                        modified.push(file.clone());
                    }
                }
                '?' => untracked.push(file),
                _ => {}
            }
        }

        Some((staged, modified, untracked))
    }

    pub fn has_uncommitted_changes(&self) -> bool {
        self.is_dirty
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        parts.push(format!("branch: {}", self.current_branch));
        parts.push(format!("commit: {}", self.head_commit));

        if self.is_dirty {
            let mut changes = Vec::new();
            if !self.staged_files.is_empty() {
                changes.push(format!("{} staged", self.staged_files.len()));
            }
            if !self.modified_files.is_empty() {
                changes.push(format!("{} modified", self.modified_files.len()));
            }
            if !self.untracked_files.is_empty() {
                changes.push(format!("{} untracked", self.untracked_files.len()));
            }
            parts.push(format!("changes: {}", changes.join(", ")));
        } else {
            parts.push("clean".to_string());
        }

        parts.join(" | ")
    }
}

impl Default for GitRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_repository_new() {
        let repo = GitRepository::new();
        assert!(repo.root_path.as_os_str().is_empty());
        assert!(repo.current_branch.is_empty());
        assert!(!repo.is_dirty);
    }

    #[test]
    fn test_git_repository_detect() {
        if let Some(repo) = GitRepository::detect(std::path::Path::new(".")) {
            assert!(!repo.head_commit.is_empty());
        }
    }

    #[test]
    fn test_git_repository_summary() {
        let mut repo = GitRepository::new();
        repo.current_branch = "main".to_string();
        repo.head_commit = "abc123".to_string();
        repo.is_dirty = true;
        repo.staged_files = vec!["file1.rs".to_string()];
        repo.modified_files = vec!["file2.rs".to_string()];

        let summary = repo.summary();
        assert!(summary.contains("main"));
        assert!(summary.contains("abc123"));
        assert!(summary.contains("1 staged"));
        assert!(summary.contains("1 modified"));
    }
}
