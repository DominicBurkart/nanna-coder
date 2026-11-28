use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const GRANT: &str = "OperatorGrant";
const HOME: &str = "hub/operator.rs";
const TRAIT_ALLOWED: &[&str] = &["Debug"];
const DERIVE_ALLOWED: &[&str] = &["Clone", "Copy", "PartialEq", "Eq"];

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

fn named_in(stream: proc_macro2::TokenStream) -> usize {
    let mut found = 0;
    for tree in stream {
        match tree {
            proc_macro2::TokenTree::Ident(id) if id == GRANT => found += 1,
            proc_macro2::TokenTree::Group(group) => found += named_in(group.stream()),
            _ => {}
        }
    }
    found
}

#[derive(Default)]
struct Facts {
    stack: Vec<String>,
    mentions: usize,
    constructors: BTreeSet<String>,
    check_callers: BTreeSet<String>,
    traits: BTreeSet<String>,
    derives: BTreeSet<String>,
}

impl Facts {
    fn current(&self) -> String {
        self.stack.last().cloned().unwrap_or_default()
    }
}

fn is_grant_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(path) if path.path.is_ident(GRANT))
}

impl<'ast> Visit<'ast> for Facts {
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

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if node.ident == GRANT {
            for attr in &node.attrs {
                let _ = attr.parse_nested_meta(|meta| {
                    if let Some(id) = meta.path.get_ident() {
                        self.derives.insert(id.to_string());
                    }
                    Ok(())
                });
            }
        }
        syn::visit::visit_item_struct(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_cfg_test(&node.attrs) && is_grant_type(&node.self_ty) {
            let path = node.trait_.as_ref().map(|(_, path, _)| path);
            let last = path.and_then(|p| p.segments.last());
            if let Some(segment) = last {
                self.traits.insert(segment.ident.to_string());
            }
        }
        syn::visit::visit_item_impl(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func {
            let segments = &path.path.segments;
            let first = segments.first().map(|s| s.ident.to_string());
            let last = segments.last().map(|s| s.ident.to_string());
            let single = segments.len() == 1;
            let named = first.as_deref() == Some("Self") || first.as_deref() == Some(GRANT);
            if single && named {
                self.constructors.insert(self.current());
            }
            if !single && named && last.as_deref() == Some("check") {
                self.check_callers.insert(self.current());
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.mentions += named_in(node.tokens.clone());
        syn::visit::visit_macro(self, node);
    }

    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        if node == GRANT {
            self.mentions += 1;
        }
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

fn mentions_by_file() -> BTreeMap<String, usize> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut all = BTreeMap::new();
    for path in files {
        let found = facts(&fs::read_to_string(&path).unwrap());
        let relative = path.strip_prefix(&src).unwrap();
        all.insert(
            relative.to_string_lossy().replace('\\', "/"),
            found.mentions,
        );
    }
    all
}

fn home() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(HOME);
    fs::read_to_string(path).unwrap()
}

fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| item.to_string()).collect()
}

fn grant_is_sealed(text: &str) -> bool {
    let found = facts(text);
    let traits_ok = found.traits.is_subset(&names(TRAIT_ALLOWED));
    let derives_ok = found.derives.is_subset(&names(DERIVE_ALLOWED));
    let built_once = found.constructors == names(&["check"]);
    let checked_once = found.check_callers == names(&["available", "from_environment"]);
    traits_ok && derives_ok && built_once && checked_once
}

const GOOD: &str = "
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OperatorGrant(());
impl OperatorGrant {
    pub(crate) fn check(a: bool, b: bool) -> Result<Self, E> { Ok(Self(())) }
    pub fn from_environment() -> Result<Self, E> { Self::check(true, false) }
}
fn available(a: bool, b: bool) -> bool { OperatorGrant::check(a, b).is_ok() }
impl fmt::Debug for OperatorGrant { fn fmt(&self, f: &mut F) -> R { f.write_str(\"x\") } }
#[cfg(test)]
mod tests { fn forge() -> OperatorGrant { OperatorGrant(()) } }
";

#[test]
fn only_the_operator_module_names_the_grant() {
    let all = mentions_by_file();
    assert!(all.len() > 50, "scanned too few files: {}", all.len());
    assert!(all.contains_key("mcp/mod.rs"));
    assert!(all.contains_key("tools.rs"));
    assert!(all.contains_key("main.rs"));
    let named: BTreeSet<&str> = all
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(file, _)| file.as_str())
        .collect();
    assert_eq!(named, BTreeSet::from([HOME]));
}

#[test]
fn the_grant_has_one_constructor_one_checker_and_no_forging_traits() {
    assert!(grant_is_sealed(&home()));
}

#[test]
fn the_scanner_accepts_the_good_shape() {
    assert!(grant_is_sealed(GOOD));
    assert_eq!(facts(GOOD).mentions, 4);
}

#[test]
fn the_scanner_flags_a_second_construction_site() {
    let planted = "fn sneaky() -> OperatorGrant { OperatorGrant(()) }";
    assert!(!grant_is_sealed(&format!("{GOOD} {planted}")));
    let method = "impl OperatorGrant { fn sneaky() -> Self { Self(()) } }";
    assert!(!grant_is_sealed(&format!("{GOOD} {method}")));
}

#[test]
fn the_scanner_flags_a_second_check_caller() {
    let planted = "fn sneaky() { let _ = OperatorGrant::check(true, false); }";
    assert!(!grant_is_sealed(&format!("{GOOD} {planted}")));
    let inside = GOOD.replace("Self::check(true, false)", "Self(())");
    assert!(!grant_is_sealed(&inside));
}

#[test]
fn the_scanner_flags_forging_traits_and_derives() {
    let default = "impl Default for OperatorGrant { fn default() -> Self { todo!() } }";
    assert!(!grant_is_sealed(&format!("{GOOD} {default}")));
    let derived = GOOD.replace("Clone, Copy", "Clone, Copy, Default, serde::Deserialize");
    assert!(!grant_is_sealed(&derived));
}

#[test]
fn the_scanner_sees_a_grant_hidden_in_a_macro_and_ignores_test_code() {
    let hidden = "fn f() { println!(\"{:?}\", OperatorGrant::from_environment()); }";
    assert_eq!(facts(hidden).mentions, 1);
    let tests_only = "#[cfg(test)] mod t { fn g() { OperatorGrant::from_environment(); } }";
    assert_eq!(facts(tests_only).mentions, 0);
}
