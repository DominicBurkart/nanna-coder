use super::{error_outcome, io_outcome};
use crate::effects::EffectClass;
use crate::protected::ProtectedPaths;
use crate::scope::PathScope;
use crate::tool_contract::{ErrorCode, ToolOutcome, TypedTool};
use crate::tools::{reject_hardlinked, resolve_write_target};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteFileArgs {
    #[schemars(description = "Path of the file, relative to the workspace root")]
    pub path: String,
    #[schemars(description = "Complete file content")]
    pub content: String,
    #[schemars(description = "Replace the file if it already exists (default false)")]
    #[serde(default)]
    pub overwrite: bool,
}

pub struct WriteFile {
    root: PathBuf,
    scope: Option<PathScope>,
    protected: ProtectedPaths,
}

impl WriteFile {
    pub fn new(root: PathBuf, scope: Option<PathScope>, protected: ProtectedPaths) -> Self {
        Self {
            root,
            scope,
            protected,
        }
    }
}

fn line_count(text: &str) -> usize {
    text.lines().count()
}

fn exists_failure(path: &str) -> ToolOutcome {
    ToolOutcome::failure(ErrorCode::FileExists, format!("{path} already exists"))
        .with_hint("use replace to change part of an existing file")
}

#[async_trait]
impl TypedTool for WriteFile {
    type Args = WriteFileArgs;

    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create a new file with the given content, creating parent directories. Fails if the file exists unless overwrite is true; to change part of an existing file use replace."
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Workspace
    }

    async fn run(&self, args: WriteFileArgs) -> ToolOutcome {
        let path = Path::new(&args.path);
        let safe = match resolve_write_target(
            self.scope.as_ref(),
            &self.protected,
            "write_file",
            path,
            &self.root,
        ) {
            Ok(safe) => safe,
            Err(error) => return error_outcome(error, &self.root, &args.path),
        };
        let existing = match std::fs::metadata(&safe) {
            Ok(metadata) if metadata.is_dir() => {
                return ToolOutcome::failure(
                    ErrorCode::InvalidArgs,
                    format!("{} is a directory, not a file", args.path),
                )
                .with_hint("use list_directory to see its entries")
            }
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return io_outcome(&error, &args.path),
        };
        if existing && !args.overwrite {
            return exists_failure(&args.path);
        }
        if let Some(parent) = safe.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                return io_outcome(&error, &args.path);
            }
        }
        let bytes_after = args.content.len();
        let lines_after = line_count(&args.content);
        if !existing {
            let created = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&safe);
            let mut file = match created {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return exists_failure(&args.path)
                }
                Err(error) => return io_outcome(&error, &args.path),
            };
            if let Err(error) = file.write_all(args.content.as_bytes()) {
                return io_outcome(&error, &args.path);
            }
            return ToolOutcome::success(json!({
                "path": args.path,
                "created": true,
                "bytes": bytes_after,
                "lines": lines_after,
            }));
        }
        let mut file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&safe)
        {
            Ok(file) => file,
            Err(error) => return io_outcome(&error, &args.path),
        };
        if let Err(error) = reject_hardlinked(&file, path) {
            return ToolOutcome::from(&error);
        }
        let mut before = Vec::new();
        if let Err(error) = std::io::Read::read_to_end(&mut file, &mut before) {
            return io_outcome(&error, &args.path);
        }
        let lines_before = line_count(&String::from_utf8_lossy(&before));
        let written = file
            .set_len(0)
            .and_then(|_| std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(0)))
            .and_then(|_| file.write_all(args.content.as_bytes()));
        if let Err(error) = written {
            return io_outcome(&error, &args.path);
        }
        ToolOutcome::success(json!({
            "path": args.path,
            "created": false,
            "bytes": bytes_after,
            "lines": lines_after,
            "bytes_before": before.len(),
            "lines_before": lines_before,
            "bytes_after": bytes_after,
            "lines_after": lines_after,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protected::ProtectedPaths;
    use crate::scope::PathScope;
    use crate::tool_contract::Typed;
    use crate::tools::Tool;
    use serde_json::{json, Value};
    use tempfile::TempDir;

    fn tool(dir: &TempDir, scope: Option<PathScope>) -> Typed<WriteFile> {
        Typed(WriteFile::new(
            dir.path().to_path_buf(),
            scope,
            ProtectedPaths::for_repo(dir.path()),
        ))
    }

    async fn run(tool: &Typed<WriteFile>, args: Value) -> Value {
        tool.execute(args).await.unwrap()
    }

    #[tokio::test]
    async fn creates_a_new_file_and_its_parent_directories() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "a/b/c.txt", "content": "hello\nworld\n"}),
        )
        .await;

        assert_eq!(result["ok"], true);
        assert_eq!(result["data"]["created"], true);
        assert_eq!(result["data"]["bytes"], 12);
        assert_eq!(result["data"]["lines"], 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
            "hello\nworld\n"
        );
    }

    #[tokio::test]
    async fn refuses_to_overwrite_an_existing_file_by_default() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "keep").unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "content": "clobber"}),
        )
        .await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "file_exists");
        assert!(result["hint"]
            .as_str()
            .unwrap()
            .contains("use replace to change part of an existing file"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "keep"
        );
    }

    #[tokio::test]
    async fn overwrite_true_replaces_and_records_before_and_after() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "content": "x\n", "overwrite": true}),
        )
        .await;

        assert_eq!(result["ok"], true);
        assert_eq!(result["data"]["created"], false);
        assert_eq!(result["data"]["bytes_before"], 14);
        assert_eq!(result["data"]["lines_before"], 3);
        assert_eq!(result["data"]["bytes_after"], 2);
        assert_eq!(result["data"]["lines_after"], 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "x\n"
        );
    }

    #[tokio::test]
    async fn overwrite_false_explicit_still_refuses() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "keep").unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "content": "x", "overwrite": false}),
        )
        .await;

        assert_eq!(result["error_code"], "file_exists");
    }

    #[tokio::test]
    async fn writing_over_a_directory_fails_without_panicking() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "sub", "content": "x", "overwrite": true}),
        )
        .await;

        assert_eq!(result["ok"], false);
    }

    #[tokio::test]
    async fn protected_paths_are_denied() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": ".github/workflows/ci.yml", "content": "x"}),
        )
        .await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "denied");
        assert!(!dir.path().join(".github").exists());
    }

    #[tokio::test]
    async fn write_scope_denies_paths_outside_scope_paths() {
        let dir = TempDir::new().unwrap();
        let scope = PathScope::new("t", &["src/**".to_string()], None).unwrap();
        let tool = tool(&dir, Some(scope));

        let allowed = run(&tool, json!({"path": "src/a.rs", "content": "x"})).await;
        let denied = run(&tool, json!({"path": "docs/a.md", "content": "x"})).await;

        assert_eq!(allowed["ok"], true);
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["error_code"], "out_of_scope");
    }

    #[tokio::test]
    async fn a_path_outside_the_workspace_is_refused() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "../escape.txt", "content": "x"}),
        )
        .await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "out_of_scope");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hardlinked_file_is_not_written_through() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "shared").unwrap();
        std::fs::hard_link(dir.path().join("a.txt"), dir.path().join("b.txt")).unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "b.txt", "content": "x", "overwrite": true}),
        )
        .await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "denied");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "shared"
        );
    }

    #[tokio::test]
    async fn an_unknown_argument_is_rejected_by_name() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "a", "content": "x", "force": true}),
        )
        .await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("force"));
    }

    #[test]
    fn the_effect_class_is_workspace() {
        let tool = Typed(WriteFile::new(
            std::path::PathBuf::from("."),
            None,
            ProtectedPaths::for_repo(std::path::Path::new(".")),
        ));

        assert_eq!(tool.effect_class(), crate::effects::EffectClass::Workspace);
    }
}
