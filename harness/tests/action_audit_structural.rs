use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use syn::visit::Visit;

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

#[derive(Default)]
struct Functions {
    stack: Vec<String>,
    idents: BTreeMap<String, Vec<String>>,
}

impl Functions {
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

impl<'ast> Visit<'ast> for Functions {
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

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_cfg_test(&node.attrs) {
            self.stack.push(node.sig.ident.to_string());
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

fn scan(source: &str) -> Functions {
    let file = syn::parse_file(source).unwrap();
    let mut functions = Functions::default();
    functions.visit_file(&file);
    functions
}

fn users(source: &str, name: &str) -> BTreeSet<String> {
    let functions = scan(source);
    let mentions = |idents: &Vec<String>| idents.iter().any(|ident| ident == name);
    let found = functions
        .idents
        .iter()
        .filter(|(_, idents)| mentions(idents));
    found.map(|(function, _)| function.clone()).collect()
}

#[derive(Default)]
struct Finder {
    body: Option<syn::Block>,
}

impl<'ast> Visit<'ast> for Finder {
    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if node.sig.ident == "run_gate" {
            self.body = Some(node.block.clone());
        }
    }
}

fn called_method(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::MethodCall(call) => Some(call.method.to_string()),
        syn::Expr::Await(awaited) => called_method(&awaited.base),
        _ => None,
    }
}

fn run_gate_problem(source: &str) -> Option<String> {
    let file = syn::parse_file(source).unwrap();
    let mut finder = Finder::default();
    finder.visit_file(&file);
    let Some(body) = finder.body else {
        return Some("run_gate is missing".to_string());
    };
    let [first, second, last] = body.stmts.as_slice() else {
        return Some("run_gate must be exactly three statements".to_string());
    };
    let syn::Stmt::Local(local) = first else {
        return Some("the first statement must bind the decision".to_string());
    };
    let syn::Pat::Ident(bound) = &local.pat else {
        return Some("the decision must be bound to a plain name".to_string());
    };
    let decided = called_method(&local.init.as_ref()?.expr);
    if decided.as_deref() != Some("decide") {
        return Some("the decision must come from `decide`".to_string());
    }
    let emitted = match second {
        syn::Stmt::Expr(expr, Some(_)) => called_method(expr),
        _ => None,
    };
    if emitted.as_deref() != Some("emit") {
        return Some("the second statement must call `emit`".to_string());
    }
    let syn::Stmt::Expr(syn::Expr::Path(returned), None) = last else {
        return Some("run_gate must return the bound decision".to_string());
    };
    if !returned.path.is_ident(&bound.ident) {
        return Some("run_gate must return exactly the value it emitted".to_string());
    }
    None
}

fn source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(path).unwrap()
}

fn expected(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn run_gate_decides_emits_and_returns_the_value_it_emitted() {
    assert_eq!(
        run_gate_problem(&source("src/action_auditor/gate.rs")),
        None
    );
}

#[test]
fn the_log_and_the_verdict_constructors_are_reachable_only_from_decide() {
    let gate = source("src/action_auditor/gate.rs");
    for name in ["append", "block"] {
        assert_eq!(users(&gate, name), expected(&["decide"]), "{name}");
    }
    assert!(users(&gate, "escalate").is_empty());
    assert_eq!(users(&gate, "run_gate"), expected(&["run_gate"]));
}

#[test]
fn emit_builds_one_event_and_hands_it_to_the_sink_once() {
    let gate = source("src/action_auditor/gate.rs");
    let functions = scan(&gate);
    let in_emit = &functions.idents["emit"];
    let count = |name: &str| in_emit.iter().filter(|ident| *ident == name).count();
    assert_eq!(count("new"), 1);
    assert_eq!(users(&gate, "ActionAuditEvent").len(), 2);
    assert_eq!(users(&gate, "get"), expected(&["emit"]));
}

#[derive(Default)]
struct Calls {
    locals: Vec<(String, Vec<String>)>,
    calls: Vec<(String, Vec<syn::Expr>, Option<String>)>,
    paths: Vec<(String, Vec<syn::Expr>)>,
    bound: Option<String>,
}

fn joined_path(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Path(path) => {
            let segments = path.path.segments.iter();
            let names: Vec<String> = segments.map(|s| s.ident.to_string()).collect();
            Some(names.join("::"))
        }
        _ => None,
    }
}

