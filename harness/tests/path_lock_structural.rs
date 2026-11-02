use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::visit::Visit;

#[derive(Default)]
struct Facts {
    calls: BTreeSet<String>,
    methods: BTreeSet<String>,
    mutates_path: bool,
}

impl<'ast> Visit<'ast> for Facts {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*call.func {
            let segments: Vec<String> = path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let last = segments.last().cloned().unwrap_or_default();
            if matches!(last.as_str(), "set_var" | "remove_var") && first_arg_is_path(call) {
                self.mutates_path = true;
            }
            self.calls.insert(segments.join("::"));
            if segments.len() >= 2 {
                self.calls.insert(segments[segments.len() - 2..].join("::"));
            }
            self.calls.insert(last);
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.methods.insert(call.method.to_string());
        syn::visit::visit_expr_method_call(self, call);
    }
}

fn first_arg_is_path(call: &syn::ExprCall) -> bool {
    matches!(
        call.args.first(),
        Some(syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        })) if s.value() == "PATH"
    )
}

struct Function {
    is_test: bool,
    keys: BTreeSet<String>,
    facts: Facts,
}

fn path_keys(attrs: &[syn::Attribute]) -> BTreeSet<String> {
    attrs
        .iter()
        .filter_map(|attr| {
            let name = attr.path().segments.last()?.ident.to_string();
            if !matches!(name.as_str(), "serial" | "parallel") {
                return None;
            }
            let list = attr.meta.require_list().ok()?;
            (list.tokens.to_string() == "env_path").then_some(name)
        })
        .collect()
}

fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
}

fn functions(source: &str) -> BTreeMap<String, Function> {
    let file = syn::parse_file(source).expect("test source parses");
    let mut out = BTreeMap::new();
    for item in file.items {
        if let syn::Item::Fn(item_fn) = item {
            let mut facts = Facts::default();
            facts.visit_block(&item_fn.block);
            out.insert(
                item_fn.sig.ident.to_string(),
                Function {
                    is_test: is_test(&item_fn.attrs),
                    keys: path_keys(&item_fn.attrs),
                    facts,
                },
            );
        }
    }
    out
}

const PATH_READERS: [&str; 2] = ["Command::new", "detect_runtime"];
const PATH_READING_METHODS: [&str; 1] = ["is_available"];

fn closure(
    functions: &BTreeMap<String, Function>,
    seed: impl Fn(&Function) -> bool,
) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = functions
        .iter()
        .filter(|(_, f)| seed(f))
        .map(|(n, _)| n.clone())
        .collect();
    loop {
        let before = set.len();
        for (name, f) in functions {
            if f.facts.calls.iter().any(|c| set.contains(c)) {
                set.insert(name.clone());
            }
        }
        if set.len() == before {
            return set;
        }
    }
}

pub fn violations(source: &str) -> Vec<String> {
    let functions = functions(source);
    let mutators = closure(&functions, |f| f.facts.mutates_path);
    if mutators.is_empty() {
        return vec![];
    }
    let readers = closure(&functions, |f| {
        f.facts.mutates_path
            || PATH_READERS.iter().any(|r| f.facts.calls.contains(*r))
            || PATH_READING_METHODS
                .iter()
                .any(|m| f.facts.methods.contains(*m))
    });
    let mut found = vec![];
    for (name, f) in &functions {
        if !f.is_test {
            continue;
        }
        if mutators.contains(name) && !f.keys.contains("serial") {
            found.push(format!("{name}: mutates PATH without #[serial(env_path)]"));
        } else if readers.contains(name) && f.keys.is_empty() {
            found.push(format!(
                "{name}: reads PATH in a binary that mutates it without #[serial(env_path)] or #[parallel(env_path)]"
            ));
        }
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_integration_test_mutates_path_outside_the_path_lock() {
    let mut files = vec![];
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests"),
        &mut files,
    );
    files.sort();
    let mut all = vec![];
    for file in files {
        if file
            .file_name()
            .is_some_and(|n| n == "path_lock_structural.rs")
        {
            continue;
        }
        let source = std::fs::read_to_string(&file).unwrap();
        for v in violations(&source) {
            all.push(format!("{}: {v}", file.display()));
        }
    }
    assert!(all.is_empty(), "{}", all.join("\n"));
}

#[test]
fn the_lib_mutates_path_only_behind_the_shared_lock() {
    let mut files = vec![];
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut mutators: Vec<String> = files
        .iter()
        .filter(|f| {
            let source = std::fs::read_to_string(f).unwrap();
            let file = syn::parse_file(&source).unwrap();
            let mut facts = Facts::default();
            facts.visit_file(&file);
            facts.mutates_path
        })
        .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    mutators.sort();
    assert_eq!(mutators, ["test_support.rs"]);
}

#[test]
fn the_detector_flags_each_unlocked_shape() {
    let unlocked = r#"
        #[test]
        fn a() { std::env::set_var("PATH", "x"); }
        #[test]
        #[serial(env_path)]
        fn b() { helper(); }
        fn helper() { std::env::set_var("PATH", "x"); }
        #[test]
        fn c() { helper(); }
        #[test]
        fn d() { std::process::Command::new("git"); }
        #[test]
        #[parallel(env_path)]
        fn e() { std::process::Command::new("git"); }
        #[test]
        fn f() { detect_runtime(); }
        #[test]
        fn m() { rt.is_available(); }
        #[test]
        fn pure() { assert_eq!(1, 1); }
    "#;
    let found = violations(unlocked);
    let names: Vec<&str> = found.iter().map(|v| v.split(':').next().unwrap()).collect();
    assert_eq!(names, ["a", "c", "d", "f", "m"]);
    assert!(found[0].contains("mutates PATH without #[serial(env_path)]"));
    assert!(found[2].contains("reads PATH"));
}

#[test]
fn a_binary_that_never_mutates_path_is_not_constrained() {
    assert!(violations("#[test] fn g() { std::process::Command::new(\"git\"); }").is_empty());
}

#[test]
fn a_parallel_key_does_not_satisfy_a_mutator() {
    let found =
        violations("#[test] #[parallel(env_path)] fn h() { std::env::set_var(\"PATH\", \"x\"); }");
    assert_eq!(found.len(), 1);
}

#[test]
fn a_serial_key_on_another_name_does_not_count() {
    let found =
        violations("#[test] #[serial(other)] fn i() { std::env::set_var(\"PATH\", \"x\"); }");
    assert_eq!(found.len(), 1);
}
