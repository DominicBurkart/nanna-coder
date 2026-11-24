use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

struct Calls {
    method: &'static str,
    stack: Vec<String>,
    found: Vec<String>,
}

impl<'ast> Visit<'ast> for Calls {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_impl(self, node);
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

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if node.method == self.method {
            let function = self.stack.last().cloned().unwrap_or_default();
            self.found.push(function);
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let flat = node.tokens.to_string();
        let needle = format!(". {} (", self.method);
        if flat.contains(&needle) {
            let function = self.stack.last().cloned().unwrap_or_default();
            self.found.push(function);
        }
        syn::visit::visit_macro(self, node);
    }
}

fn callers(text: &str, method: &'static str) -> Vec<String> {
    let file = syn::parse_file(text).expect("source parses");
    let mut calls = Calls {
        method,
        stack: Vec::new(),
        found: Vec::new(),
    };
    calls.visit_file(&file);
    calls.found
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

fn production(method: &'static str) -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut found = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for function in callers(&fs::read_to_string(&path).unwrap(), method) {
            found.push(format!("{rel}: {function}"));
        }
    }
    found.sort();
    found
}

struct Order(Vec<String>);

impl<'ast> Visit<'ast> for Order {
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        syn::visit::visit_expr_method_call(self, node);
        self.0.push(node.method.to_string());
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        if let Some(last) = node.path.segments.last() {
            self.0.push(last.ident.to_string());
        }
    }
}

fn restore_order() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/hub/runtime.rs");
    let file = syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap();
    let mut order = Order(Vec::new());
    let mut seen = 0;
    for item in &file.items {
        if let syn::Item::Impl(block) = item {
            for member in &block.items {
                if let syn::ImplItem::Fn(function) = member {
                    if function.sig.ident == "restore" {
                        seen += 1;
                        order.visit_block(&function.block);
                    }
                }
            }
        }
    }
    assert_eq!(seen, 1);
    order.0
}

#[test]
fn the_orphan_sweep_is_called_only_by_the_runtime_helper_and_that_only_from_restore() {
    assert_eq!(
        production("sweep_orphans"),
        ["hub/runtime.rs: close_orphans"],
        "a second caller could close live sessions: pin any new caller deliberately"
    );
    assert_eq!(
        production("close_orphans"),
        ["hub/runtime.rs: restore"],
        "the sweep runs once, inside HubRuntime::restore"
    );
}

#[test]
fn restore_registers_the_sessions_before_it_sweeps() {
    let order = restore_order();
    let registered = order
        .iter()
        .position(|name| name == "restore_within_sessions");
    let swept = order.iter().position(|name| name == "close_orphans");
    assert!(registered.is_some() && swept.is_some(), "{order:?}");
    assert!(
        registered < swept,
        "the sweep must follow the registration: {order:?}"
    );
}

#[test]
fn the_call_scanner_sees_direct_calls_macro_calls_and_skips_test_code() {
    let direct = "impl X { fn run(&self) { self.registry.sweep_orphans(a, b); } }";
    assert_eq!(callers(direct, "sweep_orphans"), ["run"]);
    let free = "fn go(r: &R) { r.sweep_orphans(a, b).ok(); }";
    assert_eq!(callers(free, "sweep_orphans"), ["go"]);
    let in_macro = "fn go(r: &R) { log!(r.sweep_orphans(a, b)); }";
    assert_eq!(callers(in_macro, "sweep_orphans"), ["go"]);
    let twice = "fn go(r: &R) { r.sweep_orphans(a, b); r.sweep_orphans(a, b); }";
    assert_eq!(callers(twice, "sweep_orphans"), ["go", "go"]);
    assert!(callers("fn go(r: &R) { r.other(a); }", "sweep_orphans").is_empty());
    assert!(callers(
        "#[cfg(test)] fn go(r: &R) { r.sweep_orphans(a); }",
        "sweep_orphans"
    )
    .is_empty());
    assert!(callers(
        "#[cfg(test)] mod t { fn go(r: &R) { r.sweep_orphans(a); } }",
        "sweep_orphans"
    )
    .is_empty());
    assert!(callers(
        "#[cfg(test)] impl X { fn go(&self) { self.sweep_orphans(a); } }",
        "sweep_orphans"
    )
    .is_empty());
    assert!(callers(
        "impl X { #[cfg(test)] fn go(&self) { self.sweep_orphans(a); } }",
        "sweep_orphans"
    )
    .is_empty());
}
