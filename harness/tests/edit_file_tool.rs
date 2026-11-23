use harness::identity::AgentIdentity;
use harness::scope::{DenialReason, PathAccess};
use harness::tools::{create_tool_registry_for, Tool, ToolError, ToolRegistry};
use harness::EditFileTool;
use serde_json::{json, Value};
use std::path::Path;

fn identity(tools: &[&str], paths: &[&str]) -> AgentIdentity {
    let quote = |items: &[&str]| {
        items
            .iter()
            .map(|i| format!("\"{i}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let toml = format!(
        r#"
[identity]
name = "editor"
description = "Edits code."
loop = "inner"
model = "gemma4:e4b"
system_prompt = {{ inline = "Edit." }}

[scope]
repos = []
paths = [{paths}]
max_effect = "workspace"
tools = [{tools}]

[limits]
max_iterations = 10
max_wall_clock_secs = 60
max_concurrent = 1
"#,
        paths = quote(paths),
        tools = quote(tools),
    );
    AgentIdentity::from_toml_str(&toml, "editor.toml").unwrap()
}

fn registry(root: &Path, tools: &[&str], paths: &[&str]) -> ToolRegistry {
    create_tool_registry_for(root, &identity(tools, paths)).unwrap()
}

fn names(registry: &ToolRegistry) -> Vec<String> {
    let mut names: Vec<String> = registry
        .list_tools()
        .iter()
        .map(|s| s.to_string())
        .collect();
    names.sort();
    names
}

fn tool(root: &Path) -> EditFileTool {
    EditFileTool::new(root.to_path_buf())
}

async fn edit(root: &Path, args: Value) -> Result<Value, ToolError> {
    tool(root).execute(args).await
}

fn message(err: &ToolError) -> String {
    err.to_string()
}

const SOURCE: &str = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";

fn workspace_with(name: &str, content: &[u8]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join(name);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(target, content).unwrap();
    dir
}

#[tokio::test]
async fn a_unique_match_is_replaced_and_everything_else_is_byte_identical() {
    let dir = workspace_with("src/main.rs", SOURCE.as_bytes());
    let result = edit(
        dir.path(),
        json!({"path": "src/main.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"}),
    )
    .await
    .unwrap();
    assert_eq!(result["path"], "src/main.rs");
    assert_eq!(result["replacements"], 1);
    assert_eq!(result["success"], true);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        SOURCE.replace("let x = 1;", "let x = 2;")
    );
}

#[tokio::test]
async fn the_result_shows_three_lines_before_and_after_the_first_change() {
    let dir = workspace_with("src/main.rs", SOURCE.as_bytes());
    let result = edit(
        dir.path(),
        json!({"path": "src/main.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"}),
    )
    .await
    .unwrap();
    let before = result["before"].as_str().unwrap();
    let after = result["after"].as_str().unwrap();
    assert_eq!(before.lines().count(), 3);
    assert_eq!(after.lines().count(), 3);
    assert!(before.contains("let x = 1;") && !before.contains("let x = 2;"));
    assert!(after.contains("let x = 2;") && !after.contains("let x = 1;"));
    assert!(before.contains("fn main()") && after.contains("fn main()"));
}

#[tokio::test]
async fn a_multi_line_replacement_keeps_the_surrounding_text() {
    let dir = workspace_with("a.txt", b"one\ntwo\nthree\nfour\n");
    edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "two\nthree", "new_string": "2\n2.5\n3"}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "one\n2\n2.5\n3\nfour\n"
    );
}

#[tokio::test]
async fn deleting_text_with_an_empty_new_string_is_allowed() {
    let dir = workspace_with("a.txt", b"keep\ndrop me\nkeep\n");
    edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "drop me\n", "new_string": ""}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "keep\nkeep\n"
    );
}

#[tokio::test]
async fn crlf_files_keep_their_line_endings_whichever_spelling_the_model_uses() {
    for old in ["two\r\nthree", "two\nthree"] {
        let dir = workspace_with("a.txt", b"one\r\ntwo\r\nthree\r\nfour\r\n");
        edit(
            dir.path(),
            json!({"path": "a.txt", "old_string": old, "new_string": "2\n3"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).unwrap(),
            b"one\r\n2\r\n3\r\nfour\r\n",
            "{old:?}"
        );
    }
}

#[tokio::test]
async fn text_without_a_trailing_newline_and_non_ascii_is_preserved() {
    let dir = workspace_with("a.txt", "caf\u{e9} = 1\nz".as_bytes());
    edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "= 1", "new_string": "= 2"}),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).unwrap(),
        "caf\u{e9} = 2\nz".as_bytes()
    );
}

