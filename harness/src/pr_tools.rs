use crate::backlog::{BacklogError, GithubClient, GithubComment};
use crate::effects::EffectClass;
use crate::marker::{parse_identity_from_text, render_html_marker, render_trailer};
use crate::protected::ProtectedPaths;
use crate::qa::ARTIFACT_DIR;
use crate::tools::{parse_github_remote, Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

pub const TRUSTED_PR_COMMENTERS_ENV: &str = "NANNA_TRUSTED_PR_COMMENTERS";

const MAX_NUMBER: u64 = 999_999_999;
const TITLE_MAX: usize = 256;
const PR_BODY_MAX: usize = 20_000;
const COMMENT_MAX: usize = 10_000;
const REASON_MAX: usize = 1_000;
const SUBJECT_MAX: usize = 100;
const COMMIT_BODY_MAX: usize = 5_000;
const BRANCH_MAX: usize = 100;
const PROTECTED_BRANCHES: [&str; 2] = ["main", "master"];
const IDENTITY_KEYWORD: &str = "nanna-identity";

fn map_backlog_error(err: BacklogError) -> ToolError {
    ToolError::ExecutionFailed {
        message: err.to_string(),
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        message: message.into(),
    }
}

fn failed(message: impl Into<String>) -> ToolError {
    ToolError::ExecutionFailed {
        message: message.into(),
    }
}

fn check_args(tool: &str, args: &Value, allowed: &[&str]) -> ToolResult<()> {
    match args {
        Value::Null => Ok(()),
        Value::Object(map) => match map.keys().find(|key| !allowed.contains(&key.as_str())) {
            Some(key) => {
                let shown: String = key.chars().take(32).flat_map(char::escape_debug).collect();
                Err(invalid(format!("{tool} does not accept '{shown}'")))
            }
            None => Ok(()),
        },
        _ => Err(invalid(format!("{tool} arguments must be a JSON object"))),
    }
}

#[derive(Clone, Copy)]
enum Shape {
    Line,
    Block,
}

fn is_forbidden_char(c: char, shape: Shape) -> bool {
    if c == '\n' || c == '\t' {
        return matches!(shape, Shape::Line);
    }
    c.is_control()
        || matches!(
            c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{00ad}'
                | '\u{061c}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

fn fences_balanced(text: &str) -> bool {
    let mut open: Option<(char, usize)> = None;
    for line in text.lines() {
        let stripped = line.trim_start_matches(' ');
        if line.len() - stripped.len() > 3 {
            continue;
        }
        let fence_char = match stripped.chars().next() {
            Some(c @ ('`' | '~')) => c,
            _ => continue,
        };
        let run = stripped.chars().take_while(|c| *c == fence_char).count();
        if run < 3 {
            continue;
        }
        let rest = &stripped[run..];
        match open {
            None => {
                if fence_char == '`' && rest.contains('`') {
                    continue;
                }
                open = Some((fence_char, run));
            }
            Some((open_char, open_run)) => {
                if open_char == fence_char && run >= open_run && rest.trim().is_empty() {
                    open = None;
                }
            }
        }
    }
    open.is_none()
}

fn validate_text(key: &str, text: &str, shape: Shape, max: usize) -> ToolResult<()> {
    if text.trim().is_empty() {
        return Err(invalid(format!("'{key}' must not be empty")));
    }
    if text.chars().count() > max {
        return Err(invalid(format!("'{key}' is longer than {max} characters")));
    }
    if text.chars().any(|c| is_forbidden_char(c, shape)) {
        return Err(invalid(format!(
            "'{key}' contains a control, line-separator or invisible formatting character"
        )));
    }
    if text.to_lowercase().contains(IDENTITY_KEYWORD) {
        return Err(invalid(format!(
            "'{key}' must not mention a Nanna-Identity marker"
        )));
    }
    if matches!(shape, Shape::Block) {
        if text.contains("<!--") || text.contains("-->") {
            return Err(invalid(format!(
                "'{key}' must not contain HTML comment delimiters"
            )));
        }
        if !fences_balanced(text) {
            return Err(invalid(format!(
                "'{key}' contains an unterminated code fence"
            )));
        }
    }
    Ok(())
}

fn text_arg<'a>(args: &'a Value, key: &str, shape: Shape, max: usize) -> ToolResult<&'a str> {
    let Some(text) = args.get(key).and_then(Value::as_str) else {
        return Err(invalid(format!("'{key}' must be a string")));
    };
    validate_text(key, text, shape, max)?;
    Ok(text)
}

fn optional_text_arg<'a>(
    args: &'a Value,
    key: &str,
    shape: Shape,
    max: usize,
) -> ToolResult<Option<&'a str>> {
    if args.get(key).is_none() {
        return Ok(None);
    }
    text_arg(args, key, shape, max).map(Some)
}

fn number_arg(args: &Value, key: &str) -> ToolResult<u64> {
    match args.get(key).and_then(Value::as_u64) {
        Some(n) if (1..=MAX_NUMBER).contains(&n) => Ok(n),
        _ => Err(invalid(format!(
            "'{key}' must be an integer between 1 and {MAX_NUMBER}"
        ))),
    }
}

fn validate_branch_name(field: &str, name: &str) -> ToolResult<()> {
    let allowed_chars = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
    let malformed = name.is_empty()
        || name.len() > BRANCH_MAX
        || !allowed_chars
        || name.starts_with(['-', '.', '/'])
        || name.ends_with(['/', '.'])
        || name.contains("..")
        || name.contains("//")
        || name.contains("/.")
        || name.starts_with("refs/")
        || name.eq_ignore_ascii_case("HEAD")
        || name.split('/').any(|part| part.ends_with(".lock"));
    if malformed {
        return Err(invalid(format!(
            "'{field}' is not a plain branch name (letters, digits, '.', '_', '-' and '/' only, at most {BRANCH_MAX} characters)"
        )));
    }
    Ok(())
}

fn is_protected_branch(name: &str, default: Option<&str>) -> bool {
    PROTECTED_BRANCHES
        .iter()
        .any(|protected| name.eq_ignore_ascii_case(protected))
        || default.is_some_and(|default| name.eq_ignore_ascii_case(default))
}

fn identity_markers(identity_name: &str) -> ToolResult<(String, String)> {
    let trailer = render_trailer(identity_name);
    let marker = render_html_marker(identity_name);
    let round_trips = parse_identity_from_text(&trailer).as_deref() == Some(identity_name)
        && parse_identity_from_text(&marker).as_deref() == Some(identity_name);
    if round_trips {
        Ok((trailer, marker))
    } else {
        Err(failed(format!(
            "identity name '{identity_name}' does not produce a parseable identity marker"
        )))
    }
}

fn attributed(body: &str, identity_name: &str) -> ToolResult<String> {
    let (_, marker) = identity_markers(identity_name)?;
    Ok(format!("{body}\n\n{marker}\n"))
}

fn comment_json(comment: &GithubComment) -> Value {
    json!({
        "id": comment.id,
        "author": comment.author,
        "body": comment.body,
        "html_url": comment.html_url,
    })
}

fn ensure_owned_by_identity(
    body: Option<&str>,
    identity_name: &str,
    pr_number: u64,
) -> ToolResult<()> {
    let owner = body.and_then(parse_identity_from_text);
    if owner.as_deref() == Some(identity_name) {
        Ok(())
    } else {
        Err(failed(format!(
            "refusing to act on pull request #{pr_number}: not owned by identity '{identity_name}'"
        )))
    }
}

fn ensure_open(state: &str, pr_number: u64) -> ToolResult<()> {
    if state == "open" {
        Ok(())
    } else {
        Err(failed(format!(
            "refusing to act on pull request #{pr_number}: it is not open"
        )))
    }
}

fn is_issue_number(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 9
        && !text.starts_with('0')
        && text.bytes().all(|b| b.is_ascii_digit())
}

fn is_same_repo_issue_url(token: &str, repo: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("https://github.com/") else {
        return false;
    };
    let mut parts = rest.split('/');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some(owner), Some(name), Some("issues"), Some(number), None) => {
            format!("{owner}/{name}") == repo.to_ascii_lowercase() && is_issue_number(number)
        }
        _ => false,
    }
}

fn has_hash_reference(text: &str) -> bool {
    text.match_indices('#').any(|(index, _)| {
        let before = text[..index].chars().next_back();
        let lead_ok =
            before.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | ',' | ';' | ':'));
        let digits: String = text[index + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let after = text[index + 1 + digits.len()..].chars().next();
        let trail_ok = after.is_none_or(|c| {
            c.is_whitespace() || matches!(c, ')' | ']' | ',' | ';' | ':' | '.' | '!' | '?')
        });
        lead_ok && trail_ok && is_issue_number(&digits)
    })
}

fn references_issue(text: &str, repo: &str) -> bool {
    has_hash_reference(text)
        || text.split_whitespace().any(|token| {
            is_same_repo_issue_url(
                token.trim_matches(|c| {
                    matches!(c, '(' | ')' | '[' | ']' | ',' | '.' | ';' | ':' | '!' | '?')
                }),
                repo,
            )
        })
}

#[cfg(test)]
thread_local! {
    static TEST_GIT_BIN_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

fn git_binary() -> String {
    #[cfg(test)]
    {
        if let Some(bin) = TEST_GIT_BIN_OVERRIDE.with(|cell| cell.borrow().clone()) {
            return bin;
        }
    }
    "git".to_string()
}

#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

fn git_cmd(workspace_root: &Path) -> Command {
    let mut cmd = Command::new(git_binary());
    cmd.current_dir(workspace_root);
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "nanna")
        .env("GIT_AUTHOR_EMAIL", "nanna@local")
        .env("GIT_COMMITTER_NAME", "nanna")
        .env("GIT_COMMITTER_EMAIL", "nanna@local");
    cmd.arg("-c")
        .arg(format!("core.hooksPath={NULL_DEVICE}"))
        .arg("-c")
        .arg("commit.gpgsign=false");
    cmd
}

