use proc_macro2::{TokenStream, TokenTree};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const TYPE_CONSTRUCTORS: [(&str, &str); 6] = [
    ("ProtectedPaths", "for_repo"),
    ("ProtectedPaths", "with_config_dir"),
    ("WriteFileTool", "scoped"),
    ("WriteFileTool", "new"),
    ("GitCommitTool", "new"),
    ("GitPushBranchTool", "new"),
];

const GUARDED_TYPES: [&str; 4] = [
    "ProtectedPaths",
    "WriteFileTool",
    "GitCommitTool",
    "GitPushBranchTool",
];

const FREE_CONSTRUCTORS: [&str; 4] = [
    "create_tool_registry",
    "create_tool_registry_for",
    "create_container_tool_registry",
    "create_container_tool_registry_for",
];

const SHIPPED_ONLY_SITES: [(&str, &str, &str); 13] = [
    ("src/tools.rs", "scoped", "ProtectedPaths::for_repo"),
    (
        "src/tools.rs",
        "create_tool_registry",
        "ProtectedPaths::for_repo",
    ),
    (
        "src/tools.rs",
        "create_tool_registry_for",
        "ProtectedPaths::for_repo",
    ),
    (
        "src/tools.rs",
        "create_container_tool_registry",
        "ProtectedPaths::for_repo",
    ),
    (
        "src/tools.rs",
        "create_container_tool_registry_for",
        "ProtectedPaths::for_repo",
    ),
    ("src/pr_tools.rs", "new", "ProtectedPaths::for_repo"),
    ("src/pr_tools.rs", "register_guarded", "GitCommitTool::new"),
    (
        "src/pr_tools.rs",
        "register_guarded",
        "GitPushBranchTool::new",
    ),
    (
        "src/pr_tools.rs",
        "register_granted_at",
        "GitCommitTool::new",
    ),
    (
        "src/pr_tools.rs",
        "register_granted_at",
        "GitPushBranchTool::new",
    ),
    (
        "src/action_auditor/commands.rs",
        "protected_paths",
        "ProtectedPaths::with_config_dir",
    ),
    ("src/tools.rs", "new", "WriteFileTool::scoped"),
    (
        "src/protected.rs",
        "for_repo",
        "ProtectedPaths::with_config_dir",
    ),
];

const PINNED_LIMITS: [&str; 6] = [
    "only files under src/ are scanned; tests, benches, examples and build scripts are not",
    "test-only code (cfg(test) items and files declared by a cfg(test) mod) is not scanned",
    "constructors are matched by name and owning type, not by resolving imports: a glob import followed by a call is caught by the call, an unrelated type with the same function names is not",
    "generic or trait-dispatched construction (T::new) is not seen",
    "method-call syntax is not seen",
    "code produced by include!, proc macros or build scripts is not seen",
];

fn plain(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn cfg_holds_outside_tests(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => !path.is_ident("test"),
        syn::Meta::NameValue(_) => true,
        syn::Meta::List(list) => {
            let nested = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .unwrap();
            let holds: Vec<bool> = nested.iter().map(cfg_holds_outside_tests).collect();
            match list
                .path
                .get_ident()
                .map(|ident| ident.to_string())
                .as_deref()
            {
                Some("not") => !holds.iter().all(|held| *held),
                Some("any") => holds.iter().any(|held| *held),
                _ => holds.iter().all(|held| *held),
            }
        }
    }
}

fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .map(|meta| !cfg_holds_outside_tests(&meta))
                .unwrap_or(false)
    })
}

fn path_segments(path: &syn::Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| plain(&segment.ident.to_string()).to_string())
        .collect()
}

fn flatten(stream: TokenStream, out: &mut Vec<String>) {
    for token in stream {
        match token {
            TokenTree::Group(group) => flatten(group.stream(), out),
            TokenTree::Ident(ident) => out.push(plain(&ident.to_string()).to_string()),
            TokenTree::Punct(punct) => out.push(punct.as_char().to_string()),
            TokenTree::Literal(_) => out.push(String::new()),
        }
    }
}

fn is_constructor_method(name: &str) -> bool {
    TYPE_CONSTRUCTORS
        .iter()
        .any(|(_, constructor)| *constructor == name && name != "new")
}

fn is_guarded_name(name: &str) -> bool {
    FREE_CONSTRUCTORS.contains(&name)
        || GUARDED_TYPES.contains(&name)
        || is_constructor_method(name)
}

