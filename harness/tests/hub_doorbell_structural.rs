use std::fs;
use std::path::{Path, PathBuf};

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

fn production() -> Vec<(String, String)> {
    let mut files = Vec::new();
    rust_files(&src(), &mut files);
    let mut all = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(src()).unwrap();
        let name = rel.to_string_lossy().replace('\\', "/");
        let text = fs::read_to_string(path).unwrap();
        let plain = text.split("#[cfg(test)]\nmod tests").next().unwrap();
        let live = plain
            .split("#[cfg(test)]\npub(crate) mod tests")
            .next()
            .unwrap();
        let code: Vec<&str> = live
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        all.push((name, code.join("\n")));
    }
    assert!(all.len() > 50, "the source scan found {} files", all.len());
    all
}

fn sites(needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (name, text) in production() {
        for _ in text.matches(needle) {
            found.push(name.clone());
        }
    }
    found
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Reference(found) => format!("&{}", type_name(&found.elem)),
        syn::Type::Path(found) => {
            let parts: Vec<String> = found
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            parts.join("::")
        }
        _ => "other".to_string(),
    }
}

fn signature(sig: &syn::Signature) -> Vec<String> {
    let mut parts = vec![sig.ident.to_string()];
    for input in &sig.inputs {
        match input {
            syn::FnArg::Receiver(found) => {
                parts.push(format!("self:{}", found.reference.is_some()));
            }
            syn::FnArg::Typed(found) => parts.push(type_name(&found.ty)),
        }
    }
    parts
}

#[test]
fn the_doorbell_trait_has_one_method_that_takes_a_session_and_a_count() {
    let text = fs::read_to_string(src().join("hub/doorbell.rs")).unwrap();
    let file = syn::parse_file(&text).unwrap();
    let mut methods = Vec::new();
    let mut traits = 0;
    for item in &file.items {
        if let syn::Item::Trait(found) = item {
            traits += 1;
            assert_eq!(found.ident, "Doorbell");
            for member in &found.items {
                if let syn::TraitItem::Fn(method) = member {
                    methods.push(signature(&method.sig));
                }
            }
        }
    }
    assert_eq!(traits, 1);
    let expected = ["ring", "self:true", "&SessionId", "usize"];
    assert_eq!(methods, [expected]);
}

#[test]
fn the_hub_module_has_one_doorbell_implementor_and_it_rings_nowhere() {
    let mut implementors = sites("impl Doorbell for");
    implementors.sort();
    assert_eq!(
        implementors,
        ["harness_adapter/steer.rs", "hub/doorbell.rs"]
    );
    let text = fs::read_to_string(src().join("hub/doorbell.rs")).unwrap();
    let file = syn::parse_file(&text).unwrap();
    let mut structs = Vec::new();
    for item in &file.items {
        if let syn::Item::Struct(found) = item {
            structs.push(format!("{}:{}", found.ident, found.fields.len()));
        }
        if let syn::Item::Impl(found) = item {
            assert_eq!(found.items.len(), 1);
            if let syn::ImplItem::Fn(method) = &found.items[0] {
                assert!(method.block.stmts.is_empty());
                assert_eq!(signature(&method.sig)[0], "ring");
            }
        }
    }
    assert_eq!(structs, ["NoopDoorbell:0"]);
}

#[test]
fn the_ring_has_one_call_site_in_send_and_it_passes_the_recipient_and_the_unread_count() {
    let calls = sites(".ring(");
    assert_eq!(calls, ["hub/send.rs"]);
    let all = production();
    let send = all.iter().find(|(name, _)| name == "hub/send.rs").unwrap();
    assert_eq!(send.1.matches("self.doorbell.ring(id, count)").count(), 1);
    assert_eq!(send.1.matches("self.ring_all(&prepared.to)").count(), 1);
}

#[test]
fn the_ring_comes_after_the_store_write_in_the_deliver_arm() {
    let all = production();
    let send = all.iter().find(|(name, _)| name == "hub/send.rs").unwrap();
    let body = send.1.split("pub async fn send(").nth(1).unwrap();
    let stored = body.find("self.append_all(").unwrap();
    let rung = body.find("self.ring_all(").unwrap();
    assert!(stored < rung);
    let quarantine = body.find("MessageVerdict::Quarantine").unwrap();
    let deliver = body.find("MessageVerdict::Deliver").unwrap();
    assert!(quarantine < deliver && deliver < rung);
}

#[test]
fn only_the_hub_constructor_names_the_noop_doorbell() {
    let mut hub: Vec<String> = sites("NoopDoorbell")
        .into_iter()
        .filter(|name| name != "hub/doorbell.rs")
        .collect();
    hub.sort();
    hub.dedup();
    assert_eq!(hub, ["hub/runtime.rs", "hub/send.rs"]);
}

#[test]
fn open_hands_the_hub_open_doorbell_to_the_runtime_and_nothing_else_does() {
    let all = production();
    let runtime = all
        .iter()
        .find(|(name, _)| name == "hub/runtime.rs")
        .unwrap();
    let call = "HubRuntime::open_with(paths, factory, Arc::new(SystemClock), sink, doorbell)";
    assert_eq!(runtime.1.matches(call).count(), 1);
    assert_eq!(
        runtime
            .1
            .matches("doorbell: Arc::new(NoopDoorbell)")
            .count(),
        1
    );
    assert_eq!(runtime.1.matches("let bell = doorbell;").count(), 1);
}
