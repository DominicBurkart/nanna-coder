use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

#[derive(Default)]
struct Owners {
    impl_type: Vec<String>,
    stack: Vec<String>,
    idents: BTreeMap<String, Vec<String>>,
}

impl Owners {
    fn record(&mut self, name: String) {
        let owner = self.stack.last().cloned().unwrap_or_default();
        self.idents.entry(owner).or_default().push(name);
    }

    fn tokens(&mut self, stream: TokenStream) {
        for token in stream {
            match token {
                TokenTree::Ident(ident) => self.record(ident.to_string()),
                TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Owners {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_cfg_test(&node.attrs) {
            self.stack.push(node.sig.ident.to_string());
            syn::visit::visit_item_fn(self, node);
            self.stack.pop();
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let named = match &*node.self_ty {
            syn::Type::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        self.impl_type.push(named.unwrap_or_default());
        syn::visit::visit_item_impl(self, node);
        self.impl_type.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_cfg_test(&node.attrs) {
            let owner = self.impl_type.last().cloned().unwrap_or_default();
            self.stack.push(format!("{owner}::{}", node.sig.ident));
            syn::visit::visit_impl_item_fn(self, node);
            self.stack.pop();
        }
    }

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.record(ident.to_string());
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
    }
}

fn users(source: &str, name: &str) -> BTreeSet<String> {
    let file = syn::parse_file(source).unwrap();
    let mut owners = Owners::default();
    owners.visit_file(&file);
    let hit = |idents: &Vec<String>| idents.iter().any(|ident| ident == name);
    let found = owners.idents.iter().filter(|(_, idents)| hit(idents));
    found.map(|(owner, _)| owner.clone()).collect()
}

fn source_files() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    let mut files = Vec::new();
    for path in paths {
        let relative = path.strip_prefix(&root).unwrap();
        let name = relative.to_string_lossy().replace('\\', "/");
        let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
        files.push((name, text));
    }
    files
}

fn everywhere(name: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (file, text) in source_files() {
        for owner in users(&text, name) {
            found.insert(format!("{file}: {owner}"));
        }
    }
    found
}

fn expected(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

fn source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    String::from_utf8_lossy(&fs::read(path).unwrap()).into_owned()
}

#[derive(Default)]
struct Structs {
    fields: BTreeMap<String, BTreeSet<String>>,
}

impl<'ast> Visit<'ast> for Structs {
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        let name_of = |field: &syn::Field| field.ident.as_ref().map(ToString::to_string);
        let names: BTreeSet<String> = node.fields.iter().filter_map(name_of).collect();
        self.fields.insert(node.ident.to_string(), names);
    }
}

fn field_names(source: &str, name: &str) -> BTreeSet<String> {
    let file = syn::parse_file(source).unwrap();
    let mut structs = Structs::default();
    structs.visit_file(&file);
    structs.fields.remove(name).unwrap()
}

#[test]
fn owners_are_keyed_by_the_impl_type_and_the_function_name() {
    let text = "impl Cell { fn bind() { go(); } } impl Tool for Other { fn bind() { go(); } } impl Marker for [u8; 2] { fn run() { go(); } } fn free() { go(); }";
    assert_eq!(
        users(text, "go"),
        expected(&["Cell::bind", "Other::bind", "::run", "free"])
    );
}

#[test]
fn only_the_pull_request_open_tool_binds_a_pull_request() {
    assert_eq!(
        everywhere("bind"),
        expected(&[
            "pr_binding.rs: PrBindingCell::bind",
            "pr_tools.rs: GithubPrOpenTool::execute",
            "apprun/ports.rs: port_is_free",
            "sandbox_tools.rs: FakeSandboxTarget::deploy",
        ])
    );
    assert_eq!(
        everywhere("PrBinding"),
        expected(&[
            "pr_binding.rs: ",
            "pr_binding.rs: PrBindingCell::bind",
            "pr_binding.rs: PrBindingCell::get",
            "pr_binding.rs: PrBindingCell::slot",
            "pr_tools.rs: GithubPrOpenTool::execute",
        ])
    );
}

#[test]
fn the_binding_is_read_for_a_lease_only_through_the_repository_checked_helper() {
    let tools = source("src/tools.rs");
    assert_eq!(
        users(&tools, "number_for"),
        expected(&["ToolRegistry::review_action"])
    );
    assert_eq!(
        users(&tools, "pr_binding"),
        expected(&[
            "",
            "ToolRegistry::new",
            "ToolRegistry::pr_binding",
            "ToolRegistry::review_action"
        ])
    );
    let sandbox = source("src/sandbox_tools.rs");
    assert!(users(&sandbox, "same_repo").contains("SandboxDeployTool::execute"));
    assert_eq!(
        users(&tools, "lease_pr"),
        expected(&["ToolRegistry::review_action"])
    );
}

#[test]
fn nothing_a_task_controls_gains_a_pull_request_or_window_field() {
    let queued = source("src/scheduler/mod.rs");
    assert_eq!(
        field_names(&queued, "QueuedTask"),
        expected(&[
            "id",
            "description",
            "repo_path",
            "branch",
            "model",
            "max_iterations",
            "submitted_at",
            "seq",
            "not_before",
            "identity_hint",
            "origin",
        ])
    );
    let tools = source("src/tools.rs");
    assert_eq!(
        field_names(&tools, "ActionSubject"),
        expected(&[
            "task_id",
            "max_effect",
            "window",
            "repo",
            "branch",
            "pr",
            "environment",
            "paths",
            "description",
        ])
    );
    let sandbox = source("src/sandbox_tools.rs");
    assert_eq!(
        field_names(&sandbox, "SandboxContext"),
        expected(&[
            "target",
            "probe",
            "sandboxes",
            "accountant",
            "task_id",
            "pr_binding"
        ])
    );
    assert!(field_names(&sandbox, "SandboxDeployTool").contains("binding"));
    assert!(!field_names(&sandbox, "SandboxDeployTool").contains("pr"));
}

#[derive(Default)]
struct Keys {
    inside: Vec<String>,
    found: BTreeSet<String>,
    gets: BTreeSet<String>,
}

impl Keys {
    fn tokens(&mut self, stream: TokenStream) {
        let tokens: Vec<TokenTree> = stream.into_iter().collect();
        for (index, token) in tokens.iter().enumerate() {
            match token {
                TokenTree::Literal(literal) => {
                    let text = literal.to_string();
                    let next = tokens.get(index + 1);
                    let colon = matches!(next, Some(TokenTree::Punct(p)) if p.as_char() == ':');
                    if colon && text.starts_with('"') {
                        self.found.insert(text.trim_matches('"').to_string());
                    }
                }
                TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Keys {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.inside.push(node.sig.ident.to_string());
        syn::visit::visit_item_fn(self, node);
        self.inside.pop();
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if self.inside.last().is_some() {
            self.tokens(node.tokens.clone());
        }
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let literal = node.args.first().and_then(|arg| match arg {
            syn::Expr::Lit(lit) => match &lit.lit {
                syn::Lit::Str(text) => Some(text.value()),
                _ => None,
            },
            _ => None,
        });
        if node.method == "get" && self.inside.last().is_some() {
            self.gets.extend(literal);
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

fn keys_in(source: &str, function: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let file = syn::parse_file(source).unwrap();
    let mut item = None;
    for candidate in &file.items {
        match candidate {
            syn::Item::Fn(found) if found.sig.ident == function => item = Some(found.clone()),
            syn::Item::Impl(block) => {
                for member in &block.items {
                    if let syn::ImplItem::Fn(method) = member {
                        if method.sig.ident == function {
                            item = Some(syn::ItemFn {
                                attrs: method.attrs.clone(),
                                vis: syn::Visibility::Inherited,
                                sig: method.sig.clone(),
                                block: Box::new(method.block.clone()),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let item = item.unwrap();
    let mut keys = Keys::default();
    keys.visit_item_fn(&item);
    (keys.found, keys.gets)
}

#[test]
fn assign_task_and_plan_task_take_no_pull_request_or_window_argument() {
    let mcp = source("src/mcp/mod.rs");
    let (listed, _) = keys_in(&mcp, "base_tool_list");
    let handlers = source("src/mcp/handlers.rs");
    let (_, read) = keys_in(&handlers, "handle_assign_task");
    assert_eq!(
        read,
        expected(&[
            "description",
            "repo_path",
            "branch",
            "model",
            "max_iterations",
            "identity",
            "requested_effect",
            "dev_loop",
        ])
    );
    for forbidden in [
        "pr",
        "pr_number",
        "number",
        "window",
        "pull_request",
        "head_sha",
    ] {
        assert!(!listed.contains(forbidden), "{forbidden}");
        assert!(!read.contains(forbidden), "{forbidden}");
    }
    let plan = source("src/mcp/plan.rs");
    let (offered, parsed) = keys_in(&plan, "tool_definition");
    let (_, parse_reads) = keys_in(&plan, "parse_args");
    for forbidden in [
        "pr",
        "pr_number",
        "number",
        "window",
        "pull_request",
        "head_sha",
    ] {
        assert!(!offered.contains(forbidden), "{forbidden}");
        assert!(!parsed.contains(forbidden), "{forbidden}");
        assert!(!parse_reads.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn the_open_tool_and_the_sandbox_tools_share_the_registrys_own_cell() {
    let tools = source("src/pr_tools.rs");
    assert!(users(&tools, "with_binding").contains("register_granted_at"));
    assert!(users(&tools, "pr_binding").contains("register_granted_at"));
    assert!(!users(&tools, "PrBindingCell").contains("register_granted_at"));
    let workspace = source("src/workspace.rs");
    assert!(users(&workspace, "pr_binding").contains("TaskWorkspace::build_tool_registry_for"));
    assert!(!users(&workspace, "PrBindingCell").contains("TaskWorkspace::build_tool_registry_for"));
}

struct Open {
    order: Vec<String>,
}

impl<'ast> Visit<'ast> for Open {
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let target = matches!(&*node.self_ty, syn::Type::Path(path) if path.path.is_ident("GithubPrOpenTool"));
        if target && node.trait_.is_some() {
            for member in &node.items {
                if let syn::ImplItem::Fn(method) = member {
                    if method.sig.ident == "execute" {
                        let mut owners = Owners::default();
                        owners.stack.push("execute".to_string());
                        owners.visit_block(&method.block);
                        self.order = owners.idents.remove("execute").unwrap_or_default();
                    }
                }
            }
        }
        syn::visit::visit_item_impl(self, node);
    }
}

#[test]
fn the_pull_request_is_bound_only_after_the_host_created_it_from_host_resolved_values() {
    let tools = source("src/pr_tools.rs");
    let file = syn::parse_file(&tools).unwrap();
    let mut open = Open { order: Vec::new() };
    open.visit_file(&file);
    let at = |name: &str| open.order.iter().position(|ident| ident == name).unwrap();
    assert!(at("create_draft_pull_request") < at("bind"));
    assert!(at("created") < at("bind"));
    assert!(at("resolve_repo") < at("bind"));
    assert!(at("run_git") < at("create_draft_pull_request"));
    assert_eq!(
        open.order.iter().filter(|ident| *ident == "bind").count(),
        1
    );
    let after = &open.order[at("bind")..];
    assert!(!after.iter().any(|ident| ident == "args"));
}
