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

fn token_use(owner: &str, name: &str) -> Option<&'static str> {
    match (owner, name) {
        ("AuditedIdentity", "split") => Some("AuditedIdentity::split"),
        ("BoundIdentity", "Audited") => Some("BoundIdentity::Audited"),
        _ => None,
    }
}

fn listed_derives(attr: &syn::Attribute) -> Vec<String> {
    let parse = syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated;
    let listed = attr.parse_args_with(parse).unwrap();
    listed.iter().map(last_segment).collect()
}

fn attribute_shapes(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut shapes = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("derive") {
            shapes.push(format!("derives {}", listed_derives(attr).join(", ")));
        } else {
            shapes.push(format!("attribute {}", last_segment(attr.path())));
        }
    }
    shapes
}

fn mentions_token(ty: &syn::Type) -> bool {
    type_name(ty) == "AuditedIdentity"
}

fn plain(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn dispatch_name(name: &str) -> bool {
    let name = plain(name);
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
    item_uses: BTreeSet<String>,
    field_writes: BTreeSet<String>,
    functions: BTreeSet<String>,
    sequences: BTreeMap<String, Calls>,
    audit_tokens: BTreeSet<String>,
    bound_mentions: BTreeMap<String, usize>,
    bound_ops: BTreeMap<String, usize>,
    bound_guarded: BTreeMap<String, usize>,
    cond_guard: Vec<usize>,
    guards: BTreeMap<String, BTreeMap<usize, usize>>,
}

impl Scan {
    fn qualified(&self, name: &str) -> String {
        match self.owner.last() {
            Some(owner) => format!("{owner}::{name}"),
            None => name.to_string(),
        }
    }

    fn record_call(&mut self, name: &str) {
        let name = plain(name);
        let Some(function) = self.function.last().cloned() else {
            if dispatch_name(name) {
                self.item_uses
                    .insert(format!("{}: item-level use of {name}", self.file));
            }
            return;
        };
        let branch = self.branch.clone();
        let sequence = self.sequences.entry(function.clone()).or_default();
        sequence.push((name.to_string(), branch));
        let index = sequence.len() - 1;
        if let Some(guard) = self.cond_guard.last().copied() {
            self.guards
                .entry(function.clone())
                .or_default()
                .insert(index, guard);
        }
        if dispatch_name(name) {
            *self
                .calls
                .entry(format!("{}: {function} uses {name}", self.file))
                .or_default() += 1;
        }
    }

    fn record_bound_mention(&mut self) {
        let function = self.function.last().cloned().unwrap_or_default();
        *self
            .bound_mentions
            .entry(format!("{}: {function}", self.file))
            .or_default() += 1;
    }

    fn record_bound_op(&mut self, method: &str) {
        let function = self.function.last().cloned().unwrap_or_default();
        let key = format!("{}: {function} bound.{}", self.file, plain(method));
        if !self.branch.is_empty() {
            *self.bound_guarded.entry(key.clone()).or_default() += 1;
        }
        *self.bound_ops.entry(key).or_default() += 1;
    }

    fn record_token_shape(&mut self, what: &str) {
        self.audit_tokens
            .insert(format!("{}: token shape: {what}", self.file));
    }

    fn record_token_use(&mut self, what: &str) {
        let function = self.function.last().cloned().unwrap_or_default();
        self.audit_tokens
            .insert(format!("{}: {function} constructs {what}", self.file));
    }

    fn scan_tokens(&mut self, tokens: TokenStream) {
        let mut previous = String::new();
        let mut pending_type = false;
        let mut after_dot = false;
        for token in tokens {
            let was_dot = std::mem::take(&mut after_dot);
            match token {
                TokenTree::Punct(punct) => {
                    after_dot = punct.as_char() == '.';
                    pending_type = false;
                }
                TokenTree::Literal(_) => pending_type = false,
                TokenTree::Group(group) => {
                    if pending_type && group.delimiter() == proc_macro2::Delimiter::Brace {
                        self.record_token_use("AuditedIdentity");
                    }
                    pending_type = false;
                    self.scan_tokens(group.stream());
                }
                TokenTree::Ident(ident) => {
                    let name = plain(&ident.to_string()).to_string();
                    if was_dot && name == "bound" {
                        self.record_bound_mention();
                    }
                    self.record_call(&name);
                    if let Some(what) = token_use(&previous, &name) {
                        self.record_token_use(what);
                    }
                    pending_type = name == "AuditedIdentity"
                        || (name == "Self"
                            && self.owner.last().is_some_and(|o| o == "AuditedIdentity"));
                    previous = name;
                }
            }
        }
    }

    fn enter_fn(&mut self, name: &str, vis: &syn::Visibility, attrs: &[syn::Attribute]) -> bool {
        if is_cfg_test(attrs) {
            return false;
        }
        let name = plain(name);
        let qualified = self.qualified(name);
        self.functions.insert(format!("{}: {qualified}", self.file));
        if dispatch_name(name) {
            self.declarations
                .insert(format!("{}: {qualified} ({})", self.file, visibility(vis)));
        }
        if qualified == "AuditedIdentity::split" {
            self.record_token_shape(&format!("split is {}", visibility(vis)));
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
            if node.attrs.iter().any(|attr| attr.path().is_ident("path")) {
                self.item_uses
                    .insert(format!("{}: #[path] module {}", self.file, node.ident));
            }
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        syn::visit::visit_item_type(self, node);
        if mentions_token(&node.ty) {
            self.record_token_shape(&format!("type alias {}", node.ident));
        }
    }

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        syn::visit::visit_use_rename(self, node);
        if matches!(
            node.ident.to_string().as_str(),
            "AuditedIdentity" | "BoundIdentity"
        ) {
            self.record_token_shape(&format!("use {} as {}", node.ident, node.rename));
        }
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        syn::visit::visit_item_struct(self, node);
        if node.ident == "AuditedIdentity" {
            for shape in attribute_shapes(&node.attrs) {
                self.record_token_shape(&shape);
            }
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_cfg_test(&node.attrs) {
            if mentions_token(&node.self_ty) {
                if let Some((_, path, _)) = &node.trait_ {
                    self.record_token_shape(&format!("impl {}", last_segment(path)));
                }
            }
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
        if rooted_at_bound(&node.receiver) {
            self.record_bound_op(&node.method.to_string());
        }
        if node.method == "write" {
            if let syn::Expr::Field(field) = &*node.receiver {
                if let syn::Member::Named(member) = &field.member {
                    if member == "escalations" || member == "escalator" {
                        let function = self.function.last().cloned().unwrap_or_default();
                        self.field_writes
                            .insert(format!("{}: {function} writes {member}", self.file));
                    }
                }
            }
        }
    }

    fn visit_field_pat(&mut self, node: &'ast syn::FieldPat) {
        syn::visit::visit_field_pat(self, node);
        if names_bound(&node.member) {
            self.record_bound_mention();
        }
    }

    fn visit_expr_field(&mut self, node: &'ast syn::ExprField) {
        syn::visit::visit_expr_field(self, node);
        if names_bound(&node.member) {
            self.record_bound_mention();
        }
    }

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        syn::visit::visit_expr_struct(self, node);
        let name = last_segment(&node.path);
        let in_token = name == "AuditedIdentity"
            || (name == "Self" && self.owner.last().is_some_and(|o| o == "AuditedIdentity"));
        if in_token {
            self.record_token_use("AuditedIdentity");
        }
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        syn::visit::visit_expr_path(self, node);
        let mut tail = node.path.segments.iter().rev().map(|s| s.ident.to_string());
        let name = tail.next().unwrap_or_default();
        let owner = match &node.qself {
            Some(qself) => type_name(&qself.ty),
            None => tail.next().unwrap_or_default(),
        };
        let owner = match owner.as_str() {
            "Self" => self.owner.last().cloned().unwrap_or_default(),
            _ => owner,
        };
        if let Some(what) = token_use(&owner, &name) {
            self.record_token_use(what);
        }
        if let Some(segment) = node.path.segments.last() {
            self.record_call(&segment.ident.to_string());
        }
    }

    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.branches += 1;
        let then_id = self.branches;
        self.cond_guard.push(then_id);
        self.visit_expr(&node.cond);
        self.cond_guard.pop();
        self.branch.push(then_id);
        self.visit_block(&node.then_branch);
        self.branch.pop();
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
        if last_segment(&node.path) == "include" {
            self.item_uses
                .insert(format!("{}: include! macro", self.file));
        }
        self.scan_tokens(node.tokens.clone());
    }
}

fn names_bound(member: &syn::Member) -> bool {
    matches!(member, syn::Member::Named(name) if plain(&name.to_string()) == "bound")
}

fn rooted_at_bound(expr: &syn::Expr) -> bool {
    match expr {
        syn::Expr::Field(field) => names_bound(&field.member),
        syn::Expr::MethodCall(call) => rooted_at_bound(&call.receiver),
        syn::Expr::Await(awaited) => rooted_at_bound(&awaited.base),
        syn::Expr::Paren(paren) => rooted_at_bound(&paren.expr),
        syn::Expr::Reference(reference) => rooted_at_bound(&reference.expr),
        syn::Expr::Unary(unary) => rooted_at_bound(&unary.expr),
        _ => false,
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
            "task.rs: TaskManager::submit_audited (pub(restricted))",
            "task.rs: TaskManager::submit_spawn (pub)",
            "task.rs: TaskManager::submit_task (pub(restricted))",
            "task.rs: TaskManager::submit_bound (private)",
        ]),
        "a submit/enqueue function was added, removed or had its visibility changed; every such function must go through SpawnGate and be reviewed here"
    );
}

