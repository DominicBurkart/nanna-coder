use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::registry::{Tool, ToolError, ToolResult};

/// GitHub API connection status for transparent degradation.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum GitHubStatus {
    /// Successfully connected to GitHub API.
    Connected,
    /// No GITHUB_TOKEN environment variable configured.
    #[default]
    NoToken,
    /// API call failed with an error message.
    ApiError(String),
}

/// GitHub PR status data collected from git and GitHub REST API.
#[derive(Debug, Clone, Default)]
pub struct PrStatusData {
    /// PR number (e.g., "#42")
    pub pr_number: Option<u64>,
    /// Linked issue number (e.g., "#17")
    pub issue_number: Option<u64>,
    /// PR status: "draft", "ready", "merged", "closed"
    pub pr_status: Option<String>,
    /// Review state: "approved", "changes-requested", "review-required"
    pub review_state: Option<String>,
    /// Number of files with merge conflicts
    pub conflict_count: Option<usize>,
    /// List of conflicting file paths
    pub conflict_files: Vec<String>,
    /// Commits ahead of upstream
    pub ahead: Option<usize>,
    /// Commits behind upstream
    pub behind: Option<usize>,
    /// Lines added
    pub additions: Option<usize>,
    /// Lines deleted
    pub deletions: Option<usize>,
    /// CI status: "pass", "fail", "pending"
    pub ci_status: Option<String>,
    /// Failing CI check names
    pub ci_failing_checks: Vec<String>,
    /// Whether automerge is enabled
    pub automerge: bool,
    /// Days since last update (staleness)
    pub staleness_days: Option<u64>,
    /// Current branch name
    pub branch: Option<String>,
    /// Short HEAD commit SHA (fallback when no PR)
    pub head_sha: Option<String>,
    /// Whether this branch has an upstream
    pub has_upstream: bool,
    /// Changed file paths (for diff detail)
    pub changed_files: Vec<String>,
    /// GitHub API connection status
    pub github_status: GitHubStatus,
}

impl PrStatusData {
    /// Format as L0 compact single-line status.
    ///
    /// Only includes salient (non-default) fields. Omitted fields represent
    /// non-salient states (e.g., no conflicts, automerge disabled).
    pub fn to_l0(&self) -> String {
        let mut parts = Vec::new();

        // PR number or commit SHA as context anchor
        if let Some(pr) = self.pr_number {
            parts.push(format!("#{}", pr));
        } else if let Some(ref sha) = self.head_sha {
            parts.push(sha.clone());
        }

        // Linked issue
        if let Some(issue) = self.issue_number {
            parts.push(format!("#{}", issue));
        }

        // PR status (only show draft, since "ready" is implied if reviews exist)
        if let Some(ref status) = self.pr_status {
            match status.as_str() {
                "draft" => parts.push("draft".to_string()),
                "merged" => parts.push("merged".to_string()),
                "closed" => parts.push("closed".to_string()),
                "ready" => parts.push("ready".to_string()),
                _ => {}
            }
        }

        // Review state (only show when salient)
        if let Some(ref review) = self.review_state {
            match review.as_str() {
                "approved" => parts.push("approved".to_string()),
                "changes-requested" => parts.push("changes-requested".to_string()),
                _ => {}
            }
        }

        // Merge conflicts
        if let Some(count) = self.conflict_count {
            if count > 0 {
                parts.push(format!("conflicts:{}", count));
            }
        }

        // Sync state (ahead/behind)
        if !self.has_upstream {
            parts.push("no-upstream".to_string());
        } else {
            if let Some(behind) = self.behind {
                if behind > 0 {
                    parts.push(format!("behind:{}", behind));
                }
            }
            if let Some(ahead) = self.ahead {
                if ahead > 0 {
                    parts.push(format!("ahead:{}", ahead));
                }
            }
        }

        // Diff stats
        if let (Some(a), Some(d)) = (self.additions, self.deletions) {
            parts.push(format!("+{}/-{}", a, d));
        }

        // CI status
        if let Some(ref ci) = self.ci_status {
            match ci.as_str() {
                "pass" => parts.push("ci:pass".to_string()),
                "fail" => parts.push("ci:fail".to_string()),
                "pending" => parts.push("ci:pending".to_string()),
                _ => {}
            }
        }

        // Automerge (only show when enabled, since disabled is default)
        if self.automerge {
            parts.push("automerge".to_string());
        }

        // Staleness
        if let Some(days) = self.staleness_days {
            if days > 0 {
                parts.push(format!("{}d", days));
            }
        }

        // GitHub connection status (visible degradation)
        match &self.github_status {
            GitHubStatus::Connected => {}
            GitHubStatus::NoToken => parts.push("[github:unconfigured]".to_string()),
            GitHubStatus::ApiError(_) => parts.push("[github:error]".to_string()),
        }

        parts.join(" ")
    }

