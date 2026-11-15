use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

#[path = "support/test_module_files.rs"]
mod test_module_files;
use test_module_files::{
    cfg_test_file_modules, is_exact_test_file_module, path_targets, skipped_files,
};

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

fn production_from(sources: Vec<(String, String)>) -> Vec<Idents> {
    let skipped = skipped_files(&sources);
    sources
        .iter()
        .filter(|(file, _)| !skipped.contains(file))
        .map(|(file, source)| scan_source(file, source))
        .collect()
}

fn production() -> Vec<Idents> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    production_from(
        files
            .into_iter()
            .map(|path| {
                let text = path.strip_prefix(&root).unwrap().to_string_lossy();
                let relative = text.replace('\\', "/");
                (relative, fs::read_to_string(&path).unwrap())
            })
            .collect(),
    )
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
        pinned(&[
            ("harness_adapter/binding.rs", 3),
            ("task.rs", 5),
            ("workspace.rs", 3)
        ]),
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

#[test]
fn only_exact_cfg_test_file_modules_are_skipped_and_every_other_file_stays_scanned() {
    let sources: Vec<(String, String)> = [
        ("lib.rs", "#[cfg(test)] mod z;"),
        ("z.rs", "fn f() { TaskWorkspace; }"),
        (
            "a/mod.rs",
            "fn helper() {} #[cfg(test)] mod px; #[path = \"px.rs\"] mod py; #[cfg(test)] mod t; #[cfg(not(test))] mod u; mod v; #[cfg(test)] #[path = \"p.rs\"] mod w; #[cfg(test)] mod inline { } #[cfg(test)] mod t2; #[cfg(not(test))] mod t2; #[cfg(test)] mod t3; mod t3; #[cfg(any(test, unix))] mod ya; #[cfg(unix)] mod yb; #[cfg(feature = \"x\")] mod yc; #[cfg(all(test, unix))] mod yd;",
        ),
        ("a/t.rs", "fn f() { TaskWorkspace; }"),
        ("a/u.rs", "fn f() { TaskWorkspace; }"),
        ("a/v.rs", "fn f() { TaskWorkspace; }"),
        ("a/p.rs", "fn f() { TaskWorkspace; }"),
        ("a/w.rs", "fn f() { TaskWorkspace; }"),
        ("a/px.rs", "fn f() { TaskWorkspace; }"),
        ("a/inline.rs", "fn f() { TaskWorkspace; }"),
        ("a/t2.rs", "fn f() { TaskWorkspace; }"),
        ("a/t3.rs", "fn f() { TaskWorkspace; }"),
        ("a/ya.rs", "fn f() { TaskWorkspace; }"),
        ("a/yb.rs", "fn f() { TaskWorkspace; }"),
        ("a/yc.rs", "fn f() { TaskWorkspace; }"),
        ("a/yd.rs", "fn f() { TaskWorkspace; }"),
        ("b.rs", "#[cfg(test)] mod x;"),
        ("b/x/mod.rs", "fn f() { TaskWorkspace; }"),
        ("c.rs", "fn f() { TaskWorkspace; }"),
    ]
    .iter()
    .map(|(file, source)| (file.to_string(), source.to_string()))
    .collect();
    let scanned: BTreeSet<String> = production_from(sources)
        .into_iter()
        .flat_map(|scan| scan.found)
        .filter_map(|(entry, _)| entry.strip_suffix(": TaskWorkspace").map(str::to_string))
        .collect();
    let expected: BTreeSet<String> = [
        "a/inline.rs",
        "a/p.rs",
        "a/px.rs",
        "a/t2.rs",
        "a/t3.rs",
        "a/u.rs",
        "a/v.rs",
        "a/w.rs",
        "a/ya.rs",
        "a/yb.rs",
        "a/yc.rs",
        "a/yd.rs",
        "c.rs",
    ]
    .iter()
    .map(|file| file.to_string())
    .collect();
    assert_eq!(scanned, expected);
}

#[test]
fn the_cfg_test_match_is_exact() {
    assert!(cfg_test_file_modules("a/mod.rs", "#[cfg(any(test, unix))] mod ya;").is_empty());
    assert_eq!(
        cfg_test_file_modules("a/mod.rs", "#[cfg(test)] mod t;"),
        vec!["a/t.rs".to_string(), "a/t/mod.rs".to_string()]
    );
}

#[test]
fn a_module_name_declared_both_as_test_and_otherwise_is_never_skipped() {
    let source = "#[cfg(test)] mod t; #[cfg(not(test))] mod t;";
    assert!(cfg_test_file_modules("a/mod.rs", source).is_empty());
    let unconditional = "#[cfg(test)] mod t; mod t;";
    assert!(cfg_test_file_modules("a/mod.rs", unconditional).is_empty());
    let inline_twin = "#[cfg(test)] mod t; mod t { }";
    assert!(cfg_test_file_modules("a/mod.rs", inline_twin).is_empty());
}

#[test]
fn an_inline_test_module_never_skips_a_file_of_the_same_name() {
    assert!(cfg_test_file_modules("a/mod.rs", "#[cfg(test)] mod inline { }").is_empty());
    assert!(is_exact_test_file_module(
        &syn::parse_str("#[cfg(test)] mod m;").unwrap()
    ));
    assert!(!is_exact_test_file_module(
        &syn::parse_str("#[cfg(test)] mod m { }").unwrap()
    ));
    assert!(!is_exact_test_file_module(
        &syn::parse_str("#[cfg(test)] #[path = \"p.rs\"] mod m;").unwrap()
    ));
}

#[test]
fn a_path_declaration_anywhere_keeps_its_target_scanned() {
    let source =
        "#[cfg(test)] mod x; #[path = \"./x.rs\"] mod y; mod inner { #[path = \"z.rs\"] mod q; }";
    let targets = path_targets("a/mod.rs", source);
    assert!(targets.contains(&"a/x.rs".to_string()));
    assert!(targets.contains(&"a/z.rs".to_string()));
    let sources = [
        ("a/mod.rs".to_string(), source.to_string()),
        (
            "a/x.rs".to_string(),
            "fn f() { TaskWorkspace; }".to_string(),
        ),
    ];
    assert!(!skipped_files(&sources).contains("a/x.rs"));
    let elsewhere = [
        ("a/mod.rs".to_string(), "#[cfg(test)] mod x;".to_string()),
        (
            "b.rs".to_string(),
            "#[path = \"a/x.rs\"] mod y;".to_string(),
        ),
    ];
    assert!(!skipped_files(&elsewhere).contains("a/x.rs"));
    assert_eq!(
        skipped_files(&[("a/mod.rs".to_string(), "#[cfg(test)] mod x;".to_string())]).len(),
        2
    );
}

#[test]
fn a_path_target_is_resolved_beside_the_file_and_inside_its_module_directory() {
    let targets = path_targets("a.rs", "#[path = \"x.rs\"] mod y;");
    assert_eq!(targets, vec!["x.rs".to_string(), "a/x.rs".to_string()]);
    let sources = [(
        "a.rs".to_string(),
        "#[cfg(test)] mod x; #[path = \"x.rs\"] mod y;".to_string(),
    )];
    assert!(!skipped_files(&sources).contains("a/x.rs"));
}