fn is_constructor_name(name: &str) -> bool {
    FREE_CONSTRUCTORS.contains(&name) || is_constructor_method(name)
}

fn follows_guarded_type(tokens: &[String], at: usize) -> bool {
    let guarded = |i: usize| GUARDED_TYPES.contains(&tokens[i].as_str());
    at >= 3
        && tokens[at - 1] == ":"
        && tokens[at - 2] == ":"
        && (guarded(at - 3) || (tokens[at - 3] == ">" && at >= 4 && guarded(at - 4)))
}

fn macro_mentions(tokens: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        if is_constructor_name(token) || (token == "new" && follows_guarded_type(tokens, at)) {
            found.push(token.clone());
        }
    }
    found
}

fn type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(path) => path_segments(&path.path).last().cloned(),
        _ => None,
    }
}

fn use_renames(tree: &syn::UseTree, out: &mut Vec<String>) {
    match tree {
        syn::UseTree::Rename(rename) => out.push(plain(&rename.ident.to_string()).to_string()),
        syn::UseTree::Path(path) => use_renames(&path.tree, out),
        syn::UseTree::Group(group) => group.items.iter().for_each(|t| use_renames(t, out)),
        _ => {}
    }
}

#[derive(Default)]
struct Sites {
    function: Vec<String>,
    impl_types: Vec<Option<String>>,
    calls: Vec<(String, String)>,
    references: Vec<String>,
    renames: Vec<String>,
    aliases: Vec<String>,
    macro_mentions: Vec<String>,
}

impl Sites {
    fn note_path(&mut self, path: &syn::ExprPath, callee: bool) {
        let segments = path_segments(&path.path);
        let name = segments.last().cloned().unwrap_or_default();
        let owner = match &path.qself {
            Some(qself) => type_name(&qself.ty),
            None if segments.len() >= 2 => Some(segments[segments.len() - 2].clone()),
            None => None,
        };
        let owner = match owner.as_deref() {
            Some("Self") => self.impl_types.last().cloned().flatten(),
            _ => owner,
        };
        let label = if FREE_CONSTRUCTORS.contains(&name.as_str()) {
            Some(name.clone())
        } else {
            owner
                .filter(|ty| TYPE_CONSTRUCTORS.iter().any(|(t, c)| t == ty && *c == name))
                .map(|ty| format!("{ty}::{name}"))
        };
        if let Some(label) = label {
            if callee {
                let function = self.function.last().cloned().unwrap_or_default();
                self.calls.push((function, label));
            } else {
                self.references.push(label);
            }
        }
    }
}