#[test]
fn no_item_level_code_mentions_a_submit_or_enqueue_name_or_pulls_in_files() {
    assert_eq!(union(|scan| &scan.item_uses), BTreeSet::new());
}

#[test]
fn the_escalation_log_is_installed_at_construction_and_never_swapped_in_production() {
    assert_eq!(union(|scan| &scan.field_writes), BTreeSet::new());
    let functions = union(|scan| &scan.functions);
    assert!(!functions.contains("task.rs: TaskManager::with_escalations"));
    assert!(!functions.contains("task.rs: TaskManager::with_escalator"));
    assert!(functions.contains("task.rs: TaskManager::restore_with_escalations"));
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
        ("backlog.rs: ManagerSink::enqueue uses submit_audited", 1),
        ("backlog.rs: backlog_sync uses enqueue", 1),
        ("main.rs: run_delegate uses submit_task", 1),
        ("mcp/handlers.rs: handle_assign_task uses submit_spawn", 1),
        ("plan_dispatch.rs: TaskManager::start uses submit_spawn", 1),
        ("task.rs: TaskManager::submit_audited uses submit_task", 1),
        ("task.rs: TaskManager::submit_spawn uses submit_bound", 1),
        ("task.rs: TaskManager::submit_task uses enqueue", 1),
        ("task.rs: TaskManager::submit_bound uses submit_task", 1),
    ]
    .into_iter()
    .map(|(key, count)| (key.to_string(), count))
    .collect();
    assert_eq!(
        counts, expected,
        "a use of a submit/enqueue function appeared, vanished, was duplicated, or moved to another function (plan_dispatch.rs is the single place a plan node reaches submit_spawn)"
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

fn guarded_in(scan: &Scan, function: &str, first: &str, then: &str) -> bool {
    let calls = &scan.sequences[function];
    let Some(index) = calls.iter().position(|(name, _)| name == first) else {
        return false;
    };
    let Some(guard) = scan.guards.get(function).and_then(|g| g.get(&index)) else {
        return false;
    };
    let dependents: Vec<_> = calls.iter().filter(|(name, _)| name == then).collect();
    !dependents.is_empty() && dependents.iter().all(|(_, path)| path.contains(guard))
}

fn guarded_by_condition(function: &str, file: &str, first: &str, then: &str) -> bool {
    let scan = scan_all()
        .into_iter()
        .find(|scan| scan.file == file && scan.sequences.contains_key(function))
        .unwrap_or_else(|| {
            panic!("{file}: {function} is no longer scanned; update the pairing pin")
        });
    guarded_in(&scan, function, first, then)
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
        ("ManagerSink::enqueue", "submit_audited"),
    ] {
        let calls = calls_in(function, "backlog.rs");
        assert!(dominates(&calls, "ensure_audited", dispatch), "{function}");
    }
}

