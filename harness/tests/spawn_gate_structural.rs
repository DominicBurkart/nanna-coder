use proc_macro2::{TokenStream, TokenTree};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

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

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .meta
                .require_list()
                .map(|list| {
                    list.tokens
                        .clone()
                        .into_iter()
                        .any(|token| matches!(token, TokenTree::Ident(ident) if ident == "test"))
                })
                .unwrap_or(false)
    })
}

fn dispatch_name(name: &str) -> bool {
    name.starts_with("submit") || name.starts_with("enqueue")
}

fn visibility(vis: &syn::Visibility) -> &'static str {
    match vis {
        syn::Visibility::Public(_) => "pub",
        syn::Visibility::Restricted(_) => "pub(restricted)",
        syn::Visibility::Inherited => "private",
    }
}

#[derive(Default)]
struct Scan {
    file: String,
    owner: Vec<String>,
    function: Vec<String>,
    declarations: BTreeSet<String>,
    calls: BTreeSet<String>,
    sequences: Vec<(String, Vec<String>)>,
}

impl Scan {
    fn qualified(&self, name: &str) -> String {
        match self.owner.last() {
            Some(owner) => format!("{owner}::{name}"),
            None => name.to_string(),
        }
    }

    fn record_call(&mut self, name: &str) {
        let Some(function) = self.function.last().cloned() else {
            return;
        };
        if let Some((_, sequence)) = self.sequences.iter_mut().find(|(f, _)| *f == function) {
            sequence.push(name.to_string());
        }
        if dispatch_name(name) {
            self.calls
                .insert(format!("{}: {function} calls {name}", self.file));
        }
    }

    fn scan_tokens(&mut self, tokens: TokenStream) {
        let mut previous: Option<TokenTree> = None;
        for token in tokens {
            if let TokenTree::Group(group) = &token {
                if let Some(TokenTree::Ident(ident)) = &previous {
                    if group.delimiter() == proc_macro2::Delimiter::Parenthesis {
                        self.record_call(&ident.to_string());
                    }
                }
                self.scan_tokens(group.stream());
            }
            previous = Some(token);
        }
    }

    fn enter_fn(&mut self, name: &str, vis: &syn::Visibility, attrs: &[syn::Attribute]) -> bool {
        if is_cfg_test(attrs) {
            return false;
        }
        let qualified = self.qualified(name);
        if dispatch_name(name) {
            self.declarations
                .insert(format!("{}: {qualified} ({})", self.file, visibility(vis)));
        }
        self.function.push(qualified.clone());
        self.sequences.push((qualified, Vec::new()));
        true
    }
}

impl<'ast> Visit<'ast> for Scan {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        let owner = match &*node.self_ty {
            syn::Type::Path(path) => path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.owner.push(owner);
        syn::visit::visit_item_impl(self, node);
        self.owner.pop();
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        self.owner.push(node.ident.to_string());
        syn::visit::visit_item_trait(self, node);
        self.owner.pop();
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
        if self.enter_fn(
            &node.sig.ident.to_string(),
            &syn::Visibility::Inherited,
            &node.attrs,
        ) {
            syn::visit::visit_trait_item_fn(self, node);
            self.function.pop();
        }
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        syn::visit::visit_expr_method_call(self, node);
        self.record_call(&node.method.to_string());
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        syn::visit::visit_expr_call(self, node);
        if let syn::Expr::Path(path) = &*node.func {
            if let Some(segment) = path.path.segments.last() {
                self.record_call(&segment.ident.to_string());
            }
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.scan_tokens(node.tokens.clone());
    }
}

fn scan_all() -> Vec<Scan> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let syntax = syn::parse_file(&fs::read_to_string(&path).unwrap()).unwrap();
            let mut scan = Scan {
                file: relative,
                ..Scan::default()
            };
            for item in &syntax.items {
                scan.visit_item(item);
            }
            scan
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
fn the_set_of_submit_and_enqueue_functions_is_pinned() {
    assert_eq!(
        union(|scan| &scan.declarations),
        expected(&[
            "backlog.rs: BacklogSink::enqueue (private)",
            "backlog.rs: ManagerSink::enqueue (private)",
            "backlog.rs: StoreSink::enqueue (private)",
            "mcp/client.rs: NannaMcpClient::submit_task (pub)",
            "scheduler/dispatch.rs: Dispatcher::enqueue (pub)",
            "task.rs: TaskManager::submit_spawn (pub)",
            "task.rs: TaskManager::submit_task (pub(restricted))",
            "task.rs: TaskManager::submit_with_identity (pub(restricted))",
        ]),
        "a submit/enqueue function was added, removed or had its visibility changed; every such function must go through SpawnGate and be reviewed here"
    );
}

#[test]
fn every_call_to_a_submit_or_enqueue_function_is_from_a_pinned_function() {
    assert_eq!(
        union(|scan| &scan.calls),
        expected(&[
            "backlog.rs: ManagerSink::enqueue calls submit_task",
            "backlog.rs: backlog_sync calls enqueue",
            "main.rs: run_delegate calls submit_task",
            "mcp/handlers.rs: handle_assign_task calls submit_spawn",
            "task.rs: TaskManager::submit_spawn calls submit_with_identity",
            "task.rs: TaskManager::submit_task calls enqueue",
            "task.rs: TaskManager::submit_with_identity calls submit_task",
        ]),
        "a new call to a submit/enqueue function appeared, or an existing one moved to another function"
    );
}

fn calls_in(function: &str, file: &str) -> Vec<String> {
    scan_all()
        .into_iter()
        .filter(|scan| scan.file == file)
        .flat_map(|scan| scan.sequences)
        .filter(|(name, _)| name == function)
        .flat_map(|(_, sequence)| sequence)
        .collect()
}

fn position(calls: &[String], name: &str) -> usize {
    calls
        .iter()
        .position(|call| call == name)
        .unwrap_or_else(|| panic!("`{name}` is not called"))
}

#[test]
fn the_mcp_assign_path_checks_the_gate_before_submitting() {
    let calls = calls_in("handle_assign_task", "mcp/handlers.rs");
    assert!(position(&calls, "check") < position(&calls, "submit_spawn"));
    assert_eq!(calls.iter().filter(|c| *c == "submit_spawn").count(), 1);
}

#[test]
fn backlog_sync_checks_the_gate_before_its_single_enqueue() {
    let calls = calls_in("backlog_sync", "backlog.rs");
    assert!(position(&calls, "check") < position(&calls, "enqueue"));
    assert_eq!(calls.iter().filter(|c| *c == "enqueue").count(), 1);
}

#[test]
fn the_backlog_sinks_demand_an_allowed_proof_and_check_it_before_dispatching() {
    for (function, dispatch) in [
        ("StoreSink::enqueue", "insert"),
        ("ManagerSink::enqueue", "submit_task"),
    ] {
        let calls = calls_in(function, "backlog.rs");
        assert!(
            position(&calls, "ensure_audited") < position(&calls, dispatch),
            "{function} must verify the proof before `{dispatch}`"
        );
    }
}

#[test]
fn raw_task_manager_submission_methods_are_not_public() {
    let declared = union(|scan| &scan.declarations);
    for name in ["submit_with_identity", "submit_task"] {
        assert!(
            declared.contains(&format!("task.rs: TaskManager::{name} (pub(restricted))")),
            "`{name}` must be pub(crate)"
        );
        assert!(
            !declared.contains(&format!("task.rs: TaskManager::{name} (pub)")),
            "`{name}` must not be public"
        );
    }
}
