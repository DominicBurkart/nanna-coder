use super::{error_outcome, MAX_FILE_BYTES};
use crate::effects::EffectClass;
use crate::scope::{canonical_root, relative_to, resolve_path, validate_path_within_workspace};
use crate::scope::{PathAccess, PathScope};
use crate::tool_contract::{ErrorCode, ToolOutcome, TypedTool};
use ast_grep_core::language::Language;
use ast_grep_core::matcher::{Pattern, PatternBuilder, PatternError};
use ast_grep_core::meta_var::MetaVariable;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use ast_grep_core::AstGrep;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_RESULTS: u32 = 50;
const EXPANDO: char = '\u{00b5}';

#[derive(Clone)]
pub struct RustLang;

impl Language for RustLang {
    fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
        let mut out = String::with_capacity(query.len());
        let mut chars = query.chars().peekable();
        while let Some(ch) = chars.next() {
            let metavar = ch == '$'
                && chars
                    .peek()
                    .is_some_and(|next| next.is_ascii_uppercase() || *next == '_' || *next == '$');
            out.push(if metavar { EXPANDO } else { ch });
        }
        Cow::Owned(out)
    }

    fn expando_char(&self) -> char {
        EXPANDO
    }

    fn from_path<P: AsRef<Path>>(path: P) -> Option<Self> {
        (path.as_ref().extension()? == "rs").then_some(RustLang)
    }

    fn kind_to_id(&self, kind: &str) -> u16 {
        self.get_ts_language().id_for_node_kind(kind, true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.get_ts_language()
            .field_id_for_name(field)
            .map(|id| id.get())
    }

    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|src| StrDoc::try_new(src, self.clone()))
    }
}

impl LanguageExt for RustLang {
    fn get_ts_language(&self) -> TSLanguage {
        tree_sitter_rust::LANGUAGE.into()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AstGrepArgs {
    #[schemars(
        description = "Rust code pattern with metavariables, e.g. fn $NAME() {} ; $X matches one node, $$$XS matches several"
    )]
    pub pattern: String,
    #[schemars(
        description = "File or directory to search, relative to the workspace root (default: the root)"
    )]
    pub path: Option<String>,
    #[schemars(description = "Source language; only \"rust\" is supported (default)")]
    pub language: Option<String>,
    #[schemars(description = "Maximum matches to return (default 50)")]
    pub max_results: Option<u32>,
}

pub struct AstGrepTool {
    root: PathBuf,
    scope: Option<PathScope>,
}

impl AstGrepTool {
    pub fn new(root: PathBuf, scope: Option<PathScope>) -> Self {
        Self { root, scope }
    }
}

struct Scan<'a> {
    root: &'a Path,
    scope: Option<&'a PathScope>,
    pattern: &'a Pattern,
    limit: usize,
    matches: Vec<Value>,
}

