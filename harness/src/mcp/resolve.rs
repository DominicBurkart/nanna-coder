use crate::auditor::{AuditContext, ReasonCode, RuleAuditor, SpawnRequest, VerdictKind};
use crate::identity::{IdentityCatalog, AUDITOR_IDENTITY};
use crate::scope::{check_repo, origin_slug, repo_slug};
use serde_json::{json, Map, Value};
use std::fmt;
use std::path::{Path, PathBuf};

pub const REPOS_DIR_ENV: &str = "NANNA_REPOS_DIR";
pub const DEFAULT_REPOS_DIR: &str = "/repos";

pub fn repos_root_with(lookup: impl Fn(&str) -> Option<String>) -> PathBuf {
    lookup(REPOS_DIR_ENV)
        .filter(|value| !value.trim().is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_REPOS_DIR), PathBuf::from)
}

pub fn repos_root() -> PathBuf {
    repos_root_with(|key| std::env::var(key).ok())
}

#[derive(Debug, Clone)]
pub struct Refusal {
    error: &'static str,
    reason_code: String,
    message: String,
    fix: String,
    extra: Map<String, Value>,
}

impl Refusal {
    pub fn new(error: &'static str, message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            error,
            reason_code: error.to_string(),
            message: message.into(),
            fix: fix.into(),
            extra: Map::new(),
        }
    }

    pub fn with_reason_code(mut self, code: impl Into<String>) -> Self {
        self.reason_code = code.into();
        self
    }

    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.extra.extend([(key.to_string(), value)]);
        self
    }

    pub fn error(&self) -> &str {
        self.error
    }

    pub fn reason_code(&self) -> &str {
        &self.reason_code
    }

    pub fn fix(&self) -> &str {
        &self.fix
    }

    pub fn to_json(&self) -> Value {
        let mut body = json!({
            "error": self.error,
            "reason_code": self.reason_code,
            "message": self.message,
            "fix": self.fix,
        });
        for (key, value) in &self.extra {
            body[key] = value.clone();
        }
        body
    }

    pub fn to_tool_result(&self) -> Value {
        let mut result = super::handlers::text_tool_result(&self.to_json());
        result["isError"] = Value::Bool(true);
        result
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

impl From<String> for Refusal {
    fn from(message: String) -> Self {
        Refusal::new(
            "invalid_arguments",
            message,
            "Correct the arguments and call the tool again.",
        )
    }
}

#[derive(Debug, Clone)]
pub struct RepoEntry {
    pub path: PathBuf,
    pub name: String,
    pub origin_slug: Option<String>,
}

impl RepoEntry {
    fn slug_name(&self) -> Option<&str> {
        self.origin_slug.as_deref()?.rsplit('/').next()
    }

    fn slug_owner_name(&self) -> Option<&str> {
        let slug = self.origin_slug.as_deref()?;
        slug.split_once('/').map(|(_, rest)| rest)
    }
}

pub fn discover(root: &Path) -> Vec<RepoEntry> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut repos: Vec<RepoEntry> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join(".git").exists())
        .map(|path| RepoEntry {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            origin_slug: origin_slug(&path),
            path,
        })
        .collect();
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    repos
}

pub fn is_assignable(name: &str) -> bool {
    name != AUDITOR_IDENTITY
}

pub fn assignable_identities(catalog: &IdentityCatalog) -> Vec<&crate::identity::AgentIdentity> {
    catalog
        .iter()
        .filter(|card| is_assignable(card.name()))
        .collect()
}

fn covers(card: &crate::identity::AgentIdentity, slug: &str) -> bool {
    card.scope
        .repos
        .iter()
        .any(|repo| repo.eq_ignore_ascii_case(slug))
}

pub fn identities_for_slug(catalog: &IdentityCatalog, slug: Option<&str>) -> Vec<String> {
    let Some(slug) = slug else {
        return Vec::new();
    };
    assignable_identities(catalog)
        .into_iter()
        .filter(|card| covers(card, slug))
        .map(|card| card.name().to_string())
        .collect()
}

