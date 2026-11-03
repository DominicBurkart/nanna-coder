use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const ENTRY_FILE: &str = "mcp/plan.rs";
const ENTRY_FN: &str = "handle_plan_task";

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
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

fn plain(name: &str) -> String {
    name.strip_prefix("r#").unwrap_or(name).to_string()
}

#[derive(Default)]
struct Sites {
    file: String,
    function: Vec<String>,
    uses: BTreeMap<String, BTreeSet<String>>,
    sequences: BTreeMap<String, Vec<String>>,
}

impl Sites {
    fn record(&mut self, name: &str) {
        let name = plain(name);
        let function = self.function.last().cloned().unwrap_or_default();
        let site = format!("{}: {}", self.file, function);
        self.uses.entry(name.clone()).or_default().insert(site);
        self.sequences.entry(function).or_default().push(name);
    }

    fn tokens(&mut self, stream: TokenStream) {
        for token in stream {
            match token {
                TokenTree::Ident(ident) => self.record(&ident.to_string()),
                TokenTree::Group(group) => self.tokens(group.stream()),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Sites {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        self.function.push(plain(&node.sig.ident.to_string()));
        syn::visit::visit_item_fn(self, node);
        self.function.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        self.function.push(plain(&node.sig.ident.to_string()));
        syn::visit::visit_impl_item_fn(self, node);
        self.function.pop();
    }

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        self.record(&ident.to_string());
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
    }
}

fn scan_source(file: &str, source: &str) -> Sites {
    let syntax = syn::parse_file(source).unwrap();
    let mut sites = Sites {
        file: file.to_string(),
        ..Sites::default()
    };
    sites.visit_file(&syntax);
    sites
}

fn scan_all() -> Vec<Sites> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    let relative = |path: &PathBuf| {
        let inside = path.strip_prefix(&root).unwrap();
        inside.to_string_lossy().replace('\\', "/")
    };
    files
        .iter()
        .map(|path| scan_source(&relative(path), &fs::read_to_string(path).unwrap()))
        .collect()
}

fn use_sites(all: &[Sites], name: &str) -> BTreeSet<String> {
    let sites = all.iter().filter_map(|scan| scan.uses.get(name));
    sites.flat_map(|set| set.iter().cloned()).collect()
}

fn entry_sequence(all: &[Sites]) -> Vec<String> {
    let scan = all.iter().find(|scan| scan.file == ENTRY_FILE).unwrap();
    scan.sequences.get(ENTRY_FN).cloned().unwrap_or_default()
}

fn first(sequence: &[String], name: &str) -> Option<usize> {
    sequence.iter().position(|item| item == name)
}

fn order_problem(sequence: &[String]) -> Option<String> {
    let steps = ["Planner", "audit_plan", "into_approved", "PlanRun"];
    let mut at = Vec::new();
    for step in steps {
        match first(sequence, step) {
            Some(index) => at.push(index),
            None => return Some(format!("{step} is missing")),
        }
    }
    let ordered = at.windows(2).all(|pair| pair[0] < pair[1]);
    (!ordered).then(|| "Planner, audit_plan, into_approved, PlanRun are out of order".to_string())
}

fn expected(lines: &[&str]) -> BTreeSet<String> {
    lines.iter().map(|line| line.to_string()).collect()
}

#[test]
fn audit_plan_is_called_only_by_the_single_plan_entry() {
    let all = scan_all();
    assert_eq!(
        use_sites(&all, "audit_plan"),
        expected(&[
            "planner.rs: audit_plan",
            "mcp/plan.rs: handle_plan_task",
            "mcp/plan.rs: ",
        ]),
        "audit_plan gained or lost a caller; handle_plan_task must stay the only entry into the audit"
    );
}

#[test]
fn the_planner_and_the_plan_run_are_built_only_by_the_single_plan_entry() {
    let all = scan_all();
    let planners = use_sites(&all, "Planner");
    let outside: BTreeSet<String> = planners
        .into_iter()
        .filter(|site| !site.starts_with("planner.rs:"))
        .collect();
    assert_eq!(
        outside,
        expected(&["mcp/plan.rs: handle_plan_task", "mcp/plan.rs: "])
    );
    let run_sites = use_sites(&all, "PlanRun");
    let outside: Vec<&String> = run_sites
        .iter()
        .filter(|site| !site.starts_with("plan_dispatch.rs") && !site.starts_with(ENTRY_FILE))
        .collect();
    assert!(
        outside.is_empty(),
        "PlanRun is used outside the dispatcher and the entry: {outside:?}"
    );
    let entry: BTreeSet<String> = run_sites
        .into_iter()
        .filter(|site| site.starts_with(ENTRY_FILE))
        .collect();
    assert_eq!(
        entry,
        expected(&[
            "mcp/plan.rs: ",
            "mcp/plan.rs: handle_plan_task",
            "mcp/plan.rs: render"
        ])
    );
}

#[test]
fn the_entry_plans_then_audits_then_approves_then_builds_the_run() {
    let all = scan_all();
    assert_eq!(order_problem(&entry_sequence(&all)), None);
}

#[test]
fn the_order_checker_flags_a_missing_or_reordered_step() {
    let names = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let good = names(&["Planner", "audit_plan", "into_approved", "PlanRun"]);
    assert_eq!(order_problem(&good), None);
    let missing = names(&["Planner", "into_approved", "PlanRun"]);
    assert_eq!(order_problem(&missing).unwrap(), "audit_plan is missing");
    let swapped = names(&["PlanRun", "Planner", "audit_plan", "into_approved"]);
    assert!(order_problem(&swapped).unwrap().contains("out of order"));
}

#[test]
fn the_scanner_sees_macro_bodies_and_skips_test_code() {
    let source = r#"
        fn entry() { println!("{}", audit_plan(a)); }
        #[cfg(test)]
        mod tests { fn t() { audit_plan(b); } }
    "#;
    let scan = scan_source("x.rs", source);
    assert_eq!(scan.uses["audit_plan"], expected(&["x.rs: entry"]));
}
