use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const MAILBOX_TOKEN: &str = "from_mailbox";
const KEEP_OUT: &[&str] = &["MailboxId", "from_mailbox", "is_mailbox"];
const BINDING_SITES: &[&str] = &["task.rs", "hub/router.rs", "hub/registry.rs"];

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    })
}

#[derive(Default)]
struct Facts {
    stack: Vec<String>,
    constructors: BTreeSet<String>,
    inserts: BTreeSet<String>,
    mentions: BTreeMap<String, BTreeSet<String>>,
    idents: BTreeMap<String, usize>,
}

impl Facts {
    fn current(&self) -> String {
        self.stack.last().cloned().unwrap_or_default()
    }

    fn enter(&mut self, name: String) {
        self.stack.push(name);
    }

    fn leave(&mut self) {
        self.stack.pop();
    }
}

impl<'ast> Visit<'ast> for Facts {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_cfg_test(&node.attrs) {
            self.enter(node.sig.ident.to_string());
            syn::visit::visit_item_fn(self, node);
            self.leave();
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_cfg_test(&node.attrs) {
            self.enter(node.sig.ident.to_string());
            syn::visit::visit_impl_item_fn(self, node);
            self.leave();
        }
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func {
            let single = path.path.segments.len() == 1;
            let named = path.path.is_ident("Self") || path.path.is_ident("SessionId");
            if single && named {
                self.constructors.insert(self.current());
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if node.method == "insert" {
            self.inserts.insert(self.current());
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        let name = node.to_string();
        let function = self.current();
        self.mentions
            .entry(function)
            .or_default()
            .insert(name.clone());
        *self.idents.entry(name).or_default() += 1;
    }
}

fn facts(text: &str) -> Facts {
    let file = syn::parse_file(text).expect("source parses");
    let mut found = Facts::default();
    found.visit_file(&file);
    found
}

fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| item.to_string()).collect()
}

fn mentions(found: &Facts, function: &str, ident: &str) -> bool {
    found
        .mentions
        .get(function)
        .is_some_and(|set| set.contains(ident))
}

fn constructors_ok(text: &str) -> bool {
    let found = facts(text);
    found.constructors == names(&["from_task", "from_mailbox", "from_str"])
}

fn from_task_escapes(text: &str) -> bool {
    let found = facts(text);
    mentions(&found, "from_task", "escape_task")
}

fn audited_binding_uses_tasks_only(text: &str) -> bool {
    let found = facts(text);
    let task = mentions(&found, "from_audited", "from_task");
    let mailbox = mentions(&found, "from_audited", MAILBOX_TOKEN);
    task && !mailbox
}

fn only_enter_inserts(text: &str) -> bool {
    facts(text).inserts == names(&["enter"])
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

fn source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(relative);
    fs::read_to_string(path).unwrap()
}

fn production_idents() -> BTreeMap<String, BTreeMap<String, usize>> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut all = BTreeMap::new();
    for path in files {
        let found = facts(&fs::read_to_string(&path).unwrap());
        let relative = path.strip_prefix(&src).unwrap();
        let key = relative.to_string_lossy().replace('\\', "/");
        all.insert(key, found.idents);
    }
    all
}

const GOOD_LINEAGE: &str = "
impl SessionId {
    pub fn from_task(task: TaskId) -> Self { Self(TaskId(escape_task(task.0))) }
    pub(super) fn from_mailbox(text: String) -> Self { Self(TaskId(text)) }
}
impl FromStr for SessionId {
    fn from_str(raw: &str) -> Result<Self, E> { Ok(Self(TaskId(raw.to_string()))) }
}
impl SessionBinding {
    pub(crate) fn from_audited(i: &I, task: TaskId, l: Lineage) -> Self {
        Self { session: SessionId::from_task(task), lineage: l }
    }
}
#[cfg(test)]
mod tests { fn forge() -> SessionId { SessionId(TaskId(String::new())) } }
";

#[test]
fn session_ids_are_built_in_exactly_three_places() {
    assert!(constructors_ok(&source("hub/lineage.rs")));
}

