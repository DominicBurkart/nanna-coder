use proc_macro2::{TokenStream, TokenTree};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const MARKER: &str = "released";
const MUTATORS: &[&str] = &[
    "take",
    "replace",
    "insert",
    "get_or_insert",
    "get_or_insert_with",
    "as_mut",
    "as_deref_mut",
];
const WRITERS: &[(&str, &str)] = &[("hub/store/messages.rs", "resolve")];
const RELEASE_BUILDERS: &[(&str, &str)] = &[("hub/operator.rs", "release_message")];
const TRANSITION_CALLERS: &[(&str, &str)] = &[
    ("hub/operator.rs", "drop_message"),
    ("hub/operator.rs", "release_message"),
];

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

#[derive(Default)]
struct Facts {
    stack: Vec<String>,
    writes: BTreeSet<String>,
    builders: BTreeSet<String>,
    transitions: BTreeSet<String>,
    macro_mentions: usize,
    test_modules: Vec<String>,
}

fn names_marker(expr: &syn::Expr) -> bool {
    matches!(expr, syn::Expr::Field(field) if matches!(&field.member, syn::Member::Named(id) if id == MARKER))
}

fn is_none(expr: &syn::Expr) -> bool {
    matches!(expr, syn::Expr::Path(path) if path.path.is_ident("None"))
}

fn mentions(stream: TokenStream) -> usize {
    let mut found = 0;
    for tree in stream {
        match tree {
            TokenTree::Ident(id) if id == MARKER => found += 1,
            TokenTree::Group(group) => found += mentions(group.stream()),
            _ => {}
        }
    }
    found
}

impl Facts {
    fn here(&self) -> String {
        self.stack.last().cloned().unwrap_or_default()
    }

    fn scoped(&mut self, name: String, walk: impl FnOnce(&mut Self)) {
        self.stack.push(name);
        walk(self);
        self.stack.pop();
    }
}

