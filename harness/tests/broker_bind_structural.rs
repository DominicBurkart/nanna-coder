use proc_macro2::{TokenStream, TokenTree};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const BIND: &str = "bind_workspace";

const CONSTRUCTORS: [&str; 11] = [
    "build_tool_registry",
    "build_tool_registry_for",
    "create_tool_registry",
    "create_tool_registry_for",
    "create_tool_registry_guarded",
    "create_tool_registry_for_guarded",
    "create_tool_registry_in_checkout",
    "create_container_tool_registry",
    "create_container_tool_registry_for",
    "create_container_tool_registry_guarded",
    "create_container_tool_registry_for_guarded",
];

const PRODUCTION_BIND_CALLERS_IN_THIS_SLICE: usize = 0;

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

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .map(|meta| !cfg_holds_outside_tests(&meta))
                .unwrap_or(false)
    })
}

fn plain(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn visibility(vis: &syn::Visibility) -> &'static str {
    match vis {
        syn::Visibility::Public(_) => "pub",
        syn::Visibility::Restricted(restricted) if restricted.path.is_ident("crate") => {
            "pub(crate)"
        }
        syn::Visibility::Restricted(_) => "pub(restricted)",
        syn::Visibility::Inherited => "private",
    }
}

fn last_segment(path: &syn::Path) -> String {
    path.segments
        .last()
        .map_or(String::new(), |s| s.ident.to_string())
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => last_segment(&path.path),
        syn::Type::Reference(reference) => type_name(&reference.elem),
        _ => String::new(),
    }
}

#[derive(Default)]
struct Scan {
    file: String,
    owner: Vec<String>,
    function: Vec<String>,
    sites: BTreeSet<String>,
    binds: BTreeSet<String>,
    shapes: BTreeSet<String>,
}

impl Scan {
    fn qualified(&self, name: &str) -> String {
        match self.owner.last() {
            Some(owner) => format!("{owner}::{name}"),
            None => name.to_string(),
        }
    }

    fn context(&self) -> String {
        self.function.last().cloned().unwrap_or_default()
    }

    fn record(&mut self, name: &str) {
        let name = plain(name);
        if CONSTRUCTORS.contains(&name) {
            self.sites
                .insert(format!("{}: {} calls {name}", self.file, self.context()));
        }
        if name == BIND {
            self.binds
                .insert(format!("{}: {}", self.file, self.context()));
        }
    }

    fn scan_tokens(&mut self, tokens: TokenStream) {
        for token in tokens {
            match token {
                TokenTree::Group(group) => self.scan_tokens(group.stream()),
                TokenTree::Ident(ident) => self.record(&ident.to_string()),
                _ => {}
            }
        }
    }

    fn enter_fn(&mut self, name: &str, vis: &syn::Visibility, attrs: &[syn::Attribute]) -> bool {
        if is_cfg_test(attrs) {
            return false;
        }
        let qualified = self.qualified(plain(name));
        if plain(name) == BIND || plain(name) == "model_facing_registry" {
            self.shapes.insert(format!(
                "{}: fn {qualified} is {}",
                self.file,
                visibility(vis)
            ));
        }
        self.function.push(qualified);
        true
    }
}

