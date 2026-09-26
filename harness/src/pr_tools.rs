//! PR lifecycle tools for the middle loop: draft, comments, promote, close,
//! plus the issue read/comment tools that close out #187 for this branch.
//!
//! Every tool here is [`EffectClass::Repository`], so each call is reviewed
//! by the action auditor before it runs (see
//! [`crate::tools::ToolRegistry::execute`]); classifying a tool correctly is
//! the whole of its gating story.
//!
//! `git_push_branch` never calls GitHub: it shells out to the host `git`
//! binary against the task's worktree, matching
//! [`crate::tools::GitStatusTool`]/[`crate::tools::GitDiffTool`]'s existing
//! harness-side pattern rather than running inside the dev container. It
//! refuses to push the repository's default branch, a detached `HEAD`, or
//! any unpushed commit whose message is missing this identity's trailer --
//! the commit-side half of the identity marker the epic requires.
//!
//! `github_pr_open` is the first tool that calls GitHub. GitHub access goes
//! through the [`GithubClient`] trait ([`crate::backlog`]) so it can be
//! tested against a mock rather than a real HTTP server. Credentials follow
//! [`crate::tools::GitHubPrStatusTool`]'s existing pattern: a `GITHUB_TOKEN`
//! read from the harness process's environment, never passed into the dev
//! container. No tool here accepts a `repo` argument: the repository is
//! always resolved from the worktree's own `origin` remote, so a
//! compromised or careless model cannot aim the harness's token at an
//! arbitrary repository. The pull request it opens is always a draft: the
//! body carries this identity's marker (a trailer-style line and a hidden
//! HTML comment, via [`crate::marker`]); a caller-supplied body that already
//! carries a `Nanna-Identity` marker is rejected rather than silently
//! overridden, so a model cannot forge or displace it.
//!
//! ## Author allow-list for [`GithubPrCommentsTool`]
//!
//! The allow-list is a plain constructor parameter (`Vec<String>`), sourced
//! by [`register`] from the `NANNA_TRUSTED_PR_COMMENTERS` environment
//! variable (a comma-separated list of GitHub logins, matched
//! case-insensitively), mirroring how [`crate::tools::GitHubPrStatusTool`]
//! sources its token from `GITHUB_TOKEN`. An identity-scoped TOML field would
//! also fit; a constructor parameter was simpler and needed no schema
//! change, and it keeps the allow-list out of the model's own tool-call
//! arguments so an adversarial commenter cannot add themselves to it.

use crate::backlog::{BacklogError, GithubClient, GithubComment};
use crate::effects::EffectClass;
use crate::marker::{parse_identity_from_text, render_html_marker, render_trailer};
use crate::tools::{parse_github_remote, Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

/// Environment variable naming the comma-separated GitHub logins
/// [`GithubPrCommentsTool`] keeps comments from; unset means the allow-list
/// is empty and every comment is filtered out.
pub const TRUSTED_PR_COMMENTERS_ENV: &str = "NANNA_TRUSTED_PR_COMMENTERS";

fn map_backlog_error(err: BacklogError) -> ToolError {
    ToolError::ExecutionFailed {
        message: err.to_string(),
    }
}

fn required_str<'a>(args: &'a Value, key: &str) -> ToolResult<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArguments {
            message: format!("missing '{key}'"),
        })
}

fn required_u64(args: &Value, key: &str) -> ToolResult<u64> {
    args.get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| ToolError::InvalidArguments {
            message: format!("missing '{key}'"),
        })
}

fn comment_json(comment: &GithubComment) -> Value {
    json!({
        "id": comment.id,
        "author": comment.author,
        "body": comment.body,
        "html_url": comment.html_url,
    })
}

/// Confirm `body` carries this identity's marker before a tool acts on the
/// pull request it belongs to, so one identity cannot close or promote
/// another identity's (or a human's) pull request.
fn ensure_owned_by_identity(
    body: Option<&str>,
    identity_name: &str,
    pr_number: u64,
) -> ToolResult<()> {
    let owner = body.and_then(parse_identity_from_text);
    if owner.as_deref() == Some(identity_name) {
        Ok(())
    } else {
        Err(ToolError::ExecutionFailed {
            message: format!(
                "refusing to act on pull request #{pr_number}: not owned by identity '{identity_name}'"
            ),
        })
    }
}

