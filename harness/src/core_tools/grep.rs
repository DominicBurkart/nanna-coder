use super::{error_outcome, MAX_FILE_BYTES};
use crate::effects::EffectClass;
use crate::scope::{canonical_root, relative_to, resolve_path, validate_path_within_workspace};
use crate::scope::{PathAccess, PathScope};
use crate::tool_contract::{ErrorCode, ToolOutcome, TypedTool};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_RESULTS: u32 = 50;
const MAX_LINE_CHARS: usize = 200;
const MAX_OUTPUT_CHARS: usize = 8_000;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrepArgs {
    #[schemars(description = "Regular expression matched against each line")]
    pub pattern: String,
    #[schemars(
        description = "File or directory to search, relative to the workspace root (default: the root)"
    )]
    pub path: Option<String>,
    #[schemars(description = "Glob matched against file names or relative paths, e.g. *.rs")]
    pub glob: Option<String>,
    #[schemars(description = "Maximum matching lines to return (default 50)")]
    pub max_results: Option<u32>,
}

pub struct GrepTool {
    root: PathBuf,
    scope: Option<PathScope>,
}

impl GrepTool {
    pub fn new(root: PathBuf, scope: Option<PathScope>) -> Self {
        Self { root, scope }
    }
}

struct Search<'a> {
    root: &'a Path,
    scope: Option<&'a PathScope>,
    regex: &'a regex::Regex,
    glob: Option<&'a glob::Pattern>,
    limit: usize,
    matches: Vec<String>,
    used: usize,
    overflowed: bool,
}

impl Search<'_> {
    fn full(&self) -> bool {
        self.overflowed || self.matches.len() > self.limit
    }

    fn readable(&self, relative: &Path) -> bool {
        self.scope
            .is_none_or(|scope| scope.permits(PathAccess::Read, relative))
    }

    fn globbed(&self, path: &Path, relative: &Path) -> bool {
        let Some(glob) = self.glob else {
            return true;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        glob.matches(&name) || glob.matches_path(relative)
    }

    fn visit_file(&mut self, path: &Path) {
        let relative = relative_to(path, self.root).to_path_buf();
        if !self.readable(&relative) || !self.globbed(path, &relative) {
            return;
        }
        let small = std::fs::metadata(path).is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES);
        if !small {
            return;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let shown = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        for (index, line) in content.lines().enumerate() {
            if self.full() {
                return;
            }
            if self.regex.is_match(line) {
                let entry = format!("{shown}:{}:{}", index + 1, cut(line));
                let length = entry.chars().count();
                if self.used + length > MAX_OUTPUT_CHARS {
                    self.overflowed = true;
                    return;
                }
                self.used += length;
                self.matches.push(entry);
            }
        }
    }

    fn visit_dir(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if self.full() {
                return;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if kind.is_dir() {
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                self.visit_dir(&entry.path());
            } else if kind.is_file() {
                self.visit_file(&entry.path());
            }
        }
    }
}

fn cut(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((byte, _)) => format!("{}…", &line[..byte]),
        None => line.to_string(),
    }
}