#[test]
fn the_backlog_manager_sink_binds_the_audited_identity_and_never_calls_submit_task() {
    let calls = calls_in("ManagerSink::enqueue", "backlog.rs");
    assert!(calls.iter().any(|(name, _)| name == "submit_audited"));
    assert!(calls.iter().all(|(name, _)| name != "submit_task"));
    let audited = calls_in("TaskManager::submit_audited", "task.rs");
    assert!(audited.iter().any(|(name, _)| name == "split"));
    assert!(audited.iter().any(|(name, _)| name == "submit_task"));
    assert!(dominates(&audited, "split", "submit_task"));
    assert!(dominates(&audited, "insert", "submit_task"));
}

#[test]
fn the_name_submit_task_is_called_only_by_the_audited_binders_and_the_gated_mcp_client_entry() {
    let mut callers: BTreeSet<String> = BTreeSet::new();
    for scan in scan_all() {
        for key in scan.calls.keys() {
            if key.ends_with(" uses submit_task") {
                callers.insert(key.clone());
            }
        }
    }
    assert_eq!(
        callers,
        expected(&[
            "main.rs: run_delegate uses submit_task",
            "task.rs: TaskManager::submit_audited uses submit_task",
            "task.rs: TaskManager::submit_bound uses submit_task",
        ]),
        "the only production callers of TaskManager::submit_task are submit_bound and submit_audited; run_delegate calls NannaMcpClient::submit_task, which reaches submit_spawn through the gated in-process server; the scan matches by method name only, so it cannot tell the two submit_task methods apart and any new caller of either must be reviewed here"
    );
}

