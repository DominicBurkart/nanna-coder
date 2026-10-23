use std::path::Path;
use std::process::Command;
use thiserror::Error;

pub(crate) const WORKFLOWS_DIR: &str = ".github/workflows/";
const CI_TREE: &str = ".github";
const POLICY_FILE: &str = ".nanna/deploy.toml";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum IntegrityError {
    #[error("no base branch could be resolved for this checkout; refusing to run CI")]
    NoBase,
    #[error("{what} could not be verified against {base}: {detail}")]
    Unverifiable {
        what: String,
        base: String,
        detail: String,
    },
    #[error("{path} differs between {base} and {rev}; CI files must match the base branch")]
    Modified {
        path: &'static str,
        base: String,
        rev: String,
    },
    #[error("workflow {workflow:?} does not exist at {base}")]
    WorkflowMissingAtBase { workflow: String, base: String },
}

fn git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn object_at(repo: &Path, rev: &str, path: &str) -> Option<String> {
    git_stdout(
        repo,
        &["rev-parse", "--verify", "--quiet", &format!("{rev}:{path}")],
    )
}

fn ref_exists(repo: &Path, full_ref: &str) -> bool {
    git_stdout(repo, &["rev-parse", "--verify", "--quiet", full_ref]).is_some()
}

pub(crate) fn resolve_base(repo: &Path) -> Result<String, IntegrityError> {
    if let Some(head) = git_stdout(
        repo,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    ) {
        if ref_exists(repo, &head) {
            return Ok(head);
        }
    }
    ["refs/remotes/origin/main", "refs/remotes/origin/master"]
        .into_iter()
        .find(|candidate| ref_exists(repo, candidate))
        .map(str::to_string)
        .ok_or(IntegrityError::NoBase)
}

fn worktree_policy_object(repo: &Path) -> Option<String> {
    let file = repo.join(POLICY_FILE);
    if !file.is_file() {
        return None;
    }
    git_stdout(repo, &["hash-object", "--", POLICY_FILE])
}

pub(crate) fn verify_ci_surface(
    repo: &Path,
    branch: &str,
    extra_revs: &[String],
    workflow_file: Option<&str>,
) -> Result<(), IntegrityError> {
    let base = resolve_base(repo)?;
    if let Some(workflow) = workflow_file {
        let path = format!("{WORKFLOWS_DIR}{workflow}");
        if object_at(repo, &base, &path).is_none() {
            return Err(IntegrityError::WorkflowMissingAtBase {
                workflow: workflow.to_string(),
                base,
            });
        }
    }
    let mut revs = vec!["HEAD".to_string()];
    let pushed = format!("refs/remotes/origin/{branch}");
    if !ref_exists(repo, &pushed) {
        return Err(IntegrityError::Unverifiable {
            what: format!("the pushed tip of {branch}"),
            base,
            detail: "no remote-tracking ref; push the branch first".to_string(),
        });
    }
    revs.push(pushed);
    revs.extend(extra_revs.iter().cloned());
    for rev in &revs {
        if git_stdout(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ],
        )
        .is_none()
        {
            return Err(IntegrityError::Unverifiable {
                what: format!("revision {rev}"),
                base: base.clone(),
                detail: "not available in this checkout".to_string(),
            });
        }
        for path in [CI_TREE, POLICY_FILE] {
            if object_at(repo, &base, path) != object_at(repo, rev, path) {
                return Err(IntegrityError::Modified {
                    path: if path == CI_TREE {
                        CI_TREE
                    } else {
                        POLICY_FILE
                    },
                    base: base.clone(),
                    rev: rev.clone(),
                });
            }
        }
    }
    if worktree_policy_object(repo) != object_at(repo, &base, POLICY_FILE) {
        return Err(IntegrityError::Modified {
            path: POLICY_FILE,
            base,
            rev: "the working tree".to_string(),
        });
    }
    Ok(())
}

pub(crate) fn workflow_file_of_run_path(path: &str) -> Option<&str> {
    let without_ref = path.split('@').next().unwrap_or(path);
    let name = without_ref.strip_prefix(WORKFLOWS_DIR)?;
    if name.is_empty() || name.contains('/') {
        return None;
    }
    Some(name)
}

