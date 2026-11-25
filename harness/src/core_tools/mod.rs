use crate::protected::ProtectedPaths;
use crate::scope::PathScope;
use crate::tool_contract::{ErrorCode, FinishSlot, ToolOutcome, Typed};
use crate::tools::{Tool, ToolError};
use std::path::{Component, Path};

pub mod ast_grep;
pub mod finish;
pub mod grep;
pub mod list;
pub mod read;
pub mod replace;
pub mod write;

pub(crate) const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

const CORE_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "replace",
    "grep",
    "list_directory",
    "ast_grep",
    "finish",
];

pub fn core_tool_names() -> &'static [&'static str] {
    CORE_TOOL_NAMES
}

pub fn build_core_tool(
    name: &str,
    root: &Path,
    scope: Option<PathScope>,
    protected: ProtectedPaths,
    finish: FinishSlot,
) -> Option<Box<dyn Tool>> {
    let root = root.to_path_buf();
    let tool: Box<dyn Tool> = match name {
        "read_file" => Box::new(Typed(read::ReadFileTool::new(root, scope))),
        "write_file" => Box::new(Typed(write::WriteFile::new(root, scope, protected))),
        "replace" => Box::new(Typed(replace::ReplaceTool::new(root, scope, protected))),
        "grep" => Box::new(Typed(grep::GrepTool::new(root, scope))),
        "list_directory" => Box::new(Typed(list::ListDirectoryTool::new(root, scope))),
        "ast_grep" => Box::new(Typed(ast_grep::AstGrepTool::new(root, scope))),
        "finish" => Box::new(Typed(finish::FinishTool::new(finish))),
        _ => return None,
    };
    Some(tool)
}

pub(crate) fn error_outcome(error: ToolError, root: &Path, path: &str) -> ToolOutcome {
    let candidate = Path::new(path);
    let escapes = candidate.is_absolute()
        || candidate
            .components()
            .any(|part| matches!(part, Component::ParentDir));
    if matches!(error, ToolError::PathSecurityViolation { .. })
        && !escapes
        && !root.join(candidate).exists()
    {
        return ToolOutcome::failure(ErrorCode::NotFound, format!("{path} does not exist"))
            .with_hint("use list_directory to see what exists");
    }
    ToolOutcome::from(&error)
}

pub(crate) fn io_outcome(error: &std::io::Error, path: &str) -> ToolOutcome {
    ToolOutcome::failure(ErrorCode::Io, format!("{path}: {error}"))
}

pub(crate) fn read_text(file: &mut std::fs::File, path: &str) -> Result<String, ToolOutcome> {
    let mut bytes = Vec::new();
    if let Err(error) = std::io::Read::read_to_end(file, &mut bytes) {
        return Err(io_outcome(&error, path));
    }
    match String::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => Ok(text),
        _ => Err(ToolOutcome::failure(
            ErrorCode::InvalidArgs,
            format!("{path} is a binary file or not valid UTF-8 text"),
        )),
    }
}

pub(crate) fn too_large(path: &str) -> ToolOutcome {
    ToolOutcome::failure(
        ErrorCode::InvalidArgs,
        format!(
            "{path} is too large (limit {} MiB)",
            MAX_FILE_BYTES / (1024 * 1024)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_contract::FinishSlot;
    use serde_json::json;

    fn build(name: &str) -> Option<Box<dyn Tool>> {
        build_core_tool(
            name,
            Path::new("."),
            None,
            ProtectedPaths::for_repo(Path::new(".")),
            FinishSlot::default(),
        )
    }

    #[test]
    fn every_core_tool_name_builds_a_tool_with_that_name() {
        for name in core_tool_names() {
            let tool = build(name).unwrap_or_else(|| panic!("{name} did not build"));
            assert_eq!(tool.name(), *name);
            assert_eq!(tool.definition().function.name, *name);
        }
    }

    #[test]
    fn an_unknown_name_builds_nothing() {
        assert!(build("edit_file").is_none());
    }

    #[test]
    fn the_core_names_are_the_seven_contracted_tools() {
        assert_eq!(
            core_tool_names(),
            [
                "read_file",
                "write_file",
                "replace",
                "grep",
                "list_directory",
                "ast_grep",
                "finish"
            ]
        );
    }

    #[test]
    fn every_core_tool_publishes_a_strict_raw_schema() {
        for name in core_tool_names() {
            let schema = build(name)
                .unwrap()
                .definition()
                .function
                .raw_parameters
                .unwrap_or_else(|| panic!("{name} has no raw schema"));
            assert_eq!(schema["type"], "object", "{name}");
            assert_eq!(schema["additionalProperties"], json!(false), "{name}");
        }
    }

    #[test]
    fn mutating_tools_declare_the_workspace_effect_and_the_rest_none() {
        use crate::effects::EffectClass;
        for name in core_tool_names() {
            let expected = match *name {
                "write_file" | "replace" => EffectClass::Workspace,
                _ => EffectClass::None,
            };
            assert_eq!(build(name).unwrap().effect_class(), expected, "{name}");
        }
    }

    #[test]
    fn a_missing_relative_path_reads_as_not_found() {
        let dir = tempfile::TempDir::new().unwrap();

        let outcome = error_outcome(
            ToolError::PathSecurityViolation {
                message: "x".to_string(),
            },
            dir.path(),
            "gone.txt",
        );

        assert_eq!(outcome.error_code, Some(ErrorCode::NotFound));
    }

    #[test]
    fn a_missing_path_that_climbs_out_is_still_out_of_scope() {
        let dir = tempfile::TempDir::new().unwrap();

        let outcome = error_outcome(
            ToolError::PathSecurityViolation {
                message: "x".to_string(),
            },
            dir.path(),
            "../gone",
        );

        assert_eq!(outcome.error_code, Some(ErrorCode::OutOfScope));
    }
}
