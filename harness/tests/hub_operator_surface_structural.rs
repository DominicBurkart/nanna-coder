use proc_macro2::{TokenStream, TokenTree};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const OPERATOR_READ_AND_SEND: &[&str] = &["root_inbox", "send_as_operator"];
const HOMES: &[&str] = &["mcp/hub_tools.rs", "hub/operator.rs"];
const MCP_FORBIDDEN: &[&str] = &["resolve_quarantine", "OperatorGrant"];
const SOCKETS: &[&str] = &["UnixListener", "TcpListener", "UdpSocket", "bind", "listen"];

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

#[derive(Default)]
struct Callers {
    names: &'static [&'static str],
    stack: Vec<String>,
    found: Vec<String>,
}

impl Callers {
    fn named(&self, ident: &syn::Ident) -> bool {
        self.names.iter().any(|name| ident == name)
    }

    fn note(&mut self, what: &str) {
        let function = self.stack.last().cloned().unwrap_or_default();
        self.found.push(format!("{function}: {what}"));
    }

    fn tokens(&mut self, stream: TokenStream) {
        for token in stream {
            match token {
                TokenTree::Group(group) => self.tokens(group.stream()),
                TokenTree::Ident(ident) if self.named(&ident) => self.note(&ident.to_string()),
                _ => {}
            }
        }
    }

    fn use_tree(&mut self, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Rename(rename) if self.named(&rename.ident) => {
                self.note(&format!("rename of {}", rename.ident));
            }
            syn::UseTree::Path(path) => self.use_tree(&path.tree),
            syn::UseTree::Group(group) => group.items.iter().for_each(|t| self.use_tree(t)),
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for Callers {
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

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if self.named(&node.method) {
            self.note(&node.method.to_string());
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_field(&mut self, node: &'ast syn::ExprField) {
        if let syn::Member::Named(member) = &node.member {
            if self.named(member) {
                self.note(&format!("field {member}"));
            }
        }
        syn::visit::visit_expr_field(self, node);
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        for segment in &node.path.segments {
            if self.named(&segment.ident) {
                self.note(&format!("path {}", segment.ident));
            }
        }
        syn::visit::visit_expr_path(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.use_tree(&node.tree);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
        syn::visit::visit_macro(self, node);
    }
}

fn callers(text: &str) -> Vec<String> {
    callers_of(text, OPERATOR_READ_AND_SEND)
}

fn callers_of(text: &str, names: &'static [&'static str]) -> Vec<String> {
    let file = syn::parse_file(text).expect("source parses");
    let mut scan = Callers {
        names,
        ..Callers::default()
    };
    scan.visit_file(&file);
    let mut found = scan.found;
    found.sort();
    found
}

#[derive(Default)]
struct TestModules {
    inside: usize,
    names: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for TestModules {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let gated = usize::from(is_cfg_test(&node.attrs));
        self.inside += gated;
        if node.content.is_none() && self.inside > 0 {
            self.names.insert(node.ident.to_string());
        }
        syn::visit::visit_item_mod(self, node);
        self.inside -= gated;
    }
}

fn test_modules(text: &str) -> BTreeSet<String> {
    let file = syn::parse_file(text).expect("source parses");
    let mut scan = TestModules::default();
    scan.visit_file(&file);
    scan.names
}

fn idents(stream: TokenStream, out: &mut BTreeSet<String>) {
    for token in stream {
        match token {
            TokenTree::Group(group) => idents(group.stream(), out),
            TokenTree::Ident(ident) => {
                out.insert(ident.to_string());
            }
            _ => {}
        }
    }
}

fn socket_idents(text: &str) -> Vec<String> {
    let stream: TokenStream = text.parse().expect("source lexes");
    let mut all = BTreeSet::new();
    idents(stream, &mut all);
    SOCKETS
        .iter()
        .filter(|name| all.contains(**name))
        .map(ToString::to_string)
        .collect()
}

fn mentions(text: &str, names: &[&str]) -> Vec<String> {
    let hits = names.iter().filter(|name| text.contains(**name));
    hits.map(ToString::to_string).collect()
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

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

fn relative(path: &Path, base: &Path) -> String {
    let tail = path.strip_prefix(base).unwrap();
    tail.to_string_lossy().replace('\\', "/")
}

fn sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let all: Vec<(String, String)> = files
        .iter()
        .map(|path| (relative(path, &src), read(path)))
        .collect();
    assert!(all.len() > 50, "the source tree was not found");
    all
}

fn stem_of(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or_default();
    name.trim_end_matches(".rs").to_string()
}

fn test_only_files(all: &[(String, String)]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (_, text) in all {
        names.extend(test_modules(text));
    }
    let tests = all.iter().filter(|(rel, _)| names.contains(&stem_of(rel)));
    tests.map(|(rel, _)| rel.clone()).collect()
}

fn production_callers() -> Vec<String> {
    let all = sources();
    let skipped = test_only_files(&all);
    let mut found = Vec::new();
    for (rel, text) in &all {
        if !skipped.contains(rel) {
            found.extend(callers(text).into_iter().map(|hit| format!("{rel}: {hit}")));
        }
    }
    found.sort();
    found
}

#[test]
fn the_root_mailbox_read_and_the_operator_send_have_production_callers_only_in_their_homes() {
    let found = production_callers();
    let homes = |hit: &String| {
        HOMES
            .iter()
            .any(|home| hit.starts_with(&format!("{home}: ")))
    };
    let strangers: Vec<&String> = found.iter().filter(|hit| !homes(hit)).collect();
    assert!(strangers.is_empty(), "{strangers:?}");
    assert_eq!(found, ["mcp/hub_tools.rs: read_inbox: root_inbox"]);
}

#[test]
fn out_of_line_test_modules_are_found_so_test_files_are_not_mistaken_for_production() {
    let skipped = test_only_files(&sources());
    assert!(
        skipped.contains("hub/recv/tests/root_inbox_tests.rs"),
        "{skipped:?}"
    );
    assert!(!skipped.contains("mcp/hub_tools.rs"), "{skipped:?}");
    assert!(!skipped.contains("hub/operator.rs"), "{skipped:?}");
}

#[test]
fn no_mcp_file_mentions_the_quarantine_resolution_or_the_operator_grant() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("mcp");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() >= 6, "{files:?}");
    for path in files {
        let found = mentions(&read(&path), MCP_FORBIDDEN);
        assert!(found.is_empty(), "{}: {found:?}", path.display());
    }
}

#[test]
fn the_operator_surface_files_open_no_listener_and_bind_no_socket() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for home in HOMES {
        let text = read(&src.join(home));
        assert!(text.len() > 1000, "{home} was not read");
        assert_eq!(socket_idents(&text), Vec::<String>::new(), "{home}");
    }
}

