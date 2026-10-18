use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
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

fn dispatch_name(name: &str) -> bool {
    name.starts_with("submit") || name == "enqueue" || name.starts_with("enqueue_")
}

fn visibility(vis: &syn::Visibility) -> &'static str {
    match vis {
        syn::Visibility::Public(_) => "pub",
        syn::Visibility::Restricted(_) => "pub(restricted)",
        syn::Visibility::Inherited => "private",
    }
}

type Calls = Vec<(String, Vec<usize>)>;

#[derive(Default)]
struct Scan {
    file: String,
    owner: Vec<String>,
    function: Vec<String>,
    branch: Vec<usize>,
    branches: usize,
    declarations: BTreeSet<String>,
    calls: BTreeMap<String, usize>,
    macro_mentions: BTreeSet<String>,
    sequences: BTreeMap<String, Calls>,
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
        let branch = self.branch.clone();
        self.sequences
            .entry(function.clone())
            .or_default()
            .push((name.to_string(), branch));
        if dispatch_name(name) {
            *self
                .calls
                .entry(format!("{}: {function} uses {name}", self.file))
                .or_default() += 1;
        }
    }

    fn scan_tokens(&mut self, tokens: TokenStream) {
        for token in tokens {
            match token {
                TokenTree::Group(group) => self.scan_tokens(group.stream()),
                TokenTree::Ident(ident) => {
                    let name = ident.to_string();
                    if self.function.is_empty() {
                        if dispatch_name(&name) {
                            self.macro_mentions
                                .insert(format!("{}: item macro mentions {name}", self.file));
                        }
                    } else {
                        self.record_call(&name);
                    }
                }
                _ => {}
            }
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
        self.function.push(qualified);
        true
    }

    fn conditional(&mut self, visit: impl FnOnce(&mut Self)) {
        self.branches += 1;
        self.branch.push(self.branches);
        visit(self);
        self.branch.pop();
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

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        syn::visit::visit_expr_method_call(self, node);
        self.record_call(&node.method.to_string());
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        syn::visit::visit_expr_path(self, node);
        if let Some(segment) = node.path.segments.last() {
            self.record_call(&segment.ident.to_string());
        }
    }

    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.visit_expr(&node.cond);
        self.conditional(|scan| scan.visit_block(&node.then_branch));
        if let Some((_, otherwise)) = &node.else_branch {
            self.conditional(|scan| scan.visit_expr(otherwise));
        }
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        self.conditional(|scan| syn::visit::visit_arm(scan, node));
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        self.conditional(|scan| syn::visit::visit_expr_closure(scan, node));
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.visit_expr(&node.expr);
        self.conditional(|scan| scan.visit_block(&node.body));
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.conditional(|scan| syn::visit::visit_expr_while(scan, node));
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.conditional(|scan| syn::visit::visit_expr_loop(scan, node));
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        self.visit_expr(&node.left);
        if matches!(node.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) {
            self.conditional(|scan| scan.visit_expr(&node.right));
        } else {
            self.visit_expr(&node.right);
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.scan_tokens(node.tokens.clone());
    }
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => last_segment(&path.path),
        syn::Type::Reference(reference) => type_name(&reference.elem),
        _ => String::new(),
    }
}

