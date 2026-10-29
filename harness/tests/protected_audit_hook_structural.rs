use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .map(|meta| matches!(&meta, syn::Meta::Path(path) if path.is_ident("test")))
                .unwrap_or(false)
    })
}

#[derive(Default)]
struct Idents {
    file: String,
    found: BTreeMap<String, usize>,
    renames: BTreeSet<String>,
}

fn plain(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

impl Idents {
    fn note(&mut self, name: &str) {
        *self
            .found
            .entry(format!("{}: {}", self.file, plain(name)))
            .or_default() += 1;
    }

    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                proc_macro2::TokenTree::Ident(ident) => self.note(&ident.to_string()),
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

    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        self.note(&ident.to_string());
    }

    fn visit_use_tree(&mut self, node: &'ast syn::UseTree) {
        if let syn::UseTree::Rename(rename) = node {
            let original = rename.ident.to_string();
            let entry = format!("{}: renames {}", self.file, plain(&original));
            self.renames.insert(entry);
        }
        syn::visit::visit_use_tree(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
    }
}

fn scan_source(file: &str, source: &str) -> Idents {
    let syntax = syn::parse_file(source).unwrap();
    let mut scan = Idents {
        file: file.to_string(),
        ..Idents::default()
    };
    scan.visit_file(&syntax);
    scan
}

fn production() -> Vec<Idents> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            scan_source(&relative, &fs::read_to_string(&path).unwrap())
        })
        .collect()
}

fn mentions(name: &str) -> BTreeMap<String, usize> {
    let suffix = format!(": {name}");
    production()
        .into_iter()
        .flat_map(|scan| scan.found)
        .filter_map(|(entry, count)| {
            entry
                .strip_suffix(&suffix)
                .map(|file| (file.to_string(), count))
        })
        .collect()
}

fn pinned(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
    entries
        .iter()
        .map(|(file, count)| (file.to_string(), *count))
        .collect()
}

#[test]
fn the_noop_audit_hook_is_named_a_pinned_number_of_times_in_a_pinned_set_of_files() {
    assert_eq!(
        mentions("NoopAuditHook"),
        pinned(&[("lib.rs", 1), ("protected.rs", 2), ("workspace.rs", 3)]),
        "NoopAuditHook gained or lost a mention in production code: an alias, macro, re-export or new use could hand a task workspace the do-nothing hook"
    );
}

#[test]
fn no_production_use_renames_anything_to_hide_a_noop_hook_or_a_workspace_constructor() {
    let renames: BTreeSet<String> = production()
        .into_iter()
        .flat_map(|scan| scan.renames)
        .filter(|rename| {
            ["NoopAuditHook", "TaskWorkspace", "ProtectedPathEscalation"]
                .iter()
                .any(|name| rename.ends_with(&format!("renames {name}")))
        })
        .collect();
    assert_eq!(renames, BTreeSet::new());
}

#[test]
fn only_the_task_runner_constructs_task_workspaces_and_it_installs_a_hook_each_time() {
    assert_eq!(
        mentions("TaskWorkspace"),
        pinned(&[("task.rs", 5), ("workspace.rs", 3)]),
        "a new production path names TaskWorkspace; a workspace built without with_audit_hook silently gets NoopAuditHook"
    );
    assert_eq!(
        mentions("with_audit_hook"),
        pinned(&[("task.rs", 2), ("workspace.rs", 1)])
    );
}

#[test]
fn the_task_runner_installs_the_escalating_hook_by_default() {
    assert_eq!(
        mentions("ProtectedPathEscalation"),
        pinned(&[
            ("escalation/mod.rs", 1),
            ("escalation/protected_hook.rs", 3),
            ("task.rs", 1)
        ])
    );
}

#[test]
fn the_scanner_sees_raw_identifiers_macros_aliases_and_renames_but_not_cfg_test_items() {
    let seen = scan_source(
        "x.rs",
        "fn f() { let _ = r#NoopAuditHook; } fn g() { m!(NoopAuditHook); } pub type Sneak = protected::NoopAuditHook; macro_rules! n { () => { NoopAuditHook } } pub use p::NoopAuditHook as Other;",
    );
    assert_eq!(seen.found["x.rs: NoopAuditHook"], 5);
    assert!(seen.renames.contains("x.rs: renames NoopAuditHook"));
    let hidden = scan_source(
        "x.rs",
        "#[cfg(test)] mod t { fn f() { let _ = NoopAuditHook; } } #[cfg(test)] fn h() { NoopAuditHook; } struct S; #[cfg(test)] impl S { fn k(&self) { NoopAuditHook; } }",
    );
    assert!(!hidden.found.contains_key("x.rs: NoopAuditHook"));
}