impl<'ast> Visit<'ast> for Scan {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        syn::visit::visit_item_struct(self, node);
        if node.ident == "ModelFacingLayers" {
            self.shapes.insert(format!(
                "{}: struct ModelFacingLayers is {}",
                self.file,
                visibility(&node.vis)
            ));
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_cfg_test(&node.attrs) {
            self.owner.push(type_name(&node.self_ty));
            syn::visit::visit_item_impl(self, node);
            self.owner.pop();
        }
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if !is_cfg_test(&node.attrs) {
            self.owner.push(node.ident.to_string());
            syn::visit::visit_item_trait(self, node);
            self.owner.pop();
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if self.enter_fn(&node.sig.ident.to_string(), &node.vis, &node.attrs) {
            syn::visit::visit_item_fn(self, node);
            self.function.pop();
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if self.enter_fn(&node.sig.ident.to_string(), &node.vis, &node.attrs) {
            syn::visit::visit_impl_item_fn(self, node);
            self.function.pop();
        }
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        let vis = syn::Visibility::Inherited;
        if self.enter_fn(&node.sig.ident.to_string(), &vis, &node.attrs) {
            syn::visit::visit_trait_item_fn(self, node);
            self.function.pop();
        }
    }

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        syn::visit::visit_use_rename(self, node);
        let name = plain(&node.ident.to_string()).to_string();
        if CONSTRUCTORS.contains(&name.as_str()) || name == BIND {
            self.sites
                .insert(format!("{}: use {name} as {}", self.file, node.rename));
        }
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        syn::visit::visit_expr_method_call(self, node);
        self.record(&node.method.to_string());
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        syn::visit::visit_expr_path(self, node);
        self.record(&last_segment(&node.path));
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.scan_tokens(node.tokens.clone());
    }
}

fn scan_source(file: &str, source: &str) -> Scan {
    let syntax = syn::parse_file(source).unwrap();
    let mut scan = Scan {
        file: file.to_string(),
        ..Scan::default()
    };
    for item in &syntax.items {
        scan.visit_item(item);
    }
    scan
}

fn test_only_modules(file: &Path, source: &str) -> Vec<PathBuf> {
    let syntax = syn::parse_file(source).unwrap();
    let dir = file.parent().unwrap();
    let stem = file.file_stem().unwrap().to_string_lossy().into_owned();
    let home = if matches!(stem.as_str(), "mod" | "lib" | "main") {
        dir.to_path_buf()
    } else {
        dir.join(&stem)
    };
    syntax
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) if module.content.is_none() && is_cfg_test(&module.attrs) => {
                Some(module.ident.to_string())
            }
            _ => None,
        })
        .flat_map(|name| {
            [
                home.join(format!("{name}.rs")),
                home.join(&name).join("mod.rs"),
            ]
        })
        .collect()
}

fn scan_all() -> Vec<Scan> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    let sources: Vec<(PathBuf, String)> = files
        .into_iter()
        .map(|path| {
            let source = fs::read_to_string(&path).unwrap();
            (path, source)
        })
        .collect();
    let skipped: BTreeSet<PathBuf> = sources
        .iter()
        .flat_map(|(path, source)| test_only_modules(path, source))
        .collect();
    sources
        .iter()
        .filter(|(path, _)| !skipped.contains(path))
        .map(|(path, source)| {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            scan_source(&relative, source)
        })
        .collect()
}

fn union(select: impl Fn(&Scan) -> &BTreeSet<String>) -> BTreeSet<String> {
    scan_all()
        .iter()
        .flat_map(|scan| select(scan).iter().cloned())
        .collect()
}

fn expected(lines: &[&str]) -> BTreeSet<String> {
    lines.iter().map(|line| line.to_string()).collect()
}

#[test]
fn model_facing_registries_are_built_only_at_the_listed_sites() {
    assert_eq!(
        union(|scan| &scan.sites),
        expected(&[
            "eval/runner.rs: run_agent_in_process calls create_tool_registry_in_checkout",
            "main.rs: registry_for_checkout calls create_tool_registry_in_checkout",
            "task.rs: registry_for calls build_tool_registry",
            "task.rs: registry_for calls build_tool_registry_for",
            "tools.rs: create_container_tool_registry calls create_container_tool_registry_guarded",
            "tools.rs: create_container_tool_registry_for calls create_container_tool_registry_for_guarded",
            "tools.rs: create_container_tool_registry_guarded calls create_tool_registry_guarded",
            "tools.rs: create_tool_registry calls create_tool_registry_guarded",
            "tools.rs: create_tool_registry_for calls create_tool_registry_for_guarded",
            "tools.rs: create_tool_registry_in_checkout calls create_tool_registry_guarded",
            "workspace.rs: TaskWorkspace::build_tool_registry calls create_container_tool_registry_guarded",
            "workspace.rs: TaskWorkspace::build_tool_registry calls create_tool_registry_guarded",
            "workspace.rs: TaskWorkspace::build_tool_registry_for calls create_container_tool_registry_for_guarded",
            "workspace.rs: TaskWorkspace::build_tool_registry_for calls create_tool_registry_for_guarded",
        ])
    );
}

#[test]
fn the_two_known_unlayered_registries_are_the_cli_checkout_seam_and_the_eval_runner() {
    let unlayered: BTreeSet<String> = union(|scan| &scan.sites)
        .into_iter()
        .filter(|site| site.contains("calls create_tool_registry_in_checkout"))
        .filter(|site| !site.starts_with("tools.rs"))
        .collect();
    assert_eq!(
        unlayered,
        expected(&[
            "eval/runner.rs: run_agent_in_process calls create_tool_registry_in_checkout",
            "main.rs: registry_for_checkout calls create_tool_registry_in_checkout",
        ])
    );
}

