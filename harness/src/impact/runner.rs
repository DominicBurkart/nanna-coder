use super::{Action, BlastRadius, Change, ChangedFile, ImpactAnalyzer};
use crate::assets::AssetGraph;
use crate::deploy::DeployTemplate;
use crate::effects::EffectClass;
use crate::entities::context::types::ToolCallRecord;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImpactError {
    #[error(transparent)]
    Assets(#[from] crate::assets::AssetError),
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("failed to read diff: {0}")]
    Diff(String),
}

/// The analyzer for `repo`: its asset manifest plus, when a deploy template
/// exists, its environments.
///
/// ```
/// use harness::impact::load_repo_context;
///
/// let repo = tempfile::tempdir().unwrap();
/// assert!(load_repo_context(repo.path()).is_err());
/// ```
pub fn load_repo_context(repo: &Path) -> Result<(AssetGraph, Option<Vec<String>>), ImpactError> {
    let graph = AssetGraph::load_from_repo(repo)?;
    let environments = DeployTemplate::load_from_repo(repo)
        .ok()
        .map(|template| template.target.environments);
    Ok((graph, environments))
}

/// The unified diff of `rev` against the working tree of the repository at
/// `repo`, as `git diff <rev>` prints it.
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
    let diff = repository.diff_tree_to_workdir_with_index(Some(&tree), None)?;
    let mut out = String::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin());
        }
        out.push_str(&String::from_utf8_lossy(line.content()));
        true
    })?;
    Ok(out)
}

/// The blast radius of the working-tree changes against `rev`.
///
/// ```
/// use harness::impact::impact_of_diff;
///
/// let dir = tempfile::tempdir().unwrap();
/// assert!(impact_of_diff(dir.path(), "HEAD").is_err());
/// ```
pub fn impact_of_diff(repo: &Path, rev: &str) -> Result<BlastRadius, ImpactError> {
    let (graph, environments) = load_repo_context(repo)?;
    let mut change = Change::from_unified_diff(&diff_against(repo, rev)?);
    change.load_content(&workdir_root(repo)?);
    Ok(build(&graph, environments).analyze(&change))
}

