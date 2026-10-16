use super::{Action, ActionError, BlastRadius, Change, ChangedFile, ImpactAnalyzer, ImpactError};
use crate::assets::{AssetError, AssetGraph};
use crate::deploy::{DeployError, DeployTemplate};
use crate::effects::EffectClass;
use crate::entities::context::types::ToolCallRecord;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn is_not_found(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

fn load_environments(repo: &Path) -> Result<Option<Vec<String>>, ImpactError> {
    match DeployTemplate::load_from_repo(repo) {
        Ok(template) => Ok(Some(template.target.environments)),
        Err(DeployError::Io { source, .. }) if is_not_found(&source) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The analyzer inputs for `repo`: its asset manifest plus, when a deploy
/// template exists, its environments. A missing manifest or a malformed
/// manifest or template is an error; only a missing template is not.
///
/// ```
/// use harness::impact::load_repo_context;
///
/// let repo = tempfile::tempdir().unwrap();
/// assert!(load_repo_context(repo.path()).is_err());
/// ```
pub fn load_repo_context(repo: &Path) -> Result<(AssetGraph, Option<Vec<String>>), ImpactError> {
    let graph = AssetGraph::load_from_repo(repo)?;
    let environments = load_environments(repo)?;
    Ok((graph, environments))
}

/// The unified diff of `rev` against the working tree of the repository at
/// `repo`, as `git diff <rev>` prints it, including untracked files.
///
/// ```
/// use harness::impact::diff_against;
///
/// let dir = tempfile::tempdir().unwrap();
/// assert!(diff_against(dir.path(), "HEAD").is_err());
/// ```
pub fn diff_against(repo: &Path, rev: &str) -> Result<String, ImpactError> {
    let repository = git2::Repository::discover(repo)?;
    let tree = repository.revparse_single(rev)?.peel_to_tree()?;
    let mut options = git2::DiffOptions::new();
    options
        .show_untracked_content(true)
        .recurse_untracked_dirs(true);
    let diff = repository.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut options))?;
    let mut out = String::new();
    let mut invalid_header = false;
    let printed = diff.print(git2::DiffFormat::Patch, |_, _, line| {
        match line.origin() {
            origin @ ('+' | '-' | ' ') => {
                out.push(origin);
                out.push_str(&String::from_utf8_lossy(line.content()));
            }
            'F' | 'H' => match std::str::from_utf8(line.content()) {
                Ok(text) => out.push_str(text),
                Err(_) => {
                    invalid_header = true;
                    return false;
                }
            },
            _ => out.push_str(&String::from_utf8_lossy(line.content())),
        }
        true
    });
    if invalid_header {
        return Err(ImpactError::Diff(
            "a file path in the diff is not valid UTF-8".into(),
        ));
    }
    printed?;
    Ok(out)
}

/// The blast radius of the working-tree changes against `rev`. Paths, the
/// manifest and the deploy template are all taken from the repository's
/// working tree root, wherever inside it `repo` points.
///
/// ```
/// use harness::impact::impact_of_diff;
///
/// let dir = tempfile::tempdir().unwrap();
/// assert!(impact_of_diff(dir.path(), "HEAD").is_err());
/// ```
pub fn impact_of_diff(repo: &Path, rev: &str) -> Result<BlastRadius, ImpactError> {
    let root = workdir_root(repo)?;
    let (graph, environments) = load_repo_context(&root)?;
    let mut change = Change::from_unified_diff(&diff_against(repo, rev)?)?;
    change.load_content(&root)?;
    Ok(build(&graph, environments).analyze(&change))
}

fn workdir_root(repo: &Path) -> Result<PathBuf, ImpactError> {
    let repository = git2::Repository::discover(repo)?;
    repository
        .workdir()
        .map(Path::to_path_buf)
        .ok_or_else(|| ImpactError::Diff("bare repository has no working tree".into()))
}

fn build(graph: &AssetGraph, environments: Option<Vec<String>>) -> ImpactAnalyzer<'_> {
    let analyzer = ImpactAnalyzer::new(graph);
    match environments {
        Some(environments) => analyzer.with_environments(environments),
        None => analyzer,
    }
}

