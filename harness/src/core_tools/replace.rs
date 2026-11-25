use super::{error_outcome, io_outcome, read_text, too_large, MAX_FILE_BYTES};
use crate::effects::EffectClass;
use crate::protected::ProtectedPaths;
use crate::scope::PathScope;
use crate::tool_contract::{ErrorCode, ToolOutcome, TypedTool};
use crate::tools::{line_list, reject_hardlinked, resolve_write_target, to_crlf, uses_crlf};
use async_trait::async_trait;
use regex::Regex;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const NEAR_MATCH_LINE_CHARS: usize = 80;
const NEAR_MATCH_MAX_LINES: usize = 5;
const GUTTER_HINT: &str = "old looks like it was copied with line numbers; copy the text from read_file's content, without line numbers";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceArgs {
    #[schemars(description = "Path of an existing file, relative to the workspace root")]
    pub path: String,
    #[schemars(description = "Exact text to replace, copied from read_file including indentation")]
    pub old: String,
    #[schemars(description = "Text to put in its place")]
    pub new: String,
    #[schemars(
        description = "1-based line on which the occurrence to replace starts; needed only when old occurs more than once"
    )]
    pub line: Option<u32>,
}

pub struct ReplaceTool {
    root: PathBuf,
    scope: Option<PathScope>,
    protected: ProtectedPaths,
}

impl ReplaceTool {
    pub fn new(root: PathBuf, scope: Option<PathScope>, protected: ProtectedPaths) -> Self {
        Self {
            root,
            scope,
            protected,
        }
    }
}

fn start_line(content: &str, byte: usize) -> usize {
    content[..byte].matches('\n').count() + 1
}

fn looks_like_gutter(old: &str) -> bool {
    match Regex::new(r"^\s*\d+\s{2}") {
        Ok(pattern) => old.lines().next().is_some_and(|l| pattern.is_match(l)),
        Err(_) => false,
    }
}

fn no_match(path: &str, content: &str, old: &str) -> ToolOutcome {
    let mut message = format!(
        "old matched 0 times in {path}. Call read_file on this path and copy the text exactly, including indentation and blank lines."
    );
    let first = old
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if !first.is_empty() {
        let shown: String = first.chars().take(NEAR_MATCH_LINE_CHARS).collect();
        let hits: Vec<usize> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(first))
            .map(|(index, _)| index + 1)
            .take(NEAR_MATCH_MAX_LINES)
            .collect();
        if hits.is_empty() {
            message.push_str(&format!(
                " The first line of old `{shown}` does not appear in the file."
            ));
        } else {
            message.push_str(&format!(
                " The first line of old `{shown}` appears at {}; only the rest or the indentation differs.",
                line_list(&hits)
            ));
        }
    }
    let outcome = ToolOutcome::failure(ErrorCode::NoMatch, message);
    if looks_like_gutter(old) {
        outcome.with_hint(GUTTER_HINT)
    } else {
        outcome
    }
}

#[async_trait]
impl TypedTool for ReplaceTool {
    type Args = ReplaceArgs;

    fn name(&self) -> &str {
        "replace"
    }

    fn description(&self) -> &str {
        "Replace one exact piece of text in an existing file. Copy old from read_file's content without line numbers. If old occurs more than once, pass line, the 1-based line where the occurrence to change starts."
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Workspace
    }

