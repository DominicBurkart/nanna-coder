use proc_macro2::{TokenStream, TokenTree};
use std::fs;
use std::path::Path;
use syn::visit::Visit;

const FORBIDDEN: &[&str] = &[
    "AuditedIdentity",
    "BoundIdentity",
    "TaskManager",
    "AuditLog",
    "AuditContext",
    "RuleAuditor",
    "ModelAuditor",
    "FailClosedModelAuditor",
    "Gate",
    "Escalator",
    "EscalatorHook",
    "EscalatorSpawnHook",
    "EscalationLog",
    "EscalationPlane",
    "NoopEscalationHook",
    "SpawnEscalationHook",
    "CostAccountant",
    "Dispatcher",
    "Command",
];

const FORBIDDEN_PAIRS: &[(&str, &str)] = &[
    ("SpawnGate", "new"),
    ("SpawnGate", "with_hook"),
    ("SpawnGate", "for_repo"),
    ("Allowed", "new"),
    ("Allowed", "default"),
    ("tokio", "spawn"),
    ("tokio", "spawn_blocking"),
    ("thread", "spawn"),
];

#[derive(Default)]
struct Idents {
    sequence: Vec<String>,
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

impl Idents {
    fn tokens(&mut self, stream: TokenStream) {
        for token in stream {
            match token {
                TokenTree::Ident(ident) => self.sequence.push(ident.to_string()),
                TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Idents {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.sequence.push(ident.to_string());
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
    }
}

fn production_idents(source: &str) -> Vec<String> {
    let file = syn::parse_file(source).unwrap();
    let mut idents = Idents::default();
    idents.visit_file(&file);
    idents.sequence
}

fn function_idents(source: &str, name: &str) -> Vec<String> {
    let file = syn::parse_file(source).unwrap();
    let mut idents = Idents::default();
    for item in &file.items {
        if let syn::Item::Fn(function) = item {
            if function.sig.ident == name {
                idents.visit_item_fn(function);
            }
        }
    }
    idents.sequence
}

fn forbidden_constructs(source: &str) -> Vec<String> {
    let idents = production_idents(source);
    let mut found: Vec<String> = idents
        .iter()
        .filter(|ident| FORBIDDEN.contains(&ident.as_str()))
        .map(|ident| format!("uses {ident}"))
        .collect();
    for pair in idents.windows(2) {
        if FORBIDDEN_PAIRS.contains(&(pair[0].as_str(), pair[1].as_str())) {
            found.push(format!("calls {}::{}", pair[0], pair[1]));
        }
    }
    for ident in &idents {
        if ident.starts_with("submit") || ident.starts_with("enqueue") {
            found.push(format!("names dispatch function {ident}"));
        }
    }
    found
}

fn gate_order_problem(source: &str) -> Option<String> {
    let total = production_idents(source)
        .iter()
        .filter(|ident| *ident == "check")
        .count();
    let body = function_idents(source, "audit_plan");
    let position = |name: &str| body.iter().position(|ident| ident == name);
    let inside = body.iter().filter(|ident| *ident == "check").count();
    if total != 1 || inside != 1 {
        return Some(format!(
            "the gate is called {total} times, {inside} inside audit_plan"
        ));
    }
    match (position("request_with_caller"), position("check")) {
        (Some(request), Some(check)) if request < check => None,
        _ => Some("audit_plan does not derive the request before checking it".to_string()),
    }
}

fn planner_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/planner.rs");
    fs::read_to_string(path).unwrap()
}

#[test]
fn the_planner_constructs_no_gate_manager_escalator_accountant_or_audit_token() {
    assert_eq!(
        forbidden_constructs(&planner_source()),
        Vec::<String>::new()
    );
}

#[test]
fn the_gate_is_called_once_inside_audit_plan_after_the_request_is_derived() {
    assert_eq!(gate_order_problem(&planner_source()), None);
}

#[test]
fn the_checker_sees_forbidden_constructs_in_code_and_macro_bodies_but_not_in_tests() {
    let source = r#"
        fn a() { let _ = SpawnGate::new(x, y, z); }
        fn b() { println!("{}", TaskManager::new()); }
        fn c() { gate.submit_spawn(allowed); }
        fn d() { enqueue_all(); Allowed::new(r, i); }
        fn f() { tokio::spawn(work()); }
        #[cfg(test)]
        mod tests { fn e() { SpawnGate::new(x, y, z); AuditLog::in_memory(); } }
    "#;
    let found = forbidden_constructs(source);
    for expected in [
        "calls SpawnGate::new",
        "uses TaskManager",
        "names dispatch function submit_spawn",
        "names dispatch function enqueue_all",
        "calls Allowed::new",
        "calls tokio::spawn",
    ] {
        assert!(found.iter().any(|f| f == expected), "{expected}: {found:?}");
    }
    assert!(!found.iter().any(|f| f.contains("AuditLog")));
}

#[test]
fn the_checker_flags_a_missing_extra_or_misordered_gate_call() {
    let ordered =
        "async fn audit_plan() { let r = gate.request_with_caller(); gate.check(r).await; }";
    assert_eq!(gate_order_problem(ordered), None);
    let none = "async fn audit_plan() { let r = gate.request_with_caller(); }";
    assert!(gate_order_problem(none).unwrap().contains("0 times"));
    let twice = "async fn audit_plan() { gate.check(a); gate.check(b); }";
    assert!(gate_order_problem(twice).unwrap().contains("2 times"));
    let outside = "fn helper() { g.check(a); } async fn audit_plan() { gate.check(r); }";
    assert!(gate_order_problem(outside).unwrap().contains("2 times"));
    let reversed = "async fn audit_plan() { gate.check(r); gate.request_with_caller(); }";
    assert!(gate_order_problem(reversed)
        .unwrap()
        .contains("before checking"));
}
