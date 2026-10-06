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
