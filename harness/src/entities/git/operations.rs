//! Git operations layer
//!
//! This module provides operations for reading git state from actual repositories
//! using git2-rs.

use super::types::{
    GitBranch, GitCommit, GitDiff, GitFileStatus, GitRepository, GitWorkingDirectory,
};
use git2::{BranchType, Repository, Status};
use std::path::Path;
use thiserror::Error;

/// Errors that can occur during git operations
#[derive(Error, Debug)]
pub enum GitOperationError {
    #[error("Git error: {0}")]
    Git(#[from] git2::Error),

    #[error("Repository not found at path: {0}")]
    RepositoryNotFound(String),

    #[error("Invalid UTF-8 in git data: {0}")]
    InvalidUtf8(String),

    #[error("No HEAD commit found")]
    NoHeadCommit,

    #[error("Branch not found: {0}")]
    BranchNotFound(String),
}

pub type GitOperationResult<T> = Result<T, GitOperationError>;

/// Read git repository metadata from a path
pub fn read_repository(path: impl AsRef<Path>) -> GitOperationResult<GitRepository> {
    let repo = Repository::open(path.as_ref())?;

    // Get default branch (usually "main" or "master")
    let head = repo.head()?;
    let default_branch = if head.is_branch() {
        head.shorthand()
            .ok_or_else(|| GitOperationError::InvalidUtf8("branch name".to_string()))?
            .to_string()
    } else {
        "main".to_string() // Fallback
    };

    // Get origin remote URL
    let origin = repo.find_remote("origin").ok();
    let remote_url = origin
        .as_ref()
        .and_then(|r| r.url())
        .unwrap_or("unknown")
        .to_string();

    let mut git_repo = GitRepository::new(remote_url, default_branch);

    // Get all remotes
    let remotes = repo.remotes()?;
    for name in remotes.iter().flatten() {
        if let Ok(remote) = repo.find_remote(name) {
            if let Some(url) = remote.url() {
                git_repo.add_remote(name.to_string(), url.to_string());
            }
        }
    }

    // Get submodules
    for submodule in repo.submodules()?.iter() {
        if let Some(path) = submodule.path().to_str() {
            if let Some(url) = submodule.url() {
                git_repo.add_submodule(path.to_string(), url.to_string());
            }
        }
    }

    Ok(git_repo)
}

/// Read the current branch
pub fn read_current_branch(path: impl AsRef<Path>) -> GitOperationResult<GitBranch> {
    let repo = Repository::open(path.as_ref())?;
    let head = repo.head()?;

    if !head.is_branch() {
        return Err(GitOperationError::BranchNotFound(
            "detached HEAD".to_string(),
        ));
    }

    let branch_name = head
        .shorthand()
        .ok_or_else(|| GitOperationError::InvalidUtf8("branch name".to_string()))?
        .to_string();

    let target = head.target().ok_or(GitOperationError::NoHeadCommit)?;
    let sha = target.to_string();

    let mut branch = GitBranch::new_local(branch_name.clone(), sha);

    // Get upstream tracking info
    let git_branch = repo.find_branch(&branch_name, BranchType::Local)?;
    if let Ok(upstream) = git_branch.upstream() {
        if let Some(upstream_name) = upstream.name()? {
            let upstream_oid = upstream
                .get()
                .target()
                .ok_or(GitOperationError::NoHeadCommit)?;
            let local_oid = head.target().ok_or(GitOperationError::NoHeadCommit)?;

            // Calculate ahead/behind
            let (ahead, behind) = repo.graph_ahead_behind(local_oid, upstream_oid)?;

            branch.set_tracking(upstream_name.to_string(), ahead, behind);
        }
    }

    Ok(branch)
}

/// Read HEAD commit
pub fn read_head_commit(path: impl AsRef<Path>) -> GitOperationResult<GitCommit> {
    let repo = Repository::open(path.as_ref())?;
    let head = repo.head()?;
    let commit_oid = head.target().ok_or(GitOperationError::NoHeadCommit)?;
    read_commit(&repo, commit_oid)
}

/// Read a specific commit
fn read_commit(repo: &Repository, oid: git2::Oid) -> GitOperationResult<GitCommit> {
    let commit_obj = repo.find_commit(oid)?;

    let message = commit_obj.message().unwrap_or("");
    let mut lines = message.lines();
    let title = lines.next().unwrap_or("").to_string();
    let description = lines.collect::<Vec<_>>().join("\n");

    let author = commit_obj.author();
    let author_name = author
        .name()
        .ok_or_else(|| GitOperationError::InvalidUtf8("author name".to_string()))?
        .to_string();
    let author_email = author
        .email()
        .ok_or_else(|| GitOperationError::InvalidUtf8("author email".to_string()))?
        .to_string();

    let timestamp_seconds = commit_obj.time().seconds();
    let timestamp = chrono::DateTime::from_timestamp(timestamp_seconds, 0)
        .ok_or_else(|| GitOperationError::InvalidUtf8("timestamp".to_string()))?;

    let mut git_commit =
        GitCommit::new(oid.to_string(), title, author_name, author_email, timestamp);
    git_commit.description = description;

    // Add parents
    for parent_id in commit_obj.parent_ids() {
        git_commit.add_parent(parent_id.to_string());
    }

    // Get changed files (this is expensive, so we might want to defer it)
    // For now, we'll leave it empty and populate on demand

    Ok(git_commit)
}

/// Read working directory state
pub fn read_working_directory(path: impl AsRef<Path>) -> GitOperationResult<GitWorkingDirectory> {
    let repo = Repository::open(path.as_ref())?;
    let mut wd = GitWorkingDirectory::new();

    // Get status for each file
    let statuses = repo.statuses(None)?;
    for entry in statuses.iter() {
        let path = entry
            .path()
            .ok_or_else(|| GitOperationError::InvalidUtf8("file path".to_string()))?;

        let status = match entry.status() {
            s if s.contains(Status::INDEX_NEW) => GitFileStatus::Added,
            s if s.contains(Status::INDEX_MODIFIED) => GitFileStatus::Staged,
            s if s.contains(Status::INDEX_DELETED) => GitFileStatus::Deleted,
            s if s.contains(Status::WT_MODIFIED) => GitFileStatus::Modified,
            s if s.contains(Status::WT_NEW) => GitFileStatus::Untracked,
            s if s.contains(Status::WT_DELETED) => GitFileStatus::Deleted,
            s if s.contains(Status::WT_RENAMED) => GitFileStatus::Renamed,
            s if s.contains(Status::IGNORED) => GitFileStatus::Ignored,
            s if s.contains(Status::CONFLICTED) => GitFileStatus::Conflicted,
            _ => GitFileStatus::Unmodified,
        };

        wd.add_file(path.to_string(), status);
    }

    Ok(wd)
}

/// Read all local branches
pub fn read_local_branches(path: impl AsRef<Path>) -> GitOperationResult<Vec<GitBranch>> {
    let repo = Repository::open(path.as_ref())?;
    let mut branches = Vec::new();

    for branch_result in repo.branches(Some(BranchType::Local))? {
        let (branch, _branch_type) = branch_result?;
        if let Some(name) = branch.name()? {
            let oid = branch
                .get()
                .target()
                .ok_or(GitOperationError::NoHeadCommit)?;
            let sha = oid.to_string();

            let mut git_branch = GitBranch::new_local(name.to_string(), sha);

            // Get upstream info
            if let Ok(upstream) = branch.upstream() {
                if let Some(upstream_name) = upstream.name()? {
                    let upstream_oid = upstream
                        .get()
                        .target()
                        .ok_or(GitOperationError::NoHeadCommit)?;
                    let (ahead, behind) = repo.graph_ahead_behind(oid, upstream_oid)?;
                    git_branch.set_tracking(upstream_name.to_string(), ahead, behind);
                }
            }

            branches.push(git_branch);
        }
    }

    Ok(branches)
}

/// Read all remote branches
pub fn read_remote_branches(path: impl AsRef<Path>) -> GitOperationResult<Vec<GitBranch>> {
    let repo = Repository::open(path.as_ref())?;
    let mut branches = Vec::new();

    for branch_result in repo.branches(Some(BranchType::Remote))? {
        let (branch, _branch_type) = branch_result?;
        if let Some(full_name) = branch.name()? {
            // Parse "origin/branch-name" into remote and branch name
            let parts: Vec<&str> = full_name.splitn(2, '/').collect();
            if parts.len() == 2 {
                let remote = parts[0].to_string();
                let name = parts[1].to_string();
                let oid = branch
                    .get()
                    .target()
                    .ok_or(GitOperationError::NoHeadCommit)?;
                let sha = oid.to_string();

                branches.push(GitBranch::new_remote(remote, name, sha));
            }
        }
    }

    Ok(branches)
}

/// Calculate diff between two commits
pub fn read_diff(
    path: impl AsRef<Path>,
    from_sha: &str,
    to_sha: &str,
) -> GitOperationResult<GitDiff> {
    let repo = Repository::open(path.as_ref())?;

    let from_oid = git2::Oid::from_str(from_sha)?;
    let to_oid = git2::Oid::from_str(to_sha)?;

    let from_commit = repo.find_commit(from_oid)?;
    let to_commit = repo.find_commit(to_oid)?;

    let from_tree = from_commit.tree()?;
    let to_tree = to_commit.tree()?;

    let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None)?;

