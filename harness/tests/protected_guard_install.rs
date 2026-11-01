use std::path::PathBuf;

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative);
    std::fs::read_to_string(path)
        .expect("repository file readable")
        .replace("\r\n", "\n")
}

fn installed() -> String {
    repo_file(".github/workflows/protected-paths-guard.yml")
}

fn without_leading_comments(template: &str) -> String {
    let mut lines = template.lines().peekable();
    while let Some(line) = lines.peek() {
        if line.starts_with('#') || line.trim().is_empty() {
            lines.next();
        } else {
            break;
        }
    }
    let mut out = lines.collect::<Vec<_>>().join("\n");
    out.push('\n');
    out
}

fn run_blocks(workflow: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in workflow.lines() {
        if let Some(block) = current.as_mut() {
            if line.is_empty() || line.starts_with("          ") {
                block.push(line);
                continue;
            }
            blocks.push(current.take().unwrap().join("\n"));
        }
        if line.trim_end() == "        run: |" {
            current = Some(Vec::new());
        }
    }
    if let Some(block) = current {
        blocks.push(block.join("\n"));
    }
    blocks
}

#[test]
fn the_installed_guard_is_the_template_without_its_leading_comment_header() {
    let template = repo_file("docs/ci/protected-paths-guard.yml");
    assert_eq!(installed(), without_leading_comments(&template));
}

#[test]
fn the_installed_guard_carries_no_yaml_comments() {
    for (number, line) in installed().lines().enumerate() {
        assert!(
            !line.trim_start().starts_with('#'),
            "comment on line {}: {line}",
            number + 1
        );
    }
}

#[test]
fn the_installed_guard_has_the_safe_trigger_token_and_checkout() {
    let guard = installed();
    assert!(guard.contains("on:\n  pull_request:\n"));
    assert!(!guard.contains("pull_request_target"));
    assert!(!guard.contains("workflow_run"));
    assert!(guard.contains("permissions:\n  contents: read\n  pull-requests: read\n"));
    assert!(!guard.contains("write"));
    assert!(!guard.contains("secrets."));
    assert!(guard.contains("persist-credentials: false"));
    assert!(!guard.contains("ref:"));
    assert!(!guard.contains("pull_request.head.ref"));
}

#[test]
fn every_installed_action_is_pinned_to_a_full_commit_sha() {
    let guard = installed();
    let uses: Vec<&str> = guard
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("- uses: "))
        .collect();
    assert!(!uses.is_empty());
    for action in uses {
        let (_, reference) = action.split_once('@').expect("uses has a ref");
        assert!(
            reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit()),
            "{action} is not pinned to a SHA"
        );
    }
}

#[test]
fn no_run_block_interpolates_a_github_expression() {
    let blocks = run_blocks(&installed());
    assert_eq!(blocks.len(), 2);
    for block in blocks {
        assert!(!block.contains("${{"), "expression in run block:\n{block}");
    }
}

#[test]
fn untrusted_event_fields_reach_the_scripts_only_through_env() {
    let guard = installed();
    for field in [
        "github.event.pull_request.body",
        "github.event.pull_request.user.login",
        "github.event.pull_request.base.sha",
        "github.event.pull_request.head.sha",
    ] {
        assert!(guard.contains(field), "{field}");
    }
    assert!(!guard.contains("pull_request.title"));
}

#[test]
fn the_installed_guard_reruns_on_edits_and_retargeting() {
    assert!(installed()
        .contains("on:\n  pull_request:\n    types: [opened, synchronize, reopened, edited]\n"));
}

#[test]
fn both_steps_run_with_strict_shell_options_and_full_history() {
    let guard = installed();
    for block in run_blocks(&guard) {
        assert!(
            block.starts_with("          set -euo pipefail\n"),
            "{block}"
        );
    }
    assert!(guard.contains("          fetch-depth: 0\n"));
}

#[cfg(unix)]
mod executed {
    use super::{installed, run_blocks};
    use std::path::Path;
    use std::process::{Command, Output};

    const ENFORCE: usize = 1;
    const DETECT: usize = 0;

