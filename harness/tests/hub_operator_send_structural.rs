use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const OPERATOR_NAMES: &[&str] = &["send_as_operator", "OperatorSend", "OperatorTarget"];
const REVIEW_SIDE_DOORS: &[&str] = &[
    "auditors",
    "for_sender",
    "review_outbound",
    "review_inbound",
    "MessageAuditor",
    "oracle_for",
    "SessionBinding",
];

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    let test = |attr: &syn::Attribute| {
        let named = attr.path().is_ident("cfg");
        named && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")
    };
    attrs.iter().any(test)
}

fn last_name(path: &syn::Path) -> String {
    let last = path.segments.last();
    let name = last.map(|segment| segment.ident.to_string());
    name.unwrap_or_default()
}

fn member_name(member: &syn::Member) -> String {
    match member {
        syn::Member::Named(ident) => ident.to_string(),
        syn::Member::Unnamed(index) => index.index.to_string(),
    }
}

#[derive(Default)]
struct Idents(BTreeSet<String>);

impl<'ast> Visit<'ast> for Idents {
    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        self.0.insert(node.to_string());
    }
}

fn idents_of_expr(expr: &syn::Expr) -> BTreeSet<String> {
    let mut found = Idents::default();
    found.visit_expr(expr);
    found.0
}

#[derive(Default)]
struct Facts {
    stack: Vec<String>,
    idents: BTreeMap<String, usize>,
    by_function: BTreeMap<String, BTreeSet<String>>,
    literals: Vec<(String, String, BTreeSet<String>)>,
    struct_fields: BTreeMap<String, Vec<String>>,
}

impl Facts {
    fn current(&self) -> String {
        self.stack.last().cloned().unwrap_or_default()
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

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        let idents = node.fields.iter().filter_map(|field| field.ident.as_ref());
        let names: Vec<String> = idents.map(ToString::to_string).collect();
        self.struct_fields.insert(node.ident.to_string(), names);
        syn::visit::visit_item_struct(self, node);
    }

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        let name = last_name(&node.path);
        for field in &node.fields {
            let key = member_name(&field.member);
            let used = idents_of_expr(&field.expr);
            self.literals.push((name.clone(), key, used));
        }
        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        let name = node.to_string();
        let slot = self.by_function.entry(self.current()).or_default();
        slot.insert(name.clone());
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

fn literal_fields(found: &Facts, literal: &str, field: &str) -> Vec<BTreeSet<String>> {
    let mut all = Vec::new();
    for (name, key, used) in &found.literals {
        if name == literal && key == field {
            all.push(used.clone());
        }
    }
    all
}

fn flag_comes_from_the_sender_id_alone(text: &str) -> bool {
    let found = facts(text);
    let fields = literal_fields(&found, "RenderedMessage", "from_operator");
    let wanted = names(&["is_mailbox", "message", "from"]);
    fields == vec![wanted]
}

fn operator_request_has_no_sender_fields(text: &str) -> bool {
    let found = facts(text);
    let fields = found.struct_fields.get("OperatorSend");
    let keys = ["to", "kind", "subject", "body"];
    let wanted: Vec<String> = keys.iter().map(|item| item.to_string()).collect();
    fields == Some(&wanted)
}

fn operator_send_has_rules_only_review(text: &str) -> bool {
    let found = facts(text);
    let used = found.by_function.get("send_as_operator").cloned();
    let used = used.unwrap_or_default();
    let doors = REVIEW_SIDE_DOORS.iter().all(|door| !used.contains(*door));
    let rules = [
        "RuleMessageAuditor",
        "review_with",
        "reserve_for",
        "append_all",
    ];
    doors && rules.iter().all(|name| used.contains(*name))
}

fn operator_records_stamp_the_mailbox(text: &str) -> bool {
    let found = facts(text);
    let from = literal_fields(&found, "StoredMessage", "from");
    let identity = literal_fields(&found, "StoredMessage", "from_identity");
    let mailbox = names(&["mailbox", "session", "clone"]);
    let named = names(&["OPERATOR_IDENTITY", "to_string"]);
    from.contains(&mailbox) && identity.contains(&named)
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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    fs::read_to_string(root.join(relative)).unwrap()
}

fn production_facts() -> BTreeMap<String, Facts> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut all = BTreeMap::new();
    for path in files {
        let found = facts(&fs::read_to_string(&path).unwrap());
        let relative = path.strip_prefix(&src).unwrap();
        let key = relative.to_string_lossy().replace('\\', "/");
        all.insert(key, found);
    }
    all
}

fn holders(all: &BTreeMap<String, Facts>, ident: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (file, facts) in all {
        if facts.idents.contains_key(ident) {
            found.insert(file.clone());
        }
    }
    found
}

const GOOD_RECV: &str = "
fn deliver(message: &StoredMessage) -> RenderedMessage {
    RenderedMessage { id: message.id, from_operator: is_mailbox(&message.from) }
}
#[cfg(test)]
mod tests { fn forged() -> RenderedMessage { RenderedMessage { from_operator: true } } }
";