pub fn identity_summaries(catalog: &IdentityCatalog) -> Value {
    Value::Array(
        assignable_identities(catalog)
            .into_iter()
            .map(|card| {
                json!({
                    "name": card.name(),
                    "loop": card.identity.dev_loop.to_string(),
                    "max_effect": card.scope.max_effect.to_string(),
                    "repos": card.scope.repos,
                })
            })
            .collect(),
    )
}

pub fn list_repos(catalog: &IdentityCatalog, root: &Path) -> Value {
    let repos: Vec<Value> = discover(root)
        .into_iter()
        .map(|repo| {
            json!({
                "path": repo.path.display().to_string(),
                "name": repo.name,
                "origin_slug": repo.origin_slug,
                "identities": identities_for_slug(catalog, repo.origin_slug.as_deref()),
            })
        })
        .collect();
    super::handlers::text_tool_result(&json!({
        "repos_root": root.display().to_string(),
        "repos": repos,
    }))
}

pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_repos",
            "description": "List the git repositories assign_task can target: every repository under the server's repos root, with its `path` (pass this as `repo_path`), `name`, `origin_slug` and the assignable `identities` whose scope.repos covers it. Host paths on the caller's machine are not visible to the server; use the paths listed here.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub path: PathBuf,
    pub note: Option<String>,
}

enum Query {
    Slug(String),
    OwnerName(String),
    Name(String),
}

fn parse_query(input: &str) -> Option<(Query, Option<String>)> {
    let trimmed = input.trim();
    if Path::new(trimmed).is_absolute() {
        let name = Path::new(trimmed)
            .file_name()?
            .to_string_lossy()
            .into_owned();
        let note = format!(
            "`{trimmed}` is not a path the server can see; matched by its final segment `{name}`"
        );
        return Some((Query::Name(name), Some(note)));
    }
    if trimmed.contains("://") || trimmed.contains('@') {
        return repo_slug(trimmed).map(|slug| (Query::Slug(slug), None));
    }
    let path = trimmed.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<&str> = path.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return None;
    }
    match parts.as_slice() {
        [name] if !name.starts_with('~') => Some((Query::Name((*name).to_string()), None)),
        [owner, name] if !owner.starts_with('~') => {
            Some((Query::OwnerName(format!("{owner}/{name}")), None))
        }
        [host, owner, name] if host.contains('.') => Some((
            Query::Slug(format!("{}/{owner}/{name}", host.to_ascii_lowercase())),
            None,
        )),
        _ => None,
    }
}

fn matches<'a>(query: &Query, repos: &'a [RepoEntry]) -> Vec<&'a RepoEntry> {
    repos
        .iter()
        .filter(|repo| match query {
            Query::Slug(slug) => repo
                .origin_slug
                .as_deref()
                .is_some_and(|own| own.eq_ignore_ascii_case(slug)),
            Query::OwnerName(wanted) => repo
                .slug_owner_name()
                .is_some_and(|own| own.eq_ignore_ascii_case(wanted)),
            Query::Name(wanted) => {
                repo.name.eq_ignore_ascii_case(wanted)
                    || repo
                        .slug_name()
                        .is_some_and(|own| own.eq_ignore_ascii_case(wanted))
            }
        })
        .collect()
}

fn repo_listing(repos: &[RepoEntry]) -> Value {
    Value::Array(
        repos
            .iter()
            .map(|repo| json!({ "path": repo.path.display().to_string(), "origin_slug": repo.origin_slug }))
            .collect(),
    )
}

fn did_you_mean(input: &str, repos: &[RepoEntry]) -> Vec<String> {
    let needle = input
        .trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(".git")
        .to_ascii_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    repos
        .iter()
        .filter(|repo| {
            let name = repo.name.to_ascii_lowercase();
            name.contains(&needle) || needle.contains(&name)
        })
        .map(|repo| repo.path.display().to_string())
        .collect()
}

