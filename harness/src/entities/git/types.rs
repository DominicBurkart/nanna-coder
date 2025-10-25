//! Git entity types
//!
//! This module defines all git version control entities for tracking repository state,
//! branches, commits, and file changes as specified in issue #22.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Git repository entity
///
/// Tracks repository metadata including remotes, default branch, and configuration.
///
/// # Examples
///
/// ```
/// use harness::entities::git::GitRepository;
///
/// let repo = GitRepository::new(
///     "git@github.com:DominicBurkart/nanna-coder.git".to_string(),
///     "main".to_string(),
/// );
/// assert_eq!(repo.default_branch, "main");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepository {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Primary remote URL (typically "origin")
    pub remote_url: String,

    /// Default branch name
    pub default_branch: String,

    /// All configured remotes (name -> URL)
    pub remotes: HashMap<String, String>,

    /// Repository configuration (subset of .gitconfig)
    pub config: HashMap<String, String>,

    /// Submodules (path -> URL)
    pub submodules: HashMap<String, String>,
}

impl GitRepository {
    /// Create a new git repository entity
    pub fn new(remote_url: String, default_branch: String) -> Self {
        let mut remotes = HashMap::new();
        remotes.insert("origin".to_string(), remote_url.clone());

        Self {
            metadata: EntityMetadata::new(EntityType::Git),
            remote_url,
            default_branch,
            remotes,
            config: HashMap::new(),
            submodules: HashMap::new(),
        }
    }

    /// Add a remote to the repository
    pub fn add_remote(&mut self, name: String, url: String) {
        self.remotes.insert(name, url);
    }

    /// Add a submodule
    pub fn add_submodule(&mut self, path: String, url: String) {
        self.submodules.insert(path, url);
    }
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

/// Git branch entity
///
/// Tracks local and remote branches with their tracking relationships.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBranch {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Branch name
    pub name: String,

    /// Whether this is a local or remote branch
    pub is_remote: bool,

    /// Remote name if this is a remote branch
    pub remote: Option<String>,

    /// SHA of the HEAD commit on this branch
    pub head_sha: String,

    /// Upstream branch this tracks (if any)
    pub upstream: Option<String>,

    /// Commits ahead of upstream
    pub ahead: Option<usize>,

    /// Commits behind upstream
    pub behind: Option<usize>,
}

impl GitBranch {
    /// Create a new local branch
    pub fn new_local(name: String, head_sha: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Git),
            name,
            is_remote: false,
            remote: None,
            head_sha,
            upstream: None,
            ahead: None,
            behind: None,
        }
    }

    /// Create a new remote branch
    pub fn new_remote(remote: String, name: String, head_sha: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Git),
            name,
            is_remote: true,
            remote: Some(remote),
            head_sha,
            upstream: None,
            ahead: None,
            behind: None,
        }
    }

    /// Set tracking information
    pub fn set_tracking(&mut self, upstream: String, ahead: usize, behind: usize) {
        self.upstream = Some(upstream);
        self.ahead = Some(ahead);
        self.behind = Some(behind);
    }

    /// Get tracking status as a string
    pub fn tracking_status(&self) -> Option<String> {
        match (&self.ahead, &self.behind) {
            (Some(a), Some(b)) if *a > 0 && *b > 0 => Some(format!("{} ahead, {} behind", a, b)),
            (Some(a), _) if *a > 0 => Some(format!("{} ahead", a)),
            (_, Some(b)) if *b > 0 => Some(format!("{} behind", b)),
            _ => None,
        }
    }
}

#[async_trait]
impl Entity for GitBranch {
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

/// Git commit entity
///
/// Tracks individual commits with their metadata and relationships.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommit {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Full commit SHA
    pub sha: String,

    /// Short commit SHA (first 7 characters)
    pub short_sha: String,

    /// Commit message title (first line)
    pub title: String,

    /// Commit message body (remaining lines)
    pub description: String,

    /// Author name
    pub author: String,

    /// Author email
    pub author_email: String,

    /// Commit timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,

