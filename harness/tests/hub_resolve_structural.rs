use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const RESOLVE: &str = "resolve_quarantine";
const GRANT: &str = "OperatorGrant";
const ACTION: &str = "ResolveAction";
const HOME: &str = "hub/operator.rs";
const WRITERS: [&str; 2] = ["release_message", "drop_message"];

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

fn tokens_named(stream: proc_macro2::TokenStream, names: &[&str]) -> usize {
    let mut found = 0;
    for tree in stream {
        match tree {
            proc_macro2::TokenTree::Ident(id) if names.iter().any(|n| id == n) => found += 1,
            proc_macro2::TokenTree::Group(group) => found += tokens_named(group.stream(), names),
            _ => {}
        }
    }
    found
}

#[derive(Default)]
struct FnFacts {
    name: String,
    takes_grant: bool,
    builds_release: bool,
    transitions: bool,
    calls: BTreeSet<String>,
}

struct Facts {
    names: Vec<String>,
    mentions: usize,
    fns: Vec<FnFacts>,
    open: Vec<usize>,
    skip_tests: bool,
}

impl Facts {
    fn new(names: &[&str], skip_tests: bool) -> Self {
        Self {
            names: names.iter().map(|n| n.to_string()).collect(),
            mentions: 0,
            fns: Vec::new(),
            open: Vec::new(),
            skip_tests,
        }
    }

    fn wanted(&self) -> Vec<&str> {
        self.names.iter().map(String::as_str).collect()
    }

    fn enter(&mut self, sig: &syn::Signature) {
        let takes_grant = sig.inputs.iter().any(takes_the_grant);
        let name = sig.ident.to_string();
        self.fns.push(FnFacts {
            name,
            takes_grant,
            ..FnFacts::default()
        });
        self.open.push(self.fns.len() - 1);
    }

    fn current(&mut self) -> Option<&mut FnFacts> {
        let index = self.open.last().copied();
        index.and_then(|at| self.fns.get_mut(at))
    }

    fn skipped(&self, attrs: &[syn::Attribute]) -> bool {
        self.skip_tests && is_cfg_test(attrs)
    }
}

fn takes_the_grant(arg: &syn::FnArg) -> bool {
    let syn::FnArg::Typed(typed) = arg else {
        return false;
    };
    let syn::Type::Reference(reference) = &*typed.ty else {
        return false;
    };
    matches!(&*reference.elem, syn::Type::Path(path) if path.path.is_ident(GRANT))
}

impl<'ast> Visit<'ast> for Facts {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !self.skipped(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !self.skipped(&node.attrs) {
            self.enter(&node.sig);
            syn::visit::visit_item_fn(self, node);
            self.open.pop();
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !self.skipped(&node.attrs) {
            self.enter(&node.sig);
            syn::visit::visit_impl_item_fn(self, node);
            self.open.pop();
        }
    }

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        let last = node.path.segments.last().map(|s| s.ident.to_string());
        let release = last.as_deref() == Some("Release");
        if let Some(current) = self.current() {
            current.builds_release |= release;
        }
        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let name = node.method.to_string();
        let transition = name == "transition" && node.args.len() == 4;
        if let Some(current) = self.current() {
            current.transitions |= transition;
            current.calls.insert(name);
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.mentions += tokens_named(node.tokens.clone(), &self.wanted());
        syn::visit::visit_macro(self, node);
    }

    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        let hit = self.names.iter().any(|name| node == name);
        self.mentions += usize::from(hit);
    }
}

fn facts_of(text: &str, names: &[&str], skip_tests: bool) -> Facts {
    let file = syn::parse_file(text).expect("source parses");
    let mut found = Facts::new(names, skip_tests);
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

fn mentions_by_file(names: &[&str], skip_tests: bool) -> BTreeMap<String, usize> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut all = BTreeMap::new();
    for path in files {
        let found = facts_of(&fs::read_to_string(&path).unwrap(), names, skip_tests);
        let relative = path.strip_prefix(&src).unwrap();
        let key = relative.to_string_lossy().replace('\\', "/");
        all.insert(key, found.mentions);
    }
    all
}

fn home() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(HOME);
    fs::read_to_string(path).unwrap()
}

