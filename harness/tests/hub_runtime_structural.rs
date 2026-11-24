use proc_macro2::{TokenStream, TokenTree};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

#[derive(Clone, Copy)]
struct Target {
    owner: &'static str,
    members: &'static [&'static str],
}

struct Scan {
    target: Target,
    stack: Vec<String>,
    found: Vec<String>,
}

impl Scan {
    fn note(&mut self, what: &str) {
        let function = self.stack.last().cloned().unwrap_or_default();
        self.found.push(format!("{function}: {what}"));
    }

    fn hits(&self, owner: Option<&syn::Ident>, member: &syn::Ident) -> bool {
        let target = self.target;
        owner.is_some_and(|o| o == target.owner) && target.members.iter().any(|name| member == name)
    }

    fn path(&mut self, path: &syn::Path) {
        let target = self.target;
        let segments: Vec<&syn::Ident> = path.segments.iter().map(|s| &s.ident).collect();
        for pair in segments.windows(2) {
            if self.hits(Some(pair[0]), pair[1]) {
                self.note(&format!("{}::{}", target.owner, pair[1]));
            }
        }
    }

    fn tokens(&mut self, stream: TokenStream) {
        let target = self.target;
        let flat: Vec<TokenTree> = stream.into_iter().collect();
        for (index, token) in flat.iter().enumerate() {
            match token {
                TokenTree::Group(group) => self.tokens(group.stream()),
                TokenTree::Ident(owner) if owner == target.owner => {
                    let joined = is_colon(flat.get(index + 1)) && is_colon(flat.get(index + 2));
                    if let (true, Some(TokenTree::Ident(member))) = (joined, flat.get(index + 3)) {
                        if self.hits(Some(owner), member) {
                            self.note(&format!("{}::{}", target.owner, member));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn enter(&mut self, name: String) {
        self.stack.push(name);
    }
}

impl<'ast> Visit<'ast> for Scan {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_impl(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_cfg_test(&node.attrs) {
            self.enter(node.sig.ident.to_string());
            syn::visit::visit_item_fn(self, node);
            self.stack.pop();
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_cfg_test(&node.attrs) {
            self.enter(node.sig.ident.to_string());
            syn::visit::visit_impl_item_fn(self, node);
            self.stack.pop();
        }
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        self.path(&node.path);
        if let (Some(qself), Some(last)) = (&node.qself, node.path.segments.last()) {
            if let syn::Type::Path(ty) = &*qself.ty {
                let owner = ty.path.segments.last().map(|s| &s.ident);
                if self.hits(owner, &last.ident) {
                    self.note(&format!("<{}>::{}", self.target.owner, last.ident));
                }
            }
        }
        syn::visit::visit_expr_path(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.use_tree(&node.tree);
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if let syn::Type::Path(ty) = &*node.ty {
            if names_owner(ty, self.target.owner) {
                self.note(&format!("alias of {}", self.target.owner));
            }
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
        syn::visit::visit_macro(self, node);
    }
}

impl Scan {
    fn use_tree(&mut self, tree: &syn::UseTree) {
        let target = self.target;
        match tree {
            syn::UseTree::Rename(rename) if rename.ident == target.owner => {
                self.note(&format!("rename of {}", target.owner));
            }
            syn::UseTree::Path(path) => self.use_tree(&path.tree),
            syn::UseTree::Group(group) => group.items.iter().for_each(|t| self.use_tree(t)),
            _ => {}
        }
    }
}

fn scan(text: &str, target: Target) -> Vec<String> {
    let file = syn::parse_file(text).expect("source parses");
    let mut scan = Scan {
        target,
        stack: Vec::new(),
        found: Vec::new(),
    };
    scan.visit_file(&file);
    scan.found
}

fn is_colon(token: Option<&TokenTree>) -> bool {
    matches!(token, Some(TokenTree::Punct(p)) if p.as_char() == ':')
}

fn relative(path: &Path, base: &Path) -> String {
    let tail = path.strip_prefix(base).unwrap();
    tail.to_string_lossy().replace('\\', "/")
}

fn names_owner(ty: &syn::TypePath, owner: &str) -> bool {
    ty.path.segments.last().is_some_and(|s| s.ident == owner)
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn production(target: Target) -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut found = Vec::new();
    for path in files {
        let rel = relative(&path, &src);
        for item in scan(&fs::read_to_string(&path).unwrap(), target) {
            found.push(format!("{rel}: {item}"));
        }
    }
    found
}

const REGISTRY: Target = Target {
    owner: "SessionRegistry",
    members: &["new", "with_oracle"],
};
const RUNTIME: Target = Target {
    owner: "HubRuntime",
    members: &["open", "open_with", "assemble"],
};
const ROUTER: Target = Target {
    owner: "HubRouter",
    members: &["new"],
};
const AUDITORS: Target = Target {
    owner: "ProductionAuditors",
    members: &["new"],
};
const TOOLS: Target = Target {
    owner: "tools",
    members: &["register"],
};
const RESTORE: Target = Target {
    owner: "TaskManager",
    members: &["restore_within_sessions"],
};

fn set(items: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = items.iter().map(ToString::to_string).collect();
    all.sort();
    all
}

fn sorted(mut items: Vec<String>) -> Vec<String> {
    items.sort();
    items
}

#[test]
fn exactly_one_session_registry_is_built_in_production_and_it_is_the_hub_runtimes() {
    assert_eq!(
        sorted(production(REGISTRY)),
        set(&["hub/runtime.rs: assemble: SessionRegistry::new"]),
        "a SessionRegistry is built outside HubRuntime::assemble: the hub and the TaskManager must share one"
    );
}

#[test]
fn only_the_mcp_serve_builder_opens_a_hub_runtime() {
    assert_eq!(
        sorted(production(RUNTIME)),
        set(&[
            "hub/runtime.rs: open: HubRuntime::open_with",
            "hub/runtime.rs: open_with: HubRuntime::assemble",
            "main.rs: build_mcp_task_manager_with_budget: HubRuntime::open",
        ]),
        "delegate and backlog-sync must stay hub-less; pin any new opener deliberately"
    );
}

#[test]
fn the_router_and_the_production_auditors_are_built_only_by_the_hub_runtime() {
    assert_eq!(
        sorted(production(ROUTER)),
        set(&["hub/runtime.rs: assemble: HubRouter::new"])
    );
    assert_eq!(
        sorted(production(AUDITORS)),
        set(&["hub/runtime.rs: open: ProductionAuditors::new"])
    );
}

#[test]
fn the_hub_tools_are_registered_only_by_the_shared_constructor() {
    let found = sorted(production(TOOLS));
    let expected = set(&["task.rs: model_facing_registry: tools::register"]);
    assert_eq!(found, expected);
}

#[test]
fn restore_within_sessions_is_called_only_from_the_hub_runtime_and_is_not_public() {
    assert_eq!(
        sorted(production(RESTORE)),
        set(&["hub/runtime.rs: restore: TaskManager::restore_within_sessions"])
    );
    let task =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/task.rs")).unwrap();
    let file = syn::parse_file(&task).unwrap();
    assert_eq!(restore_visibility(&file), Some("pub(crate)".to_string()));
}

fn restore_visibility(file: &syn::File) -> Option<String> {
    struct V(Option<String>);
    impl<'ast> Visit<'ast> for V {
        fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
            if node.sig.ident == "restore_within_sessions" {
                self.0 = Some(match &node.vis {
                    syn::Visibility::Public(_) => "pub".to_string(),
                    syn::Visibility::Restricted(r) => {
                        format!("pub({})", r.path.get_ident().unwrap())
                    }
                    syn::Visibility::Inherited => String::new(),
                });
            }
            syn::visit::visit_impl_item_fn(self, node);
        }
    }
    let mut visitor = V(None);
    visitor.visit_file(file);
    visitor.0
}

#[test]
fn the_restore_visibility_reader_tells_pub_from_pub_crate() {
    let public = syn::parse_file("impl T { pub fn restore_within_sessions() {} }").unwrap();
    let narrow = syn::parse_file("impl T { pub(crate) fn restore_within_sessions() {} }").unwrap();
    let private = syn::parse_file("impl T { fn restore_within_sessions() {} }").unwrap();
    assert_eq!(restore_visibility(&public), Some("pub".to_string()));
    assert_eq!(restore_visibility(&narrow), Some("pub(crate)".to_string()));
    assert_eq!(restore_visibility(&private), Some(String::new()));
    assert_eq!(
        restore_visibility(&syn::parse_file("fn x() {}").unwrap()),
        None
    );
}

#[test]
fn the_registry_scanner_rejects_a_second_site_a_rename_and_every_bypass() {
    let found = |src: &str| sorted(scan(src, REGISTRY));
    assert_eq!(
        found("fn a() { let r = SessionRegistry::new(s); }"),
        set(&["a: SessionRegistry::new"])
    );
    assert_eq!(
        found("fn a() { SessionRegistry::with_oracle(s, o); }"),
        set(&["a: SessionRegistry::with_oracle"])
    );
    assert_eq!(
        found("fn a() { (SessionRegistry::new)(s); }"),
        set(&["a: SessionRegistry::new"]),
        "a parenthesised, non-path callee"
    );
    assert_eq!(
        found("fn a() { let f = SessionRegistry::new; f(s); }"),
        set(&["a: SessionRegistry::new"]),
        "a function pointer"
    );
    assert_eq!(
        found("fn a() { <SessionRegistry>::new(s); }"),
        set(&["a: <SessionRegistry>::new"]),
        "a qualified path"
    );
    assert_eq!(
        found("fn a() { let r = crate::hub::registry::SessionRegistry::new(s); }"),
        set(&["a: SessionRegistry::new"])
    );
    assert_eq!(
        found("fn a() { std::sync::Arc::new(vec![SessionRegistry::new(s)]); }"),
        set(&["a: SessionRegistry::new"])
    );
    assert_eq!(
        found("fn a() { make!(SessionRegistry::new(s)); }"),
        set(&["a: SessionRegistry::new"]),
        "inside a macro"
    );
    assert_eq!(
        found("use crate::hub::registry::SessionRegistry as R; fn a() { R::new(s); }"),
        set(&[": rename of SessionRegistry"]),
        "a renamed import"
    );
    assert_eq!(
        found("use crate::hub::registry::{Other, SessionRegistry as R};"),
        set(&[": rename of SessionRegistry"])
    );
    assert_eq!(
        found("type R = crate::hub::registry::SessionRegistry;"),
        set(&[": alias of SessionRegistry"])
    );
    assert_eq!(
        found("impl X { fn b(&self) { SessionRegistry::new(s); } }"),
        set(&["b: SessionRegistry::new"])
    );
    assert_eq!(
        found("fn a() { SessionRegistry::new(s); SessionRegistry::new(t); }"),
        set(&["a: SessionRegistry::new", "a: SessionRegistry::new"]),
        "two sites in one function are two sites"
    );
    assert!(found("fn a(r: Arc<SessionRegistry>) { r.tracks(x); }").is_empty());
    assert!(found("#[cfg(test)] fn a() { SessionRegistry::new(s); }").is_empty());
    assert!(found("#[cfg(test)] mod t { fn a() { SessionRegistry::new(s); } }").is_empty());
    assert!(found("#[cfg(test)] impl X { fn a() { SessionRegistry::new(s); } }").is_empty());
    assert!(found("impl X { #[cfg(test)] fn a() { SessionRegistry::new(s); } }").is_empty());
}