#[test]
fn restore_audits_every_persisted_entry_before_the_dispatcher_starts() {
    let calls = calls_in("TaskManager::restore_inner", "task.rs");
    assert!(dominates(&calls, "admit_restored", "dispatch"));
    let admit = calls_in("TaskManager::admit_restored", "task.rs");
    assert!(admit.iter().any(|(name, _)| name == "audit_restored"));
    assert!(admit.iter().any(|(name, _)| name == "cancel"));
    assert!(admit.iter().all(|(name, _)| name != "dispatch"));
    let audit = calls_in("audit_restored", "task.rs");
    assert!(dominates(&audit, "request", "check"));
}

#[test]
fn only_the_gated_restore_is_a_production_entry_point() {
    let functions = union(|scan| &scan.functions);
    assert!(functions.contains("task.rs: TaskManager::restore_with_escalations"));
    for name in ["restore", "restore_with_identities"] {
        assert!(
            !functions.contains(&format!("task.rs: TaskManager::{name}")),
            "`{name}` restores without auditing and must stay test-only"
        );
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
    assert!(declared.contains("task.rs: TaskManager::submit_bound (private)"));
    assert!(
        !union(|scan| &scan.functions).contains("task.rs: TaskManager::submit_with_identity"),
        "`submit_with_identity` binds an unaudited identity and must stay test-only"
    );
    assert!(declared.contains("task.rs: TaskManager::submit_task (pub(restricted))"));
    assert!(
        !declared.contains("task.rs: TaskManager::submit_task (pub)"),
        "`submit_task` must not be public"
    );
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
        .item_uses
        .contains("x.rs: item-level use of submit_task"));
    let generated = scan_source("x.rs", "gen! { pub async fn submit_zz() {} }");
    assert!(generated
        .item_uses
        .contains("x.rs: item-level use of submit_zz"));
}

#[test]
fn function_pointers_in_items_path_modules_includes_and_raw_identifiers_are_seen() {
    for source in [
        "const F: fn() = TaskManager::submit_task as fn();",
        "pub static F: Option<fn()> = Some(TaskManager::submit_task as fn());",
        "struct H { f: fn() } const S: H = H { f: TaskManager::submit_task as fn() };",
        "impl H { const F: fn() = TaskManager::submit_task; }",
        "const G: () = { m.submit_task(1); };",
    ] {
        let scan = scan_source("x.rs", source);
        assert!(
            scan.item_uses
                .contains("x.rs: item-level use of submit_task"),
            "{source}"
        );
    }
    let path = scan_source("x.rs", "#[path = \"../tests/ext.rs\"] mod ext;");
    assert!(path.item_uses.contains("x.rs: #[path] module ext"));
    let include = scan_source("x.rs", "include!(\"../tests/ext.rs\");");
    assert!(include.item_uses.contains("x.rs: include! macro"));
    let nested = scan_source("x.rs", "fn f() { include!(\"zz.txt\"); }");
    assert!(nested.item_uses.contains("x.rs: include! macro"));
    let raw = scan_source("x.rs", "fn f() { m.r#submit_task(1); }");
    assert_eq!(raw.calls["x.rs: f uses submit_task"], 1);
    let raw_fn = scan_source("x.rs", "pub fn r#submit_zz() {}");
    assert!(raw_fn.declarations.contains("x.rs: submit_zz (pub)"));
}