fn last_segment(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn mentions(expr: &syn::Expr) -> Vec<String> {
    let mut found = Functions::default();
    found.visit_expr(expr);
    found.idents.into_values().flatten().collect()
}

impl<'ast> Visit<'ast> for Calls {
    fn visit_local(&mut self, node: &'ast syn::Local) {
        let name = match &node.pat {
            syn::Pat::Ident(bound) => Some(bound.ident.to_string()),
            _ => None,
        };
        if let (Some(name), Some(init)) = (&name, &node.init) {
            self.locals.push((name.clone(), mentions(&init.expr)));
        }
        let outer = self.bound.replace(name.unwrap_or_default());
        syn::visit::visit_local(self, node);
        self.bound = outer;
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        let path = joined_path(&node.func).unwrap_or_default();
        if let Some(name) = last_segment(&node.func) {
            let args: Vec<syn::Expr> = node.args.iter().cloned().collect();
            self.paths.push((path, args.clone()));
            self.calls.push((name, args, self.bound.clone()));
        }
        syn::visit::visit_expr_call(self, node);
    }
}

fn borrowed_path(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Reference(reference) => last_segment(&reference.expr),
        _ => None,
    }
}

fn is_action_gate_of(expr: &syn::Expr, manager: &str) -> bool {
    let syn::Expr::Reference(reference) = expr else {
        return false;
    };
    let syn::Expr::MethodCall(call) = &*reference.expr else {
        return false;
    };
    call.method == "action_gate"
        && call.args.is_empty()
        && last_segment(&call.receiver).as_deref() == Some(manager)
}

struct Wiring<'a> {
    function: &'a str,
    builder: &'a str,
    flushes: bool,
    file: &'a str,
    work: &'a [&'a str],
}

fn work_idents(found: &Calls, arg: Option<&syn::Expr>) -> Vec<String> {
    let Some(arg) = arg else {
        return Vec::new();
    };
    let Some(name) = last_segment(arg) else {
        return mentions(arg);
    };
    let bound: Vec<&Vec<String>> = found
        .locals
        .iter()
        .filter(|(local, _)| *local == name)
        .map(|(_, idents)| idents)
        .collect();
    match bound.as_slice() {
        [only] => (*only).clone(),
        _ => Vec::new(),
    }
}

fn attachment_problem(source: &str, wiring: &Wiring) -> Option<String> {
    let function = wiring.function;
    let file = syn::parse_file(source).unwrap();
    let body = file.items.iter().find_map(|item| match item {
        syn::Item::Fn(item) if item.sig.ident == function => Some(item.clone()),
        _ => None,
    })?;
    let mut found = Calls::default();
    found.visit_item_fn(&body);
    let builds: Vec<&String> = found
        .locals
        .iter()
        .filter(|(_, idents)| idents.iter().any(|ident| ident == wiring.builder))
        .map(|(name, _)| name)
        .collect();
    let [manager] = builds.as_slice() else {
        return Some(format!(
            "{function} must bind the manager from {} exactly once",
            wiring.builder
        ));
    };
    let manager = (*manager).clone();
    let bindings = found.locals.iter().filter(|(name, _)| *name == manager);
    if bindings.count() != 1 {
        return Some(format!("{function} rebinds or shadows {manager}"));
    }
    let export = found.calls.iter().find(|(name, args, _)| {
        name == "export_action_audit"
            && args.first().is_some_and(|a| is_action_gate_of(a, &manager))
            && args
                .get(1)
                .is_some_and(|a| mentions(a).iter().any(|i| i == wiring.file))
    });
    let Some((_, _, Some(telemetry))) = export else {
        return Some(format!(
            "{function} must bind export_action_audit(&{manager}.action_gate(), .. {} ..)",
            wiring.file
        ));
    };
    let flows = found.paths.iter().any(|(path, args)| {
        path.ends_with("NannaMcpServer::new")
            && args.first().is_some_and(|a| mentions(a).contains(&manager))
    });
    if !flows {
        return Some(format!(
            "{function} must hand {manager} to NannaMcpServer::new"
        ));
    }
    let finish = found.calls.iter().find(|(name, args, _)| {
        name == "finish_with_flush"
            && args.first().and_then(borrowed_path).as_deref() == Some(telemetry.as_str())
    });
    let Some((_, args, _)) = finish else {
        return Some(format!(
            "{function} must end through finish_with_flush(&{telemetry}, ..)"
        ));
    };
    let work = work_idents(&found, args.get(2));
    for needle in wiring.work {
        if !work.iter().any(|ident| ident == needle) {
            return Some(format!(
                "{function} must pass the work that runs {needle} to finish_with_flush"
            ));
        }
    }
    let second = args.get(1).map(mentions).unwrap_or_default();
    if !wiring.flushes {
        return (second != ["None"])
            .then(|| format!("{function} must pass None for the flush task"));
    }
    let spawned = found.calls.iter().find(|(name, args, bound)| {
        name == "spawn_telemetry_flush"
            && bound
                .as_deref()
                .is_some_and(|task| second.iter().any(|ident| ident == task))
            && args
                .first()
                .is_some_and(|a| mentions(a).iter().any(|i| i == telemetry))
    });
    spawned.is_none().then(|| {
        format!("{function} must pass the spawned flush task of {telemetry} to finish_with_flush")
    })
}