impl<'ast> Visit<'ast> for Sites {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_test_only(&node.attrs) {
            self.function
                .push(plain(&node.sig.ident.to_string()).to_string());
            syn::visit::visit_item_fn(self, node);
            self.function.pop();
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_test_only(&node.attrs) {
            self.function
                .push(plain(&node.sig.ident.to_string()).to_string());
            syn::visit::visit_impl_item_fn(self, node);
            self.function.pop();
        }
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_test_only(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_test_only(&node.attrs) {
            self.impl_types.push(type_name(&node.self_ty));
            syn::visit::visit_item_impl(self, node);
            self.impl_types.pop();
        }
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        let mut sources = Vec::new();
        use_renames(&node.tree, &mut sources);
        self.renames.extend(sources);
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if type_name(&node.ty).is_some_and(|name| GUARDED_TYPES.contains(&name.as_str())) {
            self.aliases
                .push(plain(&node.ident.to_string()).to_string());
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let mut tokens = Vec::new();
        flatten(node.tokens.clone(), &mut tokens);
        self.macro_mentions.extend(macro_mentions(&tokens));
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        match &*node.func {
            syn::Expr::Path(path) => self.note_path(path, true),
            other => self.visit_expr(other),
        }
        node.args.iter().for_each(|arg| self.visit_expr(arg));
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        self.note_path(node, false);
    }
}

fn production_items(file: &syn::File) -> Vec<&syn::Item> {
    file.items
        .iter()
        .filter(|item| {
            let attrs: &[syn::Attribute] = match item {
                syn::Item::Fn(i) => &i.attrs,
                syn::Item::Impl(i) => &i.attrs,
                syn::Item::Mod(i) => &i.attrs,
                syn::Item::Use(i) => &i.attrs,
                _ => &[],
            };
            !is_test_only(attrs)
        })
        .collect()
}

fn unguarded_sites(relative: &str, source: &str) -> Vec<String> {
    let file = syn::parse_file(source).unwrap();
    let mut sites = Sites::default();
    for item in production_items(&file) {
        sites.visit_item(item);
    }
    let mut found = Vec::new();
    for (function, constructor) in &sites.calls {
        let allowed = SHIPPED_ONLY_SITES
            .iter()
            .any(|(path, f, c)| *path == relative && f == function && c == constructor);
        if !allowed {
            found.push(format!("{relative}: `{function}` calls `{constructor}`"));
        }
    }
    for source in sites.renames.iter().filter(|name| is_guarded_name(name)) {
        found.push(format!(
            "{relative}: `{source}` imported under another name"
        ));
    }
    for alias in &sites.aliases {
        found.push(format!(
            "{relative}: type alias `{alias}` to a guarded type"
        ));
    }
    for reference in &sites.references {
        found.push(format!(
            "{relative}: `{reference}` used as a value outside a call"
        ));
    }
    for mention in &sites.macro_mentions {
        found.push(format!("{relative}: macro mentions `{mention}`"));
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn test_only_module_files(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut skipped = Vec::new();
    for file in files {
        let parsed = syn::parse_file(&fs::read_to_string(file).unwrap()).unwrap();
        for item in &parsed.items {
            if let syn::Item::Mod(module) = item {
                if module.content.is_none() && is_test_only(&module.attrs) {
                    let name = plain(&module.ident.to_string()).to_string();
                    let dir = file.parent().unwrap();
                    skipped.push(dir.join(format!("{name}.rs")));
                    skipped.push(dir.join(&name).join("mod.rs"));
                }
            }
        }
    }
    skipped
}

#[test]
fn production_code_cannot_construct_a_shipped_only_variant_outside_the_listed_sites() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    assert!(files.len() > 50);
    let skipped = test_only_module_files(&files);
    assert!(skipped
        .iter()
        .any(|f| f.ends_with("harness_adapter/tests.rs")));
    let mut found = Vec::new();
    for file in files.iter().filter(|f| !skipped.contains(f)) {
        let relative = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(file).unwrap().replace("\r\n", "\n");
        found.extend(unguarded_sites(&relative, &source));
    }
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn every_listed_site_still_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (relative, function, constructor) in SHIPPED_ONLY_SITES {
        let source = fs::read_to_string(root.join(relative)).unwrap();
        let file = syn::parse_file(&source).unwrap();
        let mut sites = Sites::default();
        for item in production_items(&file) {
            sites.visit_item(item);
        }
        assert!(
            sites
                .calls
                .iter()
                .any(|(f, c)| f == function && c == constructor),
            "{relative}: `{function}` no longer calls `{constructor}`"
        );
    }
}

#[test]
fn a_production_call_to_each_constructor_is_flagged() {
    for call in [
        "create_tool_registry(root)",
        "crate::tools::create_tool_registry_for(root, id)",
        "create_container_tool_registry(root, h, d)",
        "create_container_tool_registry_for(root, h, d, id)",
        "ProtectedPaths::for_repo(root)",
        "crate::protected::ProtectedPaths::with_config_dir(root, None)",
        "WriteFileTool::scoped(root, None)",
        "WriteFileTool::new(root)",
        "GitCommitTool::new(root, n)",
        "GitPushBranchTool::new(root, n)",
    ] {
        let source = format!("fn leak() {{ let _ = {call}; }}");
        assert_eq!(unguarded_sites("src/leak.rs", &source).len(), 1, "{call}");
    }
}

#[test]
fn test_only_code_and_guarded_variants_are_not_flagged() {
    for source in [
        "#[cfg(test)] mod tests { fn t() { create_tool_registry(r); } }",
        "#[cfg(test)] fn t() { ProtectedPaths::for_repo(r); }",
        "fn ok() { create_tool_registry_guarded(r, p); create_tool_registry_in_checkout(r); }",
        "fn ok() { ProtectedPaths::for_repo_with_extras(r, e); ProtectedPaths::with_extras(r, None, e); }",
        "fn ok() { WriteFileTool::guarded(r, None, p); }",
    ] {
        assert!(unguarded_sites("src/ok.rs", source).is_empty(), "{source}");
    }
}

#[test]
fn renaming_an_import_or_hiding_a_call_in_a_macro_is_flagged() {
    assert_eq!(
        unguarded_sites(
            "src/x.rs",
            "use crate::tools::create_tool_registry as create_tool_registry_for;"
        )
        .len(),
        1
    );
    assert_eq!(
        unguarded_sites("src/x.rs", "fn f() { vec![create_tool_registry(r)]; }").len(),
        1
    );
}

#[test]
fn a_listed_site_does_not_allow_the_same_call_from_another_function_or_file() {
    let source = "fn create_tool_registry() { ProtectedPaths::for_repo(r); }";
    assert!(unguarded_sites("src/tools.rs", source).is_empty());
    assert_eq!(unguarded_sites("src/other.rs", source).len(), 1);
    let source = "fn sneaky() { ProtectedPaths::for_repo(r); }";
    assert_eq!(unguarded_sites("src/tools.rs", source).len(), 1);
}

#[test]
fn cfg_combinators_decide_what_counts_as_test_only() {
    let call = "{ create_tool_registry(r); }";
    for (attr, flagged) in [
        ("#[cfg(not(test))]", 1),
        ("#[cfg(any(test, feature = \"x\"))]", 1),
        ("#[cfg(all(test, unix))]", 0),
        ("#[cfg(all(unix, feature = \"x\"))]", 1),
        ("#[cfg(feature = \"x\")]", 1),
        ("#[cfg(test)]", 0),
    ] {
        let source = format!("{attr} fn f() {call}");
        assert_eq!(
            unguarded_sites("src/x.rs", &source).len(),
            flagged,
            "{attr}"
        );
    }
}

#[test]
fn test_only_functions_nested_in_production_modules_and_impls_are_skipped() {
    let call = "{ create_tool_registry(r); }";
    let source = format!(
        "mod m {{ #[cfg(test)] fn t() {call} fn p() {call} }} impl X {{ #[cfg(test)] fn t() {call} fn p() {call} }}"
    );
    assert_eq!(unguarded_sites("src/x.rs", &source).len(), 2);
}

fn found(source: &str) -> Vec<String> {
    unguarded_sites("src/x.rs", source)
}

#[test]
fn an_import_renamed_with_as_is_flagged_for_constructors_and_guarded_types() {
    for source in [
        "use crate::tools::create_tool_registry as ctr; fn f() { ctr(r); }",
        "use crate::tools::{create_tool_registry_for as ctf, other}; fn f() { ctf(r, i); }",
        "use crate::protected::ProtectedPaths as PP; fn f() { PP::for_repo(r); }",
        "use crate::tools::WriteFileTool as W;",
    ] {
        assert_eq!(found(source).len(), 1, "{source}");
    }
    for source in [
        "use crate::tools::Other as O;",
        "use crate::protected::ProtectedPaths;",
        "use crate::tools::{create_tool_registry_guarded as g, other};",
        "use crate::tools::*;",
    ] {
        assert!(found(source).is_empty(), "{source}");
    }
}

#[test]
fn a_type_alias_to_a_guarded_type_is_flagged() {
    for source in [
        "type PP = ProtectedPaths;",
        "type W = crate::tools::WriteFileTool;",
        "pub type C = GitCommitTool;",
    ] {
        assert_eq!(found(source).len(), 1, "{source}");
    }
    for source in [
        "type V = Vec<u8>;",
        "type Refs = &'static [ProtectedPaths];",
    ] {
        assert!(found(source).is_empty(), "{source}");
    }
}

#[test]
fn qualified_path_calls_are_flagged() {
    for source in [
        "fn f() { <ProtectedPaths>::for_repo(r); }",
        "fn f() { <WriteFileTool>::new(r); }",
        "fn f() { <crate::tools::WriteFileTool>::scoped(r, s); }",
        "fn f() { <GitPushBranchTool as Tool>::new(r, n); }",
    ] {
        assert_eq!(found(source).len(), 1, "{source}");
    }
    assert!(
        found("fn f() { <Vec<u8>>::new(); <ProtectedPaths>::with_extras(r, c, e); }").is_empty()
    );
}

#[test]
fn function_pointer_references_are_flagged() {
    for source in [
        "fn f() { let g = create_tool_registry; }",
        "fn f() { x.map(create_tool_registry_for); }",
        "fn f() { x.map(crate::tools::create_container_tool_registry); }",
        "static F: fn(&Path) -> R = create_tool_registry;",
        "const G: fn(&Path) -> R = create_tool_registry_for;",
        "fn f() { let g = ProtectedPaths::for_repo; }",
        "fn f() { let g = <WriteFileTool>::new; }",
        "fn f() { let g: fn(&Path) -> P = ProtectedPaths::with_config_dir; }",
    ] {
        assert_eq!(found(source).len(), 1, "{source}");
    }
    assert!(found("fn f() { let g = create_tool_registry_guarded; let h = Vec::new; }").is_empty());
}

#[test]
fn new_inside_macro_invocations_is_flagged_after_a_guarded_type() {
    for source in [
        "fn f() { foo!(GitCommitTool::new(r, n)); }",
        "fn f() { foo!(WriteFileTool::new(r)); }",
        "fn f() { foo!(a, GitPushBranchTool::new(r, n)); }",
        "fn f() { foo!(<WriteFileTool>::new(r)); }",
        "fn f() { foo!({ ProtectedPaths::for_repo(r) }); }",
        "fn f() { foo!(create_tool_registry(r)); }",
    ] {
        assert_eq!(found(source).len(), 1, "{source}");
    }
    for source in [
        "fn f() { foo!(Vec::new()); }",
        "fn f() { foo!(new); }",
        "fn f() { foo!(x::new); }",
        "fn f() { foo!(); }",
    ] {
        assert!(found(source).is_empty(), "{source}");
    }
}

#[test]
fn constructors_inside_a_macro_rules_body_are_flagged() {
    for source in [
        "macro_rules! m { () => { create_tool_registry(r) }; }",
        "macro_rules! m { () => { GitPushBranchTool::new(r, n) }; }",
        "macro_rules! m { ($r:expr) => { ProtectedPaths::for_repo($r) }; }",
    ] {
        assert_eq!(found(source).len(), 1, "{source}");
    }
    assert!(found("macro_rules! m { () => { Vec::new() }; }").is_empty());
}

#[test]
fn self_calls_inside_an_impl_of_a_guarded_type_are_flagged() {
    for source in [
        "impl ProtectedPaths { fn x() { Self::for_repo(r); } }",
        "impl WriteFileTool { fn x() { Self::new(r); } }",
        "impl GitCommitTool { fn x() { Self::new(r, n); } }",
        "impl ProtectedPaths { fn x() { let g = Self::with_config_dir; } }",
    ] {
        assert_eq!(found(source).len(), 1, "{source}");
    }
    for source in [
        "impl Other { fn x() { Self::for_repo(r); } }",
        "impl ProtectedPaths { fn x() { Self::with_extras(r, c, e); } }",
        "fn x() { Self::for_repo(r); }",
        "impl<T> Tr for Vec<T> { fn x() { Self::new(); } }",
        "impl (A, B) { fn x() { Self::new(); } }",
    ] {
        assert!(found(source).is_empty(), "{source}");
    }
}

#[test]
fn a_callee_that_is_not_a_path_is_inspected_for_nested_constructors() {
    assert!(found("fn f() { handler()(r); }").is_empty());
    assert_eq!(found("fn f() { create_tool_registry(r)(x); }").len(), 1);
    assert_eq!(
        found("fn f() { (move || create_tool_registry(r))(); }").len(),
        1
    );
}

#[test]
fn literals_in_macro_tokens_do_not_hide_a_constructor() {
    assert_eq!(
        found("fn f() { foo!(\"x\", 1, WriteFileTool::new(r)); }").len(),
        1
    );
    assert!(found("fn f() { foo!(\"WriteFileTool::new\"); }").is_empty());
}

#[test]
fn a_listed_site_allows_a_self_call_only_from_its_named_function() {
    let source = "impl WriteFileTool { fn new() { Self::scoped(r, None); } fn other() { Self::scoped(r, None); } }";
    assert_eq!(unguarded_sites("src/tools.rs", source).len(), 1);
}

#[test]
fn the_documented_blind_spots_are_really_not_seen() {
    assert_eq!(PINNED_LIMITS.len(), 6);
    for source in [
        "fn f<T: Ctor>() { T::new(r); }",
        "fn f(x: X) { x.scoped(r); x.for_repo(); }",
        "fn f() { include!(\"generated.rs\"); }",
        "fn f() { OtherType::for_repo(r); OtherType::new(r); }",
    ] {
        assert!(found(source).is_empty(), "{source}");
    }
}
