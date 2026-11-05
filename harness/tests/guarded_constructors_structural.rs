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

const FREE_CONSTRUCTORS: [&str; 4] = [
    "create_tool_registry",
    "create_tool_registry_for",
    "create_container_tool_registry",
    "create_container_tool_registry_for",
];

const SHIPPED_ONLY_SITES: [(&str, &str, &str); 11] = [
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

fn macro_idents(stream: TokenStream, out: &mut Vec<String>) {
    for token in stream {
        match token {
            TokenTree::Group(group) => macro_idents(group.stream(), out),
            TokenTree::Ident(ident) => out.push(plain(&ident.to_string()).to_string()),
            _ => {}
        }
    }
}

fn is_constructor_name(name: &str) -> bool {
    FREE_CONSTRUCTORS.contains(&name)
        || TYPE_CONSTRUCTORS
            .iter()
            .any(|(_, constructor)| *constructor == name && name != "new")
}

#[derive(Default)]
struct Sites {
    function: Vec<String>,
    calls: Vec<(String, String)>,
    renames: Vec<String>,
    macro_mentions: Vec<String>,
}

impl<'ast> Visit<'ast> for Sites {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if is_test_only(&node.attrs) {
            return;
        }
        self.function
            .push(plain(&node.sig.ident.to_string()).to_string());
        syn::visit::visit_item_fn(self, node);
        self.function.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if is_test_only(&node.attrs) {
            return;
        }
        self.function
            .push(plain(&node.sig.ident.to_string()).to_string());
        syn::visit::visit_impl_item_fn(self, node);
        self.function.pop();
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_test_only(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_test_only(&node.attrs) {
            syn::visit::visit_item_impl(self, node);
        }
    }

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        self.renames
            .push(plain(&node.rename.to_string()).to_string());
        syn::visit::visit_use_rename(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let mut idents = Vec::new();
        macro_idents(node.tokens.clone(), &mut idents);
        for ident in idents {
            if is_constructor_name(&ident) {
                self.macro_mentions.push(ident);
            }
        }
        syn::visit::visit_macro(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func {
            let segments = path_segments(&path.path);
            let function = self.function.last().cloned().unwrap_or_default();
            if let Some(last) = segments.last() {
                if FREE_CONSTRUCTORS.contains(&last.as_str()) {
                    self.calls.push((function.clone(), last.clone()));
                }
                if segments.len() >= 2 {
                    let ty = &segments[segments.len() - 2];
                    if TYPE_CONSTRUCTORS.iter().any(|(t, c)| t == ty && c == last) {
                        self.calls.push((function, format!("{ty}::{last}")));
                    }
                }
            }
        }
        syn::visit::visit_expr_call(self, node);
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
    for rename in &sites.renames {
        if is_constructor_name(rename) {
            found.push(format!(
                "{relative}: constructor imported under another name `{rename}`"
            ));
        }
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
