use proc_macro2::{TokenStream, TokenTree};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const WRITE_PATH_NAMES: &[&str] = &[
    "HubLock",
    "LOCK_FILE",
    "claim",
    "Claim",
    "rewrite_private",
    "open_append",
    "OpenOptions",
    "FileJournal",
    "create",
    "create_dir",
    "create_dir_all",
    "write",
    "set_permissions",
    "remove_file",
    "rename",
    "commit",
];
const READ_ONLY_TYPES: &[&str] = &["ReadOnlyMessageStore", "ReadOnlySessionStore"];

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

#[derive(Default)]
struct Facts {
    stack: Vec<String>,
    only: Option<String>,
    only_impl: Option<Vec<String>>,
    inside_impl: bool,
    idents: BTreeSet<String>,
    callers: BTreeSet<String>,
    trait_impls: BTreeSet<String>,
}

impl Facts {
    fn scoped(&self) -> bool {
        let in_fn = match &self.only {
            Some(name) => self.stack.iter().any(|f| f == name),
            None => true,
        };
        let in_impl = self.only_impl.is_none() || self.inside_impl;
        in_fn && in_impl
    }

    fn current(&self) -> String {
        self.stack.last().cloned().unwrap_or_default()
    }

    fn tokens(&mut self, stream: TokenStream) {
        for tree in stream {
            match tree {
                TokenTree::Ident(id) => self.note(id.to_string()),
                TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }

    fn note(&mut self, name: String) {
        if self.scoped() {
            self.idents.insert(name);
        }
    }
}

fn self_type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

impl<'ast> Visit<'ast> for Facts {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        self.stack.push(node.sig.ident.to_string());
        syn::visit::visit_item_fn(self, node);
        self.stack.pop();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        let name = self_type_name(&node.self_ty).unwrap_or_default();
        if let Some((_, path, _)) = &node.trait_ {
            let last = path.segments.last().map(|s| s.ident.to_string());
            self.trait_impls
                .insert(format!("{name}:{}", last.unwrap_or_default()));
        }
        let wanted = self.only_impl.as_ref();
        self.inside_impl = wanted.is_some_and(|names| names.contains(&name));
        syn::visit::visit_item_impl(self, node);
        self.inside_impl = false;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        self.stack.push(node.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, node);
        self.stack.pop();
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if self.scoped() {
            self.callers
                .insert(format!("{}->{}", self.current(), node.method));
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
        syn::visit::visit_macro(self, node);
    }

    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        self.note(node.to_string());
    }
}

fn facts(text: &str, only_fn: Option<&str>, only_impl: Option<&[&str]>) -> Facts {
    let file = syn::parse_file(text).expect("source parses");
    let mut found = Facts {
        only: only_fn.map(str::to_string),
        only_impl: only_impl.map(|names| names.iter().map(ToString::to_string).collect()),
        ..Facts::default()
    };
    found.visit_file(&file);
    found
}

fn source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(relative);
    fs::read_to_string(path).unwrap()
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

fn mentions(names: &[&str]) -> BTreeSet<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 50, "scanned too few files");
    let mut found = BTreeSet::new();
    for path in files {
        let relative = path.strip_prefix(&src).unwrap();
        let name = relative.to_string_lossy().replace('\\', "/");
        let seen = facts(&fs::read_to_string(&path).unwrap(), None, None).idents;
        if names.iter().any(|wanted| seen.contains(*wanted)) {
            found.insert(name);
        }
    }
    found
}

fn forbidden_in(found: &Facts) -> Vec<String> {
    let bad: Vec<String> = WRITE_PATH_NAMES
        .iter()
        .filter(|name| found.idents.contains(**name))
        .map(ToString::to_string)
        .collect();
    bad
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(ToString::to_string).collect()
}

#[test]
fn the_read_only_engine_entry_never_names_a_lock_a_claim_a_rewrite_or_a_write() {
    let text = source("hub/store/engine.rs");
    let found = facts(&text, Some("open_read_only"), None);
    assert!(found.idents.contains("read_existing"), "{:?}", found.idents);
    assert_eq!(forbidden_in(&found), Vec::<String>::new());
}

#[test]
fn the_read_only_stores_and_the_cli_reader_never_name_a_write_path_or_a_commit() {
    for (file, impls) in [
        ("hub/store/messages.rs", &["ReadOnlyMessageStore"][..]),
        ("hub/store/sessions.rs", &["ReadOnlySessionStore"][..]),
    ] {
        let found = facts(&source(file), None, Some(impls));
        assert!(found.idents.contains("open_read_only"), "{file}");
        assert!(found.idents.contains("read"), "{file}");
        assert_eq!(forbidden_in(&found), Vec::<String>::new(), "{file}");
    }
    let reader = facts(&source("hub/readonly.rs"), None, None);
    assert!(reader.idents.contains("ReadOnlyMessageStore"));
    assert_eq!(forbidden_in(&reader), Vec::<String>::new());
}

#[test]
fn the_read_only_stores_implement_no_store_trait() {
    for (file, name) in [
        ("hub/store/messages.rs", READ_ONLY_TYPES[0]),
        ("hub/store/sessions.rs", READ_ONLY_TYPES[1]),
    ] {
        let found = facts(&source(file), None, None);
        let traits: Vec<&String> = found
            .trait_impls
            .iter()
            .filter(|entry| entry.starts_with(&format!("{name}:")))
            .collect();
        assert!(traits.is_empty(), "{traits:?}");
    }
}

#[test]
fn open_read_only_is_named_only_by_the_engine_and_the_two_read_only_stores() {
    assert_eq!(
        mentions(&["open_read_only"]),
        set(&[
            "hub/store/engine.rs",
            "hub/store/messages.rs",
            "hub/store/sessions.rs",
        ])
    );
}