pub fn unknown_repo_message(input: &str) -> String {
    format!(
        "unknown_repo: `{input}` (the path as the nanna server sees it) is not an existing git repository. \
         repo_path must be an absolute path on the server's filesystem: `~` is not expanded and relative paths are not accepted."
    )
}

pub fn resolve_repo(input: &str, root: &Path) -> Result<Resolved, Refusal> {
    let direct = Path::new(input);
    if direct.is_absolute() && direct.join(".git").exists() {
        return Ok(Resolved {
            path: direct.to_path_buf(),
            note: None,
        });
    }
    let repos = discover(root);
    let miss = |repos: &[RepoEntry]| {
        let suggestions = did_you_mean(input, repos);
        let fix = match suggestions.as_slice() {
            [one] => format!("Did you mean `{one}`? Otherwise call list_repos and pass one of its paths as repo_path."),
            _ => "Call list_repos and pass one of its paths as repo_path.".to_string(),
        };
        Refusal::new("unknown_repo", unknown_repo_message(input), fix)
            .with("did_you_mean", json!(suggestions))
            .with("available_repos", repo_listing(repos))
    };
    let Some((query, note)) = parse_query(input) else {
        return Err(miss(&repos));
    };
    match matches(&query, &repos).as_slice() {
        [one] => Ok(Resolved {
            path: one.path.clone(),
            note,
        }),
        [] => Err(miss(&repos)),
        many => {
            let owned: Vec<RepoEntry> = many.iter().map(|repo| (*repo).clone()).collect();
            let candidates: Vec<String> = owned
                .iter()
                .map(|repo| repo.path.display().to_string())
                .collect();
            Err(Refusal::new(
                "ambiguous_repo",
                format!(
                    "ambiguous_repo: `{input}` matches {} repositories: {}",
                    candidates.len(),
                    candidates.join(", ")
                ),
                "Pass the full path of one of the candidates as repo_path.",
            )
            .with("candidates", repo_listing(&owned)))
        }
    }
}

fn identity_alternatives(refusal: Refusal, catalog: &IdentityCatalog) -> Refusal {
    refusal.with("assignable_identities", identity_summaries(catalog))
}

pub fn not_assignable(name: &str, catalog: &IdentityCatalog) -> Refusal {
    let assignable: Vec<&str> = assignable_identities(catalog)
        .iter()
        .map(|card| card.name())
        .collect();
    identity_alternatives(
        Refusal::new(
            "unknown_identity",
            format!(
                "spawn refused: no assignable identity `{name}` (assignable: {})",
                assignable.join(", ")
            ),
            "Set `identity` to one of assignable_identities, or call list_identities.",
        )
        .with_reason_code(ReasonCode::UnknownIdentity.as_str()),
        catalog,
    )
}

pub fn identity_required(catalog: &IdentityCatalog) -> Refusal {
    let names: Vec<&str> = assignable_identities(catalog)
        .iter()
        .map(|card| card.name())
        .collect();
    identity_alternatives(
        Refusal::new(
            "identity_required",
            format!(
                "identity must name one assignable identity; {} are assignable ({}), so none can be chosen automatically",
                names.len(),
                names.join(", ")
            ),
            "Set `identity` to one of assignable_identities.",
        ),
        catalog,
    )
}

fn fix_for(code: ReasonCode) -> &'static str {
    match code {
        ReasonCode::UnknownIdentity => "Set `identity` to one of assignable_identities.",
        ReasonCode::EffectAboveCeiling => {
            "Leave requested_effect unset, rephrase the task, or choose an identity whose max_effect covers it (see assignable_identities)."
        }
        ReasonCode::LoopMismatch => {
            "Omit dev_loop and avoid wording that implies another loop, or choose an identity in that loop."
        }
        ReasonCode::PromptInjection => "Rephrase the task description as a plain engineering task.",
        _ => "Rephrase the task or choose a different identity.",
    }
}