    async fn run(&self, args: ReplaceArgs) -> ToolOutcome {
        if args.old.is_empty() {
            return ToolOutcome::failure(ErrorCode::InvalidArgs, "old is empty").with_hint(
                "copy the exact text to replace from read_file; to create a file use write_file",
            );
        }
        if args.old == args.new {
            return ToolOutcome::failure(
                ErrorCode::InvalidArgs,
                "old and new are identical, so nothing would change",
            )
            .with_hint("put the corrected text in new");
        }
        let path = Path::new(&args.path);
        let safe = match resolve_write_target(
            self.scope.as_ref(),
            &self.protected,
            "replace",
            path,
            &self.root,
        ) {
            Ok(safe) => safe,
            Err(error) => return error_outcome(error, &self.root, &args.path),
        };
        match std::fs::metadata(&safe) {
            Ok(metadata) if metadata.is_file() => {
                if metadata.len() > MAX_FILE_BYTES {
                    return too_large(&args.path);
                }
            }
            Ok(_) => {
                return ToolOutcome::failure(
                    ErrorCode::InvalidArgs,
                    format!("{} is not a file", args.path),
                )
                .with_hint("use list_directory to see its entries")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ToolOutcome::failure(
                    ErrorCode::NotFound,
                    format!("{} does not exist", args.path),
                )
                .with_hint("use write_file to create a new file")
            }
            Err(error) => return io_outcome(&error, &args.path),
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
        let content = match read_text(&mut file, &args.path) {
            Ok(content) => content,
            Err(outcome) => return outcome,
        };
        let crlf = uses_crlf(&content);
        let (old, new) = if crlf {
            (to_crlf(&args.old), to_crlf(&args.new))
        } else {
            (args.old.clone(), args.new.clone())
        };
        let positions: Vec<usize> = content
            .match_indices(old.as_str())
            .map(|(index, _)| index)
            .collect();
        if positions.is_empty() {
            return no_match(&args.path, &content, &args.old);
        }
        let lines: Vec<usize> = positions
            .iter()
            .map(|position| start_line(&content, *position))
            .collect();
        let chosen = match args.line {
            None if positions.len() == 1 => 0,
            None => {
                return ToolOutcome::failure(
                    ErrorCode::AmbiguousMatch,
                    format!(
                        "old matched {} times in {} (starting at {}). It must match exactly once.",
                        positions.len(),
                        args.path,
                        line_list(&lines)
                    ),
                )
                .with_hint("pass line set to the start line of the occurrence to replace, or add surrounding text to old so it is unique")
            }
            Some(line) => {
                let wanted = line as usize;
                let candidates: Vec<usize> = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, start)| **start == wanted)
                    .map(|(index, _)| index)
                    .collect();
                match candidates.as_slice() {
                    [only] => *only,
                    [] => {
                        return ToolOutcome::failure(
                            ErrorCode::NoMatch,
                            format!(
                                "no occurrence of old starts on line {wanted} of {}; occurrences start at {}",
                                args.path,
                                line_list(&lines)
                            ),
                        )
                        .with_hint("set line to one of the listed start lines")
                    }
                    _ => {
                        return ToolOutcome::failure(
                            ErrorCode::AmbiguousMatch,
                            format!(
                                "{} occurrences of old start on line {wanted} of {}",
                                candidates.len(),
                                args.path
                            ),
                        )
                        .with_hint("add surrounding text to old so it is unique")
                    }
                }
            }
        };
        let position = positions[chosen];
        let mut updated = String::with_capacity(content.len() + new.len());
        updated.push_str(&content[..position]);
        updated.push_str(&new);
        updated.push_str(&content[position + old.len()..]);
        if updated.len() as u64 > MAX_FILE_BYTES {
            return too_large(&args.path);
        }
        let written = file
            .seek(SeekFrom::Start(0))
            .and_then(|_| file.set_len(0))
            .and_then(|_| file.write_all(updated.as_bytes()));
        if let Err(error) = written {
            return io_outcome(&error, &args.path);
        }
        ToolOutcome::success(json!({
            "path": args.path,
            "replaced_at_line": lines[chosen],
            "new_total_lines": updated.lines().count(),
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

    fn tool(dir: &TempDir, scope: Option<PathScope>) -> Typed<ReplaceTool> {
        Typed(ReplaceTool::new(
            dir.path().to_path_buf(),
            scope,
            ProtectedPaths::for_repo(dir.path()),
        ))
    }

    async fn run(tool: &Typed<ReplaceTool>, args: Value) -> Value {
        tool.execute(args).await.unwrap()
    }

    fn put(dir: &TempDir, name: &str, content: &str) {
        std::fs::write(dir.path().join(name), content).unwrap();
    }

    fn get(dir: &TempDir, name: &str) -> String {
        std::fs::read_to_string(dir.path().join(name)).unwrap()
    }

    #[tokio::test]
    async fn replaces_a_unique_occurrence_and_reports_its_line() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.rs", "old": "fn b() {}", "new": "fn b() {\n    1;\n}"}),
        )
        .await;

        assert_eq!(result["ok"], true);
        assert_eq!(result["data"]["replaced_at_line"], 2);
        assert_eq!(result["data"]["new_total_lines"], 5);
        assert_eq!(result["data"]["path"], "a.rs");
        assert_eq!(
            get(&dir, "a.rs"),
            "fn a() {}\nfn b() {\n    1;\n}\nfn c() {}\n"
        );
    }

    #[tokio::test]
    async fn an_empty_old_is_invalid_args() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "x");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "", "new": "y"}),
        )
        .await;

        assert_eq!(result["error_code"], "invalid_args");
        assert_eq!(get(&dir, "a.txt"), "x");
    }

    #[tokio::test]
    async fn identical_old_and_new_is_invalid_args() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "x");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "x", "new": "x"}),
        )
        .await;

        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn an_absent_old_is_no_match_with_a_near_match_hint() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "alpha\n    beta\ngamma\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "beta\nDELTA", "new": "y"}),
        )
        .await;

        assert_eq!(result["error_code"], "no_match");
        let message = result["message"].as_str().unwrap();
        assert!(message.contains("0 times"));
        assert!(message.contains("`beta`"));
        assert!(message.contains("line 2"));
    }

    #[tokio::test]
    async fn an_old_that_looks_like_a_copied_gutter_gets_a_gutter_hint() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "let x = 1;\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "     1  let x = 1;", "new": "let x = 2;"}),
        )
        .await;

        assert_eq!(result["error_code"], "no_match");
        assert!(result["hint"]
            .as_str()
            .unwrap()
            .contains("copy the text from read_file's content, without line numbers"));
    }

    #[tokio::test]
    async fn an_ordinary_no_match_has_no_gutter_hint() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "let x = 1;\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "let y = 1;", "new": "z"}),
        )
        .await;

        assert_eq!(result["error_code"], "no_match");
        let hint = result["hint"].as_str().unwrap_or("");
        assert!(!hint.contains("without line numbers"));
    }

    #[tokio::test]
    async fn multiple_occurrences_without_line_are_ambiguous_and_list_every_line() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "x\nfoo\nx\nbar\nx\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "x", "new": "y"}),
        )
        .await;

        assert_eq!(result["error_code"], "ambiguous_match");
        let message = result["message"].as_str().unwrap();
        assert!(message.contains("line 1"));
        assert!(message.contains("line 3"));
        assert!(message.contains("line 5"));
        assert_eq!(get(&dir, "a.txt"), "x\nfoo\nx\nbar\nx\n");
    }

    #[tokio::test]
    async fn line_selects_the_occurrence_starting_on_that_line() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "x\nfoo\nx\nbar\nx\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "x", "new": "y", "line": 3}),
        )
        .await;

        assert_eq!(result["ok"], true);
        assert_eq!(result["data"]["replaced_at_line"], 3);
        assert_eq!(get(&dir, "a.txt"), "x\nfoo\ny\nbar\nx\n");
    }

    #[tokio::test]
    async fn line_selects_a_multi_line_occurrence_by_its_start_line() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "a\nb\nc\na\nb\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "a\nb", "new": "Z", "line": 4}),
        )
        .await;

        assert_eq!(result["ok"], true);
        assert_eq!(get(&dir, "a.txt"), "a\nb\nc\nZ\n");
    }

    #[tokio::test]
    async fn line_with_no_occurrence_starting_there_is_no_match_listing_start_lines() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "x\nfoo\nx\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "x", "new": "y", "line": 2}),
        )
        .await;

        assert_eq!(result["error_code"], "no_match");
        let message = result["message"].as_str().unwrap();
        assert!(message.contains("line 1"));
        assert!(message.contains("line 3"));
        assert_eq!(get(&dir, "a.txt"), "x\nfoo\nx\n");
    }

    #[tokio::test]
    async fn line_on_a_unique_occurrence_elsewhere_is_still_no_match() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "x\nfoo\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "x", "new": "y", "line": 2}),
        )
        .await;

        assert_eq!(result["error_code"], "no_match");
        assert!(result["message"].as_str().unwrap().contains("line 1"));
    }

    #[tokio::test]
    async fn two_occurrences_starting_on_the_same_line_stay_ambiguous() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "x x\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "x", "new": "y", "line": 1}),
        )
        .await;

        assert_eq!(result["error_code"], "ambiguous_match");
    }

    #[tokio::test]
    async fn crlf_files_stay_crlf() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "one\r\ntwo\r\nthree\r\n");

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "one\ntwo", "new": "1\n2\n2b"}),
        )
        .await;

        assert_eq!(result["ok"], true);
        assert_eq!(get(&dir, "a.txt"), "1\r\n2\r\n2b\r\nthree\r\n");
    }

    #[tokio::test]
    async fn a_file_over_the_size_cap_is_refused() {
        let dir = TempDir::new().unwrap();
        let big = "a".repeat(8 * 1024 * 1024 + 1);
        put(&dir, "big.txt", &big);

        let result = run(
            &tool(&dir, None),
            json!({"path": "big.txt", "old": "a", "new": "b"}),
        )
        .await;

        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("8 MiB"));
    }

    #[tokio::test]
    async fn a_replacement_that_grows_past_the_cap_is_refused_and_leaves_the_file() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "x");
        let huge = "y".repeat(8 * 1024 * 1024 + 1);

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "old": "x", "new": huge}),
        )
        .await;

        assert_eq!(result["error_code"], "invalid_args");
        assert_eq!(get(&dir, "a.txt"), "x");
    }

    #[tokio::test]
    async fn a_missing_file_is_not_found_pointing_at_write_file() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "nope.txt", "old": "a", "new": "b"}),
        )
        .await;

        assert_eq!(result["error_code"], "not_found");
        assert!(result["hint"].as_str().unwrap().contains("write_file"));
    }

    #[tokio::test]
    async fn a_directory_is_invalid_args() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "sub", "old": "a", "new": "b"}),
        )
        .await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn protected_paths_are_denied() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("flake.nix"), "a").unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "flake.nix", "old": "a", "new": "b"}),
        )
        .await;

        assert_eq!(result["error_code"], "denied");
        assert_eq!(get(&dir, "flake.nix"), "a");
    }

    #[tokio::test]
    async fn write_scope_denies_paths_outside_scope_paths() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "a");
        let scope = PathScope::new("t", &["src/**".to_string()], None).unwrap();

        let result = run(
            &tool(&dir, Some(scope)),
            json!({"path": "a.txt", "old": "a", "new": "b"}),
        )
        .await;

        assert_eq!(result["error_code"], "out_of_scope");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hardlinked_file_is_not_edited() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "a");
        std::fs::hard_link(dir.path().join("a.txt"), dir.path().join("b.txt")).unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "b.txt", "old": "a", "new": "b"}),
        )
        .await;

        assert_eq!(result["error_code"], "denied");
        assert_eq!(get(&dir, "a.txt"), "a");
    }

    #[tokio::test]
    async fn binary_content_is_invalid_args() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("b.bin"), [0xff, 0x00, 0x01]).unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "b.bin", "old": "a", "new": "b"}),
        )
        .await;

        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn an_unknown_argument_is_rejected_by_name() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "a", "old": "a", "new": "b", "replace_all": true}),
        )
        .await;

        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("replace_all"));
    }

    #[test]
    fn the_effect_class_is_workspace() {
        let tool = Typed(ReplaceTool::new(
            std::path::PathBuf::from("."),
            None,
            ProtectedPaths::for_repo(std::path::Path::new(".")),
        ));

        assert_eq!(tool.effect_class(), crate::effects::EffectClass::Workspace);
    }
}