    let mut git_diff = GitDiff::new(from_sha.to_string(), to_sha.to_string());

    let stats = diff.stats()?;
    git_diff.additions = stats.insertions();
    git_diff.deletions = stats.deletions();

    // Get changed files
    diff.foreach(
        &mut |delta, _progress| {
            if let Some(path) = delta.new_file().path() {
                if let Some(path_str) = path.to_str() {
                    git_diff.changed_files.push(path_str.to_string());
                }
            }
            true
        },
        None,
        None,
        None,
    )?;

    Ok(git_diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require a valid git repository to run
    // They will be skipped if not running in a git repo

    #[test]
    fn test_read_repository() {
        // Try to read the current directory as a git repo
        match read_repository(".") {
            Ok(repo) => {
                // If we're in a git repo, verify basic properties
                assert!(!repo.remote_url.is_empty());
                assert!(!repo.default_branch.is_empty());
            }
            Err(GitOperationError::Git(_)) => {
                // Not in a git repo, test passes
            }
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn test_read_current_branch() {
        match read_current_branch(".") {
            Ok(branch) => {
                assert!(!branch.name.is_empty());
                assert!(!branch.head_sha.is_empty());
                assert!(!branch.is_remote);
            }
            Err(GitOperationError::Git(_)) | Err(GitOperationError::RepositoryNotFound(_)) => {
                // Not in a git repo, test passes
            }
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn test_read_head_commit() {
        match read_head_commit(".") {
            Ok(commit) => {
                assert!(!commit.sha.is_empty());
                assert!(!commit.author.is_empty());
                assert!(!commit.title.is_empty());
            }
            Err(GitOperationError::Git(_)) | Err(GitOperationError::RepositoryNotFound(_)) => {
                // Not in a git repo, test passes
            }
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn test_read_working_directory() {
        match read_working_directory(".") {
            Ok(_wd) => {
                // Just verify it completes successfully
            }
            Err(GitOperationError::Git(_)) | Err(GitOperationError::RepositoryNotFound(_)) => {
                // Not in a git repo, test passes
            }
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn test_invalid_repository_path() {
        let result = read_repository("/nonexistent/invalid/path");
        assert!(result.is_err());
        match result {
            Err(GitOperationError::Git(_)) => {
                // Expected error
            }
            _ => panic!("Expected GitOperationError::Git for invalid path"),
        }
    }

    #[test]
    fn test_read_local_branches_in_repo() {
        match read_local_branches(".") {
            Ok(branches) => {
                // If we're in a repo, we should have at least one branch
                if !branches.is_empty() {
                    // Verify structure
                    assert!(!branches[0].name.is_empty());
                    assert!(!branches[0].head_sha.is_empty());
                    assert!(!branches[0].is_remote);
                }
            }
            Err(GitOperationError::Git(_)) | Err(GitOperationError::RepositoryNotFound(_)) => {
                // Not in a git repo, test passes
            }
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn test_read_remote_branches_in_repo() {
        match read_remote_branches(".") {
            Ok(branches) => {
                // Remote branches may or may not exist
                for branch in &branches {
                    assert!(!branch.name.is_empty());
                    assert!(!branch.head_sha.is_empty());
                    assert!(branch.is_remote);
                    assert!(branch.remote.is_some());
                }
            }
            Err(GitOperationError::Git(_)) | Err(GitOperationError::RepositoryNotFound(_)) => {
                // Not in a git repo, test passes
            }
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[test]
    fn test_invalid_diff_sha() {
        // Test with invalid SHAs
        let result = read_diff(".", "invalid_sha_1", "invalid_sha_2");
        assert!(result.is_err());
    }
}