fn relative_to_root(root: &Path, path: &str) -> Result<String, ImpactError> {
    let given = Path::new(path);
    let anchored = given.is_absolute()
        || given.has_root()
        || matches!(
            given.components().next(),
            Some(std::path::Component::Prefix(_))
        );
    if !anchored {
        return Ok(path.to_string());
    }
    given
        .strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .map(str::to_string)
        .ok_or_else(|| ImpactError::OutsideWorkspace {
            path: path.to_string(),
        })
}

fn string_argument<'a>(
    call: &'a ToolCallRecord,
    field: &'static str,
) -> Result<Option<&'a str>, ImpactError> {
    match call.arguments.get(field) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(ImpactError::Action {
            tool: call.tool_name.clone(),
            source: ActionError::BadArgument { field },
        }),
    }
}

fn change_for_call(root: &Path, call: &ToolCallRecord) -> Result<Option<Change>, ImpactError> {
    let mut change = Change::default();
    let action = Action::from_tool_call(&call.tool_name, &call.arguments).map_err(|source| {
        ImpactError::Action {
            tool: call.tool_name.clone(),
            source,
        }
    })?;
    change.actions.extend(action);
    let writes_workspace = call
        .effect
        .as_ref()
        .is_some_and(|effect| effect.class == EffectClass::Workspace);
    if writes_workspace {
        if let Some(path) = string_argument(call, "path")? {
            let path = relative_to_root(root, path)?;
            change.files.push(match string_argument(call, "content")? {
                Some(content) => ChangedFile::whole(path, content),
                None => ChangedFile::new(path),
            });
        }
    }
    Ok((!change.files.is_empty() || !change.actions.is_empty()).then_some(change))
}

/// The analyzer inputs of a repository that declares a manifest, loaded once
/// before a task starts so that edits the task makes to the manifest or the
/// deploy template cannot change how the task is scored.
///
/// ```
/// use harness::impact::TaskImpact;
///
/// let root = tempfile::tempdir().unwrap();
/// assert!(TaskImpact::load(root.path()).unwrap().is_none());
/// ```
pub struct TaskImpact {
    graph: AssetGraph,
    environments: Option<Vec<String>>,
}