#[test]
fn from_task_always_escapes() {
    assert!(from_task_escapes(&source("hub/lineage.rs")));
}

#[test]
fn bindings_come_from_task_ids_and_never_from_a_mailbox() {
    assert!(audited_binding_uses_tasks_only(&source("hub/lineage.rs")));
}

#[test]
fn the_binding_table_has_one_writer_and_it_is_enter() {
    assert!(only_enter_inserts(&source("hub/router.rs")));
}

#[test]
fn the_mailbox_constructor_exists_in_two_files_and_binding_sites_do_not_name_mailboxes() {
    let all = production_idents();
    let holders: BTreeSet<&str> = all
        .iter()
        .filter(|(_, idents)| idents.contains_key(MAILBOX_TOKEN))
        .map(|(file, _)| file.as_str())
        .collect();
    let expected = BTreeSet::from(["hub/lineage.rs", "hub/mailbox.rs"]);
    assert_eq!(holders, expected);
    for site in BINDING_SITES {
        let idents = all.get(*site).expect("binding site exists");
        for token in KEEP_OUT {
            assert!(!idents.contains_key(*token), "{site} names {token}");
        }
    }
}

#[test]
fn the_scanner_accepts_the_good_shape() {
    assert!(constructors_ok(GOOD_LINEAGE));
    assert!(from_task_escapes(GOOD_LINEAGE));
    assert!(audited_binding_uses_tasks_only(GOOD_LINEAGE));
}

#[test]
fn the_scanner_flags_an_unescaped_from_task() {
    let bad = GOOD_LINEAGE.replace("escape_task(task.0)", "task.0");
    assert!(!from_task_escapes(&bad));
    assert!(constructors_ok(&bad));
}

#[test]
fn the_scanner_flags_a_new_constructor_site() {
    let planted = "impl SessionId { fn sneaky(t: String) -> Self { Self(TaskId(t)) } }";
    let bad = format!("{GOOD_LINEAGE}{planted}");
    assert!(!constructors_ok(&bad));
    let forge = "fn forge(t: String) -> SessionId { SessionId(TaskId(t)) }";
    let free = format!("{GOOD_LINEAGE} {forge}");
    assert!(!constructors_ok(&free));
    let missing = GOOD_LINEAGE.replace("from_mailbox", "other");
    assert!(!constructors_ok(&missing));
}

#[test]
fn the_scanner_ignores_test_only_constructors_and_longer_paths() {
    assert!(constructors_ok(GOOD_LINEAGE));
    let pathed_fn = "fn f() { let _ = Self::new(TaskId(String::new())); }";
    let pathed = format!("{GOOD_LINEAGE} {pathed_fn}");
    assert!(constructors_ok(&pathed));
}

#[test]
fn the_scanner_flags_a_binding_built_from_a_mailbox() {
    let from = "SessionId::from_task(task)";
    let swapped = GOOD_LINEAGE.replace(from, "SessionId::from_mailbox(task.0)");
    assert!(!audited_binding_uses_tasks_only(&swapped));
    let both = GOOD_LINEAGE.replace(from, "SessionId::from_task(SessionId::from_mailbox(t))");
    assert!(!audited_binding_uses_tasks_only(&both));
}

#[test]
fn the_scanner_flags_a_second_binding_table_writer() {
    let table = "impl T { fn enter(&self) { self.map.insert(1, 2); } }";
    assert!(only_enter_inserts(table));
    let also = "impl T { fn also(&self) { self.map.insert(3, 4); } }";
    let planted = format!("{table} {also}");
    assert!(!only_enter_inserts(&planted));
    let moved = "impl T { fn other(&self) { self.map.insert(1, 2); } }";
    assert!(!only_enter_inserts(moved));
}

#[test]
fn the_scanner_counts_identifiers_outside_test_code_only() {
    let text = "fn f() { from_mailbox(); } #[cfg(test)] mod t { fn g() { from_mailbox(); } }";
    let found = facts(text);
    assert_eq!(found.idents.get(MAILBOX_TOKEN), Some(&1));
}
