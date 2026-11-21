use proc_macro2::{TokenStream, TokenTree};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const TOKEN: &str = "from_audited";

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

#[derive(Default)]
struct Uses {
    count: usize,
}

impl Uses {
    fn tokens(&mut self, stream: TokenStream) {
        for token in stream {
            match token {
                TokenTree::Ident(ident) if ident == TOKEN => self.count += 1,
                TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Uses {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_fn(self, node);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_impl_item_fn(self, node);
        }
    }

    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        if node == TOKEN {
            self.count += 1;
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
        syn::visit::visit_macro(self, node);
    }
}

fn scan_source(text: &str) -> usize {
    let file = syn::parse_file(text).expect("source parses");
    let mut uses = Uses::default();
    uses.visit_file(&file);
    uses.count
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

#[test]
fn from_audited_is_called_only_from_the_audited_identity_in_task_rs() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut found = BTreeMap::new();
    for path in files {
        let count = scan_source(&fs::read_to_string(&path).unwrap());
        if count > 0 {
            let rel = path.strip_prefix(&src).unwrap().display().to_string();
            found.insert(rel, count);
        }
    }
    assert_eq!(
        found,
        BTreeMap::from([
            ("hub/lineage.rs".to_string(), 2),
            ("task.rs".to_string(), 1)
        ]),
        "SessionBinding::from_audited gained a production use; pin the new call site deliberately"
    );
}

#[test]
fn the_scanner_sees_calls_paths_methods_and_macros_but_not_test_code() {
    assert_eq!(
        scan_source("fn f() { SessionBinding::from_audited(a, b, c); }"),
        1
    );
    assert_eq!(
        scan_source("impl X { fn g(&self) { Self::from_audited(a); } }"),
        1
    );
    assert_eq!(
        scan_source("fn f() { let k = SessionBinding::from_audited; }"),
        1
    );
    assert_eq!(
        scan_source("fn f() { vec![SessionBinding::from_audited(a)]; }"),
        1
    );
    assert_eq!(scan_source("impl S { fn from_audited() {} }"), 1);
    assert_eq!(
        scan_source("#[cfg(test)] mod t { fn f() { S::from_audited(); } }"),
        0
    );
    assert_eq!(scan_source("#[cfg(test)] fn f() { S::from_audited(); }"), 0);
    assert_eq!(scan_source("fn f() { other::from_audited_x(); }"), 0);
}