impl<'ast> Visit<'ast> for Facts {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if is_cfg_test(&node.attrs) {
            self.test_modules.push(node.ident.to_string());
        } else {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_cfg_test(&node.attrs) {
            let name = node.sig.ident.to_string();
            self.scoped(name, |me| syn::visit::visit_item_fn(me, node));
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_cfg_test(&node.attrs) {
            let name = node.sig.ident.to_string();
            self.scoped(name, |me| syn::visit::visit_impl_item_fn(me, node));
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.macro_mentions += mentions(node.tokens.clone());
    }

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        let last = node.path.segments.last().map(|s| s.ident.to_string());
        if last.as_deref() == Some("Release") {
            self.builders.insert(self.here());
        }
        for field in &node.fields {
            let named = matches!(&field.member, syn::Member::Named(id) if id == MARKER);
            if named && !is_none(&field.expr) {
                self.writes.insert(self.here());
            }
        }
        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_expr_assign(&mut self, node: &'ast syn::ExprAssign) {
        if names_marker(&node.left) {
            self.writes.insert(self.here());
        }
        syn::visit::visit_expr_assign(self, node);
    }

    fn visit_expr_reference(&mut self, node: &'ast syn::ExprReference) {
        if node.mutability.is_some() && names_marker(&node.expr) {
            self.writes.insert(self.here());
        }
        syn::visit::visit_expr_reference(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let mutator = MUTATORS.iter().any(|name| node.method == name);
        if mutator && names_marker(&node.receiver) {
            self.writes.insert(self.here());
        }
        if node.method == "transition" && node.args.len() == 4 {
            self.transitions.insert(self.here());
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

fn facts(text: &str) -> Facts {
    let file = syn::parse_file(text).expect("source parses");
    let mut found = Facts::default();
    found.visit_file(&file);
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

fn module_dir(path: &Path) -> PathBuf {
    let stem = path.file_stem().unwrap().to_string_lossy().to_string();
    let parent = path.parent().unwrap();
    let root_file = ["mod", "lib", "main"].contains(&stem.as_str());
    if root_file {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    }
}

fn production() -> Vec<(String, Facts)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut parsed = Vec::new();
    let mut test_only: BTreeSet<PathBuf> = BTreeSet::new();
    for path in files {
        let found = facts(&fs::read_to_string(&path).unwrap());
        let dir = module_dir(&path);
        for name in &found.test_modules {
            test_only.insert(dir.join(format!("{name}.rs")));
            test_only.insert(dir.join(name).join("mod.rs"));
        }
        parsed.push((path, found));
    }
    let mut all = Vec::new();
    for (path, found) in parsed {
        if !test_only.contains(&path) {
            let relative = path.strip_prefix(&src).unwrap();
            all.push((relative.to_string_lossy().replace('\\', "/"), found));
        }
    }
    all
}

fn pairs(all: &[(String, Facts)], pick: fn(&Facts) -> &BTreeSet<String>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (file, facts) in all {
        for function in pick(facts) {
            found.insert(format!("{file}::{function}"));
        }
    }
    found
}

fn expected(items: &[(&str, &str)]) -> BTreeSet<String> {
    items.iter().map(|(f, n)| format!("{f}::{n}")).collect()
}

fn writes_of(facts: &Facts) -> &BTreeSet<String> {
    &facts.writes
}

fn builders_of(facts: &Facts) -> &BTreeSet<String> {
    &facts.builders
}

fn transitions_of(facts: &Facts) -> &BTreeSet<String> {
    &facts.transitions
}

#[test]
fn only_the_store_resolver_writes_a_release_marker() {
    let all = production();
    assert!(all.len() > 50, "scanned {} files", all.len());
    assert_eq!(pairs(&all, writes_of), expected(WRITERS));
}

#[test]
fn no_macro_in_production_code_mentions_the_marker() {
    let all = production();
    let total: usize = all.iter().map(|(_, facts)| facts.macro_mentions).sum();
    assert_eq!(total, 0);
}

#[test]
fn only_the_operator_resolvers_build_a_release_or_call_the_transition() {
    let all = production();
    assert_eq!(pairs(&all, builders_of), expected(RELEASE_BUILDERS));
    assert_eq!(pairs(&all, transitions_of), expected(TRANSITION_CALLERS));
}

#[test]
fn the_store_resolver_is_where_the_scan_finds_the_write() {
    let all = production();
    let store = all.iter().find(|(file, _)| file == "hub/store/messages.rs");
    let found = store.expect("store source scanned");
    assert!(found.1.writes.contains("resolve"));
}

const GOOD: &str = "
impl T {
    fn resolve(&self) -> M {
        let mut m = self.found.clone();
        let kept = m.released.take();
        m.released = release.or(kept);
        m
    }
    fn admit(m: M) -> M { M { released: None, ..m } }
}
#[cfg(test)]
mod tests { fn forge(m: &mut M) { m.released = Some(R); let _ = M { released: Some(R) }; } }
";

fn written_in(text: &str) -> Vec<String> {
    facts(text).writes.into_iter().collect()
}

#[test]
fn the_scanner_accepts_the_good_shape_and_ignores_test_code() {
    assert_eq!(written_in(GOOD), ["resolve"]);
    assert_eq!(facts(GOOD).macro_mentions, 0);
}

#[test]
fn the_scanner_flags_an_assignment_a_literal_a_shorthand_and_a_mutable_borrow() {
    let planted = [
        "fn a(m: &mut M) { m.released = Some(R); }",
        "fn a(m: M) -> M { M { released: Some(R), ..m } }",
        "fn a(m: M, released: Option<R>) -> M { M { released, ..m } }",
        "fn a(m: &mut M) { std::mem::swap(&mut m.released, &mut x); }",
        "fn a(m: &mut M) { m.released.replace(R); }",
        "fn a(m: &mut M) { m.released.take(); }",
    ];
    for source in planted {
        assert_eq!(written_in(source), ["a"], "{source}");
    }
    assert!(written_in("fn a(m: M) -> M { M { released: None, ..m } }").is_empty());
    assert!(written_in("fn a(m: &M) -> bool { m.released.is_some() }").is_empty());
}

#[test]
fn the_scanner_flags_a_marker_hidden_in_a_macro() {
    let in_macro = "macro_rules! m { () => { x.released = Some(1) } }";
    assert_eq!(facts(in_macro).macro_mentions, 1);
    let call = "fn a() { m!(x.released = y); }";
    assert_eq!(facts(call).macro_mentions, 1);
    let nested = "fn a() { m!({ (released) }); }";
    assert_eq!(facts(nested).macro_mentions, 1);
}

#[test]
fn the_scanner_skips_out_of_line_test_modules() {
    let text = "#[cfg(test)] mod contract; mod real;";
    assert_eq!(facts(text).test_modules, ["contract"]);
    let dir = module_dir(Path::new("/s/hub/store/mod.rs"));
    assert_eq!(dir, Path::new("/s/hub/store"));
    let nested = module_dir(Path::new("/s/hub/send.rs"));
    assert_eq!(nested, Path::new("/s/hub/send"));
}

#[test]
fn the_scanner_flags_a_release_builder_and_a_transition_caller() {
    let builder = "fn mint() -> R { Release { at: now() } }";
    assert_eq!(facts(builder).builders.len(), 1);
    let caller = "fn go(s: &S) { s.transition(a, b, c, d); }";
    assert_eq!(facts(caller).transitions.len(), 1);
    let other = "fn go(s: &S) { s.transition(a); }";
    assert!(facts(other).transitions.is_empty());
}
