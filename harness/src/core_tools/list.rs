use super::{error_outcome, io_outcome};
use crate::effects::EffectClass;
use crate::scope::{
    canonical_root, relative_to, validate_path_within_workspace, PathAccess, PathScope,
};
use crate::tool_contract::{ErrorCode, ToolOutcome, TypedTool};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_ENTRIES: u32 = 500;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDirectoryArgs {
    #[schemars(description = "Directory to list, relative to the workspace root (default \".\")")]
    pub path: Option<String>,
    #[schemars(description = "Maximum entries to return (default 500)")]
    pub max_entries: Option<u32>,
}

pub struct ListDirectoryTool {
    root: PathBuf,
    scope: Option<PathScope>,
}

impl ListDirectoryTool {
    pub fn new(root: PathBuf, scope: Option<PathScope>) -> Self {
        Self { root, scope }
    }
}

#[async_trait]
impl TypedTool for ListDirectoryTool {
    type Args = ListDirectoryArgs;

    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List the entries of one directory, sorted by name, each with kind file or dir."
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }

    async fn run(&self, args: ListDirectoryArgs) -> ToolOutcome {
        let path_arg = args.path.as_deref().unwrap_or(".");
        let limit = args.max_entries.unwrap_or(DEFAULT_MAX_ENTRIES);
        if limit == 0 {
            return ToolOutcome::failure(ErrorCode::InvalidArgs, "max_entries must be at least 1");
        }
        let target = match validate_path_within_workspace(Path::new(path_arg), &self.root) {
            Ok(target) => target,
            Err(error) => return error_outcome(error, &self.root, path_arg),
        };
        if !target.is_dir() {
            return ToolOutcome::failure(
                ErrorCode::InvalidArgs,
                format!("{path_arg} is not a directory"),
            )
            .with_hint("use read_file to read a file");
        }
        let root = match canonical_root(&self.root) {
            Ok(root) => root,
            Err(error) => return ToolOutcome::from(&error),
        };
        let reader = match std::fs::read_dir(&target) {
            Ok(reader) => reader,
            Err(error) => return io_outcome(&error, path_arg),
        };
        let mut entries: Vec<(String, &'static str)> = Vec::new();
        for entry in reader.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            let is_dir = kind.is_dir();
            let visible = match &self.scope {
                None => true,
                Some(scope) if is_dir => scope.contains_readable(&path, &root),
                Some(scope) => scope.permits(PathAccess::Read, relative_to(&path, &root)),
            };
            if visible {
                let kind = if is_dir { "dir" } else { "file" };
                entries.push((entry.file_name().to_string_lossy().to_string(), kind));
            }
        }
        entries.sort();
        let total = entries.len();
        let truncated = total > limit as usize;
        entries.truncate(limit as usize);
        let entries: Vec<_> = entries
            .into_iter()
            .map(|(name, kind)| json!({"name": name, "kind": kind}))
            .collect();
        ToolOutcome::success(json!({
            "path": path_arg,
            "entries": entries,
            "total": total,
            "truncated": truncated,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::PathScope;
    use crate::tool_contract::Typed;
    use crate::tools::Tool;
    use serde_json::{json, Value};
    use tempfile::TempDir;

    fn tool(dir: &TempDir, scope: Option<PathScope>) -> Typed<ListDirectoryTool> {
        Typed(ListDirectoryTool::new(dir.path().to_path_buf(), scope))
    }

    async fn run(tool: &Typed<ListDirectoryTool>, args: Value) -> Value {
        tool.execute(args).await.unwrap()
    }

    fn put(dir: &TempDir, name: &str, content: &str) {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn names(result: &Value) -> Vec<(String, String)> {
        result["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["name"].as_str().unwrap().to_string(),
                    e["kind"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn pair(name: &str, kind: &str) -> (String, String) {
        (name.to_string(), kind.to_string())
    }

    #[tokio::test]
    async fn lists_sorted_entries_with_kinds_defaulting_to_the_root() {
        let dir = TempDir::new().unwrap();
        put(&dir, "b.txt", "");
        put(&dir, "a.txt", "");
        put(&dir, "src/x.rs", "");

        let result = run(&tool(&dir, None), json!({})).await;

        assert_eq!(result["ok"], true);
        assert_eq!(
            names(&result),
            [
                pair("a.txt", "file"),
                pair("b.txt", "file"),
                pair("src", "dir")
            ]
        );
        assert_eq!(result["data"]["truncated"], false);
    }

    #[tokio::test]
    async fn lists_a_subdirectory() {
        let dir = TempDir::new().unwrap();
        put(&dir, "src/x.rs", "");
        put(&dir, "top.txt", "");

        let result = run(&tool(&dir, None), json!({"path": "src"})).await;

        assert_eq!(names(&result), [pair("x.rs", "file")]);
        assert_eq!(result["data"]["path"], "src");
    }

    #[tokio::test]
    async fn max_entries_truncates_and_flags_it() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a", "");
        put(&dir, "b", "");
        put(&dir, "c", "");

        let result = run(&tool(&dir, None), json!({"max_entries": 2})).await;

        assert_eq!(names(&result), [pair("a", "file"), pair("b", "file")]);
        assert_eq!(result["data"]["truncated"], true);
        assert_eq!(result["data"]["total"], 3);
    }

    #[tokio::test]
    async fn a_zero_max_entries_is_invalid_args() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"max_entries": 0})).await;

        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn scope_filters_files_and_dirs_that_lead_nowhere_readable() {
        let dir = TempDir::new().unwrap();
        put(&dir, "src/a.rs", "");
        put(&dir, "secret/b.rs", "");
        put(&dir, "README.md", "");
        let scope =
            PathScope::new("t", &["src/**".to_string()], Some(&["src/**".to_string()])).unwrap();

        let result = run(&tool(&dir, Some(scope)), json!({})).await;

        assert_eq!(names(&result), [pair("src", "dir")]);
    }

    #[tokio::test]
    async fn a_file_path_is_invalid_args_pointing_at_read_file() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "");

        let result = run(&tool(&dir, None), json!({"path": "a.txt"})).await;

        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["hint"].as_str().unwrap().contains("read_file"));
    }

    #[tokio::test]
    async fn a_missing_directory_is_not_found() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"path": "nope"})).await;

        assert_eq!(result["error_code"], "not_found");
    }

    #[tokio::test]
    async fn a_path_outside_the_workspace_is_out_of_scope() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"path": ".."})).await;

        assert_eq!(result["error_code"], "out_of_scope");
    }

    #[tokio::test]
    async fn an_unknown_argument_is_rejected_by_name() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"recursive": true})).await;

        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("recursive"));
    }
}