fn last_segment(path: &syn::Path) -> String {
    path.segments
        .last()
        .map_or(String::new(), |s| s.ident.to_string())
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
            scan_source(&relative, &fs::read_to_string(&path).unwrap())
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
fn no_macro_outside_a_function_mentions_a_submit_or_enqueue_name() {
    assert_eq!(union(|scan| &scan.macro_mentions), BTreeSet::new());
}

#[test]
fn every_use_of_a_submit_or_enqueue_name_is_counted_per_function() {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for scan in scan_all() {
        for (key, count) in scan.calls {
            *counts.entry(key).or_default() += count;
        }
    }
    let expected: BTreeMap<String, usize> = [
        ("backlog.rs: ManagerSink::enqueue uses submit_task", 1),
        ("backlog.rs: backlog_sync uses enqueue", 1),
        ("main.rs: run_delegate uses submit_task", 1),
        ("mcp/handlers.rs: handle_assign_task uses submit_spawn", 1),
        (
            "task.rs: TaskManager::submit_spawn uses submit_with_identity",
            1,
        ),
        ("task.rs: TaskManager::submit_task uses enqueue", 1),
        (
            "task.rs: TaskManager::submit_with_identity uses submit_task",
            1,
        ),
    ]
    .into_iter()
    .map(|(key, count)| (key.to_string(), count))
    .collect();
    assert_eq!(
        counts, expected,
        "a use of a submit/enqueue function appeared, vanished, was duplicated, or moved to another function"
    );
}

fn calls_in(function: &str, file: &str) -> Calls {
    scan_all()
        .into_iter()
        .filter(|scan| scan.file == file)
        .filter_map(|mut scan| scan.sequences.remove(function))
        .flatten()
        .collect()
}

fn dominates(calls: &Calls, first: &str, then: &str) -> bool {
    calls
        .iter()
        .position(|(name, _)| name == first)
        .is_some_and(|index| {
            let guard = &calls[index].1;
            let dispatches: Vec<_> = calls
                .iter()
                .enumerate()
                .filter(|(_, (name, _))| name == then)
                .collect();
            !dispatches.is_empty()
                && dispatches
                    .iter()
                    .all(|(at, (_, path))| *at > index && path.starts_with(guard))
        })
}

#[test]
fn the_mcp_assign_path_checks_the_gate_on_every_path_to_submitting() {
    let calls = calls_in("handle_assign_task", "mcp/handlers.rs");
    assert!(dominates(&calls, "check", "submit_spawn"));
}

#[test]
fn backlog_sync_checks_the_gate_on_every_path_to_its_enqueue() {
    let calls = calls_in("backlog_sync", "backlog.rs");
    assert!(dominates(&calls, "check", "enqueue"));
}

#[test]
fn the_backlog_sinks_verify_the_proof_on_every_path_to_dispatching() {
    for (function, dispatch) in [
        ("StoreSink::enqueue", "insert"),
        ("ManagerSink::enqueue", "submit_task"),
    ] {
        let calls = calls_in(function, "backlog.rs");
        assert!(dominates(&calls, "ensure_audited", dispatch), "{function}");
    }
}

#[test]
fn a_check_in_a_dead_branch_does_not_guard_a_dispatch() {
    let scan = scan_source(
        "x.rs",
        "fn f(g: G, s: S) { if false { g.check(); } s.enqueue(); }",
    );
    let calls = &scan.sequences["f"];
    assert!(!dominates(calls, "check", "enqueue"));
    let scan = scan_source("x.rs", "fn f(g: G, s: S) { g.check(); s.enqueue(); }");
    assert!(dominates(&scan.sequences["f"], "check", "enqueue"));
    assert!(!dominates(&scan.sequences["f"], "missing", "enqueue"));
    assert!(!dominates(&scan.sequences["f"], "check", "missing"));
    let scan = scan_source("x.rs", "fn f(g: G, s: S) { s.enqueue(); g.check(); }");
    assert!(!dominates(&scan.sequences["f"], "check", "enqueue"));
}

#[test]
fn raw_task_manager_submission_methods_are_not_public() {
    let declared = union(|scan| &scan.declarations);
    for name in ["submit_with_identity", "submit_task"] {
        let restricted = format!("task.rs: TaskManager::{name} (pub(restricted))");
        assert!(declared.contains(&restricted));
        assert!(
            !declared.contains(&format!("task.rs: TaskManager::{name} (pub)")),
            "`{name}` must not be public"
        );
    }
}

#[test]
fn function_references_macro_bodies_and_item_macros_are_seen() {
    let reference = scan_source(
        "x.rs",
        "fn f() { let g = TaskManager::submit_task; let h = xs.map(TaskManager::submit_task); }",
    );
    assert_eq!(reference.calls["x.rs: f uses submit_task"], 2);
    let in_macro = scan_source("x.rs", "fn f() { vec![m.submit_task(1)]; }");
    assert_eq!(in_macro.calls["x.rs: f uses submit_task"], 1);
    let definition = scan_source(
        "x.rs",
        "macro_rules! hit { ($m:expr) => { $m.submit_task(1) }; }",
    );
    assert!(definition
        .macro_mentions
        .contains("x.rs: item macro mentions submit_task"));
    let generated = scan_source("x.rs", "gen! { pub async fn submit_zz() {} }");
    assert!(generated
        .macro_mentions
        .contains("x.rs: item macro mentions submit_zz"));
}

#[test]
fn cfg_not_test_and_cfg_any_test_items_are_scanned_as_production() {
    let scan = scan_source(
        "x.rs",
        "#[cfg(not(test))] pub fn submit_a() {}
#[cfg(any(test, feature = \"x\"))] pub fn submit_b() {}
#[cfg(test)] pub fn submit_c() {}
#[cfg(all(test, unix))] pub fn submit_d() {}
#[cfg(test)] impl T { pub fn submit_e() {} }
#[cfg(test)] trait Q { fn submit_f(); }
#[cfg(test)] mod m { pub fn submit_g() {} }
impl R for &X { fn submit_h() {} }
trait P { fn submit_i(); }
impl R for (A, B) { fn submit_j() {} }
",
    );
    assert_eq!(
        scan.declarations,
        expected(&[
            "x.rs: submit_a (pub)",
            "x.rs: submit_b (pub)",
            "x.rs: X::submit_h (private)",
            "x.rs: P::submit_i (private)",
            "x.rs: ::submit_j (private)",
        ])
    );
}