fn gated(text: &str) -> bool {
    let found = facts_of(text, &[], true);
    let writers: Vec<&FnFacts> = found
        .fns
        .iter()
        .filter(|f| f.builds_release || f.transitions)
        .collect();
    let names: BTreeSet<&str> = writers.iter().map(|f| f.name.as_str()).collect();
    let expected: BTreeSet<&str> = WRITERS.into_iter().collect();
    let all_take = writers.iter().all(|f| f.takes_grant);
    let entry = found.fns.iter().find(|f| f.name == RESOLVE);
    let entry_takes = entry.is_some_and(|f| f.takes_grant);
    let callers: BTreeSet<&str> = found
        .fns
        .iter()
        .filter(|f| WRITERS.iter().any(|w| f.calls.contains(*w)))
        .map(|f| f.name.as_str())
        .collect();
    let only_entry = callers == BTreeSet::from([RESOLVE]);
    names == expected && all_take && entry_takes && only_entry
}

#[test]
fn only_the_operator_module_names_the_resolver_in_production_code() {
    let all = mentions_by_file(&[RESOLVE], true);
    assert!(all.len() > 50, "scanned too few files: {}", all.len());
    assert!(all.contains_key("mcp/mod.rs"));
    let named: BTreeSet<&str> = all
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(file, _)| file.as_str())
        .collect();
    assert_eq!(named, BTreeSet::from([HOME]));
}

#[test]
fn no_mcp_file_names_the_resolver_the_grant_or_the_action() {
    let all = mentions_by_file(&[RESOLVE, GRANT, ACTION], false);
    let mcp: Vec<(&String, &usize)> = all.iter().filter(|(f, _)| f.starts_with("mcp/")).collect();
    assert!(!mcp.is_empty());
    assert!(mcp.iter().any(|(file, _)| file.as_str() == "mcp/mod.rs"));
    assert!(mcp.iter().all(|(_, count)| **count == 0));
}

#[test]
fn every_writer_takes_the_grant_and_only_the_entry_point_calls_the_writers() {
    assert!(gated(&home()));
}

const GOOD: &str = "
impl MessageHub {
    pub fn resolve_quarantine(&self, grant: &OperatorGrant, id: u8) -> u8 {
        self.release_message(grant, id);
        self.drop_message(grant, id)
    }
    fn release_message(&self, _grant: &OperatorGrant, id: u8) -> u8 {
        let at = now();
        let release = Release { at };
        self.messages.transition(id, a, b, Some(release))
    }
    fn drop_message(&self, _grant: &OperatorGrant, id: u8) -> u8 {
        self.messages.transition(id, a, b, None)
    }
}
#[cfg(test)]
mod tests { fn forge() -> R { Release { at: 1 } } }
";

#[test]
fn the_scanner_accepts_the_good_shape_and_ignores_test_code() {
    assert!(gated(GOOD));
}

#[test]
fn the_scanner_flags_a_writer_without_the_grant() {
    let no_grant = GOOD.replace(
        "fn drop_message(&self, _grant: &OperatorGrant,",
        "fn drop_message(&self,",
    );
    assert!(!gated(&no_grant));
    let by_value = GOOD.replace(
        "_grant: &OperatorGrant, id: u8) -> u8 {\n        let at",
        "_grant: OperatorGrant, id: u8) -> u8 {\n        let at",
    );
    assert!(!gated(&by_value));
}

#[test]
fn the_scanner_flags_a_resolver_without_the_grant_and_an_extra_writer_or_caller() {
    let open = GOOD.replace(
        "grant: &OperatorGrant, id: u8) -> u8 {\n        self.release",
        "grant: u8, id: u8) -> u8 {\n        self.release",
    );
    assert!(!gated(&open));
    let extra = "fn sneaky(g: &OperatorGrant, s: &S) { s.transition(a, b, c, d); }";
    assert!(!gated(&format!("{GOOD} {extra}")));
    let forging = "fn mint() -> R { Release { at: now() } }";
    assert!(!gated(&format!("{GOOD} {forging}")));
    let caller = "fn side(s: &S) { s.release_message(g, 1); }";
    assert!(!gated(&format!("{GOOD} {caller}")));
}

#[test]
fn the_scanner_counts_mentions_in_macros_and_skips_test_code_when_asked() {
    let hidden = "fn f() { println!(\"{:?}\", resolve_quarantine); }";
    assert_eq!(facts_of(hidden, &[RESOLVE], true).mentions, 1);
    let direct = "fn f(h: &H) { h.resolve_quarantine(g, i, a); }";
    assert_eq!(facts_of(direct, &[RESOLVE], true).mentions, 1);
    let only_tests = "#[cfg(test)] mod t { fn g(h: &H) { h.resolve_quarantine(g, i, a); } }";
    assert_eq!(facts_of(only_tests, &[RESOLVE], true).mentions, 0);
    assert_eq!(facts_of(only_tests, &[RESOLVE], false).mentions, 1);
}