fn workdir_root(repo: &Path) -> Result<std::path::PathBuf, ImpactError> {
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

fn change_for_call(call: &ToolCallRecord) -> Option<Change> {
    let mut change = Change::default();
    if let Some(action) = Action::from_tool_call(&call.tool_name, &call.arguments) {
        change.actions.push(action);
    }
    let writes_workspace = call
        .effect
        .as_ref()
        .is_some_and(|effect| effect.class == EffectClass::Workspace);
    if writes_workspace {
        if let Some(path) = call.arguments.get("path").and_then(|p| p.as_str()) {
            change.files.push(ChangedFile::new(path));
        }
    }
    (!change.files.is_empty() || !change.actions.is_empty()).then_some(change)
}

/// Score a finished task: attach a per-call [`BlastRadius`] to every call that
/// writes a path or runs a deploy or CI action, and return the radius of the
/// whole task (its patch plus those actions).
///
/// Returns `None` when `root` has no valid `.nanna/effects.toml`.
///
/// ```
/// use harness::impact::analyze_task;
///
/// let root = tempfile::tempdir().unwrap();
/// assert!(analyze_task(root.path(), None, &mut []).is_none());
/// ```
pub fn analyze_task(
    root: &Path,
    patch: Option<&str>,
    calls: &mut [ToolCallRecord],
) -> Option<BlastRadius> {
    let (graph, environments) = load_repo_context(root).ok()?;
    let analyzer = build(&graph, environments);
    let mut whole = patch.map(Change::from_unified_diff).unwrap_or_default();
    whole.load_content(root);
    for call in calls.iter_mut() {
        let Some(change) = change_for_call(call) else {
            continue;
        };
        whole.actions.extend(change.actions.iter().cloned());
        if let Some(effect) = call.effect.as_mut() {
            effect.blast_radius = Some(analyzer.analyze(&change));
        }
    }
    Some(analyzer.analyze(&whole))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectRecord;
    use serde_json::json;
    use std::path::PathBuf;

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

    #[test]
    fn working_tree_edit_of_the_greeting_handler_is_scored_end_to_end() {
        let (dir, _repo) = committed_fixture();
        let lib = dir.path().join("api/src/lib.rs");
        let edited = std::fs::read_to_string(&lib).unwrap().replace(
            "HttpResponse::Ok().json(Greeting::default())",
            "HttpResponse::Ok().json(Greeting::default()) // changed",
        );
        std::fs::write(&lib, edited).unwrap();
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

    #[test]
    fn changed_migration_is_scored_end_to_end() {
        let (dir, _repo) = committed_fixture();
        std::fs::write(
            dir.path().join("migrations/20260925000000_note.sql"),
            "ALTER TABLE greetings ADD COLUMN note TEXT;\n",
        )
        .unwrap();
        std::process::Command::new("git")
            .args(["add", "-N", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let radius = impact_of_diff(dir.path(), "HEAD").unwrap();
        assert_eq!(radius.touched, ["db.greetings"]);
        assert!(radius
            .downstream
            .contains(&"http.GET /api/v1/greeting".to_string()));
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

    fn call(tool: &str, args: serde_json::Value, class: EffectClass) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: tool.to_string(),
            arguments: args,
            call_id: "c".into(),
            result: String::new(),
            effect: Some(EffectRecord::new(class)),
        }
    }

    #[test]
    fn analyze_task_annotates_calls_and_scores_the_patch() {
        let root = fixture_root();
        let mut calls = vec![
            call(
                "read_file",
                json!({"path": "api/src/lib.rs"}),
                EffectClass::None,
            ),
            call(
                "write_file",
                json!({"path": "migrations/x.sql"}),
                EffectClass::Workspace,
            ),
            call(
                "sandbox_deploy",
                json!({"environment": "sandbox"}),
                EffectClass::Sandbox,
            ),
            call("ci_trigger", json!({}), EffectClass::Ci),
        ];
        let patch = "diff --git a/ui/index.html b/ui/index.html\n--- a/ui/index.html\n+++ b/ui/index.html\n@@ -1 +1 @@\n-a\n+b\n";
        let whole = analyze_task(&root, Some(patch), &mut calls).unwrap();
        assert!(calls[0].effect.as_ref().unwrap().blast_radius.is_none());
        let write = calls[1]
            .effect
            .as_ref()
            .unwrap()
            .blast_radius
            .as_ref()
            .unwrap();
        assert_eq!(write.touched, ["db.greetings"]);
        let deploy = calls[2]
            .effect
            .as_ref()
            .unwrap()
            .blast_radius
            .as_ref()
            .unwrap();
        assert!(deploy.touched.contains(&"http.GET /health/v1".to_string()));
        let ci = calls[3]
            .effect
            .as_ref()
            .unwrap()
            .blast_radius
            .as_ref()
            .unwrap();
        assert!(ci.is_empty());
        assert!(whole.touched.contains(&"ui.greeting_page".to_string()));
        assert!(whole.score >= deploy.score);
    }

    #[test]
    fn analyze_task_without_manifest_is_none_and_leaves_calls_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut calls = vec![call("ci_trigger", json!({}), EffectClass::Ci)];
        assert!(analyze_task(dir.path(), None, &mut calls).is_none());
        assert!(calls[0].effect.as_ref().unwrap().blast_radius.is_none());
    }

    #[test]
    fn calls_without_effect_attribution_are_skipped() {
        let root = fixture_root();
        let mut record = call("ci_trigger", json!({}), EffectClass::Ci);
        record.effect = None;
        let mut calls = vec![record];
        assert!(analyze_task(&root, None, &mut calls).unwrap().is_empty());
        assert!(calls[0].effect.is_none());
    }
}