    fn script(index: usize) -> String {
        run_blocks(&installed())[index]
            .lines()
            .map(|line| line.strip_prefix("          ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn write(dir: &Path, path: &str, content: &str) {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }

    fn init(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        write(dir.path(), "README.md", "base");
        for file in files {
            write(dir.path(), file, "base");
        }
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "base"]);
        dir
    }

    fn commit(dir: &Path, message: &str) -> String {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    fn enforce(dir: &Path, base: &str, head: &str, author: &str, humans: &str) -> Output {
        Command::new("bash")
            .current_dir(dir)
            .arg("-c")
            .arg(script(ENFORCE))
            .env("BASE_SHA", base)
            .env("HEAD_SHA", head)
            .env("IDENTITY", "")
            .env("PR_AUTHOR", author)
            .env("PROTECTED_PATTERNS_HUMAN_AUTHORS", humans)
            .output()
            .unwrap()
    }

    fn stderr(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    #[test]
    fn renaming_a_protected_file_out_of_its_path_is_a_violation() {
        let dir = init(&["flake.nix"]);
        let base = git(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["mv", "flake.nix", "moved.txt"]);
        let head = commit(dir.path(), "rename out");
        let output = enforce(dir.path(), &base, &head, "stranger", "owner");
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(stderr(&output).contains("  - flake.nix (rule: flake.nix)"));
    }

    #[test]
    fn deleting_a_protected_file_is_a_violation() {
        let dir = init(&["flake.nix"]);
        let base = git(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["rm", "-q", "flake.nix"]);
        let head = commit(dir.path(), "delete");
        let output = enforce(dir.path(), &base, &head, "stranger", "owner");
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(stderr(&output).contains("  - flake.nix (rule: flake.nix)"));
    }

    #[test]
    fn a_protected_directory_replaced_by_a_file_or_symlink_is_a_violation() {
        for replacement in ["file", "symlink"] {
            let dir = init(&[]);
            let base = git(dir.path(), &["rev-parse", "HEAD"]);
            if replacement == "file" {
                write(dir.path(), ".github", "not a directory");
            } else {
                std::os::unix::fs::symlink("docs", dir.path().join(".github")).unwrap();
            }
            let head = commit(dir.path(), replacement);
            let output = enforce(dir.path(), &base, &head, "stranger", "owner");
            assert_eq!(output.status.code(), Some(1), "{replacement}: {output:?}");
            assert!(
                stderr(&output).contains("  - .github (rule: .github/**)"),
                "{replacement}: {}",
                stderr(&output)
            );
        }
    }

    #[test]
    fn empty_allow_list_entries_never_match_an_empty_author() {
        let dir = init(&[]);
        let base = git(dir.path(), &["rev-parse", "HEAD"]);
        write(dir.path(), "flake.nix", "changed");
        let head = commit(dir.path(), "work");
        for humans in [",,", " , ", ",owner,"] {
            let output = enforce(dir.path(), &base, &head, "", humans);
            assert_eq!(output.status.code(), Some(1), "{humans:?}: {output:?}");
        }
    }

    #[test]
    fn unresolvable_commits_reaching_the_enforcing_step_fail_closed() {
        let dir = init(&[]);
        let base = git(dir.path(), &["rev-parse", "HEAD"]);
        write(dir.path(), "flake.nix", "changed");
        let head = commit(dir.path(), "work");
        let zeros = "0000000000000000000000000000000000000000";
        for (b, h) in [(base.as_str(), zeros), (zeros, head.as_str())] {
            let output = enforce(dir.path(), b, h, "owner", "owner");
            assert_ne!(output.status.code(), Some(0), "{output:?}");
            assert!(!String::from_utf8_lossy(&output.stdout).contains("no protected path touched"));
            assert!(!String::from_utf8_lossy(&output.stdout).contains("listed human"));
        }
    }

    #[test]
    fn a_shallow_clone_without_the_base_commit_fails_closed_in_both_steps() {
        let origin = init(&[]);
        let base = git(origin.path(), &["rev-parse", "HEAD"]);
        write(origin.path(), "flake.nix", "changed");
        let head = commit(origin.path(), "work");
        let clone = tempfile::tempdir().unwrap();
        let url = format!("file://{}", origin.path().display());
        git(
            clone.path(),
            &["clone", "-q", "--depth", "1", &url, "checkout"],
        );
        let work = clone.path().join("checkout");
        let output = enforce(&work, &base, &head, "owner", "owner");
        assert_ne!(output.status.code(), Some(0), "{output:?}");
        let detect = Command::new("bash")
            .current_dir(&work)
            .arg("-c")
            .arg(script(DETECT))
            .env("BASE_SHA", &base)
            .env("HEAD_SHA", &head)
            .env("PR_BODY", "")
            .env("GITHUB_OUTPUT", clone.path().join("out"))
            .output()
            .unwrap();
        assert_ne!(detect.status.code(), Some(0), "{detect:?}");
    }

    #[test]
    fn a_base_that_moved_ahead_is_diffed_two_dot_so_its_protected_changes_count() {
        let dir = init(&["flake.nix"]);
        let fork_point = git(dir.path(), &["rev-parse", "HEAD"]);
        write(dir.path(), "flake.nix", "changed on base");
        let base = commit(dir.path(), "base moved");
        git(dir.path(), &["checkout", "-q", "-b", "pr", &fork_point]);
        write(dir.path(), "docs/notes.md", "pr work");
        let head = commit(dir.path(), "pr");
        let output = enforce(dir.path(), &base, &head, "stranger", "owner");
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(stderr(&output).contains("  - flake.nix (rule: flake.nix)"));
    }

    #[test]
    fn a_path_matching_several_rules_is_reported_once_under_its_first_rule() {
        let dir = init(&[]);
        let base = git(dir.path(), &["rev-parse", "HEAD"]);
        write(dir.path(), ".github/workflows/x.yml", "changed");
        let head = commit(dir.path(), "work");
        let output = enforce(dir.path(), &base, &head, "stranger", "owner");
        let stderr = stderr(&output);
        assert_eq!(
            stderr.matches("  - .github/workflows/x.yml").count(),
            1,
            "{stderr}"
        );
        assert!(stderr.contains("(rule: .github/workflows/**)"));
    }

    #[test]
    fn a_listed_human_without_a_marker_passes_case_insensitively_in_a_list() {
        let dir = init(&[]);
        let base = git(dir.path(), &["rev-parse", "HEAD"]);
        write(dir.path(), "flake.nix", "changed");
        let head = commit(dir.path(), "work");
        let output = enforce(dir.path(), &base, &head, "Owner", "other,owner");
        assert_eq!(output.status.code(), Some(0), "{output:?}");
    }
}