fn run_git_raw(workspace_root: &Path, args: &[&str]) -> ToolResult<String> {
    let output = git_cmd(workspace_root)
        .args(args)
        .output()
        .map_err(|e| failed(format!("failed to run git {}: {e}", args.join(" "))))?;
    if !output.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_git(workspace_root: &Path, args: &[&str]) -> ToolResult<String> {
    run_git_raw(workspace_root, args).map(|out| out.trim().to_string())
}

fn resolve_repo(workspace_root: &Path) -> ToolResult<String> {
    let remote_url = run_git(workspace_root, &["remote", "get-url", "origin"])?;
    parse_github_remote(&remote_url)
        .map(|(owner, repo)| format!("{owner}/{repo}"))
        .ok_or_else(|| failed("origin remote is not a GitHub remote"))
}

fn current_branch(workspace_root: &Path) -> ToolResult<String> {
    let branch = run_git(workspace_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch.is_empty() || branch == "HEAD" {
        return Err(failed("refusing to act from a detached HEAD"));
    }
    Ok(branch)
}

fn default_branch(workspace_root: &Path) -> Option<String> {
    if let Ok(target) = run_git(
        workspace_root,
        &["symbolic-ref", "refs/remotes/origin/HEAD"],
    ) {
        if let Some(name) = target.strip_prefix("refs/remotes/origin/") {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    for candidate in PROTECTED_BRANCHES {
        let reference = format!("refs/remotes/origin/{candidate}");
        if run_git(
            workspace_root,
            &["rev-parse", "--verify", "--quiet", &reference],
        )
        .is_ok()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

fn working_branch(workspace_root: &Path) -> ToolResult<String> {
    let branch = current_branch(workspace_root)?;
    validate_branch_name("current branch", &branch)?;
    let default = default_branch(workspace_root);
    if is_protected_branch(&branch, default.as_deref()) {
        return Err(invalid(format!(
            "refusing to act on '{branch}': it is the repository's default branch or a protected branch"
        )));
    }
    Ok(branch)
}

fn property(schema_type: SchemaType, description: &str) -> PropertySchema {
    PropertySchema {
        schema_type,
        description: Some(description.to_string()),
        items: None,
    }
}

fn object_schema(properties: Vec<(&str, PropertySchema)>, required: &[&str]) -> JsonSchema {
    JsonSchema {
        schema_type: SchemaType::Object,
        properties: Some(
            properties
                .into_iter()
                .map(|(name, schema)| (name.to_string(), schema))
                .collect::<HashMap<_, _>>(),
        ),
        required: Some(required.iter().map(|name| name.to_string()).collect()),
    }
}

fn definition(name: &str, description: &str, parameters: JsonSchema) -> ToolDefinition {
    ToolDefinition {
        function: FunctionDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        },
    }
}

fn pr_number_schema() -> JsonSchema {
    object_schema(
        vec![(
            "pr_number",
            property(SchemaType::Integer, "Pull request number."),
        )],
        &["pr_number"],
    )
}

pub struct GitPushBranchTool {
    workspace_root: PathBuf,
    identity_name: String,
}

impl GitPushBranchTool {
    pub fn new(workspace_root: PathBuf, identity_name: impl Into<String>) -> Self {
        Self {
            workspace_root,
            identity_name: identity_name.into(),
        }
    }
}

fn ensure_unpushed_commits_are_attributed_and_unprotected(
    workspace_root: &Path,
    identity_name: &str,
) -> ToolResult<()> {
    let protected = ProtectedPaths::for_repo(workspace_root);
    let shas = run_git(
        workspace_root,
        &["rev-list", "HEAD", "--not", "--remotes=origin"],
    )?;
    for sha in shas.lines().filter(|line| !line.is_empty()) {
        let message = run_git(workspace_root, &["log", "-1", "--format=%B", sha])?;
        if parse_identity_from_text(&message).as_deref() != Some(identity_name) {
            return Err(failed(format!(
                "refusing to push: commit {sha} is missing the identity trailer; add `{}` to its message or create it with git_commit",
                render_trailer(identity_name)
            )));
        }
        let listing = run_git_raw(
            workspace_root,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-z",
                "--no-renames",
                "-m",
                "--root",
                sha,
            ],
        )?;
        if let Some(path) = listing
            .split('\0')
            .find(|path| !path.is_empty() && protected.is_protected(Path::new(path)))
        {
            return Err(failed(format!(
                "refusing to push: commit {sha} changes protected path '{}'",
                path.chars()
                    .take(120)
                    .flat_map(char::escape_debug)
                    .collect::<String>()
            )));
        }
    }
    Ok(())
}

#[async_trait]
impl Tool for GitPushBranchTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "git_push_branch",
            "Push the current branch to the 'origin' remote. Takes no arguments. Refuses the default or a protected branch, a detached HEAD, an unsafe branch name, any unpushed commit lacking this identity's trailer, and any unpushed commit that changes a protected path. Never forces.",
            object_schema(vec![], &[]),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args("git_push_branch", &args, &[])?;
        let branch = working_branch(&self.workspace_root)?;
        ensure_unpushed_commits_are_attributed_and_unprotected(
            &self.workspace_root,
            &self.identity_name,
        )?;
        let refspec = format!("HEAD:refs/heads/{branch}");
        run_git(&self.workspace_root, &["push", "origin", &refspec])?;
        Ok(json!({ "pushed": true, "branch": branch, "remote": "origin" }))
    }

    fn name(&self) -> &str {
        "git_push_branch"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

struct StatusEntry {
    index: u8,
    worktree: u8,
    path: String,
    origin: Option<String>,
}

fn parse_status(raw: &str) -> Vec<StatusEntry> {
    let mut fields = raw.split('\0');
    let mut entries = Vec::new();
    while let Some(field) = fields.next() {
        let bytes = field.as_bytes();
        if bytes.len() < 4 {
            continue;
        }
        let (index, worktree) = (bytes[0], bytes[1]);
        let renamed = matches!(index, b'R' | b'C') || matches!(worktree, b'R' | b'C');
        entries.push(StatusEntry {
            index,
            worktree,
            path: String::from_utf8_lossy(&bytes[3..]).into_owned(),
            origin: if renamed {
                fields.next().map(str::to_string)
            } else {
                None
            },
        });
    }
    entries
}

fn escaped(path: &str) -> String {
    path.chars()
        .take(120)
        .flat_map(char::escape_debug)
        .collect()
}

pub struct GitCommitTool {
    workspace_root: PathBuf,
    identity_name: String,
}

impl GitCommitTool {
    pub fn new(workspace_root: PathBuf, identity_name: impl Into<String>) -> Self {
        Self {
            workspace_root,
            identity_name: identity_name.into(),
        }
    }
}

#[async_trait]
impl Tool for GitCommitTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "git_commit",
            "Commit the working tree changes on the current branch with this identity's Nanna-Identity trailer appended to the message. Refuses the default or a protected branch and any tracked or staged change to a protected path; untracked protected paths are left out of the commit and reported.",
            object_schema(
                vec![
                    (
                        "subject",
                        property(SchemaType::String, "Single-line commit subject."),
                    ),
                    (
                        "body",
                        property(SchemaType::String, "Optional commit message body."),
                    ),
                ],
                &["subject"],
            ),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args("git_commit", &args, &["subject", "body"])?;
        let subject = text_arg(&args, "subject", Shape::Line, SUBJECT_MAX)?;
        let body = optional_text_arg(&args, "body", Shape::Block, COMMIT_BODY_MAX)?;
        let (trailer, _) = identity_markers(&self.identity_name)?;
        let branch = working_branch(&self.workspace_root)?;
        let protected = ProtectedPaths::for_repo(&self.workspace_root);
        let status = run_git_raw(
            &self.workspace_root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        let mut to_stage: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for entry in parse_status(&status) {
            let untracked = entry.index == b'?';
            let touches_protected = std::iter::once(&entry.path)
                .chain(entry.origin.as_ref())
                .any(|path| protected.is_protected(Path::new(path)));
            if untracked && Path::new(&entry.path).starts_with(ARTIFACT_DIR) {
                continue;
            }
            if touches_protected && untracked {
                skipped.push(entry.path);
            } else if touches_protected {
                return Err(failed(format!(
                    "refusing to commit: '{}' is a protected path",
                    escaped(&entry.path)
                )));
            } else if entry.worktree != b' ' {
                to_stage.push(format!(":(literal){}", entry.path));
            }
        }
        if !to_stage.is_empty() {
            let mut add_args: Vec<&str> = vec!["add", "--all", "--"];
            add_args.extend(to_stage.iter().map(String::as_str));
            run_git_raw(&self.workspace_root, &add_args)?;
        }
        let staged = run_git_raw(&self.workspace_root, &["diff", "--cached", "--name-only"])?;
        let nothing_staged = staged.trim().is_empty();
        if nothing_staged {
            return Err(failed("nothing to commit"));
        }
        let mut commit_args: Vec<&str> = vec!["commit", "-q", "-m", subject];
        if let Some(body) = body {
            commit_args.extend(["-m", body]);
        }
        commit_args.extend(["-m", &trailer]);
        run_git_raw(&self.workspace_root, &commit_args)?;
        let commit = run_git(&self.workspace_root, &["rev-parse", "HEAD"])?;
        Ok(json!({
            "committed": true,
            "commit": commit,
            "branch": branch,
            "skipped_protected": skipped,
        }))
    }

    fn name(&self) -> &str {
        "git_commit"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

pub struct GithubPrOpenTool {
    workspace_root: PathBuf,
    identity_name: String,
    client: Arc<dyn GithubClient>,
}

impl GithubPrOpenTool {
    pub fn new(
        workspace_root: PathBuf,
        identity_name: impl Into<String>,
        client: Arc<dyn GithubClient>,
    ) -> Self {
        Self {
            workspace_root,
            identity_name: identity_name.into(),
            client,
        }
    }
}

#[async_trait]
impl Tool for GithubPrOpenTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "github_pr_open",
            "Open a draft pull request from the current branch. Always a draft; there is no way to open a ready-for-review pull request with this tool.",
            object_schema(
                vec![
                    ("title", property(SchemaType::String, "Pull request title.")),
                    (
                        "body",
                        property(
                            SchemaType::String,
                            "Pull request body. Must not mention a Nanna-Identity marker.",
                        ),
                    ),
                    (
                        "base",
                        property(
                            SchemaType::String,
                            "Base branch (optional; defaults to the repository's default branch).",
                        ),
                    ),
                ],
                &["title", "body"],
            ),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args("github_pr_open", &args, &["title", "body", "base", "draft"])?;
        if let Some(draft) = args.get("draft") {
            if draft.as_bool() != Some(true) {
                return Err(invalid(
                    "github_pr_open always creates a draft pull request; 'draft' must be true or omitted",
                ));
            }
        }
        let title = text_arg(&args, "title", Shape::Line, TITLE_MAX)?;
        let body = text_arg(&args, "body", Shape::Block, PR_BODY_MAX)?;
        let requested_base = optional_text_arg(&args, "base", Shape::Line, BRANCH_MAX)?;
        if let Some(base) = requested_base {
            validate_branch_name("base", base)?;
        }
        let (trailer, marker) = identity_markers(&self.identity_name)?;
        let repo = resolve_repo(&self.workspace_root)?;
        let head = working_branch(&self.workspace_root)?;
        let base = match requested_base {
            Some(base) => base.to_string(),
            None => default_branch(&self.workspace_root).ok_or_else(|| {
                invalid(
                    "no 'base' given and the repository's default branch could not be determined",
                )
            })?,
        };
        if base == head {
            return Err(invalid(
                "refusing to open a pull request from a branch into itself",
            ));
        }
        let full_body = format!("{body}\n\n{trailer}\n{marker}\n");
        let created = self
            .client
            .create_draft_pull_request(&repo, title, &full_body, &head, &base)
            .await
            .map_err(map_backlog_error)?;
        Ok(json!({
            "number": created.number,
            "html_url": created.html_url,
            "draft": true,
            "head": head,
            "base": base,
        }))
    }

    fn name(&self) -> &str {
        "github_pr_open"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

fn normalize_allowed_authors(allowed_authors: Vec<String>) -> Vec<String> {
    allowed_authors
        .into_iter()
        .map(|author| author.to_lowercase())
        .collect()
}

fn comment_is_allowed(comment: &GithubComment, allowed_authors: &[String]) -> bool {
    allowed_authors.contains(&comment.author.to_lowercase())
}

pub struct GithubPrCommentsTool {
    workspace_root: PathBuf,
    client: Arc<dyn GithubClient>,
    allowed_authors: Vec<String>,
}

impl GithubPrCommentsTool {
    pub fn new(
        workspace_root: PathBuf,
        client: Arc<dyn GithubClient>,
        allowed_authors: Vec<String>,
    ) -> Self {
        Self {
            workspace_root,
            client,
            allowed_authors: normalize_allowed_authors(allowed_authors),
        }
    }

    fn is_allowed(&self, comment: &GithubComment) -> bool {
        comment_is_allowed(comment, &self.allowed_authors)
    }
}

#[async_trait]
impl Tool for GithubPrCommentsTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "github_pr_comments",
            "Fetch review and issue comments on a pull request, filtered to a configured author allow-list.",
            pr_number_schema(),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args("github_pr_comments", &args, &["pr_number"])?;
        let pr_number = number_arg(&args, "pr_number")?;
        let repo = resolve_repo(&self.workspace_root)?;
        let review = self
            .client
            .list_review_comments(&repo, pr_number)
            .await
            .map_err(map_backlog_error)?;
        let issue = self
            .client
            .list_issue_comments(&repo, pr_number)
            .await
            .map_err(map_backlog_error)?;
        let total = review.len() + issue.len();
        let review_kept: Vec<&GithubComment> =
            review.iter().filter(|c| self.is_allowed(c)).collect();
        let issue_kept: Vec<&GithubComment> = issue.iter().filter(|c| self.is_allowed(c)).collect();
        let filtered_out = total - review_kept.len() - issue_kept.len();
        Ok(json!({
            "review_comments": review_kept.iter().map(|c| comment_json(c)).collect::<Vec<_>>(),
            "issue_comments": issue_kept.iter().map(|c| comment_json(c)).collect::<Vec<_>>(),
            "filtered_out": filtered_out,
        }))
    }

    fn name(&self) -> &str {
        "github_pr_comments"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

pub(crate) fn parse_allowed_authors(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(|author| author.trim().to_lowercase())
        .filter(|author| !author.is_empty())
        .collect()
}

pub struct GithubPrPromoteTool {
    workspace_root: PathBuf,
    identity_name: String,
    client: Arc<dyn GithubClient>,
}

impl GithubPrPromoteTool {
    pub fn new(
        workspace_root: PathBuf,
        identity_name: impl Into<String>,
        client: Arc<dyn GithubClient>,
    ) -> Self {
        Self {
            workspace_root,
            identity_name: identity_name.into(),
            client,
        }
    }
}

#[async_trait]
impl Tool for GithubPrPromoteTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "github_pr_promote",
            "Convert an open draft pull request carrying this identity's body marker to ready for review. The marker is advisory attribution, not authentication.",
            pr_number_schema(),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args("github_pr_promote", &args, &["pr_number"])?;
        let pr_number = number_arg(&args, "pr_number")?;
        let repo = resolve_repo(&self.workspace_root)?;
        let detail = self
            .client
            .get_pull_request(&repo, pr_number)
            .await
            .map_err(map_backlog_error)?;
        ensure_owned_by_identity(detail.body.as_deref(), &self.identity_name, pr_number)?;
        ensure_open(&detail.state, pr_number)?;
        if !detail.draft {
            return Ok(json!({ "number": pr_number, "promoted": false, "already_ready": true }));
        }
        self.client
            .mark_pull_request_ready(&repo, pr_number)
            .await
            .map_err(map_backlog_error)?;
        Ok(json!({ "number": pr_number, "promoted": true, "already_ready": false }))
    }

    fn name(&self) -> &str {
        "github_pr_promote"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

pub struct GithubPrCloseTool {
    workspace_root: PathBuf,
    identity_name: String,
    client: Arc<dyn GithubClient>,
}

impl GithubPrCloseTool {
    pub fn new(
        workspace_root: PathBuf,
        identity_name: impl Into<String>,
        client: Arc<dyn GithubClient>,
    ) -> Self {
        Self {
            workspace_root,
            identity_name: identity_name.into(),
            client,
        }
    }
}

#[async_trait]
impl Tool for GithubPrCloseTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "github_pr_close",
            "Close an open pull request carrying this identity's body marker. 'reason' must reference an issue of this repository as '#123' or its full URL, and is posted as a comment before closing. The marker is advisory attribution, not authentication; the referenced issue's existence is not verified.",
            object_schema(
                vec![
                    (
                        "pr_number",
                        property(SchemaType::Integer, "Pull request number."),
                    ),
                    (
                        "reason",
                        property(
                            SchemaType::String,
                            "Why this pull request is being closed. Must reference the originating issue, e.g. '#123'.",
                        ),
                    ),
                ],
                &["pr_number", "reason"],
            ),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args("github_pr_close", &args, &["pr_number", "reason"])?;
        let pr_number = number_arg(&args, "pr_number")?;
        let reason = text_arg(&args, "reason", Shape::Line, REASON_MAX)?;
        let repo = resolve_repo(&self.workspace_root)?;
        if !references_issue(reason, &repo) {
            return Err(invalid(
                "'reason' must reference an issue of this repository (e.g. '#123' or its full GitHub issue URL)",
            ));
        }
        let comment = attributed(reason, &self.identity_name)?;
        let detail = self
            .client
            .get_pull_request(&repo, pr_number)
            .await
            .map_err(map_backlog_error)?;
        ensure_owned_by_identity(detail.body.as_deref(), &self.identity_name, pr_number)?;
        ensure_open(&detail.state, pr_number)?;
        self.client
            .comment_on_issue(&repo, pr_number, &comment)
            .await
            .map_err(map_backlog_error)?;
        self.client
            .close_pull_request(&repo, pr_number)
            .await
            .map_err(map_backlog_error)?;
        Ok(json!({ "number": pr_number, "closed": true, "comment_posted": true }))
    }

    fn name(&self) -> &str {
        "github_pr_close"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

pub struct GithubIssueReadTool {
    workspace_root: PathBuf,
    client: Arc<dyn GithubClient>,
    allowed_authors: Vec<String>,
}

impl GithubIssueReadTool {
    pub fn new(
        workspace_root: PathBuf,
        client: Arc<dyn GithubClient>,
        allowed_authors: Vec<String>,
    ) -> Self {
        Self {
            workspace_root,
            client,
            allowed_authors: normalize_allowed_authors(allowed_authors),
        }
    }
}

#[async_trait]
impl Tool for GithubIssueReadTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "github_issue_read",
            "Read a GitHub issue's title, body, labels and comments. Comments are filtered to the configured author allow-list; the title and body are returned as written.",
            object_schema(
                vec![(
                    "issue_number",
                    property(SchemaType::Integer, "Issue number."),
                )],
                &["issue_number"],
            ),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args("github_issue_read", &args, &["issue_number"])?;
        let issue_number = number_arg(&args, "issue_number")?;
        let repo = resolve_repo(&self.workspace_root)?;
        let detail = self
            .client
            .get_issue(&repo, issue_number)
            .await
            .map_err(map_backlog_error)?;
        let comments = self
            .client
            .list_issue_comments(&repo, issue_number)
            .await
            .map_err(map_backlog_error)?;
        let total = comments.len();
        let kept: Vec<&GithubComment> = comments
            .iter()
            .filter(|c| comment_is_allowed(c, &self.allowed_authors))
            .collect();
        Ok(json!({
            "number": detail.number,
            "title": detail.title,
            "body": detail.body,
            "labels": detail.labels,
            "html_url": detail.html_url,
            "comments": kept.iter().map(|c| comment_json(c)).collect::<Vec<_>>(),
            "filtered_out": total - kept.len(),
        }))
    }

    fn name(&self) -> &str {
        "github_issue_read"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

pub struct GithubIssueCommentTool {
    workspace_root: PathBuf,
    identity_name: String,
    client: Arc<dyn GithubClient>,
}

impl GithubIssueCommentTool {
    pub fn new(
        workspace_root: PathBuf,
        identity_name: impl Into<String>,
        client: Arc<dyn GithubClient>,
    ) -> Self {
        Self {
            workspace_root,
            identity_name: identity_name.into(),
            client,
        }
    }
}

#[async_trait]
impl Tool for GithubIssueCommentTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "github_issue_comment",
            "Post a comment on a GitHub issue or pull request of this repository, attributed to this identity. It cannot close anything. Any issue number in the repository is accepted.",
            object_schema(
                vec![
                    (
                        "issue_number",
                        property(SchemaType::Integer, "Issue number."),
                    ),
                    ("body", property(SchemaType::String, "Comment body.")),
                ],
                &["issue_number", "body"],
            ),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        check_args("github_issue_comment", &args, &["issue_number", "body"])?;
        let issue_number = number_arg(&args, "issue_number")?;
        let body = text_arg(&args, "body", Shape::Block, COMMENT_MAX)?;
        let comment = attributed(body, &self.identity_name)?;
        let repo = resolve_repo(&self.workspace_root)?;
        self.client
            .comment_on_issue(&repo, issue_number, &comment)
            .await
            .map_err(map_backlog_error)?;
        Ok(json!({ "number": issue_number, "commented": true }))
    }

    fn name(&self) -> &str {
        "github_issue_comment"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

pub fn register(registry: &mut ToolRegistry, workspace_root: &Path, identity_name: &str) {
    let token = std::env::var("GITHUB_TOKEN").ok();
    let client: Arc<dyn GithubClient> =
        Arc::new(crate::backlog::ReqwestGithubClient::github(token));
    let allowed_authors =
        parse_allowed_authors(std::env::var(TRUSTED_PR_COMMENTERS_ENV).ok().as_deref());
    registry.register(Box::new(GitCommitTool::new(
        workspace_root.to_path_buf(),
        identity_name,
    )));
    registry.register(Box::new(GitPushBranchTool::new(
        workspace_root.to_path_buf(),
        identity_name,
    )));
    registry.register(Box::new(GithubPrOpenTool::new(
        workspace_root.to_path_buf(),
        identity_name,
        Arc::clone(&client),
    )));
    registry.register(Box::new(GithubPrCommentsTool::new(
        workspace_root.to_path_buf(),
        Arc::clone(&client),
        allowed_authors.clone(),
    )));
    registry.register(Box::new(GithubPrPromoteTool::new(
        workspace_root.to_path_buf(),
        identity_name,
        Arc::clone(&client),
    )));
    registry.register(Box::new(GithubPrCloseTool::new(
        workspace_root.to_path_buf(),
        identity_name,
        Arc::clone(&client),
    )));
    registry.register(Box::new(GithubIssueReadTool::new(
        workspace_root.to_path_buf(),
        Arc::clone(&client),
        allowed_authors,
    )));
    registry.register(Box::new(GithubIssueCommentTool::new(
        workspace_root.to_path_buf(),
        identity_name,
        client,
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::test_support::MockGithub;
    use crate::backlog::{GithubIssueDetail, GithubPullRequestCreated, GithubPullRequestDetail};
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git command failed to run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    struct PushFixture {
        _bare: TempDir,
        work: TempDir,
    }

    fn push_fixture() -> PushFixture {
        let bare = TempDir::new().unwrap();
        git(bare.path(), &["init", "--bare", "-q"]);
        let work = TempDir::new().unwrap();
        git(work.path(), &["init", "-q"]);
        git(work.path(), &["config", "user.email", "test@example.com"]);
        git(work.path(), &["config", "user.name", "Test User"]);
        std::fs::write(work.path().join("README.md"), "hello\n").unwrap();
        git(work.path(), &["add", "."]);
        git(work.path(), &["commit", "-q", "-m", "initial"]);
        git(work.path(), &["branch", "-M", "main"]);
        let bare_path = bare.path().to_str().unwrap().to_string();
        git(work.path(), &["remote", "add", "origin", &bare_path]);
        git(work.path(), &["push", "-q", "origin", "main"]);
        git(
            work.path(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        PushFixture { _bare: bare, work }
    }

    #[tokio::test]
    async fn git_push_branch_definition_and_effect_class() {
        let tool = GitPushBranchTool::new(PathBuf::from("/tmp"), "sdlc-dev");
        assert_eq!(tool.name(), "git_push_branch");
        assert_eq!(tool.effect_class(), EffectClass::Repository);
        assert_eq!(tool.definition().function.name, "git_push_branch");
    }

    #[tokio::test]
    async fn git_push_branch_pushes_a_feature_branch() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-x"]);
        std::fs::write(fixture.work.path().join("f.txt"), "x\n").unwrap();
        git(fixture.work.path(), &["add", "."]);
        git(
            fixture.work.path(),
            &["commit", "-q", "-m", "feature\n\nNanna-Identity: sdlc-dev"],
        );

        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["pushed"], json!(true));
        assert_eq!(result["branch"], json!("feature-x"));

        let refs = StdCommand::new("git")
            .args(["ls-remote", "origin", "refs/heads/feature-x"])
            .current_dir(fixture.work.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&refs.stdout).contains("refs/heads/feature-x"));
    }

    #[tokio::test]
    async fn git_push_branch_refuses_a_commit_missing_the_identity_trailer() {
        let fixture = push_fixture();
        git(
            fixture.work.path(),
            &["checkout", "-q", "-b", "feature-untrailed"],
        );
        std::fs::write(fixture.work.path().join("f.txt"), "x\n").unwrap();
        git(fixture.work.path(), &["add", "."]);
        git(
            fixture.work.path(),
            &["commit", "-q", "-m", "no trailer here"],
        );

        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("missing the identity trailer"));

        let refs = StdCommand::new("git")
            .args(["ls-remote", "origin", "refs/heads/feature-untrailed"])
            .current_dir(fixture.work.path())
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&refs.stdout).is_empty(),
            "must not push when a commit is missing the trailer"
        );
    }

    #[tokio::test]
    async fn git_push_branch_refuses_the_default_branch() {
        let fixture = push_fixture();
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        assert!(err.to_string().contains("default branch"));
    }

    #[tokio::test]
    async fn git_push_branch_refuses_a_detached_head() {
        let fixture = push_fixture();
        let sha = run_git(fixture.work.path(), &["rev-parse", "HEAD"]).unwrap();
        git(fixture.work.path(), &["checkout", "-q", &sha]);
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("detached HEAD"));
    }

    #[tokio::test]
    async fn git_push_branch_reports_a_push_failure() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-y"]);
        std::fs::write(fixture.work.path().join("f.txt"), "y\n").unwrap();
        git(fixture.work.path(), &["add", "."]);
        git(
            fixture.work.path(),
            &["commit", "-q", "-m", "feature\n\nNanna-Identity: sdlc-dev"],
        );
        git(
            fixture.work.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "/nanna-pr-tools-nonexistent-remote",
            ],
        );
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn run_git_reports_a_process_spawn_failure() {
        TEST_GIT_BIN_OVERRIDE.with(|cell| {
            *cell.borrow_mut() = Some("nanna-pr-tools-nonexistent-git-binary".to_string())
        });
        let err = run_git(Path::new("."), &["status"]).unwrap_err();
        TEST_GIT_BIN_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("failed to run git status"));
    }

    #[test]
    fn default_branch_falls_back_to_main_when_origin_head_is_unset() {
        let bare = TempDir::new().unwrap();
        git(bare.path(), &["init", "--bare", "-q"]);
        let work = TempDir::new().unwrap();
        git(work.path(), &["init", "-q"]);
        git(work.path(), &["config", "user.email", "test@example.com"]);
        git(work.path(), &["config", "user.name", "Test User"]);
        std::fs::write(work.path().join("f.txt"), "x\n").unwrap();
        git(work.path(), &["add", "."]);
        git(work.path(), &["commit", "-q", "-m", "c"]);
        git(work.path(), &["branch", "-M", "main"]);
        let bare_path = bare.path().to_str().unwrap().to_string();
        git(work.path(), &["remote", "add", "origin", &bare_path]);
        git(work.path(), &["push", "-q", "origin", "main"]);
        git(work.path(), &["fetch", "-q", "origin"]);
        let _ = StdCommand::new("git")
            .args(["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"])
            .current_dir(work.path())
            .output();
        assert_eq!(default_branch(work.path()), Some("main".to_string()));
    }

    fn resolve_repo_fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:example/repo.git"],
        );
        dir
    }

    #[test]
    fn resolve_repo_reads_the_origin_remote() {
        let dir = resolve_repo_fixture();
        assert_eq!(resolve_repo(dir.path()).unwrap(), "example/repo");
    }

    #[test]
    fn resolve_repo_fails_without_an_origin_remote() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"]);
        let err = resolve_repo(dir.path()).unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn resolve_repo_fails_for_a_non_github_remote() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "https://example.invalid/o/r.git"],
        );
        let err = resolve_repo(dir.path()).unwrap_err();
        assert!(err.to_string().contains("not a GitHub remote"));
    }

    #[test]
    fn parse_allowed_authors_trims_lowercases_and_drops_blanks() {
        assert_eq!(parse_allowed_authors(None), Vec::<String>::new());
        assert_eq!(
            parse_allowed_authors(Some(" Alice, bob ,, Carol")),
            vec!["alice".to_string(), "bob".to_string(), "carol".to_string()]
        );
    }

    #[test]
    fn references_issue_matches_hash_and_same_repo_url_forms() {
        assert!(references_issue("Closes #647", "o/n"));
        assert!(references_issue(
            "see https://github.com/o/n/issues/647 for detail",
            "o/n"
        ));
        assert!(!references_issue("no reference here", "o/n"));
        assert!(!references_issue(
            "see https://github.com/o/other/issues/647",
            "o/n"
        ));
    }

    fn github_repo_fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "Test User"]);
        std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "initial"]);
        git(dir.path(), &["branch", "-M", "main"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:example/repo.git"],
        );
        dir
    }

    #[tokio::test]
    async fn github_pr_open_definition_and_effect_class() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        assert_eq!(tool.name(), "github_pr_open");
        assert_eq!(tool.effect_class(), EffectClass::Repository);
        let def = tool.definition();
        assert_eq!(def.function.name, "github_pr_open");
        assert_eq!(
            def.function.parameters.required,
            Some(vec!["title".to_string(), "body".to_string()])
        );
    }

    #[tokio::test]
    async fn github_pr_open_creates_a_draft_pr_with_identity_marker() {
        let dir = github_repo_fixture();
        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        let mock = Arc::new(MockGithub {
            created_pr: Some(GithubPullRequestCreated {
                number: 9,
                html_url: "https://example.invalid/pr/9".to_string(),
                node_id: "PR_9".to_string(),
            }),
            ..Default::default()
        });
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let result = tool
            .execute(json!({ "title": "Add feature", "body": "Summary.", "base": "main" }))
            .await
            .unwrap();
        assert_eq!(result["number"], json!(9));
        assert_eq!(result["draft"], json!(true));
        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("example/repo"));
        assert!(calls[0].contains("->main"));

        let sent_body = calls[0]
            .split("body=")
            .nth(1)
            .expect("call log records the body");
        assert!(sent_body
            .lines()
            .any(|line| line == crate::marker::render_trailer("sdlc-dev")));
        assert!(sent_body.contains(&crate::marker::render_html_marker("sdlc-dev")));
        assert_eq!(
            crate::marker::parse_identity_from_text(sent_body).as_deref(),
            Some("sdlc-dev")
        );
    }

    #[tokio::test]
    async fn github_pr_open_defaults_base_to_the_default_branch() {
        let fixture = push_fixture();
        git(
            fixture.work.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:example/repo.git",
            ],
        );
        let mock = Arc::new(MockGithub {
            created_pr: Some(GithubPullRequestCreated {
                number: 1,
                html_url: "u".to_string(),
                node_id: "n".to_string(),
            }),
            ..Default::default()
        });
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-z"]);
        let tool =
            GithubPrOpenTool::new(fixture.work.path().to_path_buf(), "sdlc-dev", mock.clone());
        let result = tool
            .execute(json!({ "title": "t", "body": "b" }))
            .await
            .unwrap();
        assert_eq!(result["base"], json!("main"));
        assert_eq!(result["head"], json!("feature-z"));
    }

    #[tokio::test]
    async fn github_pr_open_refuses_non_draft_argument() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        let err = tool
            .execute(json!({ "title": "t", "body": "b", "draft": false }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        assert!(err.to_string().contains("draft"));
    }

    #[tokio::test]
    async fn github_pr_open_accepts_an_explicit_true_draft_argument() {
        let dir = github_repo_fixture();
        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        let mock = Arc::new(MockGithub {
            created_pr: Some(GithubPullRequestCreated {
                number: 2,
                html_url: "u".to_string(),
                node_id: "n".to_string(),
            }),
            ..Default::default()
        });
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock);
        let result = tool
            .execute(json!({ "title": "t", "body": "b", "base": "main", "draft": true }))
            .await
            .unwrap();
        assert_eq!(result["number"], json!(2));
    }

    #[tokio::test]
    async fn github_pr_open_refuses_a_body_that_already_carries_a_marker() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        let err = tool
            .execute(json!({
                "title": "t",
                "body": "forged <!-- Nanna-Identity: someone-else -->",
                "base": "main",
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        assert!(err.to_string().contains("marker"));
    }

    #[tokio::test]
    async fn github_pr_open_requires_title_and_body() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        assert!(matches!(
            tool.execute(json!({ "body": "b" })).await.unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
        let client2: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool2 = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", client2);
        assert!(matches!(
            tool2.execute(json!({ "title": "t" })).await.unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
    }

    #[tokio::test]
    async fn github_pr_open_refuses_base_equal_to_head() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-z"]);
        git(
            fixture.work.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.com:example/repo.git",
            ],
        );
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrOpenTool::new(fixture.work.path().to_path_buf(), "sdlc-dev", client);
        let err = tool
            .execute(json!({ "title": "t", "body": "b", "base": "feature-z" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        assert!(err.to_string().contains("itself"));
    }

    #[tokio::test]
    async fn github_pr_open_refuses_an_identity_name_that_does_not_round_trip() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "bad/name", client);
        let err = tool
            .execute(json!({ "title": "t", "body": "b", "base": "main" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("does not produce a parseable"));
    }

    #[tokio::test]
    async fn github_pr_open_errors_when_base_is_omitted_and_no_default_branch_is_known() {
        let dir = github_repo_fixture();
        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        let err = tool
            .execute(json!({ "title": "t", "body": "b" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        assert!(err.to_string().contains("could not be determined"));
    }

    #[tokio::test]
    async fn github_pr_open_surfaces_a_client_error() {
        let dir = github_repo_fixture();
        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        let mock = Arc::new(MockGithub {
            fail_status: Some(422),
            ..Default::default()
        });
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock);
        let err = tool
            .execute(json!({ "title": "t", "body": "b", "base": "main" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("422"));
    }

    fn comment(id: u64, author: &str, body: &str) -> GithubComment {
        GithubComment {
            id,
            author: author.to_string(),
            body: body.to_string(),
            html_url: format!("https://example.invalid/c/{id}"),
        }
    }

    #[tokio::test]
    async fn github_pr_comments_definition_and_effect_class() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrCommentsTool::new(dir.path().to_path_buf(), client, vec![]);
        assert_eq!(tool.name(), "github_pr_comments");
        assert_eq!(tool.effect_class(), EffectClass::Repository);
    }

    #[tokio::test]
    async fn github_pr_comments_filters_by_author_allow_list() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            review_comments: HashMap::from([(
                ("example/repo".to_string(), 5),
                vec![comment(1, "Alice", "inline")],
            )]),
            issue_comments: HashMap::from([(
                ("example/repo".to_string(), 5),
                vec![comment(2, "bob", "conversation"), comment(3, "eve", "spam")],
            )]),
            ..Default::default()
        });
        let tool = GithubPrCommentsTool::new(
            dir.path().to_path_buf(),
            mock,
            vec!["alice".to_string(), "Bob".to_string()],
        );
        let result = tool.execute(json!({ "pr_number": 5 })).await.unwrap();
        assert_eq!(result["review_comments"].as_array().unwrap().len(), 1);
        assert_eq!(result["issue_comments"].as_array().unwrap().len(), 1);
        assert_eq!(result["filtered_out"], json!(1));
        assert_eq!(result["review_comments"][0]["author"], json!("Alice"));
        assert_eq!(result["issue_comments"][0]["author"], json!("bob"));
    }

    #[tokio::test]
    async fn github_pr_comments_empty_allow_list_filters_everything() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            issue_comments: HashMap::from([(
                ("example/repo".to_string(), 5),
                vec![comment(1, "alice", "hi")],
            )]),
            ..Default::default()
        });
        let tool = GithubPrCommentsTool::new(dir.path().to_path_buf(), mock, vec![]);
        let result = tool.execute(json!({ "pr_number": 5 })).await.unwrap();
        assert_eq!(result["issue_comments"].as_array().unwrap().len(), 0);
        assert_eq!(result["filtered_out"], json!(1));
    }

    #[tokio::test]
    async fn github_pr_comments_requires_pr_number() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrCommentsTool::new(dir.path().to_path_buf(), client, vec![]);
        assert!(matches!(
            tool.execute(json!({})).await.unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
    }

    #[tokio::test]
    async fn github_pr_comments_surfaces_a_client_error() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            fail_status: Some(403),
            ..Default::default()
        });
        let tool = GithubPrCommentsTool::new(dir.path().to_path_buf(), mock, vec![]);
        let err = tool.execute(json!({ "pr_number": 1 })).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    fn pr_detail(number: u64, draft: bool, owner: &str) -> GithubPullRequestDetail {
        GithubPullRequestDetail {
            number,
            node_id: format!("PR_{number}"),
            draft,
            state: "open".to_string(),
            body: Some(format!("Closes #1\n\n<!-- Nanna-Identity: {owner} -->")),
            html_url: format!("https://example.invalid/pr/{number}"),
        }
    }

    #[tokio::test]
    async fn github_pr_promote_definition_and_effect_class() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrPromoteTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        assert_eq!(tool.name(), "github_pr_promote");
        assert_eq!(tool.effect_class(), EffectClass::Repository);
    }

    #[tokio::test]
    async fn github_pr_promote_marks_an_owned_draft_ready() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            pr_details: HashMap::from([(
                ("example/repo".to_string(), 7),
                pr_detail(7, true, "sdlc-dev"),
            )]),
            ..Default::default()
        });
        let tool = GithubPrPromoteTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let result = tool.execute(json!({ "pr_number": 7 })).await.unwrap();
        assert_eq!(result["promoted"], json!(true));
        assert!(mock
            .calls()
            .iter()
            .any(|c| c.contains("mark_pull_request_ready example/repo#7")));
    }

    #[tokio::test]
    async fn github_pr_promote_is_a_no_op_when_already_ready() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            pr_details: HashMap::from([(
                ("example/repo".to_string(), 7),
                pr_detail(7, false, "sdlc-dev"),
            )]),
            ..Default::default()
        });
        let tool = GithubPrPromoteTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let result = tool.execute(json!({ "pr_number": 7 })).await.unwrap();
        assert_eq!(result["already_ready"], json!(true));
        assert!(mock.calls().is_empty());
    }

    #[tokio::test]
    async fn github_pr_promote_refuses_a_pr_owned_by_another_identity() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            pr_details: HashMap::from([(
                ("example/repo".to_string(), 7),
                pr_detail(7, true, "someone-else"),
            )]),
            ..Default::default()
        });
        let tool = GithubPrPromoteTool::new(dir.path().to_path_buf(), "sdlc-dev", mock);
        let err = tool.execute(json!({ "pr_number": 7 })).await.unwrap_err();
        assert!(err.to_string().contains("not owned by identity"));
    }

    #[tokio::test]
    async fn github_pr_promote_surfaces_a_missing_pr_error() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrPromoteTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        let err = tool.execute(json!({ "pr_number": 404 })).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn github_pr_promote_requires_pr_number() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrPromoteTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        assert!(matches!(
            tool.execute(json!({})).await.unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
    }

    #[tokio::test]
    async fn github_pr_close_definition_and_effect_class() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        assert_eq!(tool.name(), "github_pr_close");
        assert_eq!(tool.effect_class(), EffectClass::Repository);
    }

    #[tokio::test]
    async fn github_pr_close_comments_then_closes_an_owned_pr() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            pr_details: HashMap::from([(
                ("example/repo".to_string(), 3),
                pr_detail(3, true, "sdlc-dev"),
            )]),
            ..Default::default()
        });
        let tool = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let result = tool
            .execute(json!({ "pr_number": 3, "reason": "Misaligned with #1" }))
            .await
            .unwrap();
        assert_eq!(result["closed"], json!(true));
        let calls = mock.calls();
        let comment_idx = calls
            .iter()
            .position(|c| c.starts_with("comment_on_issue"))
            .unwrap();
        let close_idx = calls
            .iter()
            .position(|c| c.starts_with("close_pull_request"))
            .unwrap();
        assert!(comment_idx < close_idx, "must comment before closing");
    }

    #[tokio::test]
    async fn github_pr_close_refuses_a_reason_without_an_issue_reference() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        let err = tool
            .execute(json!({ "pr_number": 3, "reason": "no longer needed" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn github_pr_close_accepts_an_issue_url_reason() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            pr_details: HashMap::from([(
                ("example/repo".to_string(), 3),
                pr_detail(3, true, "sdlc-dev"),
            )]),
            ..Default::default()
        });
        let tool = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", mock);
        let result = tool
            .execute(json!({
                "pr_number": 3,
                "reason": "See https://github.com/example/repo/issues/1",
            }))
            .await
            .unwrap();
        assert_eq!(result["closed"], json!(true));
    }

    #[tokio::test]
    async fn github_pr_close_refuses_a_pr_owned_by_another_identity() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            pr_details: HashMap::from([(
                ("example/repo".to_string(), 3),
                pr_detail(3, true, "someone-else"),
            )]),
            ..Default::default()
        });
        let tool = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let err = tool
            .execute(json!({ "pr_number": 3, "reason": "Closes #1" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not owned by identity"));
        assert!(mock.calls().is_empty(), "must not comment or close");
    }

    #[tokio::test]
    async fn github_pr_close_surfaces_a_missing_pr_error_without_commenting() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub::default());
        let tool = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let err = tool
            .execute(json!({ "pr_number": 404, "reason": "Closes #1" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(
            mock.calls().is_empty(),
            "must not comment when the PR lookup fails"
        );
    }

    #[tokio::test]
    async fn github_pr_close_requires_pr_number_and_reason() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        assert!(matches!(
            tool.execute(json!({ "reason": "Closes #1" }))
                .await
                .unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
        let client2: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool2 = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", client2);
        assert!(matches!(
            tool2.execute(json!({ "pr_number": 3 })).await.unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
    }

    #[tokio::test]
    async fn github_issue_read_definition_and_effect_class() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubIssueReadTool::new(dir.path().to_path_buf(), client, vec![]);
        assert_eq!(tool.name(), "github_issue_read");
        assert_eq!(tool.effect_class(), EffectClass::Repository);
    }

    #[tokio::test]
    async fn github_issue_read_returns_title_body_labels_and_comments() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            issue_details: HashMap::from([(
                ("example/repo".to_string(), 647),
                GithubIssueDetail {
                    number: 647,
                    title: "PR lifecycle tools".to_string(),
                    body: Some("Context.".to_string()),
                    labels: vec!["enhancement".to_string()],
                    html_url: "https://example.invalid/issues/647".to_string(),
                },
            )]),
            issue_comments: HashMap::from([(
                ("example/repo".to_string(), 647),
                vec![comment(1, "alice", "note")],
            )]),
            ..Default::default()
        });
        let tool =
            GithubIssueReadTool::new(dir.path().to_path_buf(), mock, vec!["alice".to_string()]);
        let result = tool.execute(json!({ "issue_number": 647 })).await.unwrap();
        assert_eq!(result["title"], json!("PR lifecycle tools"));
        assert_eq!(result["labels"], json!(["enhancement"]));
        assert_eq!(result["comments"].as_array().unwrap().len(), 1);
        assert_eq!(result["filtered_out"], json!(0));
    }

    #[tokio::test]
    async fn github_issue_read_filters_comments_by_the_same_author_allow_list_as_pr_comments() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            issue_details: HashMap::from([(
                ("example/repo".to_string(), 5),
                GithubIssueDetail {
                    number: 5,
                    title: "A pull request".to_string(),
                    body: None,
                    labels: vec![],
                    html_url: "https://example.invalid/pull/5".to_string(),
                },
            )]),
            issue_comments: HashMap::from([(
                ("example/repo".to_string(), 5),
                vec![comment(1, "alice", "trusted"), comment(2, "eve", "spam")],
            )]),
            ..Default::default()
        });
        let tool =
            GithubIssueReadTool::new(dir.path().to_path_buf(), mock, vec!["alice".to_string()]);
        let result = tool.execute(json!({ "issue_number": 5 })).await.unwrap();
        let comments = result["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["author"], json!("alice"));
        assert_eq!(result["filtered_out"], json!(1));
    }

    #[tokio::test]
    async fn github_issue_read_empty_allow_list_filters_everything() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            issue_details: HashMap::from([(
                ("example/repo".to_string(), 5),
                GithubIssueDetail {
                    number: 5,
                    title: "An issue".to_string(),
                    body: None,
                    labels: vec![],
                    html_url: "https://example.invalid/issues/5".to_string(),
                },
            )]),
            issue_comments: HashMap::from([(
                ("example/repo".to_string(), 5),
                vec![comment(1, "alice", "hi")],
            )]),
            ..Default::default()
        });
        let tool = GithubIssueReadTool::new(dir.path().to_path_buf(), mock, vec![]);
        let result = tool.execute(json!({ "issue_number": 5 })).await.unwrap();
        assert_eq!(result["comments"].as_array().unwrap().len(), 0);
        assert_eq!(result["filtered_out"], json!(1));
    }

    #[tokio::test]
    async fn github_issue_read_surfaces_a_missing_issue_error() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubIssueReadTool::new(dir.path().to_path_buf(), client, vec![]);
        let err = tool
            .execute(json!({ "issue_number": 999 }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn github_issue_read_requires_issue_number() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubIssueReadTool::new(dir.path().to_path_buf(), client, vec![]);
        assert!(matches!(
            tool.execute(json!({})).await.unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
    }

    #[tokio::test]
    async fn github_issue_comment_definition_and_effect_class() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubIssueCommentTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        assert_eq!(tool.name(), "github_issue_comment");
        assert_eq!(tool.effect_class(), EffectClass::Repository);
    }

    #[tokio::test]
    async fn github_issue_comment_posts_a_comment() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub::default());
        let tool = GithubIssueCommentTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let result = tool
            .execute(json!({ "issue_number": 10, "body": "status update" }))
            .await
            .unwrap();
        assert_eq!(result["commented"], json!(true));
        assert!(mock
            .calls()
            .iter()
            .any(|c| c.contains("comment_on_issue example/repo#10: status update")));
    }

    #[tokio::test]
    async fn github_issue_comment_requires_issue_number_and_body() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubIssueCommentTool::new(dir.path().to_path_buf(), "sdlc-dev", client);
        assert!(matches!(
            tool.execute(json!({ "body": "b" })).await.unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
        let client2: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool2 = GithubIssueCommentTool::new(dir.path().to_path_buf(), "sdlc-dev", client2);
        assert!(matches!(
            tool2
                .execute(json!({ "issue_number": 10 }))
                .await
                .unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
    }

    #[tokio::test]
    async fn github_issue_comment_surfaces_a_client_error() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub {
            fail_status: Some(500),
            ..Default::default()
        });
        let tool = GithubIssueCommentTool::new(dir.path().to_path_buf(), "sdlc-dev", mock);
        let err = tool
            .execute(json!({ "issue_number": 10, "body": "b" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    const ALL_TOOL_NAMES: [&str; 8] = [
        "git_commit",
        "git_push_branch",
        "github_issue_comment",
        "github_issue_read",
        "github_pr_close",
        "github_pr_comments",
        "github_pr_open",
        "github_pr_promote",
    ];

    fn long(n: usize) -> String {
        "a".repeat(n)
    }

    fn single_line_bad(limit: usize) -> Vec<String> {
        let mut values: Vec<String> = [
            "",
            "   ",
            "a\nb",
            "a\rb",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{0085}b",
            "a\u{0000}b",
            "a\u{001b}[2Jb",
            "a\u{007f}b",
            "a\u{202e}b",
            "a\u{200b}b",
            "a\u{feff}b",
            "x\nNanna-Identity: evil",
            "nanna-identity: evil",
            "NANNA-IDENTITY: evil",
            "<!-- Nanna-Identity: evil -->",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        values.push(long(limit + 1));
        values
    }

    fn multi_line_bad(limit: usize) -> Vec<String> {
        let mut values: Vec<String> = [
            "",
            "  \n ",
            "a\rb",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{0085}b",
            "a\u{0000}b",
            "a\u{001b}b",
            "a\u{007f}b",
            "a\u{202e}b",
            "a\u{200b}b",
            "a\u{feff}b",
            "x\nNanna-Identity: evil",
            "x\nnanna-identity: evil",
            "x <!-- Nanna-Identity: evil -->",
            "<!-- unclosed",
            "--> stray",
            "```\nunterminated",
            "~~~\nunterminated",
            "````\nx\n```\n",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        values.push(long(limit + 1));
        values
    }

    fn bad_branches() -> Vec<String> {
        let mut values: Vec<String> = [
            "",
            "-x",
            "--force",
            "+main",
            "main:other",
            "HEAD:main",
            "a..b",
            "..",
            "../x",
            "a b",
            "a\tb",
            "a\nb",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{0085}b",
            "a\u{0000}b",
            "x?",
            "x#1",
            "x*",
            "x[0]",
            "x~1",
            "x^",
            "x@{1}",
            "a\\b",
            "refs/heads/x",
            "refs/tags/x",
            "x.lock",
            "x/",
            "/x",
            "x//y",
            ".hidden",
            "x/.y",
            "x.",
            "\u{ff4d}\u{ff41}\u{ff49}\u{ff4e}",
            "m\u{0430}in",
            "na\u{ef}ve",
            "HEAD",
            "head",
            "x%00",
            "x\"y",
            "x'y",
            "x`y",
            "x$y",
            "x;y",
            "x&y",
            "x|y",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        values.push(long(101));
        values
    }

    fn bad_numbers() -> Vec<Value> {
        vec![
            json!(0),
            json!(-1),
            json!(1.5),
            json!("1"),
            json!("1#2"),
            json!("../1"),
            json!("1?x=2"),
            json!(null),
            json!(true),
            json!(u64::MAX),
            json!(1_000_000_000u64),
            json!([1]),
            json!({ "n": 1 }),
        ]
    }

    fn with_field(base: &Value, key: &str, value: Value) -> Value {
        let mut args = base.clone();
        args[key] = value;
        args
    }

    fn assert_invalid(result: ToolResult<Value>, context: &str) {
        match result {
            Err(ToolError::InvalidArguments { .. }) => {}
            other => panic!("{context}: expected InvalidArguments, got {other:?}"),
        }
    }

    fn remote_has(dir: &Path, branch: &str) -> bool {
        let out = StdCommand::new("git")
            .args(["ls-remote", "origin", &format!("refs/heads/{branch}")])
            .current_dir(dir)
            .output()
            .unwrap();
        !out.stdout.is_empty()
    }

    fn commit_file(dir: &Path, rel: &str, content: &str, message: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    fn head_sha(dir: &Path) -> String {
        run_git(dir, &["rev-parse", "HEAD"]).unwrap()
    }

    fn api_tools(dir: &Path, mock: Arc<MockGithub>) -> Vec<(&'static str, Box<dyn Tool>, Value)> {
        let client: Arc<dyn GithubClient> = mock;
        let root = dir.to_path_buf();
        vec![
            (
                "github_pr_open",
                Box::new(GithubPrOpenTool::new(
                    root.clone(),
                    "sdlc-dev",
                    Arc::clone(&client),
                )),
                json!({ "title": "t", "body": "b", "base": "main" }),
            ),
            (
                "github_pr_comments",
                Box::new(GithubPrCommentsTool::new(
                    root.clone(),
                    Arc::clone(&client),
                    vec!["alice".to_string()],
                )),
                json!({ "pr_number": 3 }),
            ),
            (
                "github_pr_promote",
                Box::new(GithubPrPromoteTool::new(
                    root.clone(),
                    "sdlc-dev",
                    Arc::clone(&client),
                )),
                json!({ "pr_number": 3 }),
            ),
            (
                "github_pr_close",
                Box::new(GithubPrCloseTool::new(
                    root.clone(),
                    "sdlc-dev",
                    Arc::clone(&client),
                )),
                json!({ "pr_number": 3, "reason": "Closes #1" }),
            ),
            (
                "github_issue_read",
                Box::new(GithubIssueReadTool::new(
                    root.clone(),
                    Arc::clone(&client),
                    vec!["alice".to_string()],
                )),
                json!({ "issue_number": 3 }),
            ),
            (
                "github_issue_comment",
                Box::new(GithubIssueCommentTool::new(
                    root.clone(),
                    "sdlc-dev",
                    Arc::clone(&client),
                )),
                json!({ "issue_number": 3, "body": "hello" }),
            ),
            (
                "git_commit",
                Box::new(GitCommitTool::new(root, "sdlc-dev")),
                json!({ "subject": "feat: x", "body": "details" }),
            ),
        ]
    }

    fn rich_mock() -> Arc<MockGithub> {
        Arc::new(MockGithub {
            pr_details: HashMap::from([(
                ("example/repo".to_string(), 3),
                pr_detail(3, true, "sdlc-dev"),
            )]),
            created_pr: Some(GithubPullRequestCreated {
                number: 9,
                html_url: "u".to_string(),
                node_id: "n".to_string(),
            }),
            ..Default::default()
        })
    }

    fn feature_repo() -> TempDir {
        let dir = github_repo_fixture();
        git(dir.path(), &["checkout", "-q", "-b", "feature"]);
        dir
    }

    #[tokio::test]
    async fn every_api_tool_rejects_unknown_arguments() {
        let dir = feature_repo();
        let mock = rich_mock();
        for (name, tool, valid) in api_tools(dir.path(), mock.clone()) {
            for key in [
                "repo", "branch", "remote", "force", "refspec", "owner", "head",
            ] {
                let args = with_field(&valid, key, json!("x"));
                assert_invalid(tool.execute(args).await, &format!("{name} unknown {key}"));
            }
            assert_invalid(
                tool.execute(json!("x")).await,
                &format!("{name} string args"),
            );
            assert_invalid(
                tool.execute(json!([1])).await,
                &format!("{name} array args"),
            );
        }
        assert!(mock.calls().is_empty());
    }

    #[tokio::test]
    async fn every_number_field_rejects_adversarial_values() {
        let dir = feature_repo();
        let mock = rich_mock();
        for (name, tool, valid) in api_tools(dir.path(), mock.clone()) {
            let field = match name {
                "github_issue_read" | "github_issue_comment" => "issue_number",
                "github_pr_open" | "git_commit" => continue,
                _ => "pr_number",
            };
            for bad in bad_numbers() {
                let args = with_field(&valid, field, bad.clone());
                assert_invalid(tool.execute(args).await, &format!("{name}.{field}={bad}"));
            }
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert_invalid(
                tool.execute(missing).await,
                &format!("{name} missing {field}"),
            );
        }
        assert!(mock.calls().is_empty());
    }

    #[tokio::test]
    async fn number_fields_accept_the_documented_bounds() {
        let dir = feature_repo();
        let mock = Arc::new(MockGithub::default());
        let client: Arc<dyn GithubClient> = mock;
        let comments = GithubPrCommentsTool::new(
            dir.path().to_path_buf(),
            Arc::clone(&client),
            vec!["alice".to_string()],
        );
        for n in [1u64, 999_999_999] {
            assert!(
                comments.execute(json!({ "pr_number": n })).await.is_ok(),
                "{n}"
            );
        }
    }

    #[tokio::test]
    async fn pr_open_title_rejects_adversarial_values_and_accepts_the_limit() {
        let dir = feature_repo();
        let mock = rich_mock();
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let valid = json!({ "title": "t", "body": "b", "base": "main" });
        for bad in single_line_bad(256) {
            let args = with_field(&valid, "title", json!(bad));
            assert_invalid(tool.execute(args).await, &format!("title {bad:?}"));
        }
        for bad in [json!(1), json!(null), json!(["t"]), json!({ "a": 1 })] {
            let args = with_field(&valid, "title", bad.clone());
            assert_invalid(tool.execute(args).await, &format!("title {bad}"));
        }
        assert!(mock.calls().is_empty());
        let args = with_field(&valid, "title", json!(long(256)));
        assert!(tool.execute(args).await.is_ok());
    }

    #[tokio::test]
    async fn pr_open_body_rejects_adversarial_values_and_accepts_the_limit() {
        let dir = feature_repo();
        let mock = rich_mock();
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let valid = json!({ "title": "t", "body": "b", "base": "main" });
        for bad in multi_line_bad(20_000) {
            let args = with_field(&valid, "body", json!(bad));
            assert_invalid(tool.execute(args).await, &format!("body {bad:?}"));
        }
        for bad in [json!(1), json!(null), json!(["b"])] {
            let args = with_field(&valid, "body", bad.clone());
            assert_invalid(tool.execute(args).await, &format!("body {bad}"));
        }
        assert!(mock.calls().is_empty());
        let args = with_field(&valid, "body", json!(long(20_000)));
        assert!(tool.execute(args).await.is_ok());
    }

    #[tokio::test]
    async fn pr_open_body_with_headings_and_balanced_fences_keeps_the_identity_marker_last() {
        let dir = feature_repo();
        let mock = rich_mock();
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        for body in [
            "## Summary\n- one\n\n# Another",
            "```rust\nlet x = 1;\n```\n",
            "~~~\ntext\n~~~",
            "````\n```\nnested\n```\n````",
            "tab\tseparated\nline",
        ] {
            tool.execute(json!({ "title": "t", "body": body, "base": "main" }))
                .await
                .unwrap_or_else(|e| panic!("{body:?}: {e}"));
        }
        for call in mock.calls() {
            let sent = call.split("body=").nth(1).unwrap();
            assert_eq!(parse_identity_from_text(sent).as_deref(), Some("sdlc-dev"));
            assert!(sent.trim_end().ends_with(&render_html_marker("sdlc-dev")));
            assert_eq!(sent.matches("Nanna-Identity").count(), 2);
        }
    }

    #[tokio::test]
    async fn pr_open_base_rejects_adversarial_values() {
        let dir = feature_repo();
        let mock = rich_mock();
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let valid = json!({ "title": "t", "body": "b", "base": "main" });
        for bad in bad_branches() {
            let args = with_field(&valid, "base", json!(bad));
            assert_invalid(tool.execute(args).await, &format!("base {bad:?}"));
        }
        for bad in [json!(1), json!(null), json!(["main"])] {
            let args = with_field(&valid, "base", bad.clone());
            assert_invalid(tool.execute(args).await, &format!("base {bad}"));
        }
        assert!(mock.calls().is_empty());
        for good in ["feature-x", "release/1.2_rc", "main"] {
            let args = with_field(&valid, "base", json!(good));
            assert!(tool.execute(args).await.is_ok(), "{good}");
        }
    }

    #[tokio::test]
    async fn pr_open_draft_is_the_only_mode() {
        let dir = feature_repo();
        let mock = rich_mock();
        let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let valid = json!({ "title": "t", "body": "b", "base": "main" });
        for bad in [
            json!(false),
            json!("true"),
            json!("false"),
            json!(0),
            json!(1),
            json!(null),
            json!([true]),
        ] {
            let args = with_field(&valid, "draft", bad.clone());
            assert_invalid(tool.execute(args).await, &format!("draft {bad}"));
        }
        for key in ["ready", "is_draft", "ready_for_review", "state"] {
            let args = with_field(&valid, key, json!(false));
            assert_invalid(tool.execute(args).await, key);
        }
        assert!(mock.calls().is_empty());
        let result = tool.execute(valid).await.unwrap();
        assert_eq!(result["draft"], json!(true));
        assert!(mock.calls()[0].starts_with("create_draft_pull_request"));
    }

    #[tokio::test]
    async fn pr_open_refuses_to_open_from_a_default_or_protected_head() {
        for head in ["main", "master", "Main", "MASTER"] {
            let dir = github_repo_fixture();
            if head != "main" {
                git(dir.path(), &["checkout", "-q", "-b", head]);
            }
            let mock = rich_mock();
            let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
            let result = tool
                .execute(json!({ "title": "t", "body": "b", "base": "release" }))
                .await;
            assert_invalid(result, head);
            assert!(mock.calls().is_empty(), "{head}");
        }
    }

    #[tokio::test]
    async fn pr_open_refuses_a_head_branch_with_an_unsafe_name() {
        for head in ["feat+x", "feat#1", "feat@x", "feat,x", "na\u{ef}ve"] {
            let dir = github_repo_fixture();
            git(dir.path(), &["checkout", "-q", "-b", head]);
            let mock = rich_mock();
            let tool = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
            let result = tool
                .execute(json!({ "title": "t", "body": "b", "base": "main" }))
                .await;
            assert_invalid(result, head);
            assert!(mock.calls().is_empty(), "{head}");
        }
    }

    #[tokio::test]
    async fn pr_close_reason_rejects_adversarial_values() {
        let dir = feature_repo();
        let mock = rich_mock();
        let tool = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let valid = json!({ "pr_number": 3, "reason": "Closes #1" });
        let mut bad = single_line_bad(1000);
        bad.extend(
            [
                "no reference",
                "#0",
                "Closes #1234567890",
                "see http://example.com/page#123",
                "See `#123` in the old thread",
                "it&#39;s done",
                "x#12",
                "See https://github.com/some-other-org/unrelated-repo/issues/1",
                "https://github.com/example/repo/issues/1#issuecomment-2",
                "https://github.com/example/repo/issues/1?x=2",
                "https://github.com/example/repo/issues/1/../../x",
                "https://github.com/example/repo/issues/0",
                "https://github.com/example/repo/pull/1",
                "https://evil.example/https://github.com/example/repo/issues/1",
                "https://github.com/example/repo-evil/issues/1",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        for reason in bad {
            let args = with_field(&valid, "reason", json!(reason));
            assert_invalid(tool.execute(args).await, &format!("reason {reason:?}"));
        }
        for bad in [json!(1), json!(null), json!(["#1"])] {
            let args = with_field(&valid, "reason", bad.clone());
            assert_invalid(tool.execute(args).await, &format!("reason {bad}"));
        }
        assert!(mock.calls().is_empty());
    }

    #[tokio::test]
    async fn pr_close_reason_accepts_same_repo_references_and_posts_an_attributed_comment() {
        let dir = feature_repo();
        for reason in [
            "Closes #12",
            "(#12)",
            "superseded, see #12.",
            "See https://github.com/example/repo/issues/12.",
            "See https://github.com/Example/Repo/issues/12",
            "#999999999",
        ] {
            let mock = rich_mock();
            let tool = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
            tool.execute(json!({ "pr_number": 3, "reason": reason }))
                .await
                .unwrap_or_else(|e| panic!("{reason}: {e}"));
            let calls = mock.calls();
            assert!(calls[0].starts_with("comment_on_issue example/repo#3"));
            assert!(calls[0].contains(reason));
            assert_eq!(
                parse_identity_from_text(&calls[0]).as_deref(),
                Some("sdlc-dev")
            );
            assert!(calls[1].starts_with("close_pull_request"));
        }
    }

    #[tokio::test]
    async fn pr_close_and_promote_refuse_a_pull_request_that_is_not_open() {
        let dir = feature_repo();
        let mut detail = pr_detail(3, true, "sdlc-dev");
        detail.state = "closed".to_string();
        let mock = Arc::new(MockGithub {
            pr_details: HashMap::from([(("example/repo".to_string(), 3), detail)]),
            ..Default::default()
        });
        let close = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let promote = GithubPrPromoteTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let a = close
            .execute(json!({ "pr_number": 3, "reason": "#1" }))
            .await;
        let b = promote.execute(json!({ "pr_number": 3 })).await;
        assert!(matches!(a, Err(ToolError::ExecutionFailed { .. })), "{a:?}");
        assert!(matches!(b, Err(ToolError::ExecutionFailed { .. })), "{b:?}");
        assert!(mock.calls().is_empty());
    }

    #[tokio::test]
    async fn pr_promote_stays_same_identity_only_so_cross_identity_handoff_is_refused() {
        let dir = feature_repo();
        let mock = Arc::new(MockGithub {
            pr_details: HashMap::from([(
                ("example/repo".to_string(), 3),
                pr_detail(3, true, "rust-implementer"),
            )]),
            ..Default::default()
        });
        let tool = GithubPrPromoteTool::new(dir.path().to_path_buf(), "pr-shepherd", mock.clone());
        let err = tool.execute(json!({ "pr_number": 3 })).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("not owned by identity 'pr-shepherd'"));
        assert!(mock.calls().is_empty());
    }

    #[tokio::test]
    async fn ownership_marker_must_be_a_real_marker_not_a_lookalike() {
        let dir = feature_repo();
        for body in [
            "Nanna\u{2011}Identity: sdlc-dev",
            "nanna-identity: sdlc-dev",
            "see Nanna-Identity: sdlc-dev inline",
            "<!-- Nanna-Identity: sdlc-dev-extra -->",
            "",
        ] {
            let mut detail = pr_detail(3, true, "sdlc-dev");
            detail.body = Some(body.to_string());
            let mock = Arc::new(MockGithub {
                pr_details: HashMap::from([(("example/repo".to_string(), 3), detail)]),
                ..Default::default()
            });
            let tool = GithubPrPromoteTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
            assert!(
                tool.execute(json!({ "pr_number": 3 })).await.is_err(),
                "{body:?}"
            );
            assert!(mock.calls().is_empty());
        }
    }

    #[tokio::test]
    async fn issue_comment_body_rejects_adversarial_values_and_is_attributed() {
        let dir = feature_repo();
        let mock = rich_mock();
        let tool = GithubIssueCommentTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let valid = json!({ "issue_number": 3, "body": "hello" });
        for bad in multi_line_bad(10_000) {
            let args = with_field(&valid, "body", json!(bad));
            assert_invalid(tool.execute(args).await, &format!("body {bad:?}"));
        }
        for bad in [json!(1), json!(null), json!(["x"])] {
            let args = with_field(&valid, "body", bad.clone());
            assert_invalid(tool.execute(args).await, &format!("body {bad}"));
        }
        assert!(mock.calls().is_empty());
        for body in ["## Heading\ntext", "```\ncode\n```", long(10_000).as_str()] {
            tool.execute(with_field(&valid, "body", json!(body)))
                .await
                .unwrap();
        }
        let calls = mock.calls();
        assert_eq!(calls.len(), 3);
        for call in calls {
            assert_eq!(parse_identity_from_text(&call).as_deref(), Some("sdlc-dev"));
            assert_eq!(call.matches("Nanna-Identity").count(), 1);
        }
    }

    #[tokio::test]
    async fn push_takes_no_arguments() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-a"]);
        commit_file(
            fixture.work.path(),
            "f.txt",
            "x",
            "f\n\nNanna-Identity: sdlc-dev",
        );
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        for key in [
            "branch",
            "remote",
            "force",
            "refspec",
            "ref",
            "set_upstream",
        ] {
            let args = json!({ key: "x" });
            assert_invalid(tool.execute(args).await, key);
        }
        assert_invalid(tool.execute(json!("x")).await, "string");
        assert_invalid(tool.execute(json!([1])).await, "array");
        assert!(!remote_has(fixture.work.path(), "feature-a"));
        assert!(tool.execute(json!(null)).await.is_ok());
        assert!(tool.execute(json!({})).await.is_ok());
    }

    #[tokio::test]
    async fn push_refuses_default_and_protected_branch_names_in_any_case() {
        for name in ["master", "Main", "MASTER"] {
            let fixture = push_fixture();
            git(fixture.work.path(), &["checkout", "-q", "-b", name]);
            commit_file(
                fixture.work.path(),
                "f.txt",
                "x",
                "f\n\nNanna-Identity: sdlc-dev",
            );
            let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
            assert_invalid(tool.execute(json!({})).await, name);
            assert!(!remote_has(fixture.work.path(), name), "{name}");
        }
    }

    #[tokio::test]
    async fn push_refuses_a_checked_out_branch_with_an_unsafe_name() {
        for name in [
            "feat+x",
            "feat#1",
            "feat@x",
            "feat,x",
            "na\u{ef}ve",
            "feat%x",
        ] {
            let fixture = push_fixture();
            git(fixture.work.path(), &["checkout", "-q", "-b", name]);
            commit_file(
                fixture.work.path(),
                "f.txt",
                "x",
                "f\n\nNanna-Identity: sdlc-dev",
            );
            let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
            assert_invalid(tool.execute(json!({})).await, name);
            assert!(!remote_has(fixture.work.path(), name), "{name}");
        }
    }

    #[tokio::test]
    async fn push_refuses_a_commit_carrying_another_identitys_trailer() {
        for trailer in [
            "Nanna-Identity: other",
            "Nanna-Identity: sdlc-dev-extra",
            "nanna-identity: sdlc-dev",
        ] {
            let fixture = push_fixture();
            git(fixture.work.path(), &["checkout", "-q", "-b", "feature-t"]);
            commit_file(
                fixture.work.path(),
                "f.txt",
                "x",
                &format!("f\n\n{trailer}"),
            );
            let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
            assert!(tool.execute(json!({})).await.is_err(), "{trailer}");
            assert!(!remote_has(fixture.work.path(), "feature-t"));
        }
    }

    #[tokio::test]
    async fn push_refuses_any_unpushed_commit_that_touches_a_protected_path() {
        for path in [
            ".github/workflows/ci.yml",
            ".github/CODEOWNERS",
            "codecov.yml",
            "windows.toml",
            ".nanna/agents/x.toml",
            "crates/api/.nanna/agents/x.toml",
        ] {
            let fixture = push_fixture();
            git(fixture.work.path(), &["checkout", "-q", "-b", "feature-p"]);
            commit_file(
                fixture.work.path(),
                "ok.txt",
                "x",
                "ok\n\nNanna-Identity: sdlc-dev",
            );
            commit_file(
                fixture.work.path(),
                path,
                "x",
                "protected\n\nNanna-Identity: sdlc-dev",
            );
            commit_file(
                fixture.work.path(),
                "later.txt",
                "x",
                "later\n\nNanna-Identity: sdlc-dev",
            );
            let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
            let err = tool.execute(json!({})).await.unwrap_err();
            assert!(err.to_string().contains("protected"), "{path}: {err}");
            assert!(!remote_has(fixture.work.path(), "feature-p"), "{path}");
        }
    }

    #[tokio::test]
    async fn push_refuses_a_commit_that_deletes_or_renames_a_protected_path() {
        let fixture = push_fixture();
        commit_file(fixture.work.path(), "codecov.yml", "a: 1\n", "base");
        git(fixture.work.path(), &["push", "-q", "origin", "main"]);
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-d"]);
        git(fixture.work.path(), &["mv", "codecov.yml", "moved.yml"]);
        git(
            fixture.work.path(),
            &["commit", "-q", "-m", "mv\n\nNanna-Identity: sdlc-dev"],
        );
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("protected"), "{err}");
        assert!(!remote_has(fixture.work.path(), "feature-d"));
    }

    #[tokio::test]
    async fn push_refuses_a_merge_commit_that_touches_a_protected_path() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-m"]);
        commit_file(
            fixture.work.path(),
            "a.txt",
            "a",
            "a\n\nNanna-Identity: sdlc-dev",
        );
        git(fixture.work.path(), &["checkout", "-q", "main"]);
        commit_file(fixture.work.path(), "codecov.yml", "x", "m");
        git(fixture.work.path(), &["checkout", "-q", "feature-m"]);
        git(
            fixture.work.path(),
            &[
                "merge",
                "-q",
                "--no-ff",
                "main",
                "-m",
                "merge\n\nNanna-Identity: sdlc-dev",
            ],
        );
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("protected"), "{err}");
    }

    #[tokio::test]
    async fn push_never_forces_over_a_diverged_remote_branch() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-f"]);
        commit_file(
            fixture.work.path(),
            "a.txt",
            "a",
            "a\n\nNanna-Identity: sdlc-dev",
        );
        git(fixture.work.path(), &["push", "-q", "origin", "feature-f"]);
        let remote_sha = head_sha(fixture.work.path());
        git(fixture.work.path(), &["reset", "-q", "--hard", "main"]);
        commit_file(
            fixture.work.path(),
            "b.txt",
            "b",
            "b\n\nNanna-Identity: sdlc-dev",
        );
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        let out = StdCommand::new("git")
            .args(["ls-remote", "origin", "refs/heads/feature-f"])
            .current_dir(fixture.work.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with(&remote_sha));
    }

    #[cfg(unix)]
    fn install_failing_hook(dir: &Path, hook: &str) {
        use std::os::unix::fs::PermissionsExt;
        let hooks = dir.join("custom-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let script = hooks.join(hook);
        std::fs::write(
            &script,
            "#!/bin/sh\ntouch \"$(dirname \"$0\")/ran\"\nexit 1\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        git(dir, &["config", "core.hooksPath", hooks.to_str().unwrap()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn push_does_not_run_repository_hooks() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-h"]);
        commit_file(
            fixture.work.path(),
            "a.txt",
            "a",
            "a\n\nNanna-Identity: sdlc-dev",
        );
        install_failing_hook(fixture.work.path(), "pre-push");
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        tool.execute(json!({})).await.unwrap();
        assert!(!fixture.work.path().join("custom-hooks/ran").exists());
        assert!(remote_has(fixture.work.path(), "feature-h"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn commit_does_not_run_repository_hooks() {
        let dir = feature_repo();
        install_failing_hook(dir.path(), "pre-commit");
        std::fs::write(dir.path().join("n.txt"), "n").unwrap();
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        tool.execute(json!({ "subject": "feat: n" })).await.unwrap();
        assert!(!dir.path().join("custom-hooks/ran").exists());
    }

    #[tokio::test]
    async fn commit_stamps_the_identity_trailer_and_the_commit_can_then_be_pushed() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-c"]);
        std::fs::write(fixture.work.path().join("n.txt"), "n").unwrap();
        let commit = GitCommitTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let result = commit
            .execute(json!({ "subject": "feat: add n", "body": "Because.\n\n## Notes" }))
            .await
            .unwrap();
        assert_eq!(result["committed"], json!(true));
        let message = run_git(fixture.work.path(), &["log", "-1", "--format=%B"]).unwrap();
        assert_eq!(message.lines().next(), Some("feat: add n"));
        assert_eq!(message.lines().last(), Some("Nanna-Identity: sdlc-dev"));
        assert_eq!(message.matches("Nanna-Identity").count(), 1);
        assert_eq!(
            parse_identity_from_text(&message).as_deref(),
            Some("sdlc-dev")
        );
        let push = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        push.execute(json!({})).await.unwrap();
        assert!(remote_has(fixture.work.path(), "feature-c"));
    }

    #[tokio::test]
    async fn commit_without_a_body_still_stamps_the_trailer() {
        let dir = feature_repo();
        std::fs::write(dir.path().join("n.txt"), "n").unwrap();
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        tool.execute(json!({ "subject": "-not-an-option" }))
            .await
            .unwrap();
        let message = run_git(dir.path(), &["log", "-1", "--format=%B"]).unwrap();
        assert_eq!(message, "-not-an-option\n\nNanna-Identity: sdlc-dev");
    }

    #[tokio::test]
    async fn commit_subject_and_body_reject_adversarial_values() {
        let dir = feature_repo();
        std::fs::write(dir.path().join("n.txt"), "n").unwrap();
        let before = head_sha(dir.path());
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        let valid = json!({ "subject": "feat: x", "body": "details" });
        for bad in single_line_bad(100) {
            let args = with_field(&valid, "subject", json!(bad));
            assert_invalid(tool.execute(args).await, &format!("subject {bad:?}"));
        }
        for bad in multi_line_bad(5000) {
            let args = with_field(&valid, "body", json!(bad));
            assert_invalid(tool.execute(args).await, &format!("body {bad:?}"));
        }
        for bad in [json!(1), json!(null), json!(["x"])] {
            let args = with_field(&valid, "subject", bad.clone());
            assert_invalid(tool.execute(args).await, &format!("subject {bad}"));
        }
        for bad in [json!(1), json!(["x"])] {
            let args = with_field(&valid, "body", bad.clone());
            assert_invalid(tool.execute(args).await, &format!("body {bad}"));
        }
        assert_eq!(head_sha(dir.path()), before);
        let ok = with_field(&valid, "subject", json!(long(100)));
        assert!(tool.execute(ok).await.is_ok());
    }

    #[tokio::test]
    async fn commit_refuses_default_and_unsafe_branches_and_detached_head() {
        for name in ["main", "master", "Main", "feat+x"] {
            let dir = github_repo_fixture();
            if name != "main" {
                git(dir.path(), &["checkout", "-q", "-b", name]);
            }
            std::fs::write(dir.path().join("n.txt"), "n").unwrap();
            let before = head_sha(dir.path());
            let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
            assert_invalid(tool.execute(json!({ "subject": "x" })).await, name);
            assert_eq!(head_sha(dir.path()), before);
        }
        let dir = github_repo_fixture();
        let sha = head_sha(dir.path());
        git(dir.path(), &["checkout", "-q", &sha]);
        std::fs::write(dir.path().join("n.txt"), "n").unwrap();
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({ "subject": "x" })).await.unwrap_err();
        assert!(err.to_string().contains("detached HEAD"));
    }

    #[tokio::test]
    async fn commit_refuses_when_there_is_nothing_to_commit() {
        let dir = feature_repo();
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({ "subject": "x" })).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("nothing to commit"));
    }

    #[tokio::test]
    async fn commit_refuses_a_tracked_change_to_a_protected_path_and_commits_nothing() {
        for path in [
            "codecov.yml",
            ".github/workflows/ci.yml",
            ".nanna/agents/x.toml",
        ] {
            let dir = github_repo_fixture();
            commit_file(dir.path(), path, "a: 1\n", "base");
            git(dir.path(), &["checkout", "-q", "-b", "feature-q"]);
            std::fs::write(dir.path().join(path), "a: 2\n").unwrap();
            std::fs::write(dir.path().join("ok.txt"), "ok").unwrap();
            let before = head_sha(dir.path());
            let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
            let err = tool.execute(json!({ "subject": "x" })).await.unwrap_err();
            assert!(err.to_string().contains("protected"), "{path}: {err}");
            assert_eq!(head_sha(dir.path()), before, "{path}");
            let staged = run_git(dir.path(), &["diff", "--cached", "--name-only"]).unwrap();
            assert!(staged.is_empty(), "{path}: {staged}");
        }
    }

    #[tokio::test]
    async fn commit_refuses_a_staged_protected_path_and_a_staged_rename_into_one() {
        let dir = github_repo_fixture();
        commit_file(dir.path(), "plain.txt", "a", "base");
        git(dir.path(), &["checkout", "-q", "-b", "feature-s"]);
        git(dir.path(), &["mv", "plain.txt", "codecov.yml"]);
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({ "subject": "x" })).await.unwrap_err();
        assert!(err.to_string().contains("protected"), "{err}");
        let dir = github_repo_fixture();
        commit_file(dir.path(), "codecov.yml", "a", "base");
        git(dir.path(), &["checkout", "-q", "-b", "feature-s"]);
        git(dir.path(), &["mv", "codecov.yml", "plain.txt"]);
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({ "subject": "x" })).await.unwrap_err();
        assert!(err.to_string().contains("protected"), "{err}");
    }

    #[tokio::test]
    async fn commit_leaves_untracked_protected_paths_out_of_the_commit() {
        let dir = feature_repo();
        std::fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        std::fs::write(dir.path().join(".github/workflows/x.yml"), "on: push").unwrap();
        std::fs::write(dir.path().join("codecov.yml"), "a: 1").unwrap();
        std::fs::write(dir.path().join("ok.txt"), "ok").unwrap();
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        let result = tool.execute(json!({ "subject": "x" })).await.unwrap();
        let committed = run_git(dir.path(), &["show", "--name-only", "--format=", "HEAD"]).unwrap();
        assert_eq!(committed, "ok.txt");
        let mut skipped: Vec<String> = result["skipped_protected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        skipped.sort();
        assert_eq!(skipped, vec![".github/workflows/x.yml", "codecov.yml"]);
    }

    #[tokio::test]
    async fn commit_handles_paths_with_spaces_and_deletions() {
        let dir = github_repo_fixture();
        commit_file(dir.path(), "gone.txt", "g", "base");
        git(dir.path(), &["checkout", "-q", "-b", "feature-w"]);
        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        std::fs::write(dir.path().join("with space.txt"), "s").unwrap();
        std::fs::write(dir.path().join("--odd.txt"), "o").unwrap();
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        tool.execute(json!({ "subject": "x" })).await.unwrap();
        let mut files: Vec<String> =
            run_git(dir.path(), &["show", "--name-only", "--format=", "HEAD"])
                .unwrap()
                .lines()
                .map(str::to_string)
                .collect();
        files.sort();
        assert_eq!(files, vec!["--odd.txt", "gone.txt", "with space.txt"]);
    }

    fn scoped_registry(dir: &Path, ceiling: EffectClass) -> ToolRegistry {
        let mut identity = crate::identity::example();
        identity.scope.max_effect = ceiling;
        identity.scope.tools = vec!["*".parse().unwrap()];
        crate::tools::create_tool_registry_for(dir, &identity).unwrap()
    }

    #[test]
    fn unscoped_registries_contain_none_of_the_pr_lifecycle_tools() {
        let dir = github_repo_fixture();
        let plain = crate::tools::create_tool_registry(dir.path());
        let read_only = crate::tools::create_tool_registry(dir.path()).read_only_unscoped();
        for registry in [&plain, &read_only] {
            for name in ALL_TOOL_NAMES {
                assert!(registry.get_tool(name).is_none(), "{name}");
            }
        }
    }

    #[test]
    fn identity_scoped_registry_holds_every_pr_tool_and_each_is_repository_class() {
        let dir = github_repo_fixture();
        let registry = scoped_registry(dir.path(), EffectClass::Repository);
        for name in ALL_TOOL_NAMES {
            let tool = registry.get_tool(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(tool.effect_class(), EffectClass::Repository, "{name}");
            assert_eq!(tool.name(), name);
            assert_eq!(tool.definition().function.name, name);
        }
        let below = scoped_registry(dir.path(), EffectClass::Workspace);
        for name in ALL_TOOL_NAMES {
            assert!(below.get_tool(name).is_none(), "{name}");
        }
    }

    #[tokio::test]
    async fn every_pr_tool_is_blocked_without_an_action_gate_and_has_no_effect() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-g"]);
        commit_file(
            fixture.work.path(),
            "a.txt",
            "a",
            "a\n\nNanna-Identity: example",
        );
        std::fs::write(fixture.work.path().join("dirty.txt"), "d").unwrap();
        let before = head_sha(fixture.work.path());
        let registry = scoped_registry(fixture.work.path(), EffectClass::Repository);
        let calls = [
            ("git_commit", json!({ "subject": "x" })),
            ("git_push_branch", json!({})),
            (
                "github_issue_comment",
                json!({ "issue_number": 1, "body": "b" }),
            ),
            ("github_issue_read", json!({ "issue_number": 1 })),
            ("github_pr_close", json!({ "pr_number": 1, "reason": "#1" })),
            ("github_pr_comments", json!({ "pr_number": 1 })),
            ("github_pr_open", json!({ "title": "t", "body": "b" })),
            ("github_pr_promote", json!({ "pr_number": 1 })),
        ];
        assert_eq!(calls.len(), ALL_TOOL_NAMES.len());
        for (name, args) in calls {
            let err = registry.execute(name, args).await.unwrap_err();
            match err {
                ToolError::ActionDenied(
                    crate::action_auditor::ActionDenied::Block { reasons }
                    | crate::action_auditor::ActionDenied::Escalate { reasons },
                ) => {
                    assert!(reasons[0].detail.contains("no action auditor configured"));
                }
                other => panic!("{name}: expected ActionDenied::Block, got {other:?}"),
            }
        }
        assert_eq!(registry.action_reviews().len(), ALL_TOOL_NAMES.len());
        assert_eq!(head_sha(fixture.work.path()), before);
        assert!(!remote_has(fixture.work.path(), "feature-g"));
    }

    fn non_test_source() -> &'static str {
        let source = include_str!("pr_tools.rs");
        let end = source.find("#[cfg(test)]\nmod tests").unwrap();
        &source[..end]
    }

    #[test]
    fn no_registered_tool_can_merge_a_pull_request() {
        let dir = github_repo_fixture();
        let registry = scoped_registry(dir.path(), EffectClass::Production);
        for name in registry.list_tools() {
            assert!(!name.contains("merge"), "{name}");
        }
        for name in ALL_TOOL_NAMES {
            let tool = registry.get_tool(name).unwrap();
            let definition = serde_json::to_string(&tool.definition())
                .unwrap()
                .to_lowercase();
            assert!(!definition.contains("merge"), "{name}");
        }
        assert!(!non_test_source().to_lowercase().contains("merge"));
    }

    #[test]
    fn the_github_client_surface_has_no_merge_method_and_is_pinned() {
        let backlog = include_str!("backlog.rs");
        let start = backlog.find("pub trait GithubClient").unwrap();
        let body = &backlog[start..];
        let end = body.find("\n}\n").unwrap();
        let trait_src = &body[..end];
        let mut methods: Vec<&str> = trait_src
            .split("async fn ")
            .skip(1)
            .map(|rest| {
                rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap()
            })
            .collect();
        methods.sort_unstable();
        assert_eq!(
            methods,
            vec![
                "close_pull_request",
                "comment_on_issue",
                "create_draft_pull_request",
                "create_issue",
                "get_issue",
                "get_pull_request",
                "list_issue_comments",
                "list_review_comments",
                "mark_pull_request_ready",
                "open_pull_requests",
                "search_open_issues",
            ]
        );
        assert!(methods.iter().all(|m| !m.contains("merge")));
    }

    #[test]
    fn tools_call_only_the_pinned_client_methods_and_never_build_urls() {
        let flattened: String = non_test_source()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("");
        let mut used: Vec<String> = flattened
            .split("self.client.")
            .skip(1)
            .map(|rest| rest.split('(').next().unwrap().to_string())
            .collect();
        used.sort();
        used.dedup();
        assert_eq!(
            used,
            vec![
                "close_pull_request",
                "comment_on_issue",
                "create_draft_pull_request",
                "get_issue",
                "get_pull_request",
                "list_issue_comments",
                "list_review_comments",
                "mark_pull_request_ready",
            ]
        );
        let source = non_test_source();
        for needle in ["api.github.com", "reqwest", "format!(\"https"] {
            assert!(!source.contains(needle), "{needle}");
        }
    }

    #[test]
    fn register_never_adds_a_tool_outside_the_pinned_set() {
        let dir = github_repo_fixture();
        let mut registry = ToolRegistry::new();
        register(&mut registry, dir.path(), "sdlc-dev");
        let mut names = registry.list_tools();
        names.sort_unstable();
        assert_eq!(names, ALL_TOOL_NAMES.to_vec());
    }

    const BENIGN_LINE: [&str; 11] = [
        "## Heading-like",
        "# x",
        "a..b",
        "what?",
        "issue #5 and a/b#c",
        "```",
        "~~~",
        "Nanna\u{2011}Identity: lookalike",
        "\u{ff46}\u{ff55}\u{ff4c}\u{ff4c}",
        "m\u{0430}in",
        "../../etc/passwd",
    ];

    const BENIGN_BLOCK: [&str; 8] = [
        "## Heading-like\n# another\n---",
        "a..b\nwhat?\n#5",
        "```\nfenced ## inside\n```",
        "~~~\nx\n~~~\ntrailing",
        "Nanna\u{2011}Identity: lookalike",
        "\u{ff46}\u{ff55}\u{ff4c}\u{ff4c}\nm\u{0430}in",
        "../../etc/passwd",
        "inline ``` not a fence`",
    ];

    #[tokio::test]
    async fn benign_special_text_is_forwarded_verbatim_and_never_displaces_the_marker() {
        let dir = feature_repo();
        let mock = rich_mock();
        let open = GithubPrOpenTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let comment =
            GithubIssueCommentTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        let close = GithubPrCloseTool::new(dir.path().to_path_buf(), "sdlc-dev", mock.clone());
        for text in BENIGN_LINE {
            open.execute(json!({ "title": text, "body": "b", "base": "main" }))
                .await
                .unwrap_or_else(|e| panic!("title {text:?}: {e}"));
            let reason = format!("{text} #1");
            close
                .execute(json!({ "pr_number": 3, "reason": reason }))
                .await
                .unwrap_or_else(|e| panic!("reason {text:?}: {e}"));
        }
        for text in BENIGN_BLOCK {
            open.execute(json!({ "title": "t", "body": text, "base": "main" }))
                .await
                .unwrap_or_else(|e| panic!("body {text:?}: {e}"));
            comment
                .execute(json!({ "issue_number": 3, "body": text }))
                .await
                .unwrap_or_else(|e| panic!("comment {text:?}: {e}"));
        }
        let calls = mock.calls();
        let marker = render_html_marker("sdlc-dev");
        let mut cursor = 0;
        for text in BENIGN_LINE {
            let sent = &calls[cursor];
            assert!(sent.contains(&format!("title={text}\nbody=")), "{text:?}");
            let comment_call = &calls[cursor + 1];
            cursor += 3;
            assert!(
                comment_call.contains(&format!("{text} #1\n\n{marker}\n")),
                "{text:?}"
            );
        }
        for text in BENIGN_BLOCK {
            let sent = &calls[cursor];
            let comment_call = &calls[cursor + 1];
            cursor += 2;
            let sent_body = sent.split("body=").nth(1).unwrap();
            assert!(sent_body.starts_with(&format!("{text}\n\n")), "{text:?}");
            assert!(sent_body.trim_end().ends_with(&marker), "{text:?}");
            assert_eq!(
                parse_identity_from_text(sent_body).as_deref(),
                Some("sdlc-dev")
            );
            assert!(
                comment_call.ends_with(&format!("{text}\n\n{marker}\n")),
                "{text:?}"
            );
            assert_eq!(
                parse_identity_from_text(comment_call).as_deref(),
                Some("sdlc-dev")
            );
        }
    }

    #[tokio::test]
    async fn benign_special_text_is_committed_verbatim_with_the_trailer_last() {
        let dir = feature_repo();
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        for (i, text) in BENIGN_LINE.iter().enumerate() {
            std::fs::write(dir.path().join(format!("s{i}.txt")), "s").unwrap();
            tool.execute(json!({ "subject": text }))
                .await
                .unwrap_or_else(|e| panic!("{text:?}: {e}"));
            let message = run_git(dir.path(), &["log", "-1", "--format=%B"]).unwrap();
            assert_eq!(
                message,
                format!("{text}\n\nNanna-Identity: sdlc-dev"),
                "{text:?}"
            );
        }
        for (i, text) in BENIGN_BLOCK.iter().enumerate() {
            std::fs::write(dir.path().join(format!("b{i}.txt")), "b").unwrap();
            tool.execute(json!({ "subject": "s", "body": text }))
                .await
                .unwrap_or_else(|e| panic!("{text:?}: {e}"));
            let message = run_git(dir.path(), &["log", "-1", "--format=%B"]).unwrap();
            assert_eq!(
                message,
                format!("s\n\n{}\n\nNanna-Identity: sdlc-dev", text.trim()),
                "{text:?}"
            );
        }
    }

    #[test]
    fn fence_balance_follows_the_markdown_opening_and_closing_rules() {
        for balanced in [
            "no fences",
            "    ```\nindented four spaces is code, not a fence",
            "``\ntwo backticks are not a fence",
            "``` has `inline` backticks\nso it is not an opener",
            "```\nx\n````",
            "   ~~~\nx\n~~~",
            "~~~ info string\nx\n~~~",
        ] {
            assert!(fences_balanced(balanced), "{balanced:?}");
        }
        for unbalanced in [
            "```",
            "~~~\n```",
            "```\n~~~",
            "````\n```",
            "```\nx\n``` trailing",
            "~~~\nx\n~~~ trailing",
        ] {
            assert!(!fences_balanced(unbalanced), "{unbalanced:?}");
        }
    }

    #[test]
    fn git_invocations_disable_repository_hooks_and_signing() {
        let cmd = git_cmd(Path::new("."));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.len(), 4);
        assert_eq!(args[0], "-c");
        assert!(args[1].starts_with("core.hooksPath="));
        assert!(args[1].ends_with(NULL_DEVICE));
        assert_eq!(args[2], "-c");
        assert_eq!(args[3], "commit.gpgsign=false");
    }

    #[tokio::test]
    async fn commits_are_authored_by_nanna_not_the_host_identity() {
        let dir = feature_repo();
        std::fs::write(dir.path().join("n.txt"), "n").unwrap();
        let tool = GitCommitTool::new(dir.path().to_path_buf(), "sdlc-dev");
        tool.execute(json!({ "subject": "x" })).await.unwrap();
        let author = run_git(dir.path(), &["log", "-1", "--format=%an <%ae>"]).unwrap();
        assert_eq!(author, "nanna <nanna@local>");
    }

    #[tokio::test]
    async fn linked_worktree_git_file_cannot_be_rewritten_by_write_file() {
        let source = github_repo_fixture();
        let holder = TempDir::new().unwrap();
        let worktree = holder.path().join("wt");
        git(
            source.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature-wt",
                worktree.to_str().unwrap(),
            ],
        );
        assert!(worktree.join(".git").is_file());
        let registry = crate::tools::create_tool_registry(&worktree);
        for path in [".git", ".git/config", "sub/../.git"] {
            let result = registry
                .execute(
                    "write_file",
                    json!({ "path": path, "content": "gitdir: /elsewhere" }),
                )
                .await;
            assert!(result.is_err(), "{path}");
        }
        assert_eq!(resolve_repo(&worktree).unwrap(), "example/repo");
    }

    #[tokio::test]
    async fn every_second_client_call_failure_is_surfaced_as_an_execution_failure() {
        let dir = feature_repo();
        let cases: [(&str, &str, Value); 5] = [
            (
                "mock:list_issue_comments",
                "github_pr_comments",
                json!({ "pr_number": 3 }),
            ),
            (
                "mock:list_issue_comments",
                "github_issue_read",
                json!({ "issue_number": 3 }),
            ),
            (
                "mock:mark_pull_request_ready",
                "github_pr_promote",
                json!({ "pr_number": 3 }),
            ),
            (
                "mock:comment_on_issue",
                "github_pr_close",
                json!({ "pr_number": 3, "reason": "#1" }),
            ),
            (
                "mock:close_pull_request",
                "github_pr_close",
                json!({ "pr_number": 3, "reason": "#1" }),
            ),
        ];
        for (failing, tool_name, args) in cases {
            let mut mock = MockGithub {
                fail_only: Some(failing),
                fail_status: Some(502),
                ..Default::default()
            };
            mock.pr_details.insert(
                ("example/repo".to_string(), 3),
                pr_detail(3, true, "sdlc-dev"),
            );
            mock.issue_details.insert(
                ("example/repo".to_string(), 3),
                GithubIssueDetail {
                    number: 3,
                    title: "t".to_string(),
                    body: None,
                    labels: vec![],
                    html_url: "u".to_string(),
                },
            );
            let tools = api_tools(dir.path(), Arc::new(mock));
            let (_, tool, _) = tools.iter().find(|(n, _, _)| *n == tool_name).unwrap();
            let err = tool.execute(args).await.unwrap_err();
            assert!(
                matches!(err, ToolError::ExecutionFailed { .. }),
                "{failing}: {err:?}"
            );
            assert!(err.to_string().contains("502"), "{failing}: {err}");
        }
    }
}