    /// Parent commit SHAs
    pub parents: Vec<String>,

    /// Files changed in this commit
    pub changed_files: Vec<String>,
}

impl GitCommit {
    /// Create a new commit entity
    pub fn new(
        sha: String,
        title: String,
        author: String,
        author_email: String,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let short_sha = sha.chars().take(7).collect();

        Self {
            metadata: EntityMetadata::new(EntityType::Git),
            sha,
            short_sha,
            title,
            description: String::new(),
            author,
            author_email,
            timestamp,
            parents: Vec::new(),
            changed_files: Vec::new(),
        }
    }

    /// Add a parent commit
    pub fn add_parent(&mut self, parent_sha: String) {
        self.parents.push(parent_sha);
    }

    /// Add a changed file
    pub fn add_changed_file(&mut self, file_path: String) {
        self.changed_files.push(file_path);
    }

    /// Check if this is a merge commit
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }

    /// Check if this is a root commit
    pub fn is_root(&self) -> bool {
        self.parents.is_empty()
    }
}

#[async_trait]
impl Entity for GitCommit {
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

/// Git file status
///
/// Represents the state of a file in the working directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitFileStatus {
    /// File is unmodified
    Unmodified,

    /// File is modified and staged
    Staged,

    /// File is modified but not staged
    Modified,

    /// File is newly created and staged
    Added,

    /// File is newly created but not staged
    Untracked,

    /// File is deleted
    Deleted,

    /// File is renamed
    Renamed,

    /// File is ignored by .gitignore
    Ignored,

    /// File has merge conflicts
    Conflicted,
}

/// Git working directory state
///
/// Aggregates the current state of all files in the working directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitWorkingDirectory {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Files by status
    pub files: HashMap<String, GitFileStatus>,

    /// Number of staged files
    pub staged_count: usize,

    /// Number of unstaged modified files
    pub unstaged_count: usize,

    /// Number of untracked files
    pub untracked_count: usize,
}

impl GitWorkingDirectory {
    /// Create a new working directory state
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Git),
            files: HashMap::new(),
            staged_count: 0,
            unstaged_count: 0,
            untracked_count: 0,
        }
    }

    /// Add a file with its status
    pub fn add_file(&mut self, path: String, status: GitFileStatus) {
        self.files.insert(path, status);

        // Update counts
        match status {
            GitFileStatus::Staged | GitFileStatus::Added => self.staged_count += 1,
            GitFileStatus::Modified => self.unstaged_count += 1,
            GitFileStatus::Untracked => self.untracked_count += 1,
            _ => {}
        }
    }

    /// Check if the working directory is clean
    pub fn is_clean(&self) -> bool {
        self.staged_count == 0 && self.unstaged_count == 0 && self.untracked_count == 0
    }
}

impl Default for GitWorkingDirectory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Entity for GitWorkingDirectory {
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

/// Diff between two commits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitDiff {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Source commit SHA
    pub from_sha: String,

    /// Target commit SHA
    pub to_sha: String,

    /// Files changed
    pub changed_files: Vec<String>,

    /// Lines added
    pub additions: usize,

    /// Lines deleted
    pub deletions: usize,
}

impl GitDiff {
    /// Create a new diff
    pub fn new(from_sha: String, to_sha: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Git),
            from_sha,
            to_sha,
            changed_files: Vec::new(),
            additions: 0,
            deletions: 0,
        }
    }

    /// Get summary string like "+66/-233"
    pub fn summary(&self) -> String {
        format!("+{}/-{}", self.additions, self.deletions)
    }
}

#[async_trait]
impl Entity for GitDiff {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_repository_creation() {
        let repo = GitRepository::new(
            "git@github.com:user/repo.git".to_string(),
            "main".to_string(),
        );

        assert_eq!(repo.remote_url, "git@github.com:user/repo.git");
        assert_eq!(repo.default_branch, "main");
        assert_eq!(repo.remotes.len(), 1);
        assert_eq!(
            repo.remotes.get("origin"),
            Some(&"git@github.com:user/repo.git".to_string())
        );
    }

