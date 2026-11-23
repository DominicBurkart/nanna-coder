use proc_macro2::{TokenStream, TokenTree};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const UNIQUE: &[&str] = &[
    "review_send",
    "review_note",
    "quarantine_send",
    "quarantine_note",
    "sessions_list",
    "sessions_tree",
    "session_describe",
];
const TWO_ARGS: &[&str] = &["send", "recv", "set_note"];

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

fn named(ident: &syn::Ident, names: &[&str]) -> bool {
    names.iter().any(|name| ident == name)
}

fn top_level_arguments(group: &proc_macro2::Group) -> usize {
    let mut commas = 0;
    let mut any = false;
    for token in group.stream() {
        any = true;
        if matches!(&token, TokenTree::Punct(p) if p.as_char() == ',') {
            commas += 1;
        }
    }
    if any {
        commas + 1
    } else {
        0
    }
}

#[derive(Default)]
struct Scan {
    stack: Vec<String>,
    found: Vec<String>,
}

impl Scan {
    fn note(&mut self, what: &str) {
        let function = self.stack.last().cloned().unwrap_or_default();
        self.found.push(format!("{function}: {what}"));
    }

    fn tokens(&mut self, stream: TokenStream) {
        let flat: Vec<TokenTree> = stream.into_iter().collect();
        for (index, token) in flat.iter().enumerate() {
            match token {
                TokenTree::Group(group) => self.tokens(group.stream()),
                TokenTree::Ident(ident) if named(ident, UNIQUE) => self.note(&ident.to_string()),
                TokenTree::Ident(ident) if named(ident, TWO_ARGS) => {
                    let next = flat.get(index + 1);
                    if let Some(TokenTree::Group(group)) = next {
                        if top_level_arguments(group) == 2 {
                            self.note(&format!("{ident}(2 arguments)"));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn use_tree(&mut self, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Rename(rename)
                if named(&rename.ident, UNIQUE) || named(&rename.ident, TWO_ARGS) =>
            {
                self.note(&format!("rename of {}", rename.ident));
            }
            syn::UseTree::Path(path) => self.use_tree(&path.tree),
            syn::UseTree::Group(group) => group.items.iter().for_each(|t| self.use_tree(t)),
            _ => {}
        }
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
        if named(&node.method, UNIQUE) {
            self.note(&node.method.to_string());
        }
        if named(&node.method, TWO_ARGS) && node.args.len() == 2 {
            self.note(&format!("{}(2 arguments)", node.method));
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_field(&mut self, node: &'ast syn::ExprField) {
        if let syn::Member::Named(member) = &node.member {
            if named(member, UNIQUE) || named(member, TWO_ARGS) {
                self.note(&format!("field {member}"));
            }
        }
        syn::visit::visit_expr_field(self, node);
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        let idents: Vec<&syn::Ident> = node.path.segments.iter().map(|s| &s.ident).collect();
        for ident in &idents {
            if named(ident, UNIQUE) {
                self.note(&ident.to_string());
            }
        }
        for pair in idents.windows(2) {
            if pair[0] == "MessageHub" && named(pair[1], TWO_ARGS) {
                self.note(&format!("MessageHub::{}", pair[1]));
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

fn scan(text: &str) -> Vec<String> {
    let file = syn::parse_file(text).expect("source parses");
    let mut scan = Scan::default();
    scan.visit_file(&file);
    let mut found = scan.found;
    found.sort();
    found
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

fn relative(path: &Path, base: &Path) -> String {
    let tail = path.strip_prefix(base).unwrap();
    tail.to_string_lossy().replace('\\', "/")
}

fn production() -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut found = Vec::new();
    for path in files {
        let rel = relative(&path, &src);
        for hit in scan(&fs::read_to_string(&path).unwrap()) {
            found.push(format!("{rel}: {hit}"));
        }
    }
    found.sort();
    found
}

fn expected(items: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = items.iter().map(ToString::to_string).collect();
    all.sort();
    all
}

#[test]
fn the_hub_is_reached_only_from_the_registered_tools_the_gate_router_and_the_hub_itself() {
    assert_eq!(
        production(),
        expected(&[
            "hub/router.rs: on_quarantine: quarantine_note",
            "hub/router.rs: on_quarantine: quarantine_send",
            "hub/router.rs: review: review_note",
            "hub/router.rs: review: review_send",
            "hub/store/contract.rs: set_note_replaces: set_note(2 arguments)",
            "hub/store/contract.rs: set_note_replaces: set_note(2 arguments)",
            "hub/store/contract.rs: set_note_replaces: set_note(2 arguments)",
            "hub/tools.rs: run: recv(2 arguments)",
            "hub/tools.rs: run: send(2 arguments)",
            "hub/tools.rs: run: session_describe",
            "hub/tools.rs: run: sessions_list",
            "hub/tools.rs: run: sessions_tree",
            "hub/tools.rs: run: set_note(2 arguments)",
        ]),
        "a production caller of the hub's agent-facing methods appeared; pin it deliberately"
    );
}

#[test]
fn the_scanner_rejects_a_second_caller_a_rename_an_alias_a_non_path_callee_and_a_macro() {
    assert_eq!(
        scan("fn a() { hub.send(sender, req); }"),
        ["a: send(2 arguments)"]
    );
    assert_eq!(
        scan("fn a() { hub.recv(to, req); }"),
        ["a: recv(2 arguments)"]
    );
    assert_eq!(
        scan("fn a() { hub.set_note(me, note); }"),
        ["a: set_note(2 arguments)"]
    );
    assert_eq!(
        scan("fn a() { hub.review_send(s, r); }"),
        ["a: review_send"]
    );
    assert_eq!(
        scan("fn a() { let _ = view::sessions_list(); }"),
        ["a: sessions_list"]
    );
    assert_eq!(
        scan("fn a() { let f = crate::hub::view::session_describe; }"),
        ["a: session_describe"]
    );
    assert_eq!(scan("fn a() { (hub.send)(s, r); }"), ["a: field send"]);
    assert_eq!(
        scan("fn a() { MessageHub::recv(&hub, a, b); }"),
        ["a: MessageHub::recv"]
    );
    assert_eq!(
        scan("use crate::hub::view::sessions_tree as walk;"),
        [": rename of sessions_tree"]
    );
    assert_eq!(
        scan("use x::{y, MessageHub as H, send as s};"),
        [": rename of send"]
    );
    assert_eq!(
        scan("fn a() { m!(hub.send(s, r)); }"),
        ["a: send(2 arguments)"]
    );
    assert_eq!(
        scan("fn a() { m!(quarantine_note(a)); }"),
        ["a: quarantine_note"]
    );
    assert_eq!(
        scan("impl X { fn b(&self) { self.hub.quarantine_send(a, b, c); } }"),
        ["b: quarantine_send"]
    );
}

#[test]
fn the_scanner_ignores_other_two_argument_look_alikes_definitions_imports_and_test_code() {
    assert!(
        scan("fn a() { tx.send(value); rx.recv(); registry.set_note(id, text, at); }").is_empty()
    );
    assert!(scan("fn send(a: A, b: B) {} impl X { fn recv(&self, a: A, b: B) {} }").is_empty());
    assert!(scan("use crate::hub::view::{sessions_list, session_describe};").is_empty());
    assert!(scan("#[cfg(test)] fn a() { hub.send(s, r); }").is_empty());
    assert!(scan("#[cfg(test)] mod t { fn a() { hub.send(s, r); } }").is_empty());
    assert!(scan("#[cfg(test)] impl X { fn a() { hub.send(s, r); } }").is_empty());
    assert!(scan("impl X { #[cfg(test)] fn a() { hub.send(s, r); } }").is_empty());
    assert!(scan("fn a() { m!(tx.send(v)); m!(); }").is_empty());
}