fn production_main() -> String {
    let main = source("src/main.rs");
    main.split("\n#[cfg(test)]\nmod tests")
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn each_cli_gate_gets_the_sink_from_its_own_manager_and_is_flushed_on_every_exit() {
    let main = production_main();
    let serve = Wiring {
        function: "run_mcp_server",
        builder: "build_mcp_task_manager",
        flushes: true,
        file: "TELEMETRY_FILE",
        work: &["serve", "shutdown_signal"],
    };
    assert_eq!(attachment_problem(&main, &serve), None);
    let delegate = Wiring {
        function: "run_delegate",
        builder: "build_delegate_task_manager",
        flushes: false,
        file: "DELEGATE_TELEMETRY_FILE",
        work: &["submit_task", "wait_result"],
    };
    assert_eq!(attachment_problem(&main, &delegate), None);
}

#[test]
fn the_shutdown_wait_listens_for_both_interrupt_and_terminate() {
    let main = production_main();
    for name in ["interrupt", "terminate"] {
        assert!(users(&main, name).contains("shutdown_signal"), "{name}");
    }
    assert!(users(&main, "shutdown_signal").contains("run_mcp_server"));
    let functions = scan(&main);
    let in_pair = &functions.idents["shutdown_on"];
    let count = |name: &str| in_pair.iter().filter(|ident| *ident == name).count();
    assert_eq!((count("first"), count("second")), (2, 2));
}

#[test]
fn the_attachment_checker_flags_each_way_the_wiring_could_be_faked() {
    let wrap = |body: &str| format!("async fn run_x() {{ {body} }}");
    let wiring = |flushes: bool| Wiring {
        function: "run_x",
        builder: "build_x_task_manager",
        flushes,
        file: "FILE",
        work: &["serve"],
    };
    let problem = |body: &str, flushes: bool| attachment_problem(&wrap(body), &wiring(flushes));
    let says = |body: &str, flushes: bool, needle: &str| -> bool {
        problem(body, flushes).is_some_and(|text| text.contains(needle))
    };
    let mgr = "let m = build_x_task_manager(a);";
    let exp = "let t = export_action_audit(&m.action_gate(), q.with_file_name(FILE));";
    let srv = "let s = NannaMcpServer::new(m, a);";
    let w = "let w = async { s.serve(r).await };";
    let spawn = "let f = spawn_telemetry_flush(Arc::clone(&t), d);";
    let base = format!("{mgr} {exp} {srv} {w}");
    let flushed = format!("{base} {spawn} finish_with_flush(&t, Some(f), w).await;");
    assert_eq!(problem(&flushed, true), None);
    let plain = format!("{base} finish_with_flush(&t, None, w).await;");
    assert_eq!(problem(&plain, false), None);
    let inline = format!(
        "{mgr} {exp} {srv} finish_with_flush(&t, None, async {{ s.serve(r).await }}).await;"
    );
    assert_eq!(problem(&inline, false), None);
    let other = "let t = export_action_audit(&TaskManager::default().action_gate(), q.with_file_name(FILE));";
    let swapped =
        format!("{mgr} {other} {srv} {w} {spawn} finish_with_flush(&t, Some(f), w).await;");
    assert!(says(&swapped, true, "export_action_audit"));
    let wrong_file = exp.replace("FILE", "OTHER");
    let wrong = format!("{mgr} {wrong_file} {srv} {w} finish_with_flush(&t, None, w).await;");
    assert!(says(&wrong, false, "export_action_audit"));
    assert!(says(
        &format!("let m = other(a); {exp}"),
        true,
        "exactly once"
    ));
    let twice = format!("{mgr} let n = build_x_task_manager(b); {exp}");
    assert!(says(&twice, true, "exactly once"));
    let dropped = format!(
        "{base} let f = 1; let _ = spawn_telemetry_flush; finish_with_flush(&t, Some(f), w).await;"
    );
    assert!(says(&dropped, true, "spawned flush task"));
    assert!(says(
        &format!("{mgr} {exp} {srv}"),
        false,
        "finish_with_flush"
    ));
    for variant in [
        "finish_with_flush(&u, None, w)",
        "finish_with_flush(t, None, w)",
    ] {
        let text = format!("{base} {variant}.await;");
        assert!(says(&text, false, "finish_with_flush"), "{variant}");
    }
    let some = format!("{base} finish_with_flush(&t, Some(f), w).await;");
    assert!(says(&some, false, "None"));
    let bare = format!("{mgr} export_action_audit(&m.action_gate(), q.with_file_name(FILE));");
    assert!(says(&bare, false, "export_action_audit"));
    for argument in ["m.action_gate()", "&m"] {
        let text = exp.replace("&m.action_gate()", argument);
        let flagged = says(&format!("{mgr} {text}"), false, "export_action_audit");
        assert!(flagged, "{argument}");
    }
    let indirect =
        format!("{mgr} (pick())(1); {exp} {srv} {w} finish_with_flush(&t, None, w).await;");
    assert_eq!(problem(&indirect, false), None);
    let shadowed = format!(
        "{base} let m = Arc::new(TaskManager::default()); finish_with_flush(&t, None, w).await;"
    );
    assert!(says(&shadowed, false, "rebinds"));
    let detached = format!("{mgr} {exp} let s = NannaMcpServer::new(other, a); {w} finish_with_flush(&t, None, w).await;");
    assert!(says(&detached, false, "NannaMcpServer::new"));
    let empty = format!("{base} finish_with_flush(&t, None, async {{ Ok(()) }}).await?; w.await?;");
    assert!(says(&empty, false, "serve"));
    let unbound = format!("{mgr} {exp} {srv} {w} finish_with_flush(&t, None, missing).await;");
    assert!(says(&unbound, false, "serve"));
    let twice_bound =
        format!("{base} let w = async {{ Ok(()) }}; finish_with_flush(&t, None, w).await;");
    assert!(says(&twice_bound, false, "serve"));
    let missing_arg = format!("{base} finish_with_flush(&t, None).await;");
    assert!(says(&missing_arg, false, "serve"));
    assert!(attachment_problem("fn other() {}", &wiring(true)).is_none());
}

#[derive(Default)]
struct GateSites {
    file: String,
    function: Vec<String>,
    sequence: BTreeMap<String, Vec<String>>,
    aliases: BTreeSet<String>,
}

impl GateSites {
    fn record(&mut self, name: String) {
        let owner = self.function.last().cloned().unwrap_or_default();
        self.sequence.entry(owner).or_default().push(name);
    }

    fn tokens(&mut self, stream: TokenStream) {
        for token in stream {
            match token {
                TokenTree::Ident(ident) => self.record(plain(&ident.to_string())),
                TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }
}

fn plain(name: &str) -> String {
    name.strip_prefix("r#").unwrap_or(name).to_string()
}

impl<'ast> Visit<'ast> for GateSites {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_cfg_test(&node.attrs) {
            self.function.push(plain(&node.sig.ident.to_string()));
            syn::visit::visit_item_fn(self, node);
            self.function.pop();
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_cfg_test(&node.attrs) {
            self.function.push(plain(&node.sig.ident.to_string()));
            syn::visit::visit_impl_item_fn(self, node);
            self.function.pop();
        }
    }

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        if plain(&node.ident.to_string()) == "ActionGate" {
            self.aliases
                .insert(format!("{}: renames ActionGate", self.file));
        }
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        let mut mentioned = Functions::default();
        mentioned.visit_type(&node.ty);
        let names: Vec<String> = mentioned.idents.into_values().flatten().collect();
        let hit = names.iter().any(|name| name == "ActionGate");
        if hit {
            self.aliases
                .insert(format!("{}: type alias of ActionGate", self.file));
        }
    }

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.record(plain(&ident.to_string()));
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
    }
}

const GATE_BUILDERS: &[&str] = &[
    "with_action_gate",
    "action_policy_gate",
    "with_action_policy",
];

fn gate_builds(file: &str, source: &str) -> BTreeSet<String> {
    let syntax = syn::parse_file(source).unwrap();
    let mut sites = GateSites {
        file: file.to_string(),
        ..GateSites::default()
    };
    sites.visit_file(&syntax);
    let mut found = sites.aliases.clone();
    for (function, idents) in &sites.sequence {
        for pair in idents.windows(2) {
            if pair[0] == "ActionGate" && pair[1] == "new" {
                found.insert(format!("{file}: {function}: ActionGate::new"));
            }
        }
        for ident in idents
            .iter()
            .filter(|i| GATE_BUILDERS.contains(&i.as_str()))
        {
            found.insert(format!("{file}: {function}: {ident}"));
        }
    }
    found
}

#[test]
fn action_gates_are_built_only_at_the_pinned_production_sites() {
    let mut files = Vec::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    collect(&root, &mut files);
    let mut found = BTreeSet::new();
    for path in files {
        let relative = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        found.extend(gate_builds(&relative, &fs::read_to_string(&path).unwrap()));
    }
    assert_eq!(
        found,
        expected(&[
            "main.rs: build_delegate_task_manager: with_action_policy",
            "main.rs: build_mcp_task_manager_with_budget: action_policy_gate",
            "task.rs: action_policy_gate: ActionGate::new",
            "task.rs: action_policy_gate: action_policy_gate",
            "task.rs: default_action_gate: ActionGate::new",
            "task.rs: model_facing_registry: with_action_gate",
            "task.rs: with_action_gate: with_action_gate",
            "task.rs: with_action_policy: action_policy_gate",
            "task.rs: with_action_policy: with_action_gate",
            "task.rs: with_action_policy: with_action_policy",
            "tools.rs: with_action_gate: with_action_gate",
        ]),
        "an action gate is built somewhere new; it must attach the audit sink or be documented"
    );
}

#[test]
fn the_gate_build_scanner_sees_aliases_calls_and_skips_test_code() {
    let source = r#"
        use crate::action_auditor::ActionGate as G;
        type H = std::sync::Arc<ActionGate>;
        type Fine = Vec<u8>;
        fn a() { let _ = ActionGate::new(x, y); }
        fn b() { m.with_action_gate(g); }
        fn c() { println!("{}", ActionGate::new(a, b)); }
        #[cfg(test)]
        fn only_in_tests() { ActionGate::new(a, b); }
    "#;
    let found = gate_builds("x.rs", source);
    assert_eq!(
        found,
        expected(&[
            "x.rs: renames ActionGate",
            "x.rs: type alias of ActionGate",
            "x.rs: a: ActionGate::new",
            "x.rs: b: with_action_gate",
            "x.rs: c: ActionGate::new",
        ])
    );
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_checker_flags_every_way_run_gate_could_skip_the_emit() {
    let wrap = |body: &str| format!("impl G {{ pub async fn run_gate(&self) -> V {{ {body} }} }}");
    let good = wrap("let decided = self.decide(r).await; self.emit(r, &decided); decided");
    assert_eq!(run_gate_problem(&good), None);
    let missing_emit = wrap("let decided = self.decide(r).await; decided");
    assert!(run_gate_problem(&missing_emit)
        .unwrap()
        .contains("three statements"));
    let early = wrap("let decided = self.decide(r).await; if x { return other; } self.emit(r, &decided); decided");
    assert!(run_gate_problem(&early)
        .unwrap()
        .contains("three statements"));
    let other_value = wrap("let decided = self.decide(r).await; self.emit(r, &decided); other");
    assert!(run_gate_problem(&other_value)
        .unwrap()
        .contains("exactly the value"));
    let wrong_source = wrap("let decided = self.review(r).await; self.emit(r, &decided); decided");
    assert!(run_gate_problem(&wrong_source)
        .unwrap()
        .contains("`decide`"));
    let wrong_call = wrap("let decided = self.decide(r).await; self.log(r, &decided); decided");
    assert!(run_gate_problem(&wrong_call).unwrap().contains("`emit`"));
    let not_a_call = wrap("let decided = other; self.emit(r, &decided); decided");
    assert!(run_gate_problem(&not_a_call).unwrap().contains("`decide`"));
    let no_semicolon = wrap("let decided = self.decide(r).await; let x = 1; decided");
    assert!(run_gate_problem(&no_semicolon).unwrap().contains("`emit`"));
    let not_a_let = wrap("self.decide(r).await; self.emit(r, &d); d");
    assert!(run_gate_problem(&not_a_let)
        .unwrap()
        .contains("bind the decision"));
    let tuple = wrap("let (decided, x) = self.decide(r).await; self.emit(r, &decided); decided");
    assert!(run_gate_problem(&tuple).unwrap().contains("plain name"));
    let returns_call = wrap("let decided = self.decide(r).await; self.emit(r, &decided); other()");
    assert!(run_gate_problem(&returns_call)
        .unwrap()
        .contains("return the bound"));
    assert_eq!(
        run_gate_problem("fn run_gate() {}").unwrap(),
        "run_gate is missing"
    );
}

#[test]
fn the_scanner_attributes_idents_to_functions_and_skips_test_code() {
    let text = r#"
        fn decide() { log.append(a); }
        fn other() { println!("{}", append); }
        mod inner { fn nested() { append(); } }
        #[cfg(test)]
        mod tests { fn t() { append(); } }
    "#;
    assert_eq!(
        users(text, "append"),
        expected(&["decide", "nested", "other"])
    );
}