impl TaskImpact {
    /// `Ok(None)` when `root` has no `.nanna/effects.toml` (tracked in #847);
    /// an unreadable or invalid manifest or deploy template is an error.
    pub fn load(root: &Path) -> Result<Option<Self>, ImpactError> {
        let graph = match AssetGraph::load_from_repo(root) {
            Ok(graph) => graph,
            Err(AssetError::Io { source, .. }) if is_not_found(&source) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let environments = load_environments(root)?;
        Ok(Some(Self {
            graph,
            environments,
        }))
    }

    /// Score a finished task: attach a per-call [`BlastRadius`] to every call
    /// that writes a path or runs a deploy or CI action, and return the radius
    /// of the whole task (its patch plus those actions). `root` is the task
    /// workspace and `patch` the complete, untruncated diff of its changes.
    ///
    /// A call whose arguments cannot be understood, a patch that is not a
    /// well-formed diff and an unreadable changed file are errors.
    pub fn analyze(
        &self,
        root: &Path,
        patch: Option<&str>,
        calls: &mut [ToolCallRecord],
    ) -> Result<BlastRadius, ImpactError> {
        let analyzer = build(&self.graph, self.environments.clone());
        let mut whole = match patch {
            Some(patch) => Change::from_unified_diff(patch)?,
            None => Change::default(),
        };
        whole.load_content(root)?;
        for call in calls.iter_mut() {
            let Some(change) = change_for_call(root, call)? else {
                continue;
            };
            whole.actions.extend(change.actions.iter().cloned());
            if let Some(effect) = call.effect.as_mut() {
                effect.blast_radius = Some(analyzer.analyze(&change));
            }
        }
        Ok(analyzer.analyze(&whole))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectRecord;
    use serde_json::json;
    use std::process::Command;

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests/fixtures/fullstack")
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() != "target" {
                    copy_dir(&entry.path(), &target);
                }
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn committed_fixture() -> (tempfile::TempDir, git2::Repository) {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture_root(), dir.path());
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        drop(tree);
        (dir, repo)
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn edit_greeting_handler(root: &Path) {
        let lib = root.join("api/src/lib.rs");
        let edited = std::fs::read_to_string(&lib).unwrap().replace(
            "HttpResponse::Ok().json(Greeting::default())",
            "HttpResponse::Ok().json(Greeting::default()) // changed",
        );
        std::fs::write(&lib, edited).unwrap();
    }

    fn set_manifest(root: &Path, text: &str) {
        std::fs::write(root.join(".nanna/effects.toml"), text).unwrap();
    }

    fn only_handler_manifest() -> &'static str {
        "[concern.content]\nweight = 3\n[asset.\"http.GET /api/v1/greeting\"]\nkind = \"endpoint\"\nconcerns = [\"content\"]\n[asset.\"http.GET /health/v1\"]\nkind = \"endpoint\"\nconcerns = [\"content\"]\n"
    }

    #[test]
    fn working_tree_edit_of_the_greeting_handler_is_scored_end_to_end() {
        let (dir, _repo) = committed_fixture();
        edit_greeting_handler(dir.path());
        let radius = impact_of_diff(dir.path(), "HEAD").unwrap();
        assert!(radius
            .touched
            .contains(&"http.GET /api/v1/greeting".to_string()));
        assert!(radius
            .evidence
            .iter()
            .any(|e| e.extractor == "routes" && e.asset == "http.GET /api/v1/greeting"));
        assert!(radius.concerns.contains_key("content"));
    }

    fn edit_fixture_source(root: &Path, from: &str, to: &str) {
        let lib = root.join("api/src/lib.rs");
        let source = std::fs::read_to_string(&lib).unwrap();
        assert!(source.contains(from));
        std::fs::write(lib, source.replace(from, to)).unwrap();
    }

    #[test]
    fn route_extraction_attributes_each_handler_edit_to_its_own_endpoint_only() {
        for (from, to, endpoint) in [
            (
                "HttpResponse::Ok().json(Greeting::default())",
                "HttpResponse::Ok().json(Greeting::default()) // changed",
                "http.GET /api/v1/greeting",
            ),
            (
                "HttpResponse::Ok().json(Health::ok())",
                "HttpResponse::Ok().json(Health::ok()) // changed",
                "http.GET /health/v1",
            ),
            (
                ".route(\"/health/v1\", web::get().to(health))",
                ".route(\"/health/v1\", web::get().to(health)) // changed",
                "http.GET /health/v1",
            ),
        ] {
            let (dir, _repo) = committed_fixture();
            set_manifest(dir.path(), only_handler_manifest());
            edit_fixture_source(dir.path(), from, to);
            let radius = impact_of_diff(dir.path(), "HEAD").unwrap();
            assert_eq!(radius.touched, [endpoint], "{from}");
            assert!(radius.downstream.is_empty());
        }
    }

    #[test]
    fn a_new_unstaged_migration_is_scored_without_being_added_to_the_index() {
        let (dir, _repo) = committed_fixture();
        std::fs::write(
            dir.path().join("migrations/20260925000000_note.sql"),
            "ALTER TABLE greetings ADD COLUMN note TEXT;\n",
        )
        .unwrap();
        let radius = impact_of_diff(dir.path(), "HEAD").unwrap();
        assert_eq!(radius.touched, ["db.greetings"]);
        assert!(radius
            .downstream
            .contains(&"http.GET /api/v1/greeting".to_string()));
        assert!(radius.evidence.iter().any(|e| e.extractor == "sql"
            && e.subject == "migrations/20260925000000_note.sql"
            && e.detail == "ALTER greetings"));
    }

    #[test]
    fn the_repository_is_found_from_a_subdirectory() {
        let (dir, _repo) = committed_fixture();
        edit_greeting_handler(dir.path());
        let from_root = impact_of_diff(dir.path(), "HEAD").unwrap();
        let from_subdirectory = impact_of_diff(&dir.path().join("api/src"), "HEAD").unwrap();
        assert_eq!(from_subdirectory, from_root);
        assert!(!from_root.is_empty());
    }

    #[test]
    fn unchanged_tree_has_empty_radius_and_bad_ref_is_an_error() {
        let (dir, _repo) = committed_fixture();
        assert!(impact_of_diff(dir.path(), "HEAD").unwrap().is_empty());
        assert!(matches!(
            impact_of_diff(dir.path(), "no-such-ref"),
            Err(ImpactError::Git(_))
        ));
    }

    #[test]
    fn missing_manifest_is_an_asset_error() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        assert!(matches!(
            impact_of_diff(dir.path(), "HEAD"),
            Err(ImpactError::Assets(_))
        ));
    }

