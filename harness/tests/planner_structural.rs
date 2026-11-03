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
    aliases: Vec<String>,
}

fn plain(name: &str) -> String {
    name.strip_prefix("r#").unwrap_or(name).to_string()
}

fn gate_type(name: &str) -> bool {
    matches!(name, "SpawnGate" | "Allowed")
}

fn watched(name: &str) -> bool {
    FORBIDDEN.contains(&name) || matches!(name, "SpawnGate" | "Allowed")
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

impl Idents {
    fn alias(&mut self, name: &syn::Ident, ty: &syn::Type) {
        let mut mentioned = Idents::default();
        mentioned.visit_type(ty);
        if mentioned.sequence.iter().any(|ident| watched(ident)) {
            self.aliases
                .push(format!("aliases a watched type as {name}"));
        }
    }

    fn tokens(&mut self, stream: TokenStream) {
        for token in stream {
            match token {
                TokenTree::Ident(ident) => {
                    let name = plain(&ident.to_string());
                    if name == "SpawnGate" {
                        self.aliases.push("passes SpawnGate to a macro".to_string());
                    }
                    self.sequence.push(name);
                }
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

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        let renamed = plain(&node.ident.to_string());
        if watched(&renamed) {
            self.aliases
                .push(format!("renames {renamed} as {}", node.rename));
        }
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        self.alias(&node.ident, &node.ty);
        syn::visit::visit_item_type(self, node);
    }

    fn visit_impl_item_type(&mut self, node: &'ast syn::ImplItemType) {
        self.alias(&node.ident, &node.ty);
        syn::visit::visit_impl_item_type(self, node);
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        if node.mac.path.is_ident("macro_rules") {
            self.aliases.push("defines a macro_rules macro".to_string());
        }
        syn::visit::visit_item_macro(self, node);
    }

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.sequence.push(plain(&ident.to_string()));
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let mut mentioned = Idents::default();
        mentioned.visit_type(&node.self_ty);
        for ident in mentioned.sequence.iter().filter(|i| gate_type(i)) {
            self.aliases.push(format!("implements on {ident}"));
        }
        syn::visit::visit_item_impl(self, node);
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        if let Some(qself) = &node.qself {
            let mut mentioned = Idents::default();
            mentioned.visit_type(&qself.ty);
            for ident in mentioned.sequence.iter().filter(|i| gate_type(i)) {
                self.aliases.push(format!("qualified path through {ident}"));
            }
        }
        syn::visit::visit_expr_path(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
    }
}

fn scan(source: &str) -> Idents {
    let file = syn::parse_file(source).unwrap();
    let mut idents = Idents::default();
    idents.visit_file(&file);
    idents
}

fn production_idents(source: &str) -> Vec<String> {
    scan(source).sequence
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
    let scanned = scan(source);
    let idents = scanned.sequence;
    let mut found: Vec<String> = scanned.aliases;
    found.extend(
        idents
            .iter()
            .filter(|ident| FORBIDDEN.contains(&ident.as_str()))
            .map(|ident| format!("uses {ident}")),
    );
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
    let body = function_idents(source, "audit_plan_within");
    let position = |name: &str| body.iter().position(|ident| ident == name);
    let inside = body.iter().filter(|ident| *ident == "check").count();
    if total != 1 || inside != 1 {
        return Some(format!(
            "the gate is called {total} times, {inside} inside audit_plan_within"
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
        mod inner { fn g() { let _ = AuditedIdentity::split(proof); } }
        use crate::auditor::SpawnGate as Gx;
        type Gy = SpawnGate;
        use crate::auditor::AuditLog as L;
        use std::fmt::Debug as D;
        type Fine = Vec<u8>;
        fn h() { let _ = Gx::new(a, b, c); let _ = Gy::new(a, b, c); }
        #[cfg(test)]
        mod tests { fn e() { SpawnGate::new(x, y, z); RuleAuditor::new(); } }
    "#;
    let found = forbidden_constructs(source);
    for expected in [
        "calls SpawnGate::new",
        "uses TaskManager",
        "names dispatch function submit_spawn",
        "names dispatch function enqueue_all",
        "calls Allowed::new",
        "calls tokio::spawn",
        "uses AuditedIdentity",
        "renames SpawnGate as Gx",
        "aliases a watched type as Gy",
        "renames AuditLog as L",
    ] {
        assert!(found.iter().any(|f| f == expected), "{expected}: {found:?}");
    }
    assert!(!found.iter().any(|f| f.contains("RuleAuditor")));
    assert!(!found
        .iter()
        .any(|f| f.contains("as D") || f.contains("Fine")));
}

#[test]
fn the_checker_sees_macro_pasted_types_and_associated_type_aliases() {
    let source = r#"
        macro_rules! mk { ($t:ty) => { $t::new() }; }
        fn m() { mk!(SpawnGate); }
        trait Tr { type A; }
        struct S;
        impl Tr for S { type A = SpawnGate; }
        impl Tr for T { type A = Vec<u8>; }
        fn n() { let _ = <S as Tr>::A::new(a, b, c); }
    "#;
    let found = forbidden_constructs(source);
    for expected in [
        "defines a macro_rules macro",
        "passes SpawnGate to a macro",
        "aliases a watched type as A",
    ] {
        assert!(found.iter().any(|f| f == expected), "{expected}: {found:?}");
    }
    assert_eq!(found.iter().filter(|f| f.contains("as A")).count(), 1);
}

#[test]
fn the_checker_sees_raw_identifiers_inherent_impls_and_qualified_paths() {
    let source = r#"
        fn a() { SpawnGate::r#new(x); }
        fn b() { r#SpawnGate::new(x); }
        fn c() { SpawnGate::r#with_hook(x); }
        impl SpawnGate { fn d() {} }
        impl Allowed { fn e() {} }
        impl Display for Plan { fn f() {} }
        fn g() { <SpawnGate>::new(x); <Allowed as Default>::default(); <u8 as Default>::default(); }
    "#;
    let found = forbidden_constructs(source);
    for expected in [
        "calls SpawnGate::new",
        "calls SpawnGate::with_hook",
        "implements on SpawnGate",
        "implements on Allowed",
        "qualified path through SpawnGate",
        "qualified path through Allowed",
    ] {
        assert!(found.iter().any(|f| f == expected), "{expected}: {found:?}");
    }
    assert!(!found.iter().any(|f| f.contains("Plan") || f.contains("u8")));
}

#[test]
fn a_raw_identifier_in_a_use_rename_or_a_macro_body_is_normalised() {
    let renamed = forbidden_constructs("use a::r#SpawnGate as G;");
    assert_eq!(renamed, ["renames SpawnGate as G"]);
    let in_macro = forbidden_constructs("fn z() { m!(r#SpawnGate::new); }");
    assert!(in_macro.contains(&"passes SpawnGate to a macro".to_string()));
    assert!(in_macro.contains(&"calls SpawnGate::new".to_string()));
}

#[test]
fn the_checker_flags_a_missing_extra_or_misordered_gate_call() {
    let ordered =
        "async fn audit_plan_within() { let r = gate.request_with_caller(); gate.check(r).await; }";
    assert_eq!(gate_order_problem(ordered), None);
    let none = "async fn audit_plan_within() { let r = gate.request_with_caller(); }";
    assert!(gate_order_problem(none).unwrap().contains("0 times"));
    assert!(gate_order_problem(none)
        .unwrap()
        .contains("audit_plan_within"));
    let twice = "async fn audit_plan_within() { gate.check(a); gate.check(b); }";
    assert!(gate_order_problem(twice).unwrap().contains("2 times"));
    let outside = "fn helper() { g.check(a); } async fn audit_plan_within() { gate.check(r); }";
    assert!(gate_order_problem(outside).unwrap().contains("2 times"));
    let reversed = "async fn audit_plan_within() { gate.check(r); gate.request_with_caller(); }";
    assert!(gate_order_problem(reversed)
        .unwrap()
        .contains("before checking"));
}