#[tokio::test]
async fn no_match_is_an_error_naming_the_count_the_remedy_and_where_the_first_line_is() {
    let dir = workspace_with("a.txt", b"alpha\n    let value = 1;\nomega\n");
    let err = edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "let value = 1;\nextra line", "new_string": "x"}),
    )
    .await
    .unwrap_err();
    let text = message(&err);
    assert!(text.contains("0 times"), "{text}");
    assert!(text.contains("read_file"), "{text}");
    assert!(text.contains("indentation"), "{text}");
    assert!(text.contains("let value = 1;"), "{text}");
    assert!(text.contains("line 2"), "{text}");
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).unwrap(),
        b"alpha\n    let value = 1;\nomega\n"
    );
}

#[tokio::test]
async fn the_no_match_hint_stays_bounded() {
    let body = "needle\n".repeat(500);
    let dir = workspace_with("a.txt", body.as_bytes());
    let long_first_line = "z".repeat(5000);
    let err = edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": format!("{long_first_line}\nneedle"), "new_string": "x"}),
    )
    .await
    .unwrap_err();
    assert!(message(&err).len() < 1500, "{}", message(&err).len());
    let err = edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "needle\nabsent", "new_string": "x"}),
    )
    .await
    .unwrap_err();
    assert!(message(&err).len() < 1500, "{}", message(&err).len());
}

#[tokio::test]
async fn multiple_matches_are_an_error_with_the_count_and_the_file_is_untouched() {
    let dir = workspace_with("a.txt", b"x = 1\ny = 1\nz = 1\n");
    let err = edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "= 1", "new_string": "= 2"}),
    )
    .await
    .unwrap_err();
    let text = message(&err);
    assert!(text.contains("3 times"), "{text}");
    assert!(text.contains("read_file"), "{text}");
    assert!(text.contains("replace_all"), "{text}");
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).unwrap(),
        b"x = 1\ny = 1\nz = 1\n"
    );
}

#[tokio::test]
async fn replace_all_changes_every_match_and_reports_the_count() {
    let dir = workspace_with("a.txt", b"x = 1\ny = 1\nz = 1\n");
    let result = edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "= 1", "new_string": "= 2", "replace_all": true}),
    )
    .await
    .unwrap();
    assert_eq!(result["replacements"], 3);
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).unwrap(),
        b"x = 2\ny = 2\nz = 2\n"
    );
}

#[tokio::test]
async fn replace_all_false_behaves_like_the_default() {
    let dir = workspace_with("a.txt", b"a a\n");
    let err = edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "a", "new_string": "b", "replace_all": false}),
    )
    .await
    .unwrap_err();
    assert!(message(&err).contains("2 times"));
}

#[tokio::test]
async fn an_empty_or_unchanged_old_string_is_refused() {
    let dir = workspace_with("a.txt", b"abc\n");
    let empty = edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "", "new_string": "x"}),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(empty, ToolError::InvalidArguments { .. }),
        "{empty:?}"
    );
    assert!(message(&empty).contains("old_string"));
    let same = edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "abc", "new_string": "abc"}),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(same, ToolError::InvalidArguments { .. }),
        "{same:?}"
    );
    assert!(message(&same).contains("identical"));
    assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"abc\n");
}

#[tokio::test]
async fn missing_arguments_are_invalid() {
    let dir = workspace_with("a.txt", b"abc\n");
    for args in [
        json!({"old_string": "a", "new_string": "b"}),
        json!({"path": "a.txt", "new_string": "b"}),
        json!({"path": "a.txt", "old_string": "a"}),
        json!({"path": "a.txt", "old_string": "a", "new_string": "b", "replace_all": "yes"}),
    ] {
        let err = edit(dir.path(), args.clone()).await.unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArguments { .. }),
            "{args}: {err:?}"
        );
    }
}

#[tokio::test]
async fn a_missing_file_is_refused_and_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let err = edit(
        dir.path(),
        json!({"path": "new.rs", "old_string": "a", "new_string": "b"}),
    )
    .await
    .unwrap_err();
    let text = message(&err);
    assert!(text.contains("does not exist"), "{text}");
    assert!(text.contains("write_file"), "{text}");
    assert!(!dir.path().join("new.rs").exists());
}