#[test]
fn a_late_write_of_the_escalation_log_is_seen() {
    let late = scan_source(
        "x.rs",
        "impl M { fn restore(&self) { *self.runner.escalations.write().unwrap() = log; } }",
    );
    assert!(late
        .field_writes
        .contains("x.rs: M::restore writes escalations"));
    let escalator = scan_source(
        "x.rs",
        "fn f(m: M) { *m.escalator.write().unwrap() = None; }",
    );
    assert!(escalator.field_writes.contains("x.rs: f writes escalator"));
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

#[test]
fn the_audit_token_is_minted_only_where_the_gate_proof_is_consumed() {
    assert_eq!(
        union(|scan| &scan.audit_tokens),
        expected(&[
            "task.rs: AuditedIdentity::split constructs AuditedIdentity",
            "task.rs: TaskManager::admit_restored constructs BoundIdentity::Audited",
            "task.rs: TaskManager::submit_audited constructs AuditedIdentity::split",
            "task.rs: TaskManager::submit_audited constructs BoundIdentity::Audited",
            "task.rs: TaskManager::submit_spawn constructs AuditedIdentity::split",
            "task.rs: TaskManager::submit_spawn constructs BoundIdentity::Audited",
            "task.rs: audit_restored constructs AuditedIdentity::split",
            "task.rs: token shape: derives Debug",
            "task.rs: token shape: split is private",
        ]),
        "the audited-identity token may only be created from a gate proof in submit_spawn, submit_audited and audit_restored"
    );
}

#[test]
fn token_construction_is_seen_in_expressions_literals_and_macro_bodies() {
    let split = scan_source("x.rs", "fn f(a: A) { let t = AuditedIdentity::split(a); }");
    assert!(split
        .audit_tokens
        .contains("x.rs: f constructs AuditedIdentity::split"));
    let literal = scan_source(
        "x.rs",
        "fn f(i: I) { let t = AuditedIdentity { identity: i }; }",
    );
    assert!(literal
        .audit_tokens
        .contains("x.rs: f constructs AuditedIdentity"));
    let selfish = scan_source(
        "x.rs",
        "impl AuditedIdentity { fn forge(i: I) -> Self { Self { identity: i } } }",
    );
    assert!(selfish
        .audit_tokens
        .contains("x.rs: AuditedIdentity::forge constructs AuditedIdentity"));
    let variant = scan_source("x.rs", "fn f(t: T) { let b = BoundIdentity::Audited(t); }");
    assert!(variant
        .audit_tokens
        .contains("x.rs: f constructs BoundIdentity::Audited"));
    let in_macro = scan_source(
        "x.rs",
        "fn f(i: I) { vec![AuditedIdentity { identity: i }]; }",
    );
    assert!(in_macro
        .audit_tokens
        .contains("x.rs: f constructs AuditedIdentity"));
    let in_macro_call = scan_source("x.rs", "fn f(a: A) { vec![AuditedIdentity::split(a)]; }");
    assert!(in_macro_call
        .audit_tokens
        .contains("x.rs: f constructs AuditedIdentity::split"));
    let in_macro_variant = scan_source("x.rs", "fn f(t: T) { vec![BoundIdentity::Audited(t)]; }");
    assert!(in_macro_variant
        .audit_tokens
        .contains("x.rs: f constructs BoundIdentity::Audited"));
    let unrelated = scan_source(
        "x.rs",
        "fn f(b: B) { let _ = BoundIdentity::Unaudited(b); }",
    );
    assert!(unrelated.audit_tokens.is_empty());
}

#[test]
fn indirect_routes_to_the_token_constructor_and_widened_derives_are_seen() {
    let alias = scan_source("x.rs", "type Alias = AuditedIdentity;");
    assert!(alias
        .audit_tokens
        .contains("x.rs: token shape: type alias Alias"));
    let reference = scan_source("x.rs", "type Alias = &'static AuditedIdentity;");
    assert!(reference
        .audit_tokens
        .contains("x.rs: token shape: type alias Alias"));
    let renamed = scan_source("x.rs", "use self::AuditedIdentity as AI;");
    assert!(renamed
        .audit_tokens
        .contains("x.rs: token shape: use AuditedIdentity as AI"));
    let renamed_enum = scan_source("x.rs", "use crate::task::{BoundIdentity as B};");
    assert!(renamed_enum
        .audit_tokens
        .contains("x.rs: token shape: use BoundIdentity as B"));
    let selfish = scan_source(
        "x.rs",
        "impl AuditedIdentity { fn again(a: A) { let _ = Self::split(a); } }",
    );
    assert!(selfish
        .audit_tokens
        .contains("x.rs: AuditedIdentity::again constructs AuditedIdentity::split"));
    let other_self = scan_source(
        "x.rs",
        "impl Other { fn again(a: A) { let _ = Self::split(a); } }",
    );
    assert!(other_self.audit_tokens.is_empty());
    let derives = scan_source(
        "x.rs",
        "#[derive(Debug, Clone)] #[derive(Default)] struct AuditedIdentity { i: I }",
    );
    assert!(derives
        .audit_tokens
        .contains("x.rs: token shape: derives Debug, Clone"));
    assert!(derives
        .audit_tokens
        .contains("x.rs: token shape: derives Default"));
    for source in [
        "#[cfg_attr(all(), derive(Clone))] struct AuditedIdentity { i: I }",
        "#[::core::prelude::v1::derive(Clone)] struct AuditedIdentity { i: I }",
    ] {
        let hidden = scan_source("x.rs", source);
        assert!(
            hidden
                .audit_tokens
                .iter()
                .any(|shape| shape.contains("token shape: attribute")),
            "{source}"
        );
    }
    let qualified = scan_source(
        "x.rs",
        "fn f(a: A) { let _ = <AuditedIdentity>::split(a); }",
    );
    assert!(qualified
        .audit_tokens
        .contains("x.rs: f constructs AuditedIdentity::split"));
    let qualified_self = scan_source(
        "x.rs",
        "impl AuditedIdentity { fn again(a: A) { let _ = <Self>::split(a); } }",
    );
    assert!(qualified_self
        .audit_tokens
        .contains("x.rs: AuditedIdentity::again constructs AuditedIdentity::split"));
    let qualified_other = scan_source("x.rs", "fn f(a: A) { let _ = <Other>::split(a); }");
    assert!(qualified_other.audit_tokens.is_empty());
    let unrelated = scan_source("x.rs", "#[derive(Clone)] struct Other { i: I }");
    assert!(unrelated.audit_tokens.is_empty());
    let manual = scan_source(
        "x.rs",
        "impl Clone for AuditedIdentity { fn clone(&self) -> Self { todo!() } }",
    );
    assert!(manual
        .audit_tokens
        .contains("x.rs: token shape: impl Clone"));
    let other_impl = scan_source(
        "x.rs",
        "impl Clone for Other { fn clone(&self) -> Self { todo!() } }",
    );
    assert!(other_impl.audit_tokens.is_empty());
    let public = scan_source(
        "x.rs",
        "impl AuditedIdentity { pub(crate) fn split(a: A) {} }",
    );
    assert!(public
        .audit_tokens
        .contains("x.rs: token shape: split is pub(restricted)"));
    let other_split = scan_source("x.rs", "impl Other { pub fn split(a: A) {} }");
    assert!(other_split.audit_tokens.is_empty());
    assert_eq!(
        token_use("AuditedIdentity", "split"),
        Some("AuditedIdentity::split")
    );
    assert_eq!(
        token_use("BoundIdentity", "Audited"),
        Some("BoundIdentity::Audited")
    );
    assert_eq!(token_use("BoundIdentity", "Unaudited"), None);
    assert_eq!(token_use("Other", "split"), None);
}

fn union_counts(select: impl Fn(&Scan) -> &BTreeMap<String, usize>) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for scan in scan_all() {
        for (key, count) in select(&scan) {
            *counts.entry(key.clone()).or_default() += count;
        }
    }
    counts
}

