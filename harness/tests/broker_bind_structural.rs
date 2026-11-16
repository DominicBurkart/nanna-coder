use proc_macro2::{TokenStream, TokenTree};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

#[path = "support/test_module_files.rs"]
mod test_module_files;
use test_module_files::{
    cfg_test_file_modules, is_exact_test_file_module, path_attribute_files, skipped_files,
};

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

const PRODUCTION_BIND_CALLERS_IN_THIS_SLICE: usize = 1;

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
    audited_refs: BTreeSet<String>,
    aliases: BTreeSet<String>,
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
        let mut previous_ref = false;
        for token in tokens {
            match token {
                TokenTree::Group(group) => {
                    if previous_ref && group.delimiter() == proc_macro2::Delimiter::Brace {
                        let context = self.context();
                        self.audited_refs
                            .insert(format!("{}: {context}", self.file));
                    }
                    previous_ref = false;
                    self.scan_tokens(group.stream());
                }
                TokenTree::Ident(ident) => {
                    let name = ident.to_string();
                    previous_ref = name == "AuditedRef" || self.aliases.contains(&name);
                    self.record(&name);
                }
                _ => previous_ref = false,
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
            let vis = visibility(&node.vis);
            let file = &self.file;
            self.shapes
                .insert(format!("{file}: struct ModelFacingLayers is {vis}"));
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

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        syn::visit::visit_item_type(self, node);
        let target = type_name(&node.ty);
        if target == "AuditedRef" || self.aliases.contains(&target) {
            self.aliases.insert(node.ident.to_string());
        }
    }

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        syn::visit::visit_use_rename(self, node);
        let name = plain(&node.ident.to_string()).to_string();
        if name == "AuditedRef" {
            self.aliases.insert(node.rename.to_string());
        }
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

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        syn::visit::visit_expr_struct(self, node);
        let name = last_segment(&node.path);
        let in_ref = name == "Self" && self.owner.last().is_some_and(|o| o == "AuditedRef");
        if name == "AuditedRef" || in_ref || self.aliases.contains(&name) {
            self.audited_refs
                .insert(format!("{}: {}", self.file, self.context()));
        }
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
    for _ in 0..4 {
        for item in &syntax.items {
            scan.visit_item(item);
        }
    }
    scan
}

fn scan_all() -> Vec<Scan> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    let sources: Vec<(String, String)> = files
        .into_iter()
        .map(|path| {
            let text = path.strip_prefix(&root).unwrap().to_string_lossy();
            (text.replace('\\', "/"), fs::read_to_string(&path).unwrap())
        })
        .collect();
    scan_sources(sources)
}

fn scan_sources(sources: Vec<(String, String)>) -> Vec<Scan> {
    let skipped = skipped_files(&sources).unwrap_or_else(|reason| panic!("{reason}"));
    sources
        .iter()
        .filter(|(file, _)| !skipped.contains(file))
        .map(|(file, source)| scan_source(file, source))
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
fn the_audited_reference_bind_workspace_requires_is_minted_only_by_require_audited() {
    assert_eq!(
        union(|scan| &scan.audited_refs),
        expected(&["harness_adapter/binding.rs: require_audited"])
    );
    let scan = scanned("fn sneak() { let _ = AuditedRef { identity }; } #[cfg(test)] fn t() { AuditedRef { identity }; }");
    assert_eq!(
        scan.audited_refs.iter().collect::<Vec<_>>(),
        vec!["x.rs: sneak"]
    );
    let hidden = scanned(
        "impl AuditedRef<'_> { fn again(identity: &I) -> Self { Self { identity } } } fn via_alias() { let _ = Alias { identity }; } fn via_rename() { let _ = Renamed { identity }; } type Alias = AuditedRef<'static>; use crate::AuditedRef as Renamed;",
    );
    let chained = scanned(
        "fn deep() { let _ = Outer { identity }; } type Outer = Middle; type Middle = AuditedRef<'static>; macro_rules! forge { () => { AuditedRef { identity } } }",
    );
    assert_eq!(
        chained.audited_refs.iter().collect::<Vec<_>>(),
        vec!["x.rs: ", "x.rs: deep"]
    );
    assert_eq!(
        hidden.audited_refs.iter().collect::<Vec<_>>(),
        vec![
            "x.rs: AuditedRef::again",
            "x.rs: via_alias",
            "x.rs: via_rename"
        ]
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
        #[cfg(any(test, unix))]
        fn anyfn() { create_tool_registry(p); }
        #[cfg(all(test, unix))]
        fn allfn() { create_tool_registry(p); }
        impl R for &Y { fn k(&self) { create_tool_registry(p); } }
        impl R for [u8; 2] { fn k(&self) { create_tool_registry(p); } }
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
            "x.rs: ::k calls create_tool_registry",
            "x.rs: T::k calls create_tool_registry",
            "x.rs: Y::k calls create_tool_registry",
            "x.rs: anyfn calls create_tool_registry",
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
fn test_only_file_modules_follow_the_same_rule_as_the_audit_hook_guard() {
    let found = cfg_test_file_modules(
        "harness_adapter/mod.rs",
        "#[cfg(test)] mod tests; mod live;",
    );
    assert_eq!(
        found,
        vec![
            "harness_adapter/tests.rs".to_string(),
            "harness_adapter/tests/mod.rs".to_string()
        ]
    );
    let leaf = cfg_test_file_modules("task.rs", "#[cfg(test)] mod tests;");
    assert_eq!(leaf[0], "task/tests.rs");
    let twin = "#[cfg(test)] mod t; #[cfg(not(test))] mod t;";
    assert!(cfg_test_file_modules("a/mod.rs", twin).is_empty());
    let pathed = "#[cfg(test)] mod x; #[path = \"x.rs\"] mod y;";
    let sources = [
        ("a/mod.rs".to_string(), pathed.to_string()),
        (
            "a/x.rs".to_string(),
            "fn f() { create_tool_registry(p); }".to_string(),
        ),
    ];
    assert_eq!(path_attribute_files(&sources), vec!["a/mod.rs".to_string()]);
    assert!(skipped_files(&sources).unwrap_err().contains("a/mod.rs"));
    let module: syn::ItemMod = syn::parse_str("#[cfg(test)] mod m;").unwrap();
    assert!(is_exact_test_file_module(&module));
}

#[test]
#[should_panic(expected = "cannot reason about module files")]
fn the_scan_fails_when_a_path_attribute_exists_anywhere() {
    let sources = vec![("a.rs".to_string(), "#[path = \"b.rs\"] mod m;".to_string())];
    let _ = scan_sources(sources);
}

#[test]
fn the_scan_skips_only_cfg_test_file_modules_when_no_path_attribute_exists() {
    let sources = vec![
        (
            "a/mod.rs".to_string(),
            "#[cfg(test)] mod t; mod u;".to_string(),
        ),
        (
            "a/t.rs".to_string(),
            "fn f() { create_tool_registry(p); }".to_string(),
        ),
        (
            "a/u.rs".to_string(),
            "fn f() { create_tool_registry(p); }".to_string(),
        ),
    ];
    let scans = scan_sources(sources);
    let files: Vec<&str> = scans.iter().map(|scan| scan.file.as_str()).collect();
    assert_eq!(files, vec!["a/mod.rs", "a/u.rs"]);
}
