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

#[test]
fn every_gate_the_cli_builds_gets_the_telemetry_sink() {
    let main = source("src/main.rs");
    let production = main.split("\n#[cfg(test)]\nmod tests").next().unwrap();
    for builder in ["build_mcp_task_manager", "build_delegate_task_manager"] {
        let callers: BTreeSet<String> = users(production, builder)
            .into_iter()
            .filter(|caller| !caller.starts_with("build_"))
            .collect();
        assert!(!callers.is_empty(), "{builder} has no caller");
        for caller in callers {
            let attached = users(production, "export_action_audit").contains(&caller);
            assert!(
                attached,
                "{caller} builds a gate without attaching the sink"
            );
        }
    }
    assert!(users(production, "spawn_telemetry_flush").contains("run_mcp_server"));
}

#[test]
fn action_gates_are_built_only_in_the_task_manager_and_the_cli() {
    let mut files = Vec::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    collect(&root, &mut files);
    for path in files {
        let relative = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if relative == "task.rs" || relative == "main.rs" {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for name in ["action_policy_gate", "with_action_policy"] {
            let builders = users(&text, name);
            assert!(builders.is_empty(), "{relative} builds a gate with {name}");
        }
    }
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
        #[cfg(test)]
        mod tests { fn t() { append(); } }
    "#;
    assert_eq!(users(text, "append"), expected(&["decide", "other"]));
}