#[async_trait]
impl TypedTool for GrepTool {
    type Args = GrepArgs;

    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents with a regular expression. Returns lines as path:line:text, skipping dot directories, target and node_modules. Long lines are cut and output stops at 8000 characters with truncated true; narrow the pattern or path to see more."
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }

    async fn run(&self, args: GrepArgs) -> ToolOutcome {
        let regex = match regex::Regex::new(&args.pattern) {
            Ok(regex) => regex,
            Err(error) => {
                return ToolOutcome::failure(
                    ErrorCode::InvalidArgs,
                    format!("invalid regex: {error}"),
                )
            }
        };
        let glob = match args.glob.as_deref().map(glob::Pattern::new).transpose() {
            Ok(glob) => glob,
            Err(error) => {
                return ToolOutcome::failure(
                    ErrorCode::InvalidArgs,
                    format!("invalid glob: {error}"),
                )
            }
        };
        let limit = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS);
        if limit == 0 {
            return ToolOutcome::failure(ErrorCode::InvalidArgs, "max_results must be at least 1");
        }
        let path_arg = args.path.as_deref().unwrap_or(".");
        let target = match validate_path_within_workspace(Path::new(path_arg), &self.root) {
            Ok(target) => target,
            Err(error) => return error_outcome(error, &self.root, path_arg),
        };
        let root = match canonical_root(&self.root) {
            Ok(root) => root,
            Err(error) => return ToolOutcome::from(&error),
        };
        let mut search = Search {
            root: &root,
            scope: self.scope.as_ref(),
            regex: &regex,
            glob: glob.as_ref(),
            limit: limit as usize,
            matches: Vec::new(),
            used: 0,
            overflowed: false,
        };
        if target.is_dir() {
            search.visit_dir(&target);
        } else {
            let allowed = resolve_path(
                self.scope.as_ref(),
                "grep",
                PathAccess::Read,
                Path::new(path_arg),
                &self.root,
            );
            match allowed {
                Ok(file) => search.visit_file(&file),
                Err(error) => return error_outcome(error, &self.root, path_arg),
            }
        }
        let truncated = search.overflowed || search.matches.len() > search.limit;
        search.matches.truncate(search.limit);
        ToolOutcome::success(json!({
            "matches": search.matches,
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

    fn tool(dir: &TempDir, scope: Option<PathScope>) -> Typed<GrepTool> {
        Typed(GrepTool::new(dir.path().to_path_buf(), scope))
    }

    async fn run(tool: &Typed<GrepTool>, args: Value) -> Value {
        tool.execute(args).await.unwrap()
    }

    fn put(dir: &TempDir, name: &str, content: &str) {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn matches(result: &Value) -> Vec<String> {
        result["data"]["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn finds_matches_as_path_line_text() {
        let dir = TempDir::new().unwrap();
        put(&dir, "src/a.rs", "fn one() {}\nlet x = 1;\nfn two() {}\n");

        let result = run(&tool(&dir, None), json!({"pattern": r"fn \w+"})).await;

        assert_eq!(result["ok"], true);
        assert_eq!(
            matches(&result),
            ["src/a.rs:1:fn one() {}", "src/a.rs:3:fn two() {}"]
        );
        assert_eq!(result["data"]["truncated"], false);
    }

    #[tokio::test]
    async fn results_are_ordered_by_path_then_line() {
        let dir = TempDir::new().unwrap();
        put(&dir, "b.txt", "hit\n");
        put(&dir, "a.txt", "hit\nhit\n");

        let result = run(&tool(&dir, None), json!({"pattern": "hit"})).await;

        assert_eq!(
            matches(&result),
            ["a.txt:1:hit", "a.txt:2:hit", "b.txt:1:hit"]
        );
    }

    #[tokio::test]
    async fn a_path_restricts_the_search_to_that_subtree() {
        let dir = TempDir::new().unwrap();
        put(&dir, "src/a.rs", "hit\n");
        put(&dir, "docs/b.md", "hit\n");

        let result = run(&tool(&dir, None), json!({"pattern": "hit", "path": "docs"})).await;

        assert_eq!(matches(&result), ["docs/b.md:1:hit"]);
    }

    #[tokio::test]
    async fn a_path_may_name_a_single_file() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "hit\n");
        put(&dir, "b.txt", "hit\n");

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "hit", "path": "b.txt"}),
        )
        .await;

        assert_eq!(matches(&result), ["b.txt:1:hit"]);
    }

    #[tokio::test]
    async fn a_glob_filters_by_file_name() {
        let dir = TempDir::new().unwrap();
        put(&dir, "src/a.rs", "hit\n");
        put(&dir, "src/b.md", "hit\n");

        let result = run(&tool(&dir, None), json!({"pattern": "hit", "glob": "*.rs"})).await;

        assert_eq!(matches(&result), ["src/a.rs:1:hit"]);
    }

    #[tokio::test]
    async fn an_invalid_glob_is_invalid_args() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"pattern": "x", "glob": "[a"})).await;

        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn dot_directories_target_and_node_modules_are_skipped() {
        let dir = TempDir::new().unwrap();
        put(&dir, ".git/config", "hit\n");
        put(&dir, "target/debug/x.rs", "hit\n");
        put(&dir, "node_modules/p/i.js", "hit\n");
        put(&dir, "src/a.rs", "hit\n");

        let result = run(&tool(&dir, None), json!({"pattern": "hit"})).await;

        assert_eq!(matches(&result), ["src/a.rs:1:hit"]);
    }

    #[tokio::test]
    async fn only_files_the_read_scope_allows_are_searched() {
        let dir = TempDir::new().unwrap();
        put(&dir, "src/a.rs", "hit\n");
        put(&dir, "secret/b.rs", "hit\n");
        let scope =
            PathScope::new("t", &["src/**".to_string()], Some(&["src/**".to_string()])).unwrap();

        let result = run(&tool(&dir, Some(scope)), json!({"pattern": "hit"})).await;

        assert_eq!(matches(&result), ["src/a.rs:1:hit"]);
    }

    #[tokio::test]
    async fn naming_a_file_outside_the_read_scope_is_out_of_scope() {
        let dir = TempDir::new().unwrap();
        put(&dir, "secret/b.rs", "hit\n");
        let scope =
            PathScope::new("t", &["src/**".to_string()], Some(&["src/**".to_string()])).unwrap();

        let result = run(
            &tool(&dir, Some(scope)),
            json!({"pattern": "hit", "path": "secret/b.rs"}),
        )
        .await;

        assert_eq!(result["error_code"], "out_of_scope");
    }

    #[tokio::test]
    async fn results_beyond_max_results_are_truncated_and_flagged() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "hit\nhit\nhit\nhit\nhit\n");

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "hit", "max_results": 2}),
        )
        .await;

        assert_eq!(matches(&result).len(), 2);
        assert_eq!(result["data"]["truncated"], true);
    }

    #[tokio::test]
    async fn exactly_max_results_is_not_truncated() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "hit\nhit\n");

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "hit", "max_results": 2}),
        )
        .await;

        assert_eq!(matches(&result).len(), 2);
        assert_eq!(result["data"]["truncated"], false);
    }

    #[tokio::test]
    async fn a_long_matching_line_is_cut() {
        let dir = TempDir::new().unwrap();
        put(&dir, "min.js", &format!("hit{}\n", "é".repeat(5_000)));

        let result = run(&tool(&dir, None), json!({"pattern": "hit"})).await;

        let line = result["data"]["matches"][0].as_str().unwrap();
        assert!(line.chars().count() <= MAX_LINE_CHARS + "min.js:1:".len() + 1);
        assert!(line.ends_with('…'));
    }

    #[tokio::test]
    async fn the_output_stays_under_the_character_cap_whatever_max_results_asks() {
        let dir = TempDir::new().unwrap();
        put(
            &dir,
            "a.txt",
            &format!("hit{}\n", "x".repeat(150)).repeat(1_000),
        );

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "hit", "max_results": 1_000}),
        )
        .await;

        let total: usize = result["data"]["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap().chars().count())
            .sum();
        assert!(total <= MAX_OUTPUT_CHARS);
        assert_eq!(result["data"]["truncated"], true);
    }

    #[tokio::test]
    async fn the_default_cap_is_fifty() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", &"hit\n".repeat(80));

        let result = run(&tool(&dir, None), json!({"pattern": "hit"})).await;

        assert_eq!(matches(&result).len(), 50);
        assert_eq!(result["data"]["truncated"], true);
    }

    #[tokio::test]
    async fn a_zero_max_results_is_invalid_args() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "hit", "max_results": 0}),
        )
        .await;

        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn an_invalid_regex_is_invalid_args() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"pattern": "(unclosed"})).await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn a_missing_path_is_not_found() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"pattern": "x", "path": "nope"})).await;

        assert_eq!(result["error_code"], "not_found");
    }

    #[tokio::test]
    async fn a_path_outside_the_workspace_is_out_of_scope() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"pattern": "x", "path": "../"})).await;

        assert_eq!(result["error_code"], "out_of_scope");
    }

    #[tokio::test]
    async fn binary_files_are_skipped_silently() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("b.bin"), [0xff, 0xfe, b'h', b'i', b't']).unwrap();
        put(&dir, "a.txt", "hit\n");

        let result = run(&tool(&dir, None), json!({"pattern": "hit"})).await;

        assert_eq!(matches(&result), ["a.txt:1:hit"]);
    }

    #[tokio::test]
    async fn no_matches_is_a_successful_empty_result() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.txt", "nothing\n");

        let result = run(&tool(&dir, None), json!({"pattern": "zzz"})).await;

        assert_eq!(result["ok"], true);
        assert!(matches(&result).is_empty());
    }

    #[tokio::test]
    async fn an_unknown_argument_is_rejected_by_name() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "x", "file_pattern": "*.rs"}),
        )
        .await;

        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("file_pattern"));
    }
}