#[tokio::test]
async fn a_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let err = edit(
        dir.path(),
        json!({"path": "sub", "old_string": "a", "new_string": "b"}),
    )
    .await
    .unwrap_err();
    assert!(message(&err).contains("not a file"), "{}", message(&err));
}

#[tokio::test]
async fn binary_files_are_refused_and_untouched() {
    for bytes in [vec![0xff, 0xfe, b'a', b'b'], b"ab\0cd".to_vec()] {
        let dir = workspace_with("blob.bin", &bytes);
        let err = edit(
            dir.path(),
            json!({"path": "blob.bin", "old_string": "ab", "new_string": "xy"}),
        )
        .await
        .unwrap_err();
        assert!(message(&err).contains("binary"), "{}", message(&err));
        assert_eq!(std::fs::read(dir.path().join("blob.bin")).unwrap(), bytes);
    }
}

#[tokio::test]
async fn an_oversized_file_is_refused() {
    let dir = workspace_with("big.txt", &vec![b'a'; 9 * 1024 * 1024]);
    let err = edit(
        dir.path(),
        json!({"path": "big.txt", "old_string": "a", "new_string": "b"}),
    )
    .await
    .unwrap_err();
    assert!(message(&err).contains("too large"), "{}", message(&err));
}

#[tokio::test]
async fn a_result_that_would_exceed_the_size_cap_is_refused() {
    let dir = workspace_with("a.txt", b"a\n");
    let err = edit(
        dir.path(),
        json!({"path": "a.txt", "old_string": "a", "new_string": "b".repeat(9 * 1024 * 1024)}),
    )
    .await
    .unwrap_err();
    assert!(message(&err).contains("too large"), "{}", message(&err));
    assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"a\n");
}

#[tokio::test]
async fn paths_outside_scope_paths_are_denied_like_write_file() {
    let dir = workspace_with("api/a.rs", b"old\n");
    std::fs::write(dir.path().join("Cargo.toml"), "old\n").unwrap();
    let registry = registry(dir.path(), &["write_file", "read_file"], &["api/**"]);
    registry
        .execute(
            "edit_file",
            json!({"path": "api/a.rs", "old_string": "old", "new_string": "new"}),
        )
        .await
        .unwrap();
    let err = registry
        .execute(
            "edit_file",
            json!({"path": "Cargo.toml", "old_string": "old", "new_string": "new"}),
        )
        .await
        .unwrap_err();
    match err {
        ToolError::ScopeDenied(denial) => {
            assert_eq!(denial.tool, "edit_file");
            assert_eq!(
                denial.reason,
                DenialReason::PathOutsideScope {
                    access: PathAccess::Write,
                    path: "Cargo.toml".to_string()
                }
            );
        }
        other => panic!("expected ScopeDenied, got {other:?}"),
    }
    assert_eq!(registry.denial_count(), 1);
    assert_eq!(
        std::fs::read(dir.path().join("Cargo.toml")).unwrap(),
        b"old\n"
    );
}

#[tokio::test]
async fn protected_paths_are_refused_like_write_file() {
    let dir = workspace_with(".nanna/agents/x.toml", b"old\n");
    for path in [".nanna/agents/x.toml", ".nanna/effects.toml"] {
        std::fs::write(dir.path().join(path), "old\n").unwrap();
        let err = edit(
            dir.path(),
            json!({"path": path, "old_string": "old", "new_string": "new"}),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ToolError::ProtectedPath(_)),
            "{path}: {err:?}"
        );
        assert_eq!(std::fs::read(dir.path().join(path)).unwrap(), b"old\n");
    }
}

