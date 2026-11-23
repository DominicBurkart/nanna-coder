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

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        let path = names(&node.path);
        if path.contains("SenderNotice") || path.contains("NoticeTable") {
            self.note(path);
        }
        syn::visit::visit_expr_path(self, node);
    }

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        let path = names(&node.path);
        if path.contains("SenderNotice") || path.contains("Notices") {
            self.note(format!("{path} {{..}}"));
        }
        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        if let syn::Expr::Field(field) = &*node.receiver {
            if matches!(&field.member, syn::Member::Named(id) if id == "notices") {
                self.note(format!(".notices.{method}"));
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }
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

fn sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    rust_files(&src(), &mut files);
    let mut all: Vec<(String, String)> = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(src()).unwrap();
        let name = rel.to_string_lossy().replace('\\', "/");
        all.push((name, fs::read_to_string(path).unwrap()));
    }
    assert!(all.len() > 50, "the source scan found {} files", all.len());
    all
}

fn files_containing(needle: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (name, text) in sources() {
        if text.contains(needle) {
            names.push(name);
        }
    }
    names
}

fn production_sites() -> Vec<String> {
    let mut found = Vec::new();
    for (name, text) in sources() {
        let file = syn::parse_file(&text).expect("source parses");
        let mut sites = Sites {
            stack: Vec::new(),
            found: Vec::new(),
        };
        sites.visit_file(&file);
        for site in sites.found {
            found.push(format!("{name}: {site}"));
        }
    }
    found.sort();
    found
}

#[test]
fn a_sender_notice_is_built_in_exactly_one_place_the_inbound_block_path() {
    let sites = production_sites();
    let builds: Vec<&String> = sites
        .iter()
        .filter(|s| s.contains("SenderNotice::new"))
        .collect();
    assert_eq!(
        builds,
        ["hub/recv.rs: retire: SenderNotice::new"],
        "{sites:#?}"
    );
}

#[test]
fn only_recv_stores_notices_and_only_recv_and_send_hand_them_over() {
    let sites = production_sites();
    let table: Vec<&String> = sites.iter().filter(|s| s.contains(".notices.")).collect();
    assert_eq!(
        table,
        [
            "hub/recv.rs: recv: .notices.take",
            "hub/recv.rs: retire: .notices.push",
            "hub/send.rs: send: .notices.take",
        ],
        "{sites:#?}"
    );
}

#[test]
fn no_production_code_builds_a_notice_or_a_notice_list_with_a_struct_literal() {
    let sites = production_sites();
    let literals: Vec<&String> = sites.iter().filter(|s| s.ends_with("{..}")).collect();
    assert_eq!(literals, ["hub/notice.rs: take: Notices {..}"]);
}

fn struct_fields(text: &str, name: &str) -> (Vec<(String, String)>, Vec<String>) {
    let file = syn::parse_file(text).unwrap();
    for item in file.items {
        if let syn::Item::Struct(found) = item {
            if found.ident == name {
                let mut fields = Vec::new();
                for field in found.fields.iter() {
                    let ident = field.ident.as_ref().unwrap().to_string();
                    let kind = match &field.ty {
                        syn::Type::Path(ty) => names(&ty.path),
                        _ => "?".to_string(),
                    };
                    fields.push((ident, kind));
                }
                let mut derives: Vec<String> = Vec::new();
                for attr in found.attrs.iter().filter(|a| a.path().is_ident("derive")) {
                    let collect = |meta: syn::meta::ParseNestedMeta| {
                        derives.push(names(&meta.path));
                        Ok(())
                    };
                    attr.parse_nested_meta(collect).unwrap();
                }
                return (fields, derives);
            }
        }
    }
    panic!("struct {name} not found");
}

#[test]
fn a_sender_notice_has_no_free_text_field_and_cannot_be_deserialized() {
    let text = fs::read_to_string(src().join("hub/notice.rs")).unwrap();
    let (fields, derives) = struct_fields(&text, "SenderNotice");
    let want = [
        ("message_id", "MessageId"),
        ("codes", "Vec"),
        ("at", "DateTime"),
        ("rendered", "String"),
    ];
    let mut want_owned: Vec<(String, String)> = Vec::new();
    for (a, b) in want {
        want_owned.push((a.to_string(), b.to_string()));
    }
    let want = want_owned;
    assert_eq!(fields, want);
    assert!(derives.contains(&"Serialize".to_string()), "{derives:?}");
    assert!(!derives.contains(&"Deserialize".to_string()), "{derives:?}");
    assert!(text.contains("codes: Vec<ReasonCode>"));
    assert!(
        text.contains("\n    rendered: String,"),
        "rendered must stay private"
    );
}

#[test]
fn the_tool_layer_never_names_a_notice() {
    let tools = fs::read_to_string(src().join("hub/tools.rs")).unwrap();
    let production = tools.split("#[cfg(test)]").next().unwrap();
    assert!(!production.contains("SenderNotice"));
    assert!(!production.contains("notices"));
}

const BELL: &str = "new message(s) waiting";

#[test]
fn the_doorbell_text_has_one_integer_hole_one_parameter_and_one_source() {
    let holders = files_containing(BELL);
    assert_eq!(holders, ["hub/doorbell.rs"]);
    let text = fs::read_to_string(src().join("hub/doorbell.rs")).unwrap();
    let file = syn::parse_file(&text).unwrap();
    let mut functions = Vec::new();
    let mut templates = Vec::new();
    for item in &file.items {
        match item {
            syn::Item::Fn(function) => functions.push(function),
            syn::Item::Const(constant) => templates.push(constant),
            _ => {}
        }
    }
    assert_eq!(functions.len(), 1);
    assert_eq!(templates.len(), 1);
    let function = functions[0];
    assert_eq!(function.sig.ident, "doorbell_text");
    assert_eq!(function.sig.inputs.len(), 1);
    let syn::FnArg::Typed(argument) = &function.sig.inputs[0] else {
        panic!("doorbell_text must not take self");
    };
    assert!(matches!(&*argument.ty, syn::Type::Path(ty) if names(&ty.path) == "usize"));
    let syn::Expr::Lit(literal) = &*templates[0].expr else {
        panic!("the template must be a literal");
    };
    let syn::Lit::Str(template) = &literal.lit else {
        panic!("the template must be a string");
    };
    let template = template.value();
    assert_eq!(template.matches("{}").count(), 1, "{template}");
    assert_eq!(template.matches('{').count(), 1, "{template}");
    assert_eq!(template.matches('}').count(), 1, "{template}");
    let production = text.split("#[cfg(test)]").next().unwrap();
    assert!(
        !production.contains("format!"),
        "the doorbell must not format"
    );
    let defined = files_containing("fn doorbell_text(");
    assert_eq!(defined, ["hub/doorbell.rs"]);
}