    #[test]
    fn a_malformed_manifest_is_an_asset_error() {
        let (dir, _repo) = committed_fixture();
        set_manifest(dir.path(), "[asset.\"db.a\"\n");
        assert!(matches!(
            impact_of_diff(dir.path(), "HEAD"),
            Err(ImpactError::Assets(crate::assets::AssetError::Parse { .. }))
        ));
    }

    #[test]
    fn a_malformed_deploy_template_is_an_error_not_an_unrestricted_environment_list() {
        let (dir, _repo) = committed_fixture();
        std::fs::write(dir.path().join(".nanna/deploy.toml"), "[target\n").unwrap();
        let error = impact_of_diff(dir.path(), "HEAD").unwrap_err();
        assert!(
            matches!(&error, ImpactError::Deploy(e) if matches!(**e, DeployError::Parse { .. }))
        );
        assert!(error.to_string().contains("deploy.toml"));
    }

    #[test]
    fn an_unreadable_deploy_template_is_an_error() {
        let (dir, _repo) = committed_fixture();
        let deploy = dir.path().join(".nanna/deploy.toml");
        std::fs::remove_file(&deploy).unwrap();
        std::fs::create_dir(&deploy).unwrap();
        assert!(matches!(
            load_repo_context(dir.path()),
            Err(ImpactError::Deploy(e)) if matches!(*e, DeployError::Io { .. })
        ));
    }

    #[test]
    fn a_repository_without_a_deploy_template_has_no_environment_restriction() {
        let (dir, _repo) = committed_fixture();
        std::fs::remove_file(dir.path().join(".nanna/deploy.toml")).unwrap();
        let (_, environments) = load_repo_context(dir.path()).unwrap();
        assert_eq!(environments, None);
        assert!(impact_of_diff(dir.path(), "HEAD").unwrap().is_empty());
    }

    #[test]
    fn a_repository_with_a_deploy_template_restricts_environments_to_it() {
        let (_, environments) = load_repo_context(&fixture_root()).unwrap();
        assert_eq!(environments.unwrap(), ["sandbox", "staging", "production"]);
    }

    #[test]
    fn diff_against_includes_tracked_edits_and_untracked_files_with_git_prefixes() {
        let (dir, _repo) = committed_fixture();
        edit_greeting_handler(dir.path());
        std::fs::write(dir.path().join("fresh.txt"), "one\ntwo\n").unwrap();
        let diff = diff_against(dir.path(), "HEAD").unwrap();
        assert!(diff.contains("diff --git a/api/src/lib.rs b/api/src/lib.rs\n"));
        assert!(diff.contains("diff --git a/fresh.txt b/fresh.txt\n"));
        assert!(diff.contains("+one\n+two\n"));
        let change = Change::from_unified_diff(&diff).unwrap();
        assert_eq!(change.files.len(), 2);
    }

    #[test]
    fn diff_against_reports_files_without_a_trailing_newline_in_a_parseable_form() {
        let (dir, _repo) = committed_fixture();
        std::fs::write(dir.path().join("fresh.txt"), "no newline").unwrap();
        let diff = diff_against(dir.path(), "HEAD").unwrap();
        assert!(diff.contains("\\ No newline at end of file"));
        assert_eq!(Change::from_unified_diff(&diff).unwrap().files.len(), 1);
    }

