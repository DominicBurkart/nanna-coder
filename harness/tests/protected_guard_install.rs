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