fn issue_url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"https://github\.com/[^/\s]+/[^/\s]+/issues/\d+")
            .expect("issue url regex is valid")
    })
}

/// Whether `text` references an issue as `#123` or as a full GitHub issue
/// URL, the two forms [`GithubPrCloseTool`] accepts as a valid close reason.
fn contains_issue_reference(text: &str) -> bool {
    !crate::backlog::referenced_issues(text).is_empty() || issue_url_regex().is_match(text)
}

#[cfg(test)]
thread_local! {
    /// Per-thread override for the `git` binary [`git_cmd`] invokes, so a
    /// test can force a process-spawn failure deterministically without
    /// mutating process-global state (`std::env`) that parallel tests share.
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

/// A `git` invocation against `workspace_root` with no terminal prompt, so a
/// missing credential fails fast instead of hanging the task.
fn git_cmd(workspace_root: &Path) -> Command {
    let mut cmd = Command::new(git_binary());
    cmd.current_dir(workspace_root);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

fn run_git(workspace_root: &Path, args: &[&str]) -> ToolResult<String> {
    let output =
        git_cmd(workspace_root)
            .args(args)
            .output()
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("failed to run git {}: {e}", args.join(" ")),
            })?;
    if !output.status.success() {
        return Err(ToolError::ExecutionFailed {
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The `owner/name` GitHub repository the worktree's `origin` remote points
/// at. Never accepted as a tool argument: always read from the worktree
/// itself, so a tool call cannot aim the harness's token at another
/// repository.
fn resolve_repo(workspace_root: &Path) -> ToolResult<String> {
    let remote_url = run_git(workspace_root, &["remote", "get-url", "origin"])?;
    parse_github_remote(&remote_url)
        .map(|(owner, repo)| format!("{owner}/{repo}"))
        .ok_or_else(|| ToolError::ExecutionFailed {
            message: "origin remote is not a GitHub remote".to_string(),
        })
}

/// The worktree's current branch. Refuses a detached `HEAD`, since pushing
/// one would push whatever commit `HEAD` happens to point at under no
/// branch name at all.
fn current_branch(workspace_root: &Path) -> ToolResult<String> {
    let branch = run_git(workspace_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch.is_empty() || branch == "HEAD" {
        return Err(ToolError::ExecutionFailed {
            message: "refusing to act from a detached HEAD".to_string(),
        });
    }
    Ok(branch)
}

/// The repository's default branch, resolved from `origin/HEAD` and, when
/// that symbolic ref is absent, from a `main`/`master` fallback. `None` when
/// neither could be determined.
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
    for candidate in ["main", "master"] {
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

fn empty_object_schema() -> JsonSchema {
    JsonSchema {
        schema_type: SchemaType::Object,
        properties: Some(HashMap::new()),
        required: Some(vec![]),
    }
}

/// Pushes the task's current branch to its configured `origin` remote. Runs
/// on the harness side, against the worktree's own git configuration and
/// credential helpers, never inside the dev container.
///
/// Refuses to push the repository's default branch (that would be pushing
/// straight to the branch other work merges into) and refuses a detached
/// `HEAD`. Always pushes `HEAD:refs/heads/<branch>` explicitly, never a bare
/// branch name or a force/`+` refspec.
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

/// Refuse to push if any commit reachable from `HEAD` but not yet on any
/// `origin` remote-tracking ref is missing this identity's trailer. This is
/// how the identity marker required "on commits" (as well as on PR bodies)
/// is enforced: nothing on this branch authors commits yet, so this check
/// fails closed until whatever does include the trailer itself.
fn ensure_unpushed_commits_carry_the_identity_trailer(
    workspace_root: &Path,
    identity_name: &str,
) -> ToolResult<()> {
    let shas = run_git(
        workspace_root,
        &["rev-list", "HEAD", "--not", "--remotes=origin"],
    )?;
    for sha in shas.lines().filter(|line| !line.is_empty()) {
        let message = run_git(workspace_root, &["log", "-1", "--format=%B", sha])?;
        if parse_identity_from_text(&message).as_deref() != Some(identity_name) {
            return Err(ToolError::ExecutionFailed {
                message: format!(
                    "refusing to push: commit {sha} is missing the identity trailer; add `{}` to its message",
                    render_trailer(identity_name)
                ),
            });
        }
    }
    Ok(())
}

#[async_trait]
impl Tool for GitPushBranchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "git_push_branch".to_string(),
                description:
                    "Push the current branch to its configured 'origin' remote. Refuses the repository's default branch, a detached HEAD, and any unpushed commit missing this identity's trailer."
                        .to_string(),
                parameters: empty_object_schema(),
            },
        }
    }

    async fn execute(&self, _args: Value) -> ToolResult<Value> {
        let branch = current_branch(&self.workspace_root)?;
        if let Some(default) = default_branch(&self.workspace_root) {
            if branch == default {
                return Err(ToolError::InvalidArguments {
                    message: format!(
                        "refusing to push '{branch}': it is the repository's default branch"
                    ),
                });
            }
        }
        ensure_unpushed_commits_carry_the_identity_trailer(
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

/// Opens a pull request. Always a draft: there is no argument that can
/// change this. The body carries this identity's marker (a trailer-style
/// line and a hidden HTML comment, via [`crate::marker`]); a caller-supplied
/// body that already carries a `Nanna-Identity` marker is rejected rather
/// than silently overridden, so a model cannot forge or displace it.
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
        ToolDefinition {
            function: FunctionDefinition {
                name: "github_pr_open".to_string(),
                description: "Open a draft pull request from the current branch. Always a draft; there is no way to open a ready-for-review pull request with this tool.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "title".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("Pull request title.".to_string()),
                                items: None,
                            },
                        );
                        props.insert(
                            "body".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Pull request body. Must not already carry a Nanna-Identity marker.".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "base".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Base branch (optional; defaults to the repository's default branch).".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["title".to_string(), "body".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        if let Some(draft) = args.get("draft") {
            if draft.as_bool() != Some(true) {
                return Err(ToolError::InvalidArguments {
                    message: "github_pr_open always creates a draft pull request; 'draft' must be true or omitted".to_string(),
                });
            }
        }
        let title = required_str(&args, "title")?;
        let body = required_str(&args, "body")?;
        if parse_identity_from_text(body).is_some() {
            return Err(ToolError::InvalidArguments {
                message: "'body' must not already carry a Nanna-Identity marker".to_string(),
            });
        }
        let trailer = render_trailer(&self.identity_name);
        let marker = render_html_marker(&self.identity_name);
        if parse_identity_from_text(&marker).as_deref() != Some(self.identity_name.as_str()) {
            return Err(ToolError::ExecutionFailed {
                message: format!(
                    "identity name '{}' does not produce a parseable identity marker",
                    self.identity_name
                ),
            });
        }
        let repo = resolve_repo(&self.workspace_root)?;
        let head = current_branch(&self.workspace_root)?;
        let base = match args.get("base").and_then(|v| v.as_str()) {
            Some(base) => base.to_string(),
            None => default_branch(&self.workspace_root).ok_or_else(|| {
                ToolError::InvalidArguments {
                    message:
                        "no 'base' given and the repository's default branch could not be determined"
                            .to_string(),
                }
            })?,
        };
        if base == head {
            return Err(ToolError::InvalidArguments {
                message: "refusing to open a pull request from a branch into itself".to_string(),
            });
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

/// Fetches a pull request's review comments and issue (conversation)
/// comments, filtered to a configured allow-list of authors so an arbitrary
/// commenter cannot inject instructions into an agent's context. See the
/// module docs for where the allow-list comes from.
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
        let allowed_authors = allowed_authors
            .into_iter()
            .map(|author| author.to_lowercase())
            .collect();
        Self {
            workspace_root,
            client,
            allowed_authors,
        }
    }

    fn is_allowed(&self, comment: &GithubComment) -> bool {
        self.allowed_authors
            .contains(&comment.author.to_lowercase())
    }
}

#[async_trait]
impl Tool for GithubPrCommentsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "github_pr_comments".to_string(),
                description: "Fetch review and issue comments on a pull request, filtered to a configured author allow-list.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "pr_number".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some("Pull request number.".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["pr_number".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let pr_number = required_u64(&args, "pr_number")?;
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

/// The allow-list [`GithubPrCommentsTool`] is constructed with by
/// [`register`]: [`TRUSTED_PR_COMMENTERS_ENV`] split on commas, trimmed,
/// lowercased, and emptied of blanks. `None` (the variable unset) yields an
/// empty allow-list, so every comment is filtered out until it is set.
pub(crate) fn parse_allowed_authors(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(|author| author.trim().to_lowercase())
        .filter(|author| !author.is_empty())
        .collect()
}

/// Converts a draft pull request to ready for review. Refuses a pull
/// request whose body marker does not name this identity.
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
        ToolDefinition {
            function: FunctionDefinition {
                name: "github_pr_promote".to_string(),
                description: "Convert a draft pull request this identity owns to ready for review."
                    .to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "pr_number".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some("Pull request number.".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["pr_number".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let pr_number = required_u64(&args, "pr_number")?;
        let repo = resolve_repo(&self.workspace_root)?;
        let detail = self
            .client
            .get_pull_request(&repo, pr_number)
            .await
            .map_err(map_backlog_error)?;
        ensure_owned_by_identity(detail.body.as_deref(), &self.identity_name, pr_number)?;
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

/// Closes a pull request this identity owns. Requires a `reason` that
/// references the originating issue (`#123` or a full GitHub issue URL) and
/// posts it as a comment before closing.
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
        ToolDefinition {
            function: FunctionDefinition {
                name: "github_pr_close".to_string(),
                description: "Close a pull request this identity owns. 'reason' must reference the originating issue and is posted as a comment before closing.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "pr_number".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some("Pull request number.".to_string()),
                                items: None,
                            },
                        );
                        props.insert(
                            "reason".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Why this pull request is being closed. Must reference the originating issue, e.g. '#123'.".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["pr_number".to_string(), "reason".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let pr_number = required_u64(&args, "pr_number")?;
        let reason = required_str(&args, "reason")?;
        if !contains_issue_reference(reason) {
            return Err(ToolError::InvalidArguments {
                message: "'reason' must reference the originating issue (e.g. '#123' or a GitHub issue URL)".to_string(),
            });
        }
        let repo = resolve_repo(&self.workspace_root)?;
        let detail = self
            .client
            .get_pull_request(&repo, pr_number)
            .await
            .map_err(map_backlog_error)?;
        ensure_owned_by_identity(detail.body.as_deref(), &self.identity_name, pr_number)?;
        self.client
            .comment_on_issue(&repo, pr_number, reason)
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

/// Reads a GitHub issue's title, body, labels and comments. Closes the #187
/// ask on this branch.
pub struct GithubIssueReadTool {
    workspace_root: PathBuf,
    client: Arc<dyn GithubClient>,
}

impl GithubIssueReadTool {
    pub fn new(workspace_root: PathBuf, client: Arc<dyn GithubClient>) -> Self {
        Self {
            workspace_root,
            client,
        }
    }
}

#[async_trait]
impl Tool for GithubIssueReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "github_issue_read".to_string(),
                description: "Read a GitHub issue's title, body, labels and comments.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "issue_number".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some("Issue number.".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["issue_number".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let issue_number = required_u64(&args, "issue_number")?;
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
        Ok(json!({
            "number": detail.number,
            "title": detail.title,
            "body": detail.body,
            "labels": detail.labels,
            "html_url": detail.html_url,
            "comments": comments.iter().map(comment_json).collect::<Vec<_>>(),
        }))
    }

    fn name(&self) -> &str {
        "github_issue_read"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

/// Posts a comment on a GitHub issue. Deliberately has no way to close an
/// issue: agents close pull requests, never issues.
pub struct GithubIssueCommentTool {
    workspace_root: PathBuf,
    client: Arc<dyn GithubClient>,
}

impl GithubIssueCommentTool {
    pub fn new(workspace_root: PathBuf, client: Arc<dyn GithubClient>) -> Self {
        Self {
            workspace_root,
            client,
        }
    }
}

#[async_trait]
impl Tool for GithubIssueCommentTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "github_issue_comment".to_string(),
                description: "Post a comment on a GitHub issue.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "issue_number".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some("Issue number.".to_string()),
                                items: None,
                            },
                        );
                        props.insert(
                            "body".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("Comment body.".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["issue_number".to_string(), "body".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let issue_number = required_u64(&args, "issue_number")?;
        let body = required_str(&args, "body")?;
        let repo = resolve_repo(&self.workspace_root)?;
        self.client
            .comment_on_issue(&repo, issue_number, body)
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

/// Register every PR lifecycle tool against `registry`, scoped to
/// `workspace_root` and `identity_name`.
pub fn register(registry: &mut ToolRegistry, workspace_root: &Path, identity_name: &str) {
    let token = std::env::var("GITHUB_TOKEN").ok();
    let client: Arc<dyn GithubClient> =
        Arc::new(crate::backlog::ReqwestGithubClient::github(token));
    let allowed_authors =
        parse_allowed_authors(std::env::var(TRUSTED_PR_COMMENTERS_ENV).ok().as_deref());
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
        allowed_authors,
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
    )));
    registry.register(Box::new(GithubIssueCommentTool::new(
        workspace_root.to_path_buf(),
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

    /// A bare `origin` plus a working checkout on `main` with one commit,
    /// for `git_push_branch` tests.
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
        // `git fetch` auto-populates `refs/remotes/origin/HEAD` on some git
        // versions; delete it so this test actually exercises the
        // `main`/`master` rev-parse fallback rather than the symref path.
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
    fn contains_issue_reference_matches_hash_and_url_forms() {
        assert!(contains_issue_reference("Closes #647"));
        assert!(contains_issue_reference(
            "see https://github.com/o/n/issues/647 for detail"
        ));
        assert!(!contains_issue_reference("no reference here"));
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

    // ---- github_pr_open ----

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

        // Round-trip: the body the mock received carries both the trailer
        // line and the hidden HTML comment, and marker.rs's own parser (not
        // a bespoke one here) parses either back to the same identity.
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
            .execute(json!({ "title": "t", "body": "b", "base": "main" }))
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

    // ---- github_pr_comments ----

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

    // ---- github_pr_promote ----

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

    // ---- github_pr_close ----

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

    // ---- github_issue_read ----

    #[tokio::test]
    async fn github_issue_read_definition_and_effect_class() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubIssueReadTool::new(dir.path().to_path_buf(), client);
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
        let tool = GithubIssueReadTool::new(dir.path().to_path_buf(), mock);
        let result = tool.execute(json!({ "issue_number": 647 })).await.unwrap();
        assert_eq!(result["title"], json!("PR lifecycle tools"));
        assert_eq!(result["labels"], json!(["enhancement"]));
        assert_eq!(result["comments"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn github_issue_read_surfaces_a_missing_issue_error() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubIssueReadTool::new(dir.path().to_path_buf(), client);
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
        let tool = GithubIssueReadTool::new(dir.path().to_path_buf(), client);
        assert!(matches!(
            tool.execute(json!({})).await.unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
    }

    // ---- github_issue_comment ----

    #[tokio::test]
    async fn github_issue_comment_definition_and_effect_class() {
        let dir = github_repo_fixture();
        let client: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool = GithubIssueCommentTool::new(dir.path().to_path_buf(), client);
        assert_eq!(tool.name(), "github_issue_comment");
        assert_eq!(tool.effect_class(), EffectClass::Repository);
    }

    #[tokio::test]
    async fn github_issue_comment_posts_a_comment() {
        let dir = github_repo_fixture();
        let mock = Arc::new(MockGithub::default());
        let tool = GithubIssueCommentTool::new(dir.path().to_path_buf(), mock.clone());
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
        let tool = GithubIssueCommentTool::new(dir.path().to_path_buf(), client);
        assert!(matches!(
            tool.execute(json!({ "body": "b" })).await.unwrap_err(),
            ToolError::InvalidArguments { .. }
        ));
        let client2: Arc<dyn GithubClient> = Arc::new(MockGithub::default());
        let tool2 = GithubIssueCommentTool::new(dir.path().to_path_buf(), client2);
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
        let tool = GithubIssueCommentTool::new(dir.path().to_path_buf(), mock);
        let err = tool
            .execute(json!({ "issue_number": 10, "body": "b" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }
}
