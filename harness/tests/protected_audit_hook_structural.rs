use std::collections::BTreeSet;
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
    found: BTreeSet<String>,
}

impl Idents {
    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                proc_macro2::TokenTree::Ident(ident) => {
                    self.found.insert(format!("{}: {ident}", self.file));
                }
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
        self.found.insert(format!("{}: {ident}", self.file));
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
    }
}

fn idents_of(file: &str, source: &str) -> BTreeSet<String> {
    let syntax = syn::parse_file(source).unwrap();
    let mut scan = Idents {
        file: file.to_string(),
        ..Idents::default()
    };
    scan.visit_file(&syntax);
    scan.found
}

fn production_idents() -> BTreeSet<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files
        .into_iter()
        .flat_map(|path| {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            idents_of(&relative, &fs::read_to_string(&path).unwrap())
        })
        .collect()
}

fn files_naming(name: &str) -> BTreeSet<String> {
    let suffix = format!(": {name}");
    production_idents()
        .into_iter()
        .filter_map(|entry| entry.strip_suffix(&suffix).map(str::to_string))
        .collect()
}

fn expected(files: &[&str]) -> BTreeSet<String> {
    files.iter().map(|file| file.to_string()).collect()
}

#[test]
fn the_noop_audit_hook_is_named_only_by_its_definition_and_the_bare_workspace_default() {
    assert_eq!(
        files_naming("NoopAuditHook"),
        expected(&["lib.rs", "protected.rs", "workspace.rs"]),
        "a production path now names NoopAuditHook; the task manager must build workspaces with the escalating hook"
    );
}

#[test]
fn no_task_dispatching_file_names_the_noop_audit_hook() {
    let naming = files_naming("NoopAuditHook");
    for file in [
        "task.rs",
        "main.rs",
        "backlog.rs",
        "mcp/mod.rs",
        "mcp/handlers.rs",
        "mcp/client.rs",
    ] {
        assert!(!naming.contains(file), "{file} names NoopAuditHook");
    }
}

#[test]
fn the_task_runner_installs_the_escalating_hook_by_default() {
    assert_eq!(
        files_naming("ProtectedPathEscalation"),
        expected(&[
            "escalation/mod.rs",
            "escalation/protected_hook.rs",
            "task.rs"
        ])
    );
}

#[test]
fn only_the_task_manager_hands_a_hook_to_a_workspace_and_no_production_path_overrides_it() {
    assert_eq!(
        files_naming("with_audit_hook"),
        expected(&["task.rs", "workspace.rs"])
    );
}

#[test]
fn the_scanner_sees_idents_in_production_code_and_macros_but_not_in_cfg_test_items() {
    let seen = idents_of(
        "x.rs",
        "fn f() { let _ = NoopAuditHook; } fn g() { m!(NoopAuditHook2); }",
    );
    assert!(seen.contains("x.rs: NoopAuditHook"));
    assert!(seen.contains("x.rs: NoopAuditHook2"));
    let hidden = idents_of(
        "x.rs",
        "#[cfg(test)] mod t { fn f() { let _ = NoopAuditHook; } } #[cfg(test)] fn h() { NoopAuditHook; } struct S; #[cfg(test)] impl S { fn k(&self) { NoopAuditHook; } }",
    );
    assert!(!hidden.contains("x.rs: NoopAuditHook"));
}