#[tokio::test]
async fn traversal_and_absolute_escapes_are_refused() {
    let outer = tempfile::tempdir().unwrap();
    let inner = outer.path().join("ws");
    std::fs::create_dir(&inner).unwrap();
    std::fs::write(outer.path().join("secret.txt"), "old\n").unwrap();
    for path in [
        "../secret.txt".to_string(),
        outer.path().join("secret.txt").display().to_string(),
    ] {
        let err = edit(
            &inner,
            json!({"path": path, "old_string": "old", "new_string": "new"}),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ToolError::PathSecurityViolation { .. }),
            "{path}: {err:?}"
        );
    }
    assert_eq!(
        std::fs::read(outer.path().join("secret.txt")).unwrap(),
        b"old\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_out_of_the_workspace_is_refused() {
    let outer = tempfile::tempdir().unwrap();
    let inner = outer.path().join("ws");
    std::fs::create_dir(&inner).unwrap();
    std::fs::write(outer.path().join("secret.txt"), "old\n").unwrap();
    std::os::unix::fs::symlink(outer.path().join("secret.txt"), inner.join("link.txt")).unwrap();
    std::os::unix::fs::symlink(outer.path(), inner.join("dir")).unwrap();
    for path in ["link.txt", "dir/secret.txt"] {
        let err = edit(
            &inner,
            json!({"path": path, "old_string": "old", "new_string": "new"}),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ToolError::PathSecurityViolation { .. }),
            "{path}: {err:?}"
        );
    }
    assert_eq!(
        std::fs::read(outer.path().join("secret.txt")).unwrap(),
        b"old\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_hardlinked_file_is_refused_and_both_names_keep_their_content() {
    let dir = workspace_with("a.txt", b"shared\n");
    std::fs::hard_link(dir.path().join("a.txt"), dir.path().join("b.txt")).unwrap();
    let err = edit(
        dir.path(),
        json!({"path": "b.txt", "old_string": "shared", "new_string": "changed"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::ProtectedPath(_)), "{err:?}");
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).unwrap(),
        b"shared\n"
    );
}

#[test]
fn the_tool_is_workspace_class_with_the_documented_schema() {
    let t = tool(Path::new("."));
    assert_eq!(t.name(), "edit_file");
    assert_eq!(t.effect_class(), harness::effects::EffectClass::Workspace);
    let def = serde_json::to_value(t.definition()).unwrap();
    let function = &def["function"];
    assert_eq!(function["name"], "edit_file");
    let description = function["description"].as_str().unwrap();
    assert!(description.contains("read_file") && description.contains("write_file"));
    assert!(description.len() < 400);
    let props = function["parameters"]["properties"].as_object().unwrap();
    for key in ["path", "old_string", "new_string", "replace_all"] {
        assert!(props.contains_key(key), "{key}");
    }
    assert_eq!(
        function["parameters"]["required"],
        json!(["path", "old_string", "new_string"])
    );
}

#[test]
fn the_default_registry_holds_edit_file_beside_write_file() {
    let registry = harness::tools::create_tool_registry(Path::new("."));
    assert!(registry.get_tool("edit_file").is_some());
    assert_eq!(
        registry.effect_class_of("edit_file"),
        registry.effect_class_of("write_file")
    );
}

#[test]
fn a_card_granting_write_file_also_gets_edit_file() {
    let dir = tempfile::tempdir().unwrap();
    let scoped = registry(dir.path(), &["read_file", "write_file"], &["**"]);
    assert_eq!(names(&scoped), vec!["edit_file", "read_file", "write_file"]);
}

#[test]
fn a_card_without_write_file_never_gets_edit_file() {
    let dir = tempfile::tempdir().unwrap();
    let read_only = registry(dir.path(), &["read_file", "search"], &["**"]);
    assert_eq!(names(&read_only), vec!["read_file", "search"]);
    let only_edit = registry(dir.path(), &["edit_file", "read_file"], &["**"]);
    assert_eq!(names(&only_edit), vec!["read_file"]);
}

#[test]
fn edit_file_is_dropped_above_the_effect_ceiling_like_write_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut card = identity(&["read_file", "write_file"], &["**"]);
    card.scope.max_effect = harness::effects::EffectClass::None;
    let scoped = create_tool_registry_for(dir.path(), &card).unwrap();
    assert_eq!(names(&scoped), vec!["read_file"]);
}

#[tokio::test]
async fn the_scoped_registry_edits_through_the_same_scope_as_write_file() {
    let dir = workspace_with("src/a.rs", b"old\n");
    let scoped = registry(dir.path(), &["write_file"], &["src/**"]);
    let ok = scoped
        .execute(
            "edit_file",
            json!({"path": "src/a.rs", "old_string": "old", "new_string": "new"}),
        )
        .await
        .unwrap();
    assert_eq!(ok["replacements"], 1);
    assert_eq!(
        std::fs::read(dir.path().join("src/a.rs")).unwrap(),
        b"new\n"
    );
}