const GOOD_SEND: &str = "
pub struct OperatorSend { pub to: OperatorTarget, pub kind: MessageKind, pub subject: Option<String>, pub body: String }
impl MessageHub {
    pub fn send_as_operator(&self, root: &RootId, req: OperatorSend) -> R {
        let rules = RuleMessageAuditor::new();
        let held = self.reserve_for(a, b, c);
        let outcome = rules.review_with(a, b, c, d);
        let ids = self.append_all(stored);
    }
    fn operator_records(&self) -> Vec<StoredMessage> {
        StoredMessage { from: mailbox.session().clone(), from_identity: OPERATOR_IDENTITY.to_string() }
    }
}
";

#[test]
fn the_operator_flag_is_computed_from_the_sender_id_in_exactly_one_place() {
    assert!(flag_comes_from_the_sender_id_alone(&source("hub/recv.rs")));
    let all = production_facts();
    let held = holders(&all, "from_operator");
    assert_eq!(held, names(&["hub/recv.rs"]));
    let built = all.get("hub/recv.rs").unwrap();
    let literals = built.literals.iter().filter(|l| l.0 == "RenderedMessage");
    assert_eq!(literals.filter(|l| l.1 == "id").count(), 1);
    let elsewhere: Vec<&String> = all.keys().filter(|k| *k != "hub/recv.rs").collect();
    for file in elsewhere {
        let literals = &all.get(file).unwrap().literals;
        assert!(literals.iter().all(|l| l.0 != "RenderedMessage"), "{file}");
    }
}

#[test]
fn the_operator_request_has_no_field_that_could_name_a_sender() {
    assert!(operator_request_has_no_sender_fields(&source(
        "hub/send.rs"
    )));
}

#[test]
fn the_operator_send_reviews_with_the_rules_layer_only_and_never_reaches_an_auditor() {
    assert!(operator_send_has_rules_only_review(&source("hub/send.rs")));
}

#[test]
fn the_stored_sender_is_the_roots_mailbox_and_the_identity_is_the_host_constant() {
    assert!(operator_records_stamp_the_mailbox(&source("hub/send.rs")));
}

#[test]
fn the_operator_send_is_named_only_in_the_send_module_of_the_hub() {
    let all = production_facts();
    for name in OPERATOR_NAMES {
        let holding = holders(&all, name).into_iter();
        let hub_files: BTreeSet<String> = holding.filter(|f| f.starts_with("hub/")).collect();
        assert_eq!(hub_files, names(&["hub/send.rs"]), "{name}");
    }
}

#[test]
fn the_scanners_accept_the_good_shapes() {
    assert!(flag_comes_from_the_sender_id_alone(GOOD_RECV));
    assert!(operator_request_has_no_sender_fields(GOOD_SEND));
    assert!(operator_send_has_rules_only_review(GOOD_SEND));
    assert!(operator_records_stamp_the_mailbox(GOOD_SEND));
}

#[test]
fn the_scanner_flags_a_flag_computed_from_the_identity_name() {
    let from_name =
        "from_operator: is_mailbox(&message.from) || message.from_identity == \"operator\"";
    let bad = GOOD_RECV.replace("from_operator: is_mailbox(&message.from)", from_name);
    assert!(!flag_comes_from_the_sender_id_alone(&bad));
    let named = GOOD_RECV.replace(
        "is_mailbox(&message.from)",
        "message.from_identity == \"operator\"",
    );
    assert!(!flag_comes_from_the_sender_id_alone(&named));
    let constant = GOOD_RECV.replace("is_mailbox(&message.from)", "true");
    assert!(!flag_comes_from_the_sender_id_alone(&constant));
}

#[test]
fn the_scanner_flags_a_sender_field_on_the_operator_request() {
    let from = "pub from: SessionId, pub to: OperatorTarget";
    let bad = GOOD_SEND.replace("pub to: OperatorTarget", from);
    assert!(!operator_request_has_no_sender_fields(&bad));
    let root = GOOD_SEND.replace("pub body: String", "pub body: String, pub root: RootId");
    assert!(!operator_request_has_no_sender_fields(&root));
}

#[test]
fn the_scanner_flags_a_model_auditor_in_the_operator_send() {
    let first = "let rules = RuleMessageAuditor::new();";
    let spied = format!("{first} let a = self.auditors.clone();");
    let bad = GOOD_SEND.replace(first, &spied);
    assert!(!operator_send_has_rules_only_review(&bad));
    let review = GOOD_SEND.replace(first, &format!("{first} x.review_outbound(r);"));
    assert!(!operator_send_has_rules_only_review(&review));
    let dropped = GOOD_SEND.replace("rules.review_with(a, b, c, d)", "ok()");
    assert!(!operator_send_has_rules_only_review(&dropped));
}

#[test]
fn the_scanner_flags_a_caller_chosen_sender_in_the_stored_record() {
    let chosen = GOOD_SEND.replace("mailbox.session().clone()", "req.from.clone()");
    assert!(!operator_records_stamp_the_mailbox(&chosen));
    let named = GOOD_SEND.replace("OPERATOR_IDENTITY.to_string()", "req.name.clone()");
    assert!(!operator_records_stamp_the_mailbox(&named));
}

#[test]
fn the_scanner_ignores_test_only_code() {
    let found = facts(GOOD_RECV);
    let literals = found.literals.iter().filter(|l| l.0 == "RenderedMessage");
    assert_eq!(literals.count(), 2);
    assert_eq!(found.idents.get("forged"), None);
}