#[test]
fn the_read_only_stores_are_named_only_by_the_store_module_and_the_cli_reader() {
    assert_eq!(
        mentions(READ_ONLY_TYPES),
        set(&[
            "hub/readonly.rs",
            "hub/store/messages.rs",
            "hub/store/mod.rs",
            "hub/store/sessions.rs",
        ])
    );
}

#[test]
fn the_cli_binary_reaches_the_journals_only_through_the_operator_reports() {
    let main = facts(&source("main.rs"), Some("run_hub"), None);
    for banned in [
        "HubRuntime",
        "HubLock",
        "JsonlMessageStore",
        "JsonlSessionStore",
        "MessageHub",
        "OperatorGrant",
        "ReadOnlyHub",
        "resolve_quarantine",
    ] {
        assert!(!main.idents.contains(banned), "{banned}");
    }
    for wanted in [
        "sessions_report",
        "quarantine_list_report",
        "quarantine_show_report",
    ] {
        assert!(main.idents.contains(wanted), "{wanted}");
    }
}

#[test]
fn quarantined_text_leaves_the_reader_only_through_the_grant_gated_show() {
    let operator = facts(&source("hub/operator.rs"), None, None);
    assert!(operator.callers.contains("show_report_with->message"));
    let message_callers: Vec<&String> = operator
        .callers
        .iter()
        .filter(|entry| entry.ends_with("->message"))
        .collect();
    assert_eq!(message_callers.len(), 1, "{message_callers:?}");
    assert!(operator
        .callers
        .contains("quarantine_show_report->and_then"));
    let gated = signature_takes_grant(&source("hub/operator.rs"), "show_report_with");
    assert!(gated);
}

fn signature_takes_grant(text: &str, name: &str) -> bool {
    struct Sig(String, bool);
    impl<'ast> Visit<'ast> for Sig {
        fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
            if node.sig.ident == self.0 {
                let first = node.sig.inputs.first();
                self.1 = matches!(first, Some(syn::FnArg::Typed(t)) if names_grant(&t.ty));
            }
        }
    }
    let file = syn::parse_file(text).unwrap();
    let mut sig = Sig(name.to_string(), false);
    sig.visit_file(&file);
    sig.1
}

fn names_grant(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Reference(reference) => names_grant(&reference.elem),
        other => self_type_name(other).is_some_and(|n| n == "OperatorGrant"),
    }
}

const GOOD_ENGINE: &str = "
impl<T> Engine<T> {
    pub(super) fn open_read_only(path: &Path) -> Result<Self, E> {
        let existing = read_existing(path)?;
        Ok(Self { journal: None })
    }
    pub(super) fn open(path: &Path) -> Result<Self, E> { let c = lock.claim(path)?; rewrite_private(path); Ok(x) }
}
#[cfg(test)]
mod tests { fn t() { OpenOptions::new(); } }
";

#[test]
fn the_scanner_accepts_the_read_only_entry_and_ignores_the_writer_and_test_code() {
    let found = facts(GOOD_ENGINE, Some("open_read_only"), None);
    assert_eq!(forbidden_in(&found), Vec::<String>::new());
    assert!(found.idents.contains("read_existing"));
}

#[test]
fn the_scanner_flags_a_lock_a_rewrite_an_append_handle_and_a_write_in_the_read_entry() {
    for planted in [
        "let c = lock.claim(path)?;",
        "rewrite_private(path, &b)?;",
        "let f = open_append(path)?;",
        "let f = OpenOptions::new();",
        "std::fs::write(path, b)?;",
        "let l = HubLock::acquire(d)?;",
        "fs::create_dir_all(d)?;",
        "self.commit(plan)?;",
    ] {
        let text = GOOD_ENGINE.replace("let existing =", &format!("{planted} let existing ="));
        let found = facts(&text, Some("open_read_only"), None);
        assert!(!forbidden_in(&found).is_empty(), "{planted}");
    }
}

#[test]
fn the_scanner_sees_a_name_hidden_in_a_macro_and_a_trait_impl_on_a_read_only_type() {
    let hidden = "fn open_read_only() { println!(\"{:?}\", rewrite_private); }";
    assert!(!forbidden_in(&facts(hidden, Some("open_read_only"), None)).is_empty());
    let traits = "impl MessageStore for ReadOnlyMessageStore { fn append(&self) {} }";
    let found = facts(traits, None, None);
    assert!(found
        .trait_impls
        .contains("ReadOnlyMessageStore:MessageStore"));
    let inherent = "impl ReadOnlyMessageStore { fn get(&self) {} }";
    assert!(facts(inherent, None, None).trait_impls.is_empty());
}

#[test]
fn the_scanner_scopes_to_the_named_impls_and_methods() {
    let text = "impl A { fn x(&self) { self.commit(); } } impl ReadOnlyMessageStore { fn y(&self) { self.read(); } }";
    let found = facts(text, None, Some(&["ReadOnlyMessageStore"]));
    assert!(found.idents.contains("read") && !found.idents.contains("commit"));
    assert!(found.callers.contains("y->read"));
}

#[test]
fn the_grant_signature_check_accepts_a_grant_first_and_refuses_its_absence() {
    let gated = "fn show(_g: &OperatorGrant, p: u8) {}";
    assert!(signature_takes_grant(gated, "show"));
    let open = "fn show(p: u8, _g: &OperatorGrant) {}";
    assert!(!signature_takes_grant(open, "show"));
    assert!(!signature_takes_grant("fn show() {}", "show"));
}