    /// Format L1 detail for a specific field.
    pub fn to_l1(&self, field: &str) -> Result<String, String> {
        match field {
            "conflicts" => {
                if self.conflict_files.is_empty() {
                    Ok("No merge conflicts.".to_string())
                } else {
                    Ok(format!(
                        "Conflicting files ({}):\n{}",
                        self.conflict_files.len(),
                        self.conflict_files
                            .iter()
                            .map(|f| format!("  - {}", f))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "ci" => {
                let status = self.ci_status.as_deref().unwrap_or("unknown");
                if self.ci_failing_checks.is_empty() {
                    Ok(format!("CI status: {}", status))
                } else {
                    Ok(format!(
                        "CI status: {}\nFailing checks:\n{}",
                        status,
                        self.ci_failing_checks
                            .iter()
                            .map(|c| format!("  - {}", c))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "diff" => {
                let stats = match (self.additions, self.deletions) {
                    (Some(a), Some(d)) => format!("+{}/-{}", a, d),
                    _ => "no diff data".to_string(),
                };
                if self.changed_files.is_empty() {
                    Ok(format!("Diff: {}", stats))
                } else {
                    Ok(format!(
                        "Diff: {}\nChanged files ({}):\n{}",
                        stats,
                        self.changed_files.len(),
                        self.changed_files
                            .iter()
                            .map(|f| format!("  - {}", f))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "sync" => {
                if !self.has_upstream {
                    Ok("No upstream tracking branch configured.".to_string())
                } else {
                    let ahead = self.ahead.unwrap_or(0);
                    let behind = self.behind.unwrap_or(0);
                    Ok(format!(
                        "Sync: {} ahead, {} behind upstream",
                        ahead, behind
                    ))
                }
            }
            "review" => {
                let state = self.review_state.as_deref().unwrap_or("none");
                Ok(format!("Review state: {}", state))
            }
            "automerge" => Ok(format!(
                "Automerge: {}",
                if self.automerge {
                    "enabled"
                } else {
                    "disabled"
                }
            )),
            "staleness" => match self.staleness_days {
                Some(days) => Ok(format!("Last updated {} days ago", days)),
                None => Ok("Staleness data not available.".to_string()),
            },
            "github" => match &self.github_status {
                GitHubStatus::Connected => {
                    Ok("GitHub API: connected (token configured)".to_string())
                }
                GitHubStatus::NoToken => Ok(
                    "GitHub API: not configured. Set GITHUB_TOKEN env var with repo:status and read:org scopes to enable PR data, CI status, and review information.".to_string(),
                ),
                GitHubStatus::ApiError(msg) => {
                    Ok(format!("GitHub API: error — {}", msg))
                }
            },
            _ => Err(format!(
                "Unknown field '{}'. Valid fields: conflicts, ci, diff, sync, review, automerge, staleness, github",
                field
            )),
        }
    }
}

/// Collect PR status data from git and (optionally) the GitHub REST API.
fn collect_pr_status(workspace_root: &Path) -> ToolResult<PrStatusData> {
    let mut data = PrStatusData::default();

    // -- Git-based data --

    // Branch and HEAD SHA
    let branch_output = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(workspace_root)
        .output()
        .map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to get branch: {}", e),
        })?;
    if branch_output.status.success() {
        let branch = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string();
        if !branch.is_empty() {
            data.branch = Some(branch);
        }
    }

    let sha_output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to get HEAD SHA: {}", e),
        })?;
    if sha_output.status.success() {
        data.head_sha = Some(
            String::from_utf8_lossy(&sha_output.stdout)
                .trim()
                .to_string(),
        );
    }

    // Ahead/behind upstream
    let tracking_output = std::process::Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(workspace_root)
        .output();

    match tracking_output {
        Ok(ref output) if output.status.success() => {
            data.has_upstream = true;
            let counts = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = counts.trim().split('\t').collect();
            if parts.len() == 2 {
                data.ahead = parts[0].parse().ok();
                data.behind = parts[1].parse().ok();
            }
        }
        _ => {
            data.has_upstream = false;
        }
    }

    // Diff stats against upstream (or default branch)
    let diff_base = if data.has_upstream {
        "@{upstream}".to_string()
    } else {
        // Try origin/main, then origin/master
        let main_check = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "origin/main"])
            .current_dir(workspace_root)
            .output();
        if main_check.map(|o| o.status.success()).unwrap_or(false) {
            "origin/main".to_string()
        } else {
            "origin/master".to_string()
        }
    };

    let diff_stat_output = std::process::Command::new("git")
        .args(["diff", "--stat", &diff_base])
        .current_dir(workspace_root)
        .output();

    if let Ok(ref output) = diff_stat_output {
        if output.status.success() {
            let stat_text = String::from_utf8_lossy(&output.stdout);
            // Parse "X files changed, Y insertions(+), Z deletions(-)" from last line
            if let Some(last_line) = stat_text.lines().last() {
                let mut additions = 0usize;
                let mut deletions = 0usize;
                for part in last_line.split(',') {
                    let part = part.trim();
                    if part.contains("insertion") {
                        if let Some(n) = part.split_whitespace().next() {
                            additions = n.parse().unwrap_or(0);
                        }
                    } else if part.contains("deletion") {
                        if let Some(n) = part.split_whitespace().next() {
                            deletions = n.parse().unwrap_or(0);
                        }
                    }
                }
                data.additions = Some(additions);
                data.deletions = Some(deletions);
            }
        }
    }

    // Changed files list (for L1 diff detail)
    let diff_files_output = std::process::Command::new("git")
        .args(["diff", "--name-only", &diff_base])
        .current_dir(workspace_root)
        .output();

    if let Ok(ref output) = diff_files_output {
        if output.status.success() {
            data.changed_files = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
        }
    }

    // Merge conflicts (check for unmerged paths)
    let conflict_output = std::process::Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(workspace_root)
        .output();

    if let Ok(ref output) = conflict_output {
        if output.status.success() {
            let files: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
            if !files.is_empty() {
                data.conflict_count = Some(files.len());
                data.conflict_files = files;
            }
        }
    }

    // -- GitHub REST API data (explicit degradation) --
    let token = std::env::var("GITHUB_TOKEN").ok();

    if let Some(ref token) = token {
        match fetch_github_pr_data(workspace_root, &data, token) {
            Ok(gh_data) => {
                data.pr_number = gh_data.pr_number.or(data.pr_number);
                data.issue_number = gh_data.issue_number.or(data.issue_number);
                data.pr_status = gh_data.pr_status.or(data.pr_status);
                data.review_state = gh_data.review_state.or(data.review_state);
                data.ci_status = gh_data.ci_status.or(data.ci_status);
                data.ci_failing_checks = if gh_data.ci_failing_checks.is_empty() {
                    data.ci_failing_checks
                } else {
                    gh_data.ci_failing_checks
                };
                data.automerge = gh_data.automerge;
                data.staleness_days = gh_data.staleness_days.or(data.staleness_days);
                if let Some(count) = gh_data.conflict_count {
                    if data.conflict_count.is_none() {
                        data.conflict_count = Some(count);
                    }
                }
                data.github_status = GitHubStatus::Connected;
            }
            Err(e) => {
                data.github_status = GitHubStatus::ApiError(e);
            }
        }
    }
    // else: data.github_status remains NoToken (the default)

    Ok(data)
}

/// Parse a GitHub remote URL into (owner, repo).
fn parse_github_remote(url: &str) -> Option<(String, String)> {
    // Handle SSH: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let path = rest.trim_end_matches(".git");
        let (owner, repo) = path.split_once('/')?;
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }
    // Handle HTTPS: https://github.com/owner/repo.git
    if url.contains("github.com") {
        let path = url
            .split("github.com")
            .nth(1)?
            .trim_start_matches('/')
            .trim_start_matches(':')
            .trim_end_matches(".git");
        let (owner, repo) = path.split_once('/')?;
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }
    None
}

/// Fetch PR data from the GitHub REST API. Returns partial data on success,
/// or an error message string on failure.
fn fetch_github_pr_data(
    workspace_root: &Path,
    local_data: &PrStatusData,
    token: &str,
) -> Result<PrStatusData, String> {
    let mut gh_data = PrStatusData::default();

    // Get remote URL
    let remote_output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(workspace_root)
        .output()
        .map_err(|e| format!("failed to get git remote: {}", e))?;

    if !remote_output.status.success() {
        return Err("no 'origin' remote configured".to_string());
    }

    let remote_url = String::from_utf8_lossy(&remote_output.stdout)
        .trim()
        .to_string();
    let (owner, repo) =
        parse_github_remote(&remote_url).ok_or_else(|| "not a GitHub remote".to_string())?;

    let branch = local_data
        .branch
        .as_deref()
        .ok_or_else(|| "no branch detected".to_string())?;

    let client = reqwest::blocking::Client::new();
    let api_base = "https://api.github.com";

    // Find PR for current branch
    let pr_url = format!(
        "{}/repos/{}/{}/pulls?head={}:{}&state=open",
        api_base, owner, repo, owner, branch
    );
    let pr_resp = client
        .get(&pr_url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nanna-coder-harness")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|e| format!("GitHub API request failed: {}", e))?;

    if !pr_resp.status().is_success() {
        let status = pr_resp.status();
        return Err(format!("GitHub API returned {}", status));
    }

    let prs: Vec<Value> = pr_resp
        .json()
        .map_err(|e| format!("failed to parse PR response: {}", e))?;

    let pr_json = match prs.first() {
        Some(pr) => pr,
        None => return Ok(gh_data), // No open PR for this branch — not an error
    };

    // PR number
    gh_data.pr_number = pr_json.get("number").and_then(|v| v.as_u64());

    // PR status (draft/ready/merged/closed)
    let is_draft = pr_json
        .get("draft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let state = pr_json.get("state").and_then(|v| v.as_str()).unwrap_or("");
    gh_data.pr_status = Some(match (state, is_draft) {
        (_, true) => "draft".to_string(),
        ("closed", _) => "closed".to_string(),
        _ => "ready".to_string(),
    });

    // Merge conflicts from mergeable_state
    if let Some(mergeable_state) = pr_json.get("mergeable_state").and_then(|v| v.as_str()) {
        if mergeable_state == "dirty" {
            gh_data.conflict_count = Some(1);
        }
    }

    // Automerge
    gh_data.automerge = pr_json
        .get("auto_merge")
        .map(|v| !v.is_null())
        .unwrap_or(false);

    // Staleness
    if let Some(updated_at) = pr_json.get("updated_at").and_then(|v| v.as_str()) {
        if let Ok(updated) = chrono::DateTime::parse_from_rfc3339(updated_at) {
            let now = chrono::Utc::now();
            let duration = now.signed_duration_since(updated);
            gh_data.staleness_days = Some(duration.num_days() as u64);
        }
    }

    // Linked issues from body (look for "Closes #N" / "Fixes #N" patterns)
    if let Some(body) = pr_json.get("body").and_then(|v| v.as_str()) {
        let issue_re =
            regex::Regex::new(r"(?i)(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#(\d+)").ok();
        if let Some(re) = issue_re {
            if let Some(caps) = re.captures(body) {
                if let Some(num) = caps.get(1).and_then(|m| m.as_str().parse::<u64>().ok()) {
                    gh_data.issue_number = Some(num);
                }
            }
        }
    }

    let pr_number = match gh_data.pr_number {
        Some(n) => n,
        None => return Ok(gh_data),
    };

    // Fetch reviews for review decision
    let reviews_url = format!(
        "{}/repos/{}/{}/pulls/{}/reviews",
        api_base, owner, repo, pr_number
    );
    if let Ok(reviews_resp) = client
        .get(&reviews_url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nanna-coder-harness")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
    {
        if reviews_resp.status().is_success() {
            if let Ok(reviews) = reviews_resp.json::<Vec<Value>>() {
                // Use the last substantive review state per reviewer
                let mut latest_states: HashMap<String, String> = HashMap::new();
                for review in &reviews {
                    let user = review
                        .get("user")
                        .and_then(|u| u.get("login"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let state = review.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    if state == "APPROVED" || state == "CHANGES_REQUESTED" || state == "DISMISSED" {
                        latest_states.insert(user.to_string(), state.to_string());
                    }
                }
                if latest_states.values().any(|s| s == "CHANGES_REQUESTED") {
                    gh_data.review_state = Some("changes-requested".to_string());
                } else if latest_states.values().any(|s| s == "APPROVED") {
                    gh_data.review_state = Some("approved".to_string());
                } else if !latest_states.is_empty() {
                    gh_data.review_state = Some("review-required".to_string());
                }
            }
        }
    }

    // Fetch CI status via check-runs
    let head_sha = pr_json
        .get("head")
        .and_then(|h| h.get("sha"))
        .and_then(|v| v.as_str());
    if let Some(sha) = head_sha {
        let checks_url = format!(
            "{}/repos/{}/{}/commits/{}/check-runs",
            api_base, owner, repo, sha
        );
        if let Ok(checks_resp) = client
            .get(&checks_url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "nanna-coder-harness")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
        {
            if checks_resp.status().is_success() {
                if let Ok(checks_json) = checks_resp.json::<Value>() {
                    if let Some(check_runs) =
                        checks_json.get("check_runs").and_then(|v| v.as_array())
                    {
                        let mut has_fail = false;
                        let mut has_pending = false;
                        let mut failing_names = Vec::new();

                        for check in check_runs {
                            let conclusion = check
                                .get("conclusion")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let status = check.get("status").and_then(|v| v.as_str()).unwrap_or("");
                            let name = check
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");

                            if conclusion == "failure" || conclusion == "timed_out" {
                                has_fail = true;
                                failing_names.push(name.to_string());
                            } else if status == "queued"
                                || status == "in_progress"
                                || status == "waiting"
                            {
                                has_pending = true;
                            }
                        }

                        gh_data.ci_status = Some(if has_fail {
                            "fail".to_string()
                        } else if has_pending {
                            "pending".to_string()
                        } else {
                            "pass".to_string()
                        });
                        gh_data.ci_failing_checks = failing_names;
                    }
                }
            }
        }
    }

    Ok(gh_data)
}

pub struct GitHubPrStatusTool {
    workspace_root: PathBuf,
}

impl GitHubPrStatusTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for GitHubPrStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "github_pr_status".to_string(),
                description: "Get GitHub PR status. L0: compact single-line status. L1: detailed expansion of a specific field.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "level".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Detail level: 'l0' for compact status line (default), 'l1' for expanded field detail".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "field".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Field to expand (required for l1). Options: conflicts, ci, diff, sync, review, automerge, staleness, github".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec![]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let level = args.get("level").and_then(|v| v.as_str()).unwrap_or("l0");

        let data = collect_pr_status(&self.workspace_root)?;
        let github_connected = data.github_status == GitHubStatus::Connected;

        match level {
            "l0" => {
                let status_line = data.to_l0();
                Ok(json!({
                    "level": "l0",
                    "status": status_line,
                    "github_connected": github_connected
                }))
            }
            "l1" => {
                let field = args.get("field").and_then(|v| v.as_str()).ok_or_else(|| {
                    ToolError::InvalidArguments {
                        message: "Missing 'field' parameter for l1 query".to_string(),
                    }
                })?;

                let detail = data
                    .to_l1(field)
                    .map_err(|e| ToolError::InvalidArguments { message: e })?;

                Ok(json!({
                    "level": "l1",
                    "field": field,
                    "detail": detail,
                    "github_connected": github_connected
                }))
            }
            _ => Err(ToolError::InvalidArguments {
                message: format!("Invalid level '{}'. Use 'l0' or 'l1'.", level),
            }),
        }
    }

    fn name(&self) -> &str {
        "github_pr_status"
    }
}