fn outside_scope(
    identity: &str,
    error: crate::scope::ScopeError,
    catalog: &IdentityCatalog,
    root: &Path,
    slug: Option<&str>,
) -> Refusal {
    let repos = discover(root);
    let covered: Vec<RepoEntry> = repos
        .into_iter()
        .filter(|repo| {
            catalog
                .get(identity)
                .zip(repo.origin_slug.as_deref())
                .is_some_and(|(card, slug)| covers(card, slug))
        })
        .collect();
    Refusal::new(
        "repo_outside_scope",
        error.to_string(),
        "Choose a repository listed in available_repos for this identity, or an identity listed in identities_for_repo.",
    )
    .with("available_repos", repo_listing(&covered))
    .with("identities_for_repo", json!(identities_for_slug(catalog, slug)))
}

pub fn precheck(
    context: &AuditContext,
    request: &SpawnRequest,
    repo_path: &Path,
    root: &Path,
) -> Result<(), Refusal> {
    let catalog = context.catalog();
    if !is_assignable(&request.identity) {
        return Err(not_assignable(&request.identity, catalog));
    }
    let verdict = RuleAuditor::new().evaluate(request, context);
    if verdict.kind() == VerdictKind::Block {
        let codes: Vec<ReasonCode> = verdict.reasons().iter().map(|r| r.code).collect();
        let primary = codes.first().copied().unwrap_or(ReasonCode::Other);
        let mut fixes: Vec<&str> = Vec::new();
        for code in &codes {
            let fix = fix_for(*code);
            if !fixes.contains(&fix) {
                fixes.push(fix);
            }
        }
        let refusal = Refusal::new(
            "spawn_refused",
            format!("spawn refused: {verdict}"),
            fixes.join(" "),
        )
        .with_reason_code(primary.as_str())
        .with(
            "reason_codes",
            json!(codes.iter().map(|c| c.as_str()).collect::<Vec<_>>()),
        );
        return Err(identity_alternatives(refusal, catalog));
    }
    let Some(card) = catalog.get(&request.identity) else {
        return Err(not_assignable(&request.identity, catalog));
    };
    if let Err(error) = check_repo(card, repo_path) {
        let slug = origin_slug(repo_path);
        return Err(outside_scope(
            &request.identity,
            error,
            catalog,
            root,
            slug.as_deref(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::test_support::{fixture_gate, implementer_only_gate, repo_with_origin};

    struct Repos {
        root: tempfile::TempDir,
        _keep: Vec<tempfile::TempDir>,
    }

    fn repos(entries: &[(&str, &str)]) -> Repos {
        let root = tempfile::tempdir().unwrap();
        let mut keep = Vec::new();
        for (name, origin) in entries {
            let repo = repo_with_origin(origin);
            std::os::unix::fs::symlink(repo.path(), root.path().join(name)).unwrap();
            keep.push(repo);
        }
        std::fs::create_dir(root.path().join("not-a-repo")).unwrap();
        std::fs::write(root.path().join("file.txt"), "x").unwrap();
        Repos { root, _keep: keep }
    }

    fn standard() -> Repos {
        repos(&[
            ("marigold", "https://github.com/Example/Marigold.git"),
            ("repo", "git@github.com:example/repo.git"),
            ("other", "https://github.com/example/other.git"),
        ])
    }

    fn resolved(input: &str, repos: &Repos) -> Resolved {
        resolve_repo(input, repos.root.path()).unwrap_or_else(|e| panic!("{input}: {e}"))
    }

    fn target(repos: &Repos, name: &str) -> PathBuf {
        repos.root.path().join(name)
    }

    fn body(result: &Value) -> Value {
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn summary(identity_repo: &str) -> crate::auditor::TaskSummary {
        crate::auditor::TaskSummary::new("mcp-t", "d", identity_repo)
    }

    #[test]
    fn repos_root_defaults_and_reads_the_injected_lookup() {
        assert_eq!(repos_root_with(|_| None), PathBuf::from("/repos"));
        assert_eq!(
            repos_root_with(|_| Some("  ".into())),
            PathBuf::from("/repos")
        );
        assert_eq!(
            repos_root_with(|key| (key == REPOS_DIR_ENV).then(|| "/srv/r".to_string())),
            PathBuf::from("/srv/r")
        );
    }

    #[test]
    fn list_repos_reports_git_repositories_with_slug_and_assignable_identities() {
        let repos = standard();
        let gate = fixture_gate();
        let result = list_repos(gate.context().catalog(), repos.root.path());
        assert_eq!(result["isError"], false);
        let body = body(&result);
        let listed = body["repos"].as_array().unwrap();
        let names: Vec<&str> = listed.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["marigold", "other", "repo"]);
        let repo = listed.iter().find(|r| r["name"] == "repo").unwrap();
        assert_eq!(repo["origin_slug"], "github.com/example/repo");
        assert_eq!(repo["path"], target(&repos, "repo").display().to_string());
        let identities: Vec<&str> = repo["identities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(identities.contains(&"rust-implementer"));
        assert!(!identities.contains(&"auditor"));
        let marigold = listed.iter().find(|r| r["name"] == "marigold").unwrap();
        assert_eq!(marigold["identities"], json!([]));
    }

    #[test]
    fn list_repos_of_a_missing_root_is_empty() {
        let gate = fixture_gate();
        let result = list_repos(
            gate.context().catalog(),
            Path::new("/nonexistent/nanna/repos"),
        );
        assert_eq!(body(&result)["repos"], json!([]));
    }

    #[test]
    fn an_existing_absolute_repository_path_is_used_as_is() {
        let repos = standard();
        let direct = repo_with_origin("https://github.com/example/direct.git");
        let found = resolve_repo(direct.path().to_str().unwrap(), repos.root.path()).unwrap();
        assert_eq!(found.path, direct.path());
        assert!(found.note.is_none());
    }

    #[test]
    fn urls_and_slugs_resolve_by_origin_case_insensitively() {
        let repos = standard();
        let expected = target(&repos, "marigold");
        for input in [
            "https://github.com/example/marigold",
            "https://github.com/Example/Marigold.git",
            "https://github.com/example/marigold/",
            "git@github.com:example/marigold.git",
            "github.com/example/marigold",
            "GitHub.com/EXAMPLE/marigold",
            "example/marigold",
            "Example/Marigold.git",
        ] {
            assert_eq!(resolved(input, &repos).path, expected, "{input}");
        }
    }

    #[test]
    fn a_bare_name_resolves_by_directory_name_or_origin_name() {
        let repos = repos(&[("checkout-7", "https://github.com/example/widget.git")]);
        assert_eq!(
            resolved("widget", &repos).path,
            target(&repos, "checkout-7")
        );
        assert_eq!(
            resolved("CHECKOUT-7", &repos).path,
            target(&repos, "checkout-7")
        );
    }

    #[test]
    fn a_host_path_resolves_by_its_final_segment_and_says_so() {
        let repos = standard();
        let found = resolved("/nonexistent-host/code/marigold", &repos);
        assert_eq!(found.path, target(&repos, "marigold"));
        let note = found.note.unwrap();
        assert!(note.contains("/nonexistent-host/code/marigold"), "{note}");
        assert!(note.contains("final segment"), "{note}");
    }

    #[test]
    fn an_ambiguous_name_lists_every_candidate_and_picks_none() {
        let repos = repos(&[
            ("widget-a", "https://github.com/one/widget.git"),
            ("widget-b", "https://github.com/two/widget.git"),
        ]);
        let refusal = resolve_repo("widget", repos.root.path()).unwrap_err();
        assert_eq!(refusal.error(), "ambiguous_repo");
        assert_eq!(refusal.to_json()["candidates"].as_array().unwrap().len(), 2);
        assert!(refusal.to_string().contains("widget-a"));
        assert!(refusal.to_string().contains("widget-b"));
        assert!(resolve_repo("one/widget", repos.root.path()).is_ok());
    }

    #[test]
    fn a_miss_lists_available_repos_and_suggests_a_near_name() {
        let repos = standard();
        let refusal =
            resolve_repo("/nonexistent-host/code/marigolds", repos.root.path()).unwrap_err();
        assert_eq!(refusal.error(), "unknown_repo");
        let json = refusal.to_json();
        assert_eq!(json["available_repos"].as_array().unwrap().len(), 3);
        assert_eq!(
            json["did_you_mean"],
            json!([target(&repos, "marigold").display().to_string()])
        );
        assert!(refusal.fix().contains("Did you mean"));
        assert!(refusal.fix().contains("list_repos"));
    }

    #[test]
    fn malformed_and_unmatched_inputs_are_misses_not_panics() {
        let repos = standard();
        for input in [
            "",
            "~/code/marigold",
            "some/relative/repo",
            "a/b/c/d",
            "../marigold",
            "nosuch",
            "nosuch/owner",
            "https://github.com/example",
        ] {
            let refusal = resolve_repo(input, repos.root.path()).unwrap_err();
            assert_eq!(refusal.error(), "unknown_repo", "{input}");
            assert!(refusal.to_string().contains("unknown_repo"), "{input}");
        }
    }

    #[test]
    fn a_resolution_never_widens_scope() {
        let repos = standard();
        let found = resolved("marigold", &repos);
        let gate = fixture_gate();
        let request = gate.request(
            summary("github.com/example/marigold"),
            "rust-implementer",
            "Add a test.",
        );
        let refusal =
            precheck(gate.context(), &request, &found.path, repos.root.path()).unwrap_err();
        assert_eq!(refusal.error(), "repo_outside_scope");
        assert!(refusal.to_string().contains("scope.repos"));
        let json = refusal.to_json();
        let available: Vec<&str> = json["available_repos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["origin_slug"].as_str().unwrap())
            .collect();
        assert_eq!(available, ["github.com/example/repo"]);
    }

    #[test]
    fn precheck_allows_an_in_scope_request_and_logs_nothing() {
        let repos = standard();
        let gate = fixture_gate();
        let request = gate.request(
            summary("github.com/example/repo"),
            "rust-implementer",
            "Add a test.",
        );
        let path = target(&repos, "repo");
        assert!(precheck(gate.context(), &request, &path, repos.root.path()).is_ok());
        assert!(gate.log().entries().unwrap().is_empty());
    }

    #[test]
    fn precheck_refuses_an_unknown_identity_and_the_auditor_listing_assignable_ones() {
        let repos = standard();
        let gate = fixture_gate();
        let path = target(&repos, "repo");
        for name in ["ghost", "auditor"] {
            let request = gate.request(summary("github.com/example/repo"), name, "Add a test.");
            let refusal = precheck(gate.context(), &request, &path, repos.root.path()).unwrap_err();
            assert_eq!(refusal.reason_code(), "unknown_identity", "{name}");
            let json = refusal.to_json();
            let names: Vec<&str> = json["assignable_identities"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["name"].as_str().unwrap())
                .collect();
            assert!(names.contains(&"rust-implementer"), "{name}");
            assert!(!names.contains(&"auditor"), "{name}");
        }
    }

    #[test]
    fn precheck_refuses_an_effect_above_the_ceiling_naming_it() {
        let repos = standard();
        let gate = implementer_only_gate();
        let path = target(&repos, "repo");
        let request = gate.request(
            summary("github.com/example/repo"),
            "rust-implementer",
            "Deploy build 42 to production for all users.",
        );
        let refusal = precheck(gate.context(), &request, &path, repos.root.path()).unwrap_err();
        assert_eq!(refusal.error(), "spawn_refused");
        let json = refusal.to_json();
        assert!(json["reason_codes"]
            .as_array()
            .unwrap()
            .contains(&json!("effect_above_ceiling")));
        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("capped at `repository`"));
        assert!(json["fix"].as_str().unwrap().contains("requested_effect"));
    }
}