    #[test]
    fn libgit2_diffs_of_spaced_and_non_ascii_paths_are_attributed_to_those_paths() {
        let (dir, _repo) = committed_fixture();
        std::fs::write(dir.path().join("ui/my page.html"), "<p>a</p>\n").unwrap();
        std::fs::write(dir.path().join("ui/é.html"), "<p>b</p>\n").unwrap();
        let diff = diff_against(dir.path(), "HEAD").unwrap();
        let change = Change::from_unified_diff(&diff).unwrap();
        let paths: Vec<_> = change.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["ui/my page.html", "ui/é.html"]);
        let radius = impact_of_diff(dir.path(), "HEAD").unwrap();
        assert!(radius
            .evidence
            .iter()
            .any(|e| e.subject == "ui/my page.html"));
        assert!(radius.evidence.iter().any(|e| e.subject == "ui/é.html"));
    }

    #[test]
    fn bare_repository_has_no_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init_bare(dir.path()).unwrap();
        let error = workdir_root(dir.path()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "failed to read diff: bare repository has no working tree"
        );
    }

    #[test]
    fn build_restricts_actions_to_the_given_environments_only_when_given_any() {
        let graph = AssetGraph::parse(
            "[concern.c]\nweight = 5\n[asset.\"http.GET /a\"]\nkind = \"endpoint\"\nconcerns = [\"c\"]\n",
        )
        .unwrap();
        let deploy = Change::from_actions([Action::SandboxDeploy {
            environment: "sandbox".into(),
        }]);
        assert_eq!(build(&graph, None).analyze(&deploy).score, 5);
        assert_eq!(
            build(&graph, Some(vec!["sandbox".to_string()]))
                .analyze(&deploy)
                .score,
            5
        );
        assert_eq!(
            build(&graph, Some(vec!["prod".to_string()]))
                .analyze(&deploy)
                .score,
            0
        );
    }

    fn call(tool: &str, args: serde_json::Value, class: EffectClass) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: tool.to_string(),
            arguments: args,
            call_id: "c".into(),
            result: String::new(),
            effect: Some(EffectRecord::new(class)),
        }
    }

    fn radius_of(call: &ToolCallRecord) -> &BlastRadius {
        call.effect
            .as_ref()
            .unwrap()
            .blast_radius
            .as_ref()
            .expect("per-call blast radius")
    }

    fn fixture_impact() -> TaskImpact {
        TaskImpact::load(&fixture_root()).unwrap().unwrap()
    }

    const UI_PATCH: &str = "diff --git a/ui/index.html b/ui/index.html\n--- a/ui/index.html\n+++ b/ui/index.html\n@@ -1 +1 @@\n-a\n+b\n";

    #[test]
    fn analyze_annotates_calls_and_scores_the_patch() {
        let root = fixture_root();
        let mut calls = vec![
            call(
                "read_file",
                json!({"path": "api/src/lib.rs"}),
                EffectClass::None,
            ),
            call(
                "write_file",
                json!({"path": "migrations/x.sql", "content": "ALTER TABLE greetings ADD COLUMN x INT;\n"}),
                EffectClass::Workspace,
            ),
            call(
                "sandbox_deploy",
                json!({"environment": "sandbox"}),
                EffectClass::Sandbox,
            ),
            call("ci_trigger", json!({}), EffectClass::Ci),
        ];
        let whole = fixture_impact()
            .analyze(&root, Some(UI_PATCH), &mut calls)
            .unwrap();
        assert!(calls[0].effect.as_ref().unwrap().blast_radius.is_none());
        assert_eq!(radius_of(&calls[1]).touched, ["db.greetings"]);
        assert!(radius_of(&calls[1])
            .evidence
            .iter()
            .any(|e| e.extractor == "sql"));
        let deploy = radius_of(&calls[2]);
        assert!(deploy.touched.contains(&"http.GET /health/v1".to_string()));
        assert!(radius_of(&calls[3]).is_empty());
        assert!(whole.touched.contains(&"ui.greeting_page".to_string()));
        assert!(whole.touched.contains(&"http.GET /health/v1".to_string()));
        assert!(whole.score >= deploy.score);
    }

    #[test]
    fn task_scoring_restricts_deploys_to_the_templates_environments() {
        let root = fixture_root();
        let deploy = |environment: &str| {
            vec![call(
                "sandbox_deploy",
                json!({"environment": environment}),
                EffectClass::Sandbox,
            )]
        };
        let open = TaskImpact {
            graph: AssetGraph::load_from_repo(&root).unwrap(),
            environments: None,
        };
        let restricted = TaskImpact {
            graph: AssetGraph::load_from_repo(&root).unwrap(),
            environments: Some(vec!["production".to_string()]),
        };
        let mut calls = deploy("sandbox");
        let unrestricted = open.analyze(&root, None, &mut calls).unwrap();
        assert!(!unrestricted.touched.is_empty());
        assert!(!radius_of(&calls[0]).touched.is_empty());
        let mut calls = deploy("sandbox");
        let blocked = restricted.analyze(&root, None, &mut calls).unwrap();
        assert!(blocked.touched.is_empty());
        assert!(radius_of(&calls[0]).touched.is_empty());
    }

    #[test]
    fn a_written_file_is_attributed_by_its_content_not_only_its_path() {
        let root = fixture_root();
        let mut calls = vec![call(
            "write_file",
            json!({"path": "elsewhere/handlers.rs", "content": "#[get(\"/api/v1/greeting\")]\nasync fn g() {}\n"}),
            EffectClass::Workspace,
        )];
        fixture_impact().analyze(&root, None, &mut calls).unwrap();
        assert_eq!(radius_of(&calls[0]).touched, ["http.GET /api/v1/greeting"]);
        assert_eq!(radius_of(&calls[0]).evidence[0].extractor, "routes");
    }

    #[test]
    fn a_workspace_call_without_a_path_or_content_gets_no_radius() {
        let root = fixture_root();
        let mut calls = vec![
            call("cargo_build", json!({}), EffectClass::Workspace),
            call(
                "write_file",
                json!({"path": "migrations/y.sql"}),
                EffectClass::Workspace,
            ),
        ];
        fixture_impact().analyze(&root, None, &mut calls).unwrap();
        assert!(calls[0].effect.as_ref().unwrap().blast_radius.is_none());
        assert_eq!(radius_of(&calls[1]).touched, ["db.greetings"]);
    }

    #[test]
    fn absolute_paths_inside_the_workspace_are_made_relative() {
        let root = fixture_root();
        let absolute = root.join("migrations/z.sql");
        let mut calls = vec![call(
            "write_file",
            json!({"path": absolute.to_str().unwrap(), "content": ""}),
            EffectClass::Workspace,
        )];
        fixture_impact().analyze(&root, None, &mut calls).unwrap();
        assert_eq!(radius_of(&calls[0]).touched, ["db.greetings"]);
    }

    #[test]
    fn absolute_paths_outside_the_workspace_are_an_error() {
        let root = fixture_root();
        let mut calls = vec![call(
            "write_file",
            json!({"path": "/etc/passwd", "content": ""}),
            EffectClass::Workspace,
        )];
        let error = fixture_impact()
            .analyze(&root, None, &mut calls)
            .unwrap_err();
        assert!(matches!(error, ImpactError::OutsideWorkspace { .. }));
        assert_eq!(
            error.to_string(),
            "`/etc/passwd` is outside the task workspace"
        );
    }

    #[test]
    fn calls_with_arguments_of_the_wrong_type_are_errors_naming_the_tool() {
        let root = fixture_root();
        for (tool, args, field) in [
            ("write_file", json!({"path": 5, "content": ""}), "path"),
            (
                "write_file",
                json!({"path": "a.txt", "content": 5}),
                "content",
            ),
            ("sandbox_deploy", json!({}), "environment"),
            ("prod_rollout", json!({"environment": 1}), "environment"),
        ] {
            let class = if tool == "write_file" {
                EffectClass::Workspace
            } else {
                EffectClass::Sandbox
            };
            let mut calls = vec![call(tool, args, class)];
            let error = fixture_impact()
                .analyze(&root, None, &mut calls)
                .unwrap_err();
            assert!(
                matches!(&error, ImpactError::Action { tool: t, source: ActionError::BadArgument { field: f } } if t == tool && *f == field),
                "{error}"
            );
            assert!(error
                .to_string()
                .starts_with(&format!("tool call `{tool}`")));
        }
    }

    #[test]
    fn calls_without_effect_attribution_still_contribute_their_actions() {
        let root = fixture_root();
        let mut record = call(
            "sandbox_deploy",
            json!({"environment": "sandbox"}),
            EffectClass::Sandbox,
        );
        record.effect = None;
        let mut calls = vec![record];
        let whole = fixture_impact().analyze(&root, None, &mut calls).unwrap();
        assert!(calls[0].effect.is_none());
        assert!(whole.touched.contains(&"http.GET /health/v1".to_string()));
    }

    #[test]
    fn a_patch_that_is_not_a_diff_is_an_error() {
        let root = fixture_root();
        let error = fixture_impact()
            .analyze(&root, Some("garbage"), &mut [])
            .unwrap_err();
        assert!(matches!(error, ImpactError::Parse(_)));
        assert!(error.to_string().starts_with("malformed diff: "));
    }

    #[cfg(unix)]
    #[test]
    fn a_changed_file_that_cannot_be_read_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture_root(), dir.path());
        std::os::unix::fs::symlink("loop.html", dir.path().join("ui/loop.html")).unwrap();
        let patch = "diff --git a/ui/loop.html b/ui/loop.html\nnew file mode 120000\n";
        let impact = TaskImpact::load(dir.path()).unwrap().unwrap();
        assert!(matches!(
            impact.analyze(dir.path(), Some(patch), &mut []),
            Err(ImpactError::Io { .. })
        ));
    }

    #[test]
    fn a_large_patch_is_analysed_to_its_last_file() {
        let root = fixture_root();
        let mut patch = String::from(
            "diff --git a/ui/big.txt b/ui/big.txt\nnew file mode 100644\n--- /dev/null\n+++ b/ui/big.txt\n@@ -0,0 +1,60000 @@\n",
        );
        for _ in 0..60_000 {
            patch.push_str("+0123456789012345678901234567890123456789\n");
        }
        patch.push_str("diff --git a/migrations/last.sql b/migrations/last.sql\nnew file mode 100644\n--- /dev/null\n+++ b/migrations/last.sql\n@@ -0,0 +1 @@\n+ALTER TABLE greetings ADD COLUMN z INT;\n");
        assert!(patch.len() > 2_000_000);
        let whole = fixture_impact()
            .analyze(&root, Some(&patch), &mut [])
            .unwrap();
        assert!(whole.touched.contains(&"db.greetings".to_string()));
        assert!(whole.touched.contains(&"ui.greeting_page".to_string()));
    }

    #[test]
    fn a_real_git_diff_with_a_rename_a_space_and_a_non_ascii_name_is_attributed_to_every_path() {
        let (dir, _repo) = committed_fixture();
        git(dir.path(), &["mv", "ui/index.html", "docs-index.html"]);
        std::fs::write(dir.path().join("ui/my page.html"), "<p>a</p>\n").unwrap();
        std::fs::write(dir.path().join("ui/é.html"), "<p>b</p>\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        let patch = git(dir.path(), &["diff", "--cached", "HEAD"]);
        assert!(patch.contains("rename from ui/index.html"));
        assert!(patch.contains("\"b/ui/\\303\\251.html\""));
        let change = Change::from_unified_diff(&patch).unwrap();
        let paths: Vec<_> = change.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "ui/index.html",
                "docs-index.html",
                "ui/my page.html",
                "ui/é.html"
            ]
        );
        let whole = fixture_impact()
            .analyze(dir.path(), Some(&patch), &mut [])
            .unwrap();
        assert_eq!(whole.touched, ["ui.greeting_page"]);
        let subjects: Vec<_> = whole.evidence.iter().map(|e| e.subject.as_str()).collect();
        assert_eq!(subjects, ["ui/index.html", "ui/my page.html", "ui/é.html"]);
    }

    #[test]
    fn the_manifest_is_read_once_so_a_task_cannot_rewrite_how_it_is_scored() {
        let (dir, _repo) = committed_fixture();
        let impact = TaskImpact::load(dir.path()).unwrap().unwrap();
        set_manifest(dir.path(), "");
        let whole = impact.analyze(dir.path(), Some(UI_PATCH), &mut []).unwrap();
        assert_eq!(whole.touched, ["ui.greeting_page"]);
        assert!(TaskImpact::load(dir.path())
            .unwrap()
            .unwrap()
            .analyze(dir.path(), Some(UI_PATCH), &mut [])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn task_impact_load_distinguishes_missing_from_broken() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TaskImpact::load(dir.path()).unwrap().is_none());

        std::fs::create_dir(dir.path().join(".nanna")).unwrap();
        std::fs::write(dir.path().join(".nanna/effects.toml"), "[asset\n").unwrap();
        assert!(matches!(
            TaskImpact::load(dir.path()),
            Err(ImpactError::Assets(crate::assets::AssetError::Parse { .. }))
        ));

        std::fs::write(dir.path().join(".nanna/effects.toml"), "").unwrap();
        assert!(TaskImpact::load(dir.path()).unwrap().is_some());
        std::fs::write(dir.path().join(".nanna/deploy.toml"), "[target\n").unwrap();
        assert!(matches!(
            TaskImpact::load(dir.path()),
            Err(ImpactError::Deploy(e)) if matches!(*e, DeployError::Parse { .. })
        ));
    }

    #[test]
    fn task_impact_load_surfaces_an_unreadable_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".nanna/effects.toml")).unwrap();
        assert!(matches!(
            TaskImpact::load(dir.path()),
            Err(ImpactError::Assets(crate::assets::AssetError::Io { .. }))
        ));
    }
}