impl Scan<'_> {
    fn full(&self) -> bool {
        self.matches.len() > self.limit
    }

    fn visit_file(&mut self, path: &Path) {
        if RustLang::from_path(path).is_none() {
            return;
        }
        let relative = relative_to(path, self.root).to_path_buf();
        if self
            .scope
            .is_some_and(|scope| !scope.permits(PathAccess::Read, &relative))
        {
            return;
        }
        if !std::fs::metadata(path).is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES) {
            return;
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(doc) = StrDoc::try_new(&source, RustLang) else {
            return;
        };
        let tree = AstGrep::doc(doc);
        let shown = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        for found in tree.root().find_all(self.pattern) {
            if self.full() {
                return;
            }
            let env = found.get_env();
            let mut captures = Map::new();
            for variable in env.get_matched_variables() {
                match variable {
                    MetaVariable::Capture(name, _) => {
                        if let Some(node) = env.get_match(&name) {
                            captures.insert(name, Value::String(node.text().to_string()));
                        }
                    }
                    MetaVariable::MultiCapture(name) => {
                        let texts: Vec<String> = env
                            .get_multiple_matches(&name)
                            .iter()
                            .map(|node| node.text().to_string())
                            .collect();
                        captures.insert(name, json!(texts));
                    }
                    _ => {}
                }
            }
            self.matches.push(json!({
                "path": shown,
                "line": found.start_pos().line() + 1,
                "text": found.text(),
                "captures": captures,
            }));
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

#[async_trait]
impl TypedTool for AstGrepTool {
    type Args = AstGrepArgs;

    fn name(&self) -> &str {
        "ast_grep"
    }

    fn description(&self) -> &str {
        "Structural search of Rust code. The pattern is Rust syntax where $NAME matches any one node and $$$ARGS matches a sequence, for example fn $NAME() {} or $X.unwrap(). Returns path, line, matched text and captured metavariables."
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }

    async fn run(&self, args: AstGrepArgs) -> ToolOutcome {
        let language = args.language.as_deref().unwrap_or("rust");
        if !language.eq_ignore_ascii_case("rust") {
            return ToolOutcome::failure(
                ErrorCode::InvalidArgs,
                format!("language {language:?} is not supported; only \"rust\" is"),
            );
        }
        if args.pattern.trim().is_empty() {
            return ToolOutcome::failure(ErrorCode::InvalidArgs, "pattern is empty");
        }
        let limit = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS);
        if limit == 0 {
            return ToolOutcome::failure(ErrorCode::InvalidArgs, "max_results must be at least 1");
        }
        let pattern = match Pattern::try_new(&args.pattern, RustLang) {
            Ok(pattern) if !pattern.has_error() => pattern,
            Ok(_) => {
                return ToolOutcome::failure(
                    ErrorCode::InvalidArgs,
                    "pattern does not parse as Rust code",
                )
                .with_hint(
                    "write a complete Rust item, expression or statement, e.g. fn $NAME() {}",
                )
            }
            Err(error) => {
                return ToolOutcome::failure(
                    ErrorCode::InvalidArgs,
                    format!("invalid pattern: {error}"),
                )
            }
        };
        let path_arg = args.path.as_deref().unwrap_or(".");
        let target = match validate_path_within_workspace(Path::new(path_arg), &self.root) {
            Ok(target) => target,
            Err(error) => return error_outcome(error, &self.root, path_arg),
        };
        let root = match canonical_root(&self.root) {
            Ok(root) => root,
            Err(error) => return ToolOutcome::from(&error),
        };
        let mut scan = Scan {
            root: &root,
            scope: self.scope.as_ref(),
            pattern: &pattern,
            limit: limit as usize,
            matches: Vec::new(),
        };
        if target.is_dir() {
            scan.visit_dir(&target);
        } else {
            match resolve_path(
                self.scope.as_ref(),
                "ast_grep",
                PathAccess::Read,
                Path::new(path_arg),
                &self.root,
            ) {
                Ok(file) => scan.visit_file(&file),
                Err(error) => return error_outcome(error, &self.root, path_arg),
            }
        }
        let truncated = scan.matches.len() > scan.limit;
        scan.matches.truncate(scan.limit);
        ToolOutcome::success(json!({
            "matches": scan.matches,
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

    fn tool(dir: &TempDir, scope: Option<PathScope>) -> Typed<AstGrepTool> {
        Typed(AstGrepTool::new(dir.path().to_path_buf(), scope))
    }

    async fn run(tool: &Typed<AstGrepTool>, args: Value) -> Value {
        tool.execute(args).await.unwrap()
    }

    fn put(dir: &TempDir, name: &str, content: &str) {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn found(result: &Value) -> &Vec<Value> {
        result["data"]["matches"].as_array().unwrap()
    }

    #[tokio::test]
    async fn matches_functions_with_a_metavariable_and_captures_the_name() {
        let dir = TempDir::new().unwrap();
        put(
            &dir,
            "src/lib.rs",
            "fn alpha() {}\n\nfn gamma(x: i32) {}\n\nfn beta() {}\n",
        );

        let result = run(&tool(&dir, None), json!({"pattern": "fn $NAME() {}"})).await;

        assert_eq!(result["ok"], true);
        let matches = found(&result);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["path"], "src/lib.rs");
        assert_eq!(matches[0]["line"], 1);
        assert_eq!(matches[0]["text"], "fn alpha() {}");
        assert_eq!(matches[0]["captures"]["NAME"], "alpha");
        assert_eq!(matches[1]["line"], 5);
        assert_eq!(matches[1]["captures"]["NAME"], "beta");
        assert_eq!(result["data"]["truncated"], false);
    }

    #[tokio::test]
    async fn matches_several_metavariables_in_one_pattern() {
        let dir = TempDir::new().unwrap();
        put(
            &dir,
            "a.rs",
            "fn f() {\n    let x = 1;\n    let y = foo(2);\n}\n",
        );

        let result = run(&tool(&dir, None), json!({"pattern": "let $A = $B;"})).await;

        let matches = found(&result);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["captures"]["A"], "x");
        assert_eq!(matches[0]["captures"]["B"], "1");
        assert_eq!(matches[1]["captures"]["B"], "foo(2)");
        assert_eq!(matches[1]["line"], 3);
    }

    #[tokio::test]
    async fn a_literal_pattern_without_metavariables_matches_exactly() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.rs", "fn f() { a.unwrap(); b.expect(\"x\"); }\n");

        let result = run(&tool(&dir, None), json!({"pattern": "a.unwrap()"})).await;

        assert_eq!(found(&result).len(), 1);
    }

    #[tokio::test]
    async fn only_rust_files_are_searched() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.rs", "fn a() {}\n");
        put(&dir, "b.txt", "fn b() {}\n");

        let result = run(&tool(&dir, None), json!({"pattern": "fn $N() {}"})).await;

        assert_eq!(found(&result).len(), 1);
        assert_eq!(found(&result)[0]["path"], "a.rs");
    }

    #[tokio::test]
    async fn a_path_restricts_the_search() {
        let dir = TempDir::new().unwrap();
        put(&dir, "x/a.rs", "fn a() {}\n");
        put(&dir, "y/b.rs", "fn b() {}\n");

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "fn $N() {}", "path": "y"}),
        )
        .await;

        assert_eq!(found(&result).len(), 1);
        assert_eq!(found(&result)[0]["path"], "y/b.rs");
    }

    #[tokio::test]
    async fn dot_directories_and_target_are_skipped() {
        let dir = TempDir::new().unwrap();
        put(&dir, ".hidden/a.rs", "fn a() {}\n");
        put(&dir, "target/b.rs", "fn b() {}\n");
        put(&dir, "src/c.rs", "fn c() {}\n");

        let result = run(&tool(&dir, None), json!({"pattern": "fn $N() {}"})).await;

        assert_eq!(found(&result).len(), 1);
        assert_eq!(found(&result)[0]["path"], "src/c.rs");
    }

    #[tokio::test]
    async fn only_files_the_read_scope_allows_are_searched() {
        let dir = TempDir::new().unwrap();
        put(&dir, "src/a.rs", "fn a() {}\n");
        put(&dir, "secret/b.rs", "fn b() {}\n");
        let scope =
            PathScope::new("t", &["src/**".to_string()], Some(&["src/**".to_string()])).unwrap();

        let result = run(&tool(&dir, Some(scope)), json!({"pattern": "fn $N() {}"})).await;

        assert_eq!(found(&result).len(), 1);
        assert_eq!(found(&result)[0]["path"], "src/a.rs");
    }

    #[tokio::test]
    async fn results_are_capped_and_flagged() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "fn $N() {}", "max_results": 2}),
        )
        .await;

        assert_eq!(found(&result).len(), 2);
        assert_eq!(result["data"]["truncated"], true);
    }

    #[tokio::test]
    async fn an_unsupported_language_is_invalid_args() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "x", "language": "python"}),
        )
        .await;

        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("rust"));
    }

    #[tokio::test]
    async fn an_empty_pattern_is_invalid_args() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"pattern": "  "})).await;

        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn an_unparseable_pattern_is_invalid_args_not_a_panic() {
        let dir = TempDir::new().unwrap();
        put(&dir, "a.rs", "fn a() {}\n");

        let result = run(&tool(&dir, None), json!({"pattern": "fn $N( {{{"})).await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn a_missing_path_is_not_found() {
        let dir = TempDir::new().unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"pattern": "fn $N() {}", "path": "nope"}),
        )
        .await;

        assert_eq!(result["error_code"], "not_found");
    }

    #[tokio::test]
    async fn an_unknown_argument_is_rejected_by_name() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"pattern": "x", "rewrite": "y"})).await;

        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("rewrite"));
    }
}