#[test]
fn mcp_serve_enables_the_hub_tools_only_through_the_managers_own_hub() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    let text = read(&path);
    assert_eq!(
        callers_of(&text, &["with_manager_hub"]),
        ["run_mcp_server: with_manager_hub"]
    );
    assert!(callers_of(&text, &["with_hub", "hub_inbox"]).is_empty());
}

#[test]
fn the_single_door_denylist_and_this_pin_do_not_list_each_others_names() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/hub_single_door_structural.rs");
    let door = read(&path);
    assert!(door.contains("const UNIQUE"), "the single-door pin moved");
    assert!(mentions(&door, OPERATOR_READ_AND_SEND).is_empty());
}

#[test]
fn the_caller_scanner_sees_a_call_a_path_a_field_a_rename_and_a_macro() {
    assert_eq!(
        callers("fn a() { hub.root_inbox(&r, q); }"),
        ["a: root_inbox"]
    );
    assert_eq!(
        callers("fn a() { hub.send_as_operator(r, q); }"),
        ["a: send_as_operator"]
    );
    assert_eq!(
        callers("fn a() { MessageHub::root_inbox(&hub, r, q); }"),
        ["a: path root_inbox"]
    );
    assert_eq!(
        callers("fn a() { (hub.root_inbox)(r, q); }"),
        ["a: field root_inbox"]
    );
    assert_eq!(
        callers("use x::y::send_as_operator as go;"),
        [": rename of send_as_operator"]
    );
    assert_eq!(
        callers("fn a() { m!(hub.root_inbox(r, q)); }"),
        ["a: root_inbox"]
    );
    assert_eq!(
        callers("impl X { fn b(&self) { self.hub.root_inbox(r, q); } }"),
        ["b: root_inbox"]
    );
}

#[test]
fn the_caller_scanner_ignores_definitions_imports_look_alikes_and_test_code() {
    assert!(callers("fn root_inbox(a: A) {} impl X { fn send_as_operator(&self) {} }").is_empty());
    assert!(callers("use crate::hub::recv::{root_inbox, RootRequest};").is_empty());
    assert!(callers("fn a() { hub.inbox(r); hub.root_inbox_tests(); }").is_empty());
    assert!(callers("#[cfg(test)] fn a() { hub.root_inbox(r, q); }").is_empty());
    assert!(callers("#[cfg(test)] mod t { fn a() { hub.root_inbox(r, q); } }").is_empty());
    assert!(callers("#[cfg(test)] impl X { fn a() { hub.root_inbox(r, q); } }").is_empty());
    assert!(callers("impl X { #[cfg(test)] fn a() { hub.send_as_operator(r); } }").is_empty());
}

#[test]
fn the_test_module_scanner_finds_out_of_line_modules_under_a_test_gate_only() {
    let text = "#[cfg(test)] mod tests { mod inner; } #[cfg(test)] mod outer; mod plain;";
    let found: Vec<String> = test_modules(text).into_iter().collect();
    assert_eq!(found, ["inner", "outer"]);
    assert!(test_modules("mod plain; mod inline { fn a() {} }").is_empty());
}

#[test]
fn the_socket_scanner_sees_each_listener_name_in_code_and_macros_but_not_in_comments() {
    assert_eq!(
        socket_idents("fn a() { let s = UnixListener::bind(p); }"),
        ["UnixListener", "bind"]
    );
    assert_eq!(
        socket_idents("fn a() { m!(TcpListener::new()); }"),
        ["TcpListener"]
    );
    assert_eq!(socket_idents("fn a() { s.listen(5); }"), ["listen"]);
    assert_eq!(socket_idents("fn a() { UdpSocket::x(); }"), ["UdpSocket"]);
    assert!(socket_idents("// UnixListener bind\nfn a() { let x = \"bind\"; }").is_empty());
}

#[test]
fn the_mention_scanner_sees_a_name_anywhere_in_the_text() {
    assert_eq!(
        mentions(
            "// resolve_quarantine\nfn a(g: OperatorGrant) {}",
            MCP_FORBIDDEN
        ),
        ["resolve_quarantine", "OperatorGrant"]
    );
    assert!(mentions("fn a() { hub.root_inbox(r, q); }", MCP_FORBIDDEN).is_empty());
}