pub(crate) fn is_workflow_file_name(workflow: &str) -> bool {
    workflow.ends_with(".yml") || workflow.ends_with(".yaml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn git(dir: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "feat/x"]);
        git(dir.path(), &["config", "user.email", "t@t.invalid"]);
        git(dir.path(), &["config", "user.name", "t"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        std::fs::write(dir.path().join(".github/workflows/ci.yml"), "name: ci\n").unwrap();
        std::fs::write(dir.path().join("f"), "1").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    fn publish(dir: &Path) {
        git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(dir, &["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
    }

    #[test]
    fn the_base_is_origin_head_then_origin_main_then_origin_master() {
        let dir = repo();
        assert_eq!(resolve_base(dir.path()), Err(IntegrityError::NoBase));
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/master", "HEAD"],
        );
        assert_eq!(
            resolve_base(dir.path()).unwrap(),
            "refs/remotes/origin/master"
        );
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        assert_eq!(
            resolve_base(dir.path()).unwrap(),
            "refs/remotes/origin/main"
        );
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/trunk", "HEAD"],
        );
        git(
            dir.path(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        assert_eq!(
            resolve_base(dir.path()).unwrap(),
            "refs/remotes/origin/trunk"
        );
        git(
            dir.path(),
            &["update-ref", "-d", "refs/remotes/origin/trunk"],
        );
        assert_eq!(
            resolve_base(dir.path()).unwrap(),
            "refs/remotes/origin/main"
        );
    }

    #[test]
    fn a_clean_published_branch_verifies() {
        let dir = repo();
        publish(dir.path());
        assert!(verify_ci_surface(dir.path(), "feat/x", &[], Some("ci.yml")).is_ok());
        assert!(verify_ci_surface(dir.path(), "feat/x", &[], None).is_ok());
    }

    #[test]
    fn an_unpublished_branch_a_missing_workflow_and_an_unknown_revision_are_unverifiable() {
        let dir = repo();
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        assert!(matches!(
            verify_ci_surface(dir.path(), "feat/x", &[], None),
            Err(IntegrityError::Unverifiable { .. })
        ));
        publish(dir.path());
        assert!(matches!(
            verify_ci_surface(dir.path(), "feat/x", &[], Some("nope.yml")),
            Err(IntegrityError::WorkflowMissingAtBase { .. })
        ));
        let unknown = "0".repeat(40);
        assert!(matches!(
            verify_ci_surface(dir.path(), "feat/x", &[unknown], None),
            Err(IntegrityError::Unverifiable { .. })
        ));
    }

    #[test]
    fn a_change_under_github_or_to_the_policy_file_is_refused_on_any_revision_or_in_the_worktree() {
        let dir = repo();
        publish(dir.path());
        std::fs::write(dir.path().join(".github/workflows/ci.yml"), "name: evil\n").unwrap();
        git(dir.path(), &["commit", "-qam", "edit ci"]);
        let edited = verify_ci_surface(dir.path(), "feat/x", &[], None).unwrap_err();
        assert!(matches!(
            edited,
            IntegrityError::Modified {
                path: ".github",
                ..
            }
        ));
        git(dir.path(), &["reset", "-q", "--hard", "origin/main"]);

        std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
        std::fs::write(dir.path().join(".nanna/deploy.toml"), "x = 1\n").unwrap();
        let worktree = verify_ci_surface(dir.path(), "feat/x", &[], None).unwrap_err();
        assert!(matches!(
            worktree,
            IntegrityError::Modified { path: ".nanna/deploy.toml", ref rev, .. } if rev == "the working tree"
        ));
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "policy"]);
        let committed = verify_ci_surface(dir.path(), "feat/x", &[], None).unwrap_err();
        assert!(matches!(
            committed,
            IntegrityError::Modified {
                path: ".nanna/deploy.toml",
                ref rev,
                ..
            } if rev == "HEAD"
        ));
    }

    #[test]
    fn workflow_files_are_recognised_by_extension_and_run_paths_by_directory() {
        assert!(is_workflow_file_name("ci.yml"));
        assert!(is_workflow_file_name("ci.yaml"));
        assert!(!is_workflow_file_name("ci.yml.bak"));
        assert!(!is_workflow_file_name("123456"));
        assert_eq!(
            workflow_file_of_run_path(".github/workflows/ci.yml"),
            Some("ci.yml")
        );
        assert_eq!(
            workflow_file_of_run_path(".github/workflows/ci.yml@refs/heads/main"),
            Some("ci.yml")
        );
        for bad in [
            "",
            ".github/workflows/",
            ".github/workflows/a/b.yml",
            "workflows/ci.yml",
            ".github/actions/ci.yml",
            "../.github/workflows/ci.yml",
        ] {
            assert_eq!(workflow_file_of_run_path(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_directory_that_is_not_a_repository_has_no_base() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_base(dir.path()), Err(IntegrityError::NoBase));
    }
}