#[test]
fn bind_workspace_is_declared_once_and_the_layers_stay_crate_private() {
    assert_eq!(
        union(|scan| &scan.shapes),
        expected(&[
            "harness_adapter/binding.rs: fn ResolvedAgent::bind_workspace is pub(crate)",
            "task.rs: fn model_facing_registry is pub(crate)",
            "task.rs: struct ModelFacingLayers is pub(crate)",
        ])
    );
}

#[test]
fn bind_workspace_is_called_only_from_the_task_runner() {
    let callers = union(|scan| &scan.binds);
    assert_eq!(callers.len(), PRODUCTION_BIND_CALLERS_IN_THIS_SLICE);
    assert!(callers
        .iter()
        .all(|caller| caller.starts_with("task.rs: TaskRunner::")));
}

fn scanned(source: &str) -> Scan {
    scan_source("x.rs", source)
}

#[test]
fn the_scanner_sees_calls_paths_macros_and_renames() {
    let scan = scanned(
        r#"
        use crate::tools::create_tool_registry as sneaky;
        fn direct() { create_tool_registry(p); }
        fn qualified() { crate::tools::create_tool_registry_for(p, i); }
        fn method(w: W) { w.build_tool_registry_for(i); }
        fn by_value() { let f = build_tool_registry; }
        fn hidden() { go!(w.build_tool_registry()); }
        impl Runner { fn bind(&self) { self.agent.bind_workspace(a, b, c, d); } }
        "#,
    );
    let sites: Vec<&str> = scan.sites.iter().map(String::as_str).collect();
    assert_eq!(
        sites,
        vec![
            "x.rs: by_value calls build_tool_registry",
            "x.rs: direct calls create_tool_registry",
            "x.rs: hidden calls build_tool_registry",
            "x.rs: method calls build_tool_registry_for",
            "x.rs: qualified calls create_tool_registry_for",
            "x.rs: use create_tool_registry as sneaky",
        ]
    );
    assert_eq!(
        scan.binds.iter().collect::<Vec<_>>(),
        vec!["x.rs: Runner::bind"]
    );
}

#[test]
fn the_scanner_ignores_test_code_but_records_shapes() {
    let scan = scanned(
        r#"
        #[cfg(test)]
        mod tests { fn t() { create_tool_registry(p); } }
        #[cfg(test)]
        fn helper() { create_tool_registry(p); }
        #[cfg(test)]
        impl X { fn y(&self) { self.bind_workspace(); } }
        #[cfg(not(test))]
        fn live() { create_tool_registry(p); }
        trait T { fn k(&self) { create_tool_registry(p); } }
        pub(crate) struct ModelFacingLayers;
        pub fn model_facing_registry() {}
        impl A { pub(super) fn bind_workspace(&self) {} }
        "#,
    );
    let sites: Vec<&str> = scan.sites.iter().map(String::as_str).collect();
    assert_eq!(
        sites,
        vec![
            "x.rs: T::k calls create_tool_registry",
            "x.rs: live calls create_tool_registry"
        ]
    );
    assert!(scan.binds.is_empty());
    let shapes: Vec<&str> = scan.shapes.iter().map(String::as_str).collect();
    assert_eq!(
        shapes,
        vec![
            "x.rs: fn A::bind_workspace is pub(restricted)",
            "x.rs: fn model_facing_registry is pub",
            "x.rs: struct ModelFacingLayers is pub(crate)",
        ]
    );
}

#[test]
fn private_and_public_visibilities_are_told_apart() {
    let scan = scanned("struct ModelFacingLayers; pub struct Other; fn model_facing_registry() {}");
    let shapes: Vec<&str> = scan.shapes.iter().map(String::as_str).collect();
    assert_eq!(
        shapes,
        vec![
            "x.rs: fn model_facing_registry is private",
            "x.rs: struct ModelFacingLayers is private",
        ]
    );
    let public = scanned("pub struct ModelFacingLayers;");
    assert!(public
        .shapes
        .contains("x.rs: struct ModelFacingLayers is pub"));
}

#[test]
fn test_only_modules_are_resolved_beside_their_parent() {
    let file = Path::new("/src/harness_adapter/mod.rs");
    let found = test_only_modules(
        file,
        "#[cfg(test)] mod tests; mod live; #[cfg(test)] mod inline { fn f() {} }",
    );
    assert_eq!(
        found,
        vec![
            PathBuf::from("/src/harness_adapter/tests.rs"),
            PathBuf::from("/src/harness_adapter/tests/mod.rs"),
        ]
    );
    let leaf = test_only_modules(Path::new("/src/task.rs"), "#[cfg(test)] mod tests;");
    assert_eq!(leaf[0], PathBuf::from("/src/task/tests.rs"));
}
