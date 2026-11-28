use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const INDEX_FILE: &str = "hub/index.rs";
const PANICKING_MACROS: [&str; 14] = [
    "panic",
    "unreachable",
    "todo",
    "unimplemented",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "dbg",
    "println",
    "eprintln",
    "print",
    "eprint",
    "write",
];

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

#[derive(Default)]
struct Scan {
    idents: BTreeMap<String, usize>,
    struct_literals: BTreeMap<String, usize>,
    strings: Vec<String>,
    indexing: usize,
    registrations: usize,
    banned_calls: Vec<String>,
}

impl Scan {
    fn tokens(&mut self, stream: proc_macro2::TokenStream) {
        for token in stream {
            match token {
                proc_macro2::TokenTree::Ident(ident) => {
                    *self.idents.entry(ident.to_string()).or_default() += 1;
                }
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Scan {
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

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_impl(self, node);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_impl_item_fn(self, node);
        }
    }

    fn visit_attribute(&mut self, _node: &'ast syn::Attribute) {}

    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        *self.idents.entry(node.to_string()).or_default() += 1;
    }

    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        self.strings.push(node.value());
    }

    fn visit_expr_index(&mut self, node: &'ast syn::ExprIndex) {
        self.indexing += 1;
        syn::visit::visit_expr_index(self, node);
    }

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        if let Some(last) = node.path.segments.last() {
            let name = last.ident.to_string();
            let count = self.struct_literals.entry(name).or_default();
            *count += 1;
        }
        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let name = node.method.to_string();
        if name == "register" {
            self.registrations += 1;
        }
        if ["unwrap", "expect", "unwrap_unchecked"].contains(&name.as_str()) {
            self.banned_calls.push(name);
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
        if let Some(last) = node.path.segments.last() {
            let name = last.ident.to_string();
            if PANICKING_MACROS.contains(&name.as_str()) {
                self.banned_calls.push(format!("{name}!"));
            }
        }
        syn::visit::visit_macro(self, node);
    }
}

fn scan_source(text: &str) -> Scan {
    let file = syn::parse_file(text).expect("source parses");
    let mut scan = Scan::default();
    scan.visit_file(&file);
    scan
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

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn scanned_tree() -> BTreeMap<String, Scan> {
    let src = src_dir();
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 50);
    let mut all = BTreeMap::new();
    for path in files {
        let rel = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        all.insert(rel, scan_source(&fs::read_to_string(&path).unwrap()));
    }
    all
}

fn index_scan() -> Scan {
    scan_source(&fs::read_to_string(src_dir().join(INDEX_FILE)).unwrap())
}

fn counts(tree: &BTreeMap<String, Scan>, pick: impl Fn(&Scan) -> usize) -> BTreeMap<String, usize> {
    let found = tree.iter().map(|(file, scan)| (file.clone(), pick(scan)));
    found.filter(|(_, count)| *count > 0).collect()
}

#[test]
fn register_librarian_is_defined_in_index_rs_and_used_only_by_the_task_layer_seam() {
    let tree = scanned_tree();
    let found = counts(&tree, |scan| {
        scan.idents.get("register_librarian").copied().unwrap_or(0)
    });
    assert_eq!(
        found,
        BTreeMap::from([
            (INDEX_FILE.to_string(), 1),
            ("task/librarian.rs".to_string(), 2)
        ]),
        "register_librarian gained a production use; pin the new call site deliberately"
    );
}

#[test]
fn an_index_view_is_built_by_one_struct_literal_in_index_rs() {
    let tree = scanned_tree();
    let found = counts(&tree, |scan| {
        scan.struct_literals.get("IndexView").copied().unwrap_or(0)
    });
    assert_eq!(found, BTreeMap::from([(INDEX_FILE.to_string(), 1)]));
}

#[test]
fn index_rs_registers_exactly_the_two_librarian_tools_and_no_hub_tool() {
    let scan = index_scan();
    assert_eq!(scan.registrations, 1);
    let named: BTreeSet<&str> = scan
        .strings
        .iter()
        .map(String::as_str)
        .filter(|s| s.starts_with("index_"))
        .collect();
    assert_eq!(named, BTreeSet::from(["index_read", "index_search"]));
    for text in &scan.strings {
        for banned in ["msg_", "session_", "sessions_", "hub_"] {
            assert!(!text.starts_with(banned), "{text}");
        }
    }
}

#[test]
fn index_rs_has_no_io_no_indexing_and_no_panicking_call() {
    let scan = index_scan();
    for word in [
        "fs",
        "File",
        "OpenOptions",
        "Command",
        "TcpStream",
        "UdpSocket",
        "println",
        "eprintln",
        "dbg",
        "tracing",
        "Instant",
        "SystemTime",
        "env",
        "spawn",
    ] {
        assert!(!scan.idents.contains_key(word), "{word}");
    }
    assert_eq!(scan.indexing, 0);
    assert!(scan.banned_calls.is_empty(), "{:?}", scan.banned_calls);
}

#[test]
fn the_scanner_sees_idents_literals_registrations_io_indexing_and_skips_test_code() {
    let scan = scan_source(
        "fn f() { let v = IndexView { a: 1 }; r.register(Box::new(T)); \"msg_send\"; x[0]; \
         std::fs::write(a, b); y.unwrap(); panic!(); register_librarian(a); }",
    );
    assert_eq!(scan.struct_literals.get("IndexView"), Some(&1));
    assert_eq!(scan.registrations, 1);
    assert_eq!(scan.strings, ["msg_send"]);
    assert_eq!(scan.indexing, 1);
    assert!(scan.idents.contains_key("fs"));
    assert_eq!(scan.idents.get("register_librarian"), Some(&1));
    assert_eq!(scan.banned_calls, ["unwrap", "panic!"]);
    let skipped = scan_source(
        "#[cfg(test)] mod t { fn f() { IndexView { a: 1 }; register_librarian(); x[0]; } } \
         #[cfg(test)] fn g() { std::fs::read(a); } \
         struct S; #[cfg(test)] impl S { fn h() { fs::read(a); } } \
         impl S { #[cfg(test)] fn i() { File::open(a); } }",
    );
    assert!(!skipped.idents.contains_key("fs"));
    assert!(!skipped.idents.contains_key("File"));
    assert!(skipped.struct_literals.is_empty());
    assert_eq!(skipped.indexing, 0);
    let attributes = scan_source("#[doc = \"sessions_query\"] fn f() {}");
    assert!(attributes.strings.is_empty());
}