fn counts(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
    entries
        .iter()
        .map(|(key, count)| (key.to_string(), *count))
        .collect()
}

#[test]
fn every_production_mention_of_the_bound_identity_map_is_counted_per_function() {
    assert_eq!(
        union_counts(|scan| &scan.bound_mentions),
        counts(&[
            ("task.rs: TaskManager::admit_restored", 1),
            ("task.rs: TaskManager::cancel", 1),
            ("task.rs: TaskManager::submit_audited", 1),
            ("task.rs: TaskManager::submit_bound", 1),
            ("task.rs: TaskRunner::resolve_identity", 1),
            ("task.rs: TaskRunner::set_status", 1),
        ]),
        "a production function started or stopped touching `runner.bound`, or reaches it through an alias, helper or macro; review that every insert still has a remove"
    );
}

#[test]
fn every_production_operation_on_the_bound_identity_map_is_counted_per_function() {
    assert_eq!(
        union_counts(|scan| &scan.bound_ops),
        counts(&[
            ("task.rs: TaskManager::admit_restored bound.insert", 1),
            ("task.rs: TaskManager::admit_restored bound.write", 1),
            ("task.rs: TaskManager::cancel bound.remove", 1),
            ("task.rs: TaskManager::cancel bound.write", 1),
            ("task.rs: TaskManager::submit_audited bound.insert", 1),
            ("task.rs: TaskManager::submit_audited bound.write", 1),
            ("task.rs: TaskManager::submit_bound bound.insert", 1),
            ("task.rs: TaskManager::submit_bound bound.write", 1),
            ("task.rs: TaskRunner::resolve_identity bound.remove", 1),
            ("task.rs: TaskRunner::resolve_identity bound.write", 1),
            ("task.rs: TaskRunner::set_status bound.remove", 1),
            ("task.rs: TaskRunner::set_status bound.write", 1),
        ]),
        "an insert into or removal from `runner.bound` was added, removed or moved to another function; every insert site needs a matching remove path"
    );
}