    #[test]
    fn test_git_repository_add_remote() {
        let mut repo = GitRepository::new(
            "git@github.com:user/repo.git".to_string(),
            "main".to_string(),
        );

        repo.add_remote(
            "backup".to_string(),
            "https://gitlab.com/user/repo.git".to_string(),
        );
        assert_eq!(repo.remotes.len(), 2);
        assert_eq!(
            repo.remotes.get("backup"),
            Some(&"https://gitlab.com/user/repo.git".to_string())
        );
    }

    #[test]
    fn test_git_repository_entity_trait() {
        let repo = GitRepository::new(
            "git@github.com:user/repo.git".to_string(),
            "main".to_string(),
        );

        assert_eq!(repo.metadata().entity_type, EntityType::Git);
        assert!(repo.to_json().is_ok());
    }

    #[test]
    fn test_git_branch_local() {
        let branch = GitBranch::new_local("feature-branch".to_string(), "abc123def456".to_string());

        assert_eq!(branch.name, "feature-branch");
        assert!(!branch.is_remote);
        assert_eq!(branch.remote, None);
        assert_eq!(branch.head_sha, "abc123def456");
    }

    #[test]
    fn test_git_branch_remote() {
        let branch = GitBranch::new_remote(
            "origin".to_string(),
            "main".to_string(),
            "def456abc123".to_string(),
        );

        assert_eq!(branch.name, "main");
        assert!(branch.is_remote);
        assert_eq!(branch.remote, Some("origin".to_string()));
    }

    #[test]
    fn test_git_branch_tracking() {
        let mut branch = GitBranch::new_local("feature".to_string(), "abc123".to_string());

        branch.set_tracking("origin/main".to_string(), 3, 1);
        assert_eq!(branch.upstream, Some("origin/main".to_string()));
        assert_eq!(branch.ahead, Some(3));
        assert_eq!(branch.behind, Some(1));
        assert_eq!(
            branch.tracking_status(),
            Some("3 ahead, 1 behind".to_string())
        );
    }

    #[test]
    fn test_git_branch_tracking_status_ahead_only() {
        let mut branch = GitBranch::new_local("feature".to_string(), "abc123".to_string());
        branch.set_tracking("origin/main".to_string(), 5, 0);
        assert_eq!(branch.tracking_status(), Some("5 ahead".to_string()));
    }

    #[test]
    fn test_git_branch_tracking_status_behind_only() {
        let mut branch = GitBranch::new_local("feature".to_string(), "abc123".to_string());
        branch.set_tracking("origin/main".to_string(), 0, 2);
        assert_eq!(branch.tracking_status(), Some("2 behind".to_string()));
    }

    #[test]
    fn test_git_commit_creation() {
        let commit = GitCommit::new(
            "abc123def456789".to_string(),
            "Fix authentication bug".to_string(),
            "John Doe".to_string(),
            "john@example.com".to_string(),
            chrono::Utc::now(),
        );

        assert_eq!(commit.sha, "abc123def456789");
        assert_eq!(commit.short_sha, "abc123d");
        assert_eq!(commit.title, "Fix authentication bug");
        assert_eq!(commit.author, "John Doe");
        assert_eq!(commit.author_email, "john@example.com");
    }

    #[test]
    fn test_git_commit_parents() {
        let mut commit = GitCommit::new(
            "abc123".to_string(),
            "Merge branch".to_string(),
            "Jane Doe".to_string(),
            "jane@example.com".to_string(),
            chrono::Utc::now(),
        );

        assert!(commit.is_root());
        assert!(!commit.is_merge());

        commit.add_parent("parent1".to_string());
        assert!(!commit.is_root());
        assert!(!commit.is_merge());

        commit.add_parent("parent2".to_string());
        assert!(commit.is_merge());
    }

