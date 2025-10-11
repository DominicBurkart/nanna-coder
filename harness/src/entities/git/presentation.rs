//! TOML presentation layer for git entities
//!
//! This module converts git entities to minified TOML format for model consumption,
//! as specified in issue #22.

use super::types::{GitBranch, GitCommit, GitDiff, GitRepository, GitWorkingDirectory};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Maximum character length for truncated titles and descriptions
const MAX_TRUNCATE_LEN: usize = 20;

/// Minified TOML representation of git state for model consumption
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStateToml {
    pub git: GitState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitState {
    pub local: LocalGitState,
    pub remote: HashMap<String, RemoteState>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub additional_available_entities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalGitState {
    pub staged_files: usize,
    pub unstaged_files: usize,
    pub head: HeadState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadState {
    pub branch: String,
    pub title: String,
    pub description: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteState {
    #[serde(flatten)]
    pub branches: HashMap<String, RemoteBranchState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteBranchState {
    pub sha: String,
    pub status: String,
    pub diff: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
}

/// Helper struct for additional available entities
#[derive(Debug, Clone)]
pub struct AdditionalEntities {
    pub commits: Vec<String>,
    pub diffs: Vec<(String, String)>,
    pub remotes: HashMap<String, (String, String)>, // remote_name -> (url, default_branch)
}

impl AdditionalEntities {
    pub fn new() -> Self {
        Self {
            commits: Vec::new(),
            diffs: Vec::new(),
            remotes: HashMap::new(),
        }
    }

    pub fn to_entity_list(&self) -> Vec<String> {
        let mut entities = Vec::new();

        // Add commit entities
        for sha in &self.commits {
            entities.push(format!("git.commits.{}", sha));
        }

        // Add diff entities
        for (sha1, sha2) in &self.diffs {
            entities.push(format!("git.commits.diff.{}.{}", sha1, sha2));
        }

        // Add remote metadata
        for remote_name in self.remotes.keys() {
            entities.push(format!("git.remote.{}.url", remote_name));
            entities.push(format!("git.remote.{}.default_branch", remote_name));
        }

        // Always include gitignore
        entities.push("git.ignore".to_string());

        entities
    }
}

impl Default for AdditionalEntities {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert git entities to TOML representation
pub fn to_toml_presentation(
    repo: &GitRepository,
    current_branch: &GitBranch,
    head_commit: &GitCommit,
    working_dir: &GitWorkingDirectory,
    remote_branches: &HashMap<String, (GitBranch, Option<GitDiff>)>,
    additional: &AdditionalEntities,
) -> GitStateToml {
    let mut remote_state = HashMap::new();

    // Group branches by remote
    for remote_name in repo.remotes.keys() {
        let mut remote_branches_map = HashMap::new();

        // Find all branches for this remote
        for (branch_key, (branch, diff_opt)) in remote_branches {
            if branch.remote.as_ref() == Some(remote_name) {
                let status = match (&branch.upstream, branch.ahead, branch.behind) {
                    (None, _, _) => "no upstream".to_string(),
                    (Some(_), Some(0), Some(0)) => "up to date".to_string(),
                    (Some(_), Some(ahead), Some(0)) if ahead > 0 => {
                        format!("{} ahead, can push", ahead)
                    }
                    (Some(_), Some(0), Some(behind)) if behind > 0 => {
                        format!("{} behind, needs pull", behind)
                    }
                    (Some(_), Some(ahead), Some(behind)) if ahead > 0 && behind > 0 => {
                        format!("{} ahead, {} behind, needs sync", ahead, behind)
                    }
                    _ => "unknown status".to_string(),
                };

                let diff = diff_opt
                    .as_ref()
                    .map(|d| d.summary())
                    .unwrap_or_else(|| "+0/-0".to_string());

                remote_branches_map.insert(
                    branch_key.clone(),
                    RemoteBranchState {
                        sha: branch.head_sha.clone(),
                        status,
                        diff,
                        pr: None, // PR info would come from GitHub API
                    },
                );
            }
        }

        if !remote_branches_map.is_empty() {
            remote_state.insert(
                remote_name.clone(),
                RemoteState {
                    branches: remote_branches_map,
                },
            );
        } else {
            // Remote exists but has no upstream branch
            remote_state.insert(
                remote_name.clone(),
                RemoteState {
                    branches: HashMap::new(),
                },
            );
        }
    }

    // Truncate title and description
    let truncate = |s: &str| {
        if s.chars().count() > MAX_TRUNCATE_LEN {
            format!("{}…", s.chars().take(MAX_TRUNCATE_LEN).collect::<String>())
        } else {
            s.to_string()
        }
    };

    GitStateToml {
        git: GitState {
            local: LocalGitState {
                staged_files: working_dir.staged_count,
                unstaged_files: working_dir.unstaged_count,
                head: HeadState {
                    branch: current_branch.name.clone(),
                    title: truncate(&head_commit.title),
                    description: truncate(&head_commit.description),
                    sha: head_commit.sha.clone(),
                },
            },
            remote: remote_state,
            additional_available_entities: additional.to_entity_list(),
        },
    }
}

/// Serialize to minified TOML string
pub fn to_minified_toml(state: &GitStateToml) -> Result<String, toml::ser::Error> {
    toml::to_string(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn create_test_repo() -> GitRepository {
        let mut repo = GitRepository::new(
            "git@github.com:DominicBurkart/nanna-coder.git".to_string(),
            "main".to_string(),
        );
        repo.add_remote(
            "backup".to_string(),
            "https://gitlab.com/DominicBurkart/nanna-coder.git".to_string(),
        );
        repo
    }

    fn create_test_branch() -> GitBranch {
        GitBranch::new_local("git-entities".to_string(), "d9eda6b".to_string())
    }

    fn create_test_commit() -> GitCommit {
        let mut commit = GitCommit::new(
            "d9eda6b123456789".to_string(),
            "Merge pull request #29 from DominicBurkart/cache-warm-fix".to_string(),
            "Dominic Burkart".to_string(),
            "dominic@example.com".to_string(),
            Utc::now(),
        );
        commit.description = "Fix cache warming in CI workflow".to_string();
        commit
    }

    fn create_test_working_dir() -> GitWorkingDirectory {
        let mut wd = GitWorkingDirectory::new();
        wd.add_file(
            "src/main.rs".to_string(),
            super::super::types::GitFileStatus::Modified,
        );
        wd.add_file(
            "new_file.rs".to_string(),
            super::super::types::GitFileStatus::Untracked,
        );
        wd
    }

    #[test]
    fn test_truncate_title() {
        let repo = create_test_repo();
        let branch = create_test_branch();

        // Create a commit with a very long title to test truncation
        let mut commit = GitCommit::new(
            "d9eda6b123456789".to_string(),
            "This is a very long commit title that should definitely be truncated to 20 characters"
                .to_string(),
            "Dominic Burkart".to_string(),
            "dominic@example.com".to_string(),
            Utc::now(),
        );
        commit.description = "Also a long description that should be truncated".to_string();

        let wd = GitWorkingDirectory::new();
        let remote_branches = HashMap::new();
        let additional = AdditionalEntities::new();

        let toml_state =
            to_toml_presentation(&repo, &branch, &commit, &wd, &remote_branches, &additional);

        // Title should be truncated to 20 chars with ellipsis (21 chars total)
        assert_eq!(toml_state.git.local.head.title.chars().count(), 21); // 20 chars + ellipsis
        assert!(toml_state.git.local.head.title.ends_with('…'));

        // Description should also be truncated
        assert_eq!(toml_state.git.local.head.description.chars().count(), 21);
        assert!(toml_state.git.local.head.description.ends_with('…'));
    }

    #[test]
    fn test_working_directory_counts() {
        let repo = create_test_repo();
        let branch = create_test_branch();
        let commit = create_test_commit();
        let wd = create_test_working_dir();
        let remote_branches = HashMap::new();
        let additional = AdditionalEntities::new();

        let toml_state =
            to_toml_presentation(&repo, &branch, &commit, &wd, &remote_branches, &additional);

        assert_eq!(toml_state.git.local.staged_files, 0);
        assert_eq!(toml_state.git.local.unstaged_files, 1);
    }

    #[test]
    fn test_remote_branches_structure() {
        let repo = create_test_repo();
        let branch = create_test_branch();
        let commit = create_test_commit();
        let wd = GitWorkingDirectory::new();

        let mut remote_branches = HashMap::new();
        let mut remote_branch = GitBranch::new_remote(
            "origin".to_string(),
            "git-entities".to_string(),
            "abc123".to_string(),
        );
        remote_branch.set_tracking("local/git-entities".to_string(), 0, 4);

        let mut diff = GitDiff::new("abc123".to_string(), "d9eda6b".to_string());
        diff.additions = 66;
        diff.deletions = 233;

        remote_branches.insert("git-entities".to_string(), (remote_branch, Some(diff)));

        let additional = AdditionalEntities::new();
        let toml_state =
            to_toml_presentation(&repo, &branch, &commit, &wd, &remote_branches, &additional);

        assert!(toml_state.git.remote.contains_key("origin"));
        let origin = &toml_state.git.remote["origin"];
        assert!(origin.branches.contains_key("git-entities"));
        assert_eq!(origin.branches["git-entities"].diff, "+66/-233");
    }

    #[test]
    fn test_additional_entities_list() {
        let mut additional = AdditionalEntities::new();
        additional.commits.push("abc123".to_string());
        additional.commits.push("def456".to_string());
        additional
            .diffs
            .push(("abc123".to_string(), "def456".to_string()));
        additional.remotes.insert(
            "origin".to_string(),
            (
                "git@github.com:user/repo.git".to_string(),
                "main".to_string(),
            ),
        );

        let entities = additional.to_entity_list();

        assert!(entities.contains(&"git.commits.abc123".to_string()));
        assert!(entities.contains(&"git.commits.def456".to_string()));
        assert!(entities.contains(&"git.commits.diff.abc123.def456".to_string()));
        assert!(entities.contains(&"git.remote.origin.url".to_string()));
        assert!(entities.contains(&"git.remote.origin.default_branch".to_string()));
        assert!(entities.contains(&"git.ignore".to_string()));
    }

    #[test]
    fn test_toml_serialization() {
        let repo = create_test_repo();
        let branch = create_test_branch();
        let commit = create_test_commit();
        let wd = GitWorkingDirectory::new();
        let remote_branches = HashMap::new();
        let additional = AdditionalEntities::new();

        let toml_state =
            to_toml_presentation(&repo, &branch, &commit, &wd, &remote_branches, &additional);
        let toml_string = to_minified_toml(&toml_state);

        assert!(toml_string.is_ok());
        let toml = toml_string.unwrap();

        // Verify TOML contains expected sections
        assert!(toml.contains("[git.local]"));
        assert!(toml.contains("[git.local.head]"));
        assert!(toml.contains("staged_files"));
        assert!(toml.contains("unstaged_files"));
    }

    #[test]
    fn test_toml_roundtrip() {
        let repo = create_test_repo();
        let branch = create_test_branch();
        let commit = create_test_commit();
        let wd = GitWorkingDirectory::new();
        let remote_branches = HashMap::new();
        let additional = AdditionalEntities::new();

        let toml_state =
            to_toml_presentation(&repo, &branch, &commit, &wd, &remote_branches, &additional);
        let toml_string = to_minified_toml(&toml_state).unwrap();

        // Deserialize back
        let deserialized: GitStateToml = toml::from_str(&toml_string).unwrap();

        assert_eq!(
            deserialized.git.local.head.branch,
            toml_state.git.local.head.branch
        );
        assert_eq!(
            deserialized.git.local.staged_files,
            toml_state.git.local.staged_files
        );
    }

    #[test]
    fn test_no_upstream_remote() {
        let repo = create_test_repo();
        let branch = create_test_branch();
        let commit = create_test_commit();
        let wd = GitWorkingDirectory::new();
        let remote_branches = HashMap::new();
        let additional = AdditionalEntities::new();

        let toml_state =
            to_toml_presentation(&repo, &branch, &commit, &wd, &remote_branches, &additional);

        // Backup remote has no upstream, should still appear
        assert!(toml_state.git.remote.contains_key("backup"));
    }

    #[test]
    fn test_truncate_unicode_emoji() {
        let repo = create_test_repo();
        let branch = create_test_branch();

        // Create commit with Unicode emoji
        let mut commit = GitCommit::new(
            "abc123".to_string(),
            "Add feature 🚀🎉✨💡🔥 with emoji support for international users".to_string(),
            "Developer".to_string(),
            "dev@example.com".to_string(),
            Utc::now(),
        );
        commit.description = "Description with 日本語 中文 한국어 and emoji 🌟".to_string();

        let wd = GitWorkingDirectory::new();
        let remote_branches = HashMap::new();
        let additional = AdditionalEntities::new();

        let toml_state =
            to_toml_presentation(&repo, &branch, &commit, &wd, &remote_branches, &additional);

        // Truncation should handle Unicode properly (counts characters, not bytes)
        assert_eq!(toml_state.git.local.head.title.chars().count(), 21); // 20 + ellipsis
        assert!(toml_state.git.local.head.title.ends_with('…'));

        // Description should also be truncated
        assert_eq!(toml_state.git.local.head.description.chars().count(), 21);
        assert!(toml_state.git.local.head.description.ends_with('…'));
    }

    #[test]
    fn test_branch_status_messages() {
        let repo = create_test_repo();
        let branch = create_test_branch();
        let commit = create_test_commit();
        let wd = GitWorkingDirectory::new();

        // Test ahead only
        let mut remote_branch_ahead = GitBranch::new_remote(
            "origin".to_string(),
            "ahead-branch".to_string(),
            "abc123".to_string(),
        );
        remote_branch_ahead.set_tracking("local/ahead-branch".to_string(), 0, 3);

        // Test behind only
        let mut remote_branch_behind = GitBranch::new_remote(
            "origin".to_string(),
            "behind-branch".to_string(),
            "def456".to_string(),
        );
        remote_branch_behind.set_tracking("local/behind-branch".to_string(), 5, 0);

        // Test diverged (both ahead and behind)
        let mut remote_branch_diverged = GitBranch::new_remote(
            "origin".to_string(),
            "diverged-branch".to_string(),
            "ghi789".to_string(),
        );
        remote_branch_diverged.set_tracking("local/diverged-branch".to_string(), 2, 3);

        let mut remote_branches = HashMap::new();
        remote_branches.insert("ahead-branch".to_string(), (remote_branch_ahead, None));
        remote_branches.insert("behind-branch".to_string(), (remote_branch_behind, None));
        remote_branches.insert(
            "diverged-branch".to_string(),
            (remote_branch_diverged, None),
        );

        let additional = AdditionalEntities::new();
        let toml_state =
            to_toml_presentation(&repo, &branch, &commit, &wd, &remote_branches, &additional);

        // Verify status messages
        let origin = &toml_state.git.remote["origin"];
        assert_eq!(
            origin.branches["ahead-branch"].status,
            "3 behind, needs pull"
        );
        assert_eq!(origin.branches["behind-branch"].status, "5 ahead, can push");
        assert_eq!(
            origin.branches["diverged-branch"].status,
            "2 ahead, 3 behind, needs sync"
        );
    }
}