#[test]
fn every_bound_identity_insert_has_a_remove_path_on_failure_cancel_and_terminal_status() {
    let ops = union_counts(|scan| &scan.bound_ops);
    let sites = |method: &str| -> BTreeSet<String> {
        ops.keys()
            .filter_map(|key| key.strip_suffix(&format!(" bound.{method}")))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(
        sites("insert"),
        expected(&[
            "task.rs: TaskManager::admit_restored",
            "task.rs: TaskManager::submit_audited",
            "task.rs: TaskManager::submit_bound",
        ]),
        "an insert into `runner.bound` was added or moved: give the new site a remove path (terminal status, cancel, or consumption by resolve_identity) and update this list in the same change"
    );
    assert_eq!(
        sites("remove"),
        expected(&[
            "task.rs: TaskManager::cancel",
            "task.rs: TaskRunner::resolve_identity",
            "task.rs: TaskRunner::set_status",
        ]),
        "a removal from `runner.bound` was added, deleted or moved: every insert site needs a remove reachable for it; update this list only after checking that"
    );
    assert!(
        guarded_by_condition("TaskRunner::set_status", "task.rs", "is_terminal", "remove"),
        "set_status must remove the bound identity inside the `if status.is_terminal()` branch"
    );
    let guarded_removes: BTreeMap<String, usize> = union_counts(|scan| &scan.bound_guarded)
        .into_iter()
        .filter(|(key, _)| key.ends_with(" bound.remove"))
        .collect();
    assert_eq!(
        guarded_removes,
        counts(&[
            ("task.rs: TaskManager::cancel bound.remove", 1),
            ("task.rs: TaskRunner::set_status bound.remove", 1),
        ]),
        "cancel and set_status must remove the bound identity only on a conditional branch (cancel: when the task was dequeued; set_status: when the status is terminal)"
    );
    assert!(
        guarded_by_condition("TaskManager::submit_task", "task.rs", "enqueue", "fail"),
        "submit_task must call fail inside the branch taken when enqueue errors, so a failed submit reaches the terminal remove in set_status"
    );
    let fail = calls_in("TaskRunner::fail", "task.rs");
    assert!(
        fail.iter().any(|(name, _)| name == "set_status"),
        "TaskRunner::fail must reach set_status, where the terminal remove lives"
    );
}

#[test]
fn the_bound_scanner_sees_raw_identifiers_aliases_macros_and_ignores_cfg_test_items() {
    let scan = scan_source(
        "x.rs",
        "fn f(r: R) { r.r#bound.write().await.r#insert(1, 2); } \
         fn g(r: R) { let b = &r.bound; } \
         fn h(r: R) { m!(r.bound.write().await.insert(1, 2)); } \
         #[cfg(test)] fn t(r: R) { r.bound.write().await.insert(1, 2); }",
    );
    assert_eq!(scan.bound_ops["x.rs: f bound.insert"], 1);
    assert_eq!(scan.bound_ops["x.rs: f bound.write"], 1);
    assert_eq!(scan.bound_mentions["x.rs: f"], 1);
    assert_eq!(scan.bound_mentions["x.rs: g"], 1);
    assert_eq!(scan.bound_mentions["x.rs: h"], 1);
    assert!(!scan.bound_mentions.contains_key("x.rs: t"));
    assert!(!scan.bound_ops.contains_key("x.rs: h bound.insert"));
}

struct FieldTypes {
    idents: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for FieldTypes {
    fn visit_path_segment(&mut self, node: &'ast syn::PathSegment) {
        self.idents.insert(node.ident.to_string());
        syn::visit::visit_path_segment(self, node);
    }
}

fn task_runner_fields(source: &str) -> BTreeMap<String, BTreeSet<String>> {
    let syntax = syn::parse_file(source).unwrap();
    let mut fields = BTreeMap::new();
    for item in &syntax.items {
        let syn::Item::Struct(item) = item else {
            continue;
        };
        if item.ident != "TaskRunner" {
            continue;
        }
        for field in &item.fields {
            let mut types = FieldTypes {
                idents: BTreeSet::new(),
            };
            types.visit_type(&field.ty);
            let name = field
                .ident
                .as_ref()
                .map(|i| plain(&i.to_string()).to_string());
            fields.insert(name.unwrap_or_default(), types.idents);
        }
    }
    fields
}

fn task_source() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/task.rs")).unwrap()
}

#[test]
fn the_task_runner_has_one_map_of_bound_identities_and_a_pinned_field_list() {
    let fields = task_runner_fields(&task_source());
    let holders: BTreeSet<String> = fields
        .iter()
        .filter(|(_, types)| {
            ["BoundIdentity", "AuditedIdentity", "Allowed"]
                .iter()
                .any(|token| types.contains(*token))
        })
        .map(|(name, _)| name.clone())
        .collect();
    assert_eq!(
        holders,
        expected(&["bound"]),
        "a second TaskRunner field holds bound or audited identities; the pairing pin is keyed on the field name `bound` and does not see it: extend the scanner to the new field and give it a remove path"
    );
    assert_eq!(
        fields.keys().cloned().collect::<BTreeSet<_>>(),
        expected(&[
            "action_gate",
            "audit",
            "bound",
            "build_locks",
            "check_runner",
            "ci_client",
            "cost_accountant",
            "default_provider",
            "escalations",
            "escalator",
            "identities",
            "image_cache",
            "leases",
            "progress",
            "providers",
            "sink",
            "status_senders",
            "tasks",
        ]),
        "TaskRunner gained or lost a field: if it keeps per-task state, check that it is cleaned up on terminal status, then update this list"
    );
}

#[test]
fn the_field_list_scanner_reads_struct_fields_and_their_type_names() {
    let fields = task_runner_fields(
        "struct TaskRunner { bound: RwLock<HashMap<TaskId, BoundIdentity>>, r#other: Arc<X> } struct Other { z: u8 }",
    );
    assert_eq!(fields.len(), 2);
    assert!(fields["bound"].contains("BoundIdentity"));
    assert!(fields["other"].contains("X"));
}

#[test]
fn the_bound_scanner_sees_struct_pattern_destructuring_of_the_field() {
    let scan = scan_source(
        "x.rs",
        "fn f(s: S) { let TaskRunner { bound, .. } = &*s.runner; } \
         fn g(r: &TaskRunner) { let TaskRunner { bound: m, .. } = r; } \
         fn h(r: &TaskRunner) { let TaskRunner { r#bound: m, .. } = r; }",
    );
    assert_eq!(scan.bound_mentions["x.rs: f"], 1);
    assert_eq!(scan.bound_mentions["x.rs: g"], 1);
    assert_eq!(scan.bound_mentions["x.rs: h"], 1);
}

#[test]
fn a_condition_guards_only_the_branch_it_selects() {
    let guarded = |source: &str| {
        let scan = scan_source("x.rs", source);
        let calls = &scan.sequences["f"];
        let index = calls.iter().position(|(name, _)| name == "cond").unwrap();
        let guard = scan.guards["f"][&index];
        calls
            .iter()
            .filter(|(name, _)| name == "act")
            .all(|(_, path)| path.contains(&guard))
    };
    assert!(guarded("fn f(s: S) { if s.cond() { s.act(); } }"));
    assert!(!guarded("fn f(s: S) { if s.cond() { } s.act(); }"));
    assert!(!guarded(
        "fn f(s: S) { if s.cond() { } if s.other() { s.act(); } }"
    ));
    assert!(!guarded("fn f(s: S) { if s.cond() { } else { s.act(); } }"));
}

#[test]
fn a_missing_function_or_a_missing_condition_does_not_count_as_guarded() {
    let scan = scan_source(
        "x.rs",
        "fn f(s: S) { s.act(); if s.cond() { s.act(); } } fn g(s: S) { if s.cond() { s.act(); } } fn h(s: S) { s.cond(); s.act(); }",
    );
    assert!(guarded_in(&scan, "g", "cond", "act"));
    assert!(!guarded_in(&scan, "g", "missing", "act"));
    assert!(!guarded_in(&scan, "g", "cond", "missing"));
    assert!(!guarded_in(&scan, "h", "cond", "act"));
    assert!(!guarded_in(&scan, "f", "act", "act"));
}

#[test]
#[should_panic(expected = "is no longer scanned")]
fn a_function_that_is_no_longer_scanned_fails_with_an_actionable_message() {
    guarded_by_condition("TaskRunner::missing", "task.rs", "a", "b");
}

#[test]
fn the_bound_root_follows_parentheses_references_and_dereferences() {
    let rooted = |source: &str| rooted_at_bound(&syn::parse_str::<syn::Expr>(source).unwrap());
    assert!(rooted("self.runner.bound"));
    assert!(rooted("(self.runner.bound)"));
    assert!(rooted("&self.runner.bound"));
    assert!(rooted("*self.runner.bound"));
    assert!(rooted("self.runner.bound.write().await"));
    assert!(!rooted("self.runner.tasks.write().await"));
    assert!(!rooted("bound"));
}