    #[test]
    fn test_git_commit_changed_files() {
        let mut commit = GitCommit::new(
            "abc123".to_string(),
            "Update files".to_string(),
            "Dev".to_string(),
            "dev@example.com".to_string(),
            chrono::Utc::now(),
        );

        commit.add_changed_file("src/main.rs".to_string());
        commit.add_changed_file("Cargo.toml".to_string());

        assert_eq!(commit.changed_files.len(), 2);
        assert!(commit.changed_files.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn test_git_working_directory_clean() {
        let wd = GitWorkingDirectory::new();
        assert!(wd.is_clean());
        assert_eq!(wd.staged_count, 0);
        assert_eq!(wd.unstaged_count, 0);
        assert_eq!(wd.untracked_count, 0);
    }

    #[test]
    fn test_git_working_directory_with_changes() {
        let mut wd = GitWorkingDirectory::new();

        wd.add_file("src/main.rs".to_string(), GitFileStatus::Modified);
        wd.add_file("Cargo.toml".to_string(), GitFileStatus::Staged);
        wd.add_file("new_file.rs".to_string(), GitFileStatus::Untracked);

        assert!(!wd.is_clean());
        assert_eq!(wd.staged_count, 1);
        assert_eq!(wd.unstaged_count, 1);
        assert_eq!(wd.untracked_count, 1);
    }

    #[test]
    fn test_git_file_status_variants() {
        let statuses = vec![
            GitFileStatus::Unmodified,
            GitFileStatus::Staged,
            GitFileStatus::Modified,
            GitFileStatus::Added,
            GitFileStatus::Untracked,
            GitFileStatus::Deleted,
            GitFileStatus::Renamed,
            GitFileStatus::Ignored,
            GitFileStatus::Conflicted,
        ];

        // Ensure all variants are serializable
        for status in statuses {
            let json = serde_json::to_string(&status);
            assert!(json.is_ok());
        }
    }

    #[test]
    fn test_git_diff_creation() {
        let diff = GitDiff::new("abc123".to_string(), "def456".to_string());

        assert_eq!(diff.from_sha, "abc123");
        assert_eq!(diff.to_sha, "def456");
        assert_eq!(diff.additions, 0);
        assert_eq!(diff.deletions, 0);
    }

    #[test]
    fn test_git_diff_summary() {
        let mut diff = GitDiff::new("abc123".to_string(), "def456".to_string());
        diff.additions = 66;
        diff.deletions = 233;

        assert_eq!(diff.summary(), "+66/-233");
    }

    #[test]
    fn test_all_entities_implement_entity_trait() {
        // Ensure all git entities implement the Entity trait correctly
        let repo = GitRepository::new("url".to_string(), "main".to_string());
        let branch = GitBranch::new_local("branch".to_string(), "sha".to_string());
        let commit = GitCommit::new(
            "sha".to_string(),
            "title".to_string(),
            "author".to_string(),
            "email".to_string(),
            chrono::Utc::now(),
        );
        let wd = GitWorkingDirectory::new();
        let diff = GitDiff::new("from".to_string(), "to".to_string());

        // All should serialize to JSON
        assert!(repo.to_json().is_ok());
        assert!(branch.to_json().is_ok());
        assert!(commit.to_json().is_ok());
        assert!(wd.to_json().is_ok());
        assert!(diff.to_json().is_ok());

        // All should have Git entity type
        assert_eq!(repo.entity_type(), EntityType::Git);
        assert_eq!(branch.entity_type(), EntityType::Git);
        assert_eq!(commit.entity_type(), EntityType::Git);
        assert_eq!(wd.entity_type(), EntityType::Git);
        assert_eq!(diff.entity_type(), EntityType::Git);
    }

    #[test]
    fn test_serialization_deserialization_roundtrip() {
        let repo = GitRepository::new(
            "git@github.com:user/repo.git".to_string(),
            "main".to_string(),
        );

        let json = repo.to_json().unwrap();
        let deserialized: GitRepository = serde_json::from_str(&json).unwrap();

        assert_eq!(repo.remote_url, deserialized.remote_url);
        assert_eq!(repo.default_branch, deserialized.default_branch);
    }
}
