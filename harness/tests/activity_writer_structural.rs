use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

struct Sites {
    stack: Vec<String>,
    found: Vec<String>,
}

impl Sites {
    fn note(&mut self, what: String) {
        let function = self.stack.last().cloned().unwrap_or_default();
        self.found.push(format!("{function}: {what}"));
    }
}

fn names(path: &syn::Path) -> String {
    let parts: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    parts.join("::")
}

const WATCHED_METHODS: [&str; 3] = ["with_activity_sink", "activity_sink", "log_activity"];

impl<'ast> Visit<'ast> for Sites {
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

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(callee) = &*node.func {
            let path = names(&callee.path);
            if path.ends_with("ActivitySink::new") || path.ends_with("ActivityEntry::tool_call") {
                self.note(path);
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        if WATCHED_METHODS.contains(&method.as_str()) {
            self.note(format!(".{method}"));
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

fn scan(text: &str) -> Vec<String> {
    let file = syn::parse_file(text).expect("source parses");
    let mut sites = Sites {
        stack: Vec::new(),
        found: Vec::new(),
    };
    sites.visit_file(&file);
    sites.found
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

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn production_sites() -> Vec<String> {
    let mut files = Vec::new();
    rust_files(&src(), &mut files);
    assert!(
        files.len() > 50,
        "the source scan found {} files",
        files.len()
    );
    let mut found = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(src()).unwrap();
        let name = rel.to_string_lossy().replace('\\', "/");
        let text = fs::read_to_string(path).unwrap();
        let sites = scan(&text);
        for site in sites.into_iter().filter(|_| !name.ends_with("_tests.rs")) {
            found.push(format!("{name}: {site}"));
        }
    }
    found.sort();
    found
}

fn only(sites: &[String], needle: &str) -> Vec<String> {
    sites
        .iter()
        .filter(|s| s.contains(needle))
        .cloned()
        .collect()
}

#[test]
fn the_sink_is_built_in_one_place_from_the_hosts_session_binding() {
    let sites = production_sites();
    assert_eq!(
        only(&sites, "ActivitySink::new"),
        ["hub/router.rs: activity_sink: ActivitySink::new"],
        "{sites:#?}"
    );
}

#[test]
fn the_sink_is_asked_for_only_when_a_hub_binding_is_made_and_attached_only_by_the_task_runner() {
    let sites = production_sites();
    assert_eq!(
        only(&sites, ".activity_sink"),
        ["hub/tools.rs: new: .activity_sink"],
        "{sites:#?}"
    );
    assert_eq!(
        only(&sites, ".with_activity_sink"),
        ["task.rs: attach_activity: .with_activity_sink"],
        "{sites:#?}"
    );
}

#[test]
fn a_tool_call_entry_is_built_in_the_sink_only_and_logged_from_one_place_in_execute() {
    let sites = production_sites();
    assert_eq!(
        only(&sites, "tool_call"),
        ["hub/activity_sink.rs: record: ActivityEntry::tool_call"],
        "{sites:#?}"
    );
    assert_eq!(
        only(&sites, ".log_activity"),
        ["tools.rs: execute: .log_activity"],
        "{sites:#?}"
    );
}

#[test]
fn the_scanner_sees_each_shape_and_skips_test_code() {
    let live = "fn a() { let s = ActivitySink::new(x); r.with_activity_sink(s); r.activity_sink(&b); self.log_activity(t); ActivityEntry::tool_call(a); }";
    assert_eq!(
        scan(live),
        [
            "a: ActivitySink::new",
            "a: .with_activity_sink",
            "a: .activity_sink",
            "a: .log_activity",
            "a: ActivityEntry::tool_call"
        ]
    );
    let skipped = "#[cfg(test)] mod t { fn a() { ActivitySink::new(x); } } fn b() { other(); }";
    assert!(scan(skipped).is_empty());
    let method = "impl X { fn m(&self) { self.log_activity(t) } }";
    assert_eq!(scan(method), ["m: .log_activity"]);
    let free = "#[cfg(test)] fn t() { r.activity_sink(&b); } impl Y { #[cfg(test)] fn u() { r.activity_sink(&b); } }";
    assert!(scan(free).is_empty());
}

#[test]
fn the_hub_paths_struct_still_names_only_the_sessions_and_messages_journals() {
    let text = fs::read_to_string(src().join("hub/runtime.rs")).unwrap();
    let file = syn::parse_file(&text).unwrap();
    let mut fields = Vec::new();
    for item in file.items {
        if let syn::Item::Struct(found) = item {
            if found.ident == "HubPaths" {
                for field in found.fields.iter() {
                    fields.push(field.ident.as_ref().unwrap().to_string());
                }
            }
        }
    }
    assert_eq!(fields, ["sessions", "messages"]);
}
