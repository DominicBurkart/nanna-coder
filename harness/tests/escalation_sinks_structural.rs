use std::collections::{BTreeMap, BTreeSet};
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
    found: BTreeMap<String, usize>,
    renames: BTreeSet<String>,
    fanouts: Vec<String>,
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
        let mut previous_fanout = false;
        for token in tokens {
            let is_fanout = matches!(&token, proc_macro2::TokenTree::Ident(i) if plain(&i.to_string()) == "FanoutSink");
            match token {
                proc_macro2::TokenTree::Group(group) => {
                    if previous_fanout && group.delimiter() == proc_macro2::Delimiter::Parenthesis {
                        let entry = format!("{}: macro-call", self.file);
                        self.fanouts.push(entry);
                    }
                    self.tokens(group.stream())
                }
                proc_macro2::TokenTree::Ident(ident) => self.note(&ident.to_string()),
                _ => {}
            }
            previous_fanout = is_fanout;
        }
    }

    fn fanout_shape(arg: Option<&syn::Expr>) -> String {
        match arg {
            Some(syn::Expr::Macro(mac)) if mac.mac.path.is_ident("vec") => {
                if mac.mac.tokens.is_empty() {
                    "vec![] (empty)".to_string()
                } else {
                    "vec![..] (non-empty literal)".to_string()
                }
            }
            Some(syn::Expr::Path(path)) => format!(
                "path:{}",
                path.path
                    .segments
                    .iter()
                    .map(|s| plain(&s.ident.to_string()).to_string())
                    .collect::<Vec<_>>()
                    .join("::")
            ),
            _ => "other".to_string(),
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

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func {
            let last = path.path.segments.last().map(|s| s.ident.to_string());
            if last.as_deref().map(plain) == Some("FanoutSink") {
                let shape = Self::fanout_shape(node.args.first());
                let entry = format!("{}: {shape}", self.file);
                self.fanouts.push(entry);
            }
        }
        syn::visit::visit_expr_call(self, node);
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

fn production() -> Vec<Idents> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name != "test_support.rs")
        })
        .map(|path| {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            scan_source(&relative, &fs::read_to_string(&path).unwrap())
        })
        .collect()
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

fn fanouts() -> Vec<String> {
    let mut all: Vec<String> = production().into_iter().flat_map(|s| s.fanouts).collect();
    all.sort();
    all
}

#[test]
fn only_the_sink_builder_constructs_fan_outs_and_never_an_empty_or_unreviewed_one() {
    assert_eq!(
        fanouts(),
        vec![
            "escalation/plane.rs: path:remote".to_string(),
            "escalation/plane.rs: vec![..] (non-empty literal)".to_string(),
            "escalation/plane.rs: vec![..] (non-empty literal)".to_string(),
        ],
        "a production FanoutSink construction appeared or changed shape: an empty fan-out reports success and marks the escalation delivered while reaching no human"
    );
    assert_eq!(
        mentions("FanoutSink"),
        pinned(&[
            ("escalation/mod.rs", 1),
            ("escalation/plane.rs", 4),
            ("escalation/sink.rs", 2)
        ])
    );
}

#[test]
fn the_cli_and_the_library_root_never_name_a_sink_or_escalator_so_every_path_takes_the_plane() {
    for name in [
        "FanoutSink",
        "Escalator",
        "UndeliveredSink",
        "EscalatorSpawnHook",
        "LocalRecordSink",
        "GithubIssueSink",
        "WebhookSink",
        "RateLimitedSink",
    ] {
        let files = mentions(name);
        assert!(!files.contains_key("main.rs"), "{name}: {files:?}");
        assert!(!files.contains_key("lib.rs"), "{name}: {files:?}");
    }
}

#[test]
fn escalator_construction_sites_are_pinned() {
    assert_eq!(
        mentions("Escalator"),
        pinned(&[
            ("budget.rs", 3),
            ("escalation/escalator.rs", 2),
            ("escalation/mod.rs", 1),
            ("escalation/plane.rs", 3),
            ("escalation/protected_hook.rs", 3),
            ("escalation/spawn_hook.rs", 3),
            ("rollout/escalator_hook.rs", 3),
            ("rollout/executor.rs", 1),
            ("task.rs", 6),
            ("tools.rs", 3)
        ]),
        "a new production file names Escalator: it may build one over a sink that reaches no human"
    );
    assert_eq!(
        mentions("EscalatorSpawnHook"),
        pinned(&[
            ("escalation/mod.rs", 1),
            ("escalation/plane.rs", 3),
            ("escalation/spawn_hook.rs", 3)
        ])
    );
}

#[test]
fn the_undelivered_default_stays_inside_the_plane() {
    assert_eq!(
        mentions("UndeliveredSink"),
        pinned(&[("escalation/mod.rs", 1), ("escalation/plane.rs", 3)])
    );
}

#[test]
fn the_concrete_sinks_are_built_only_by_the_sink_builder() {
    assert_eq!(
        mentions("GithubIssueSink"),
        pinned(&[
            ("escalation/github.rs", 3),
            ("escalation/mod.rs", 1),
            ("escalation/plane.rs", 2)
        ])
    );
    assert_eq!(
        mentions("WebhookSink"),
        pinned(&[
            ("escalation/mod.rs", 1),
            ("escalation/plane.rs", 2),
            ("escalation/webhook.rs", 4)
        ])
    );
    assert_eq!(
        mentions("LocalRecordSink"),
        pinned(&[
            ("escalation/local.rs", 3),
            ("escalation/mod.rs", 1),
            ("escalation/plane.rs", 2)
        ])
    );
    assert_eq!(
        mentions("RateLimitedSink"),
        pinned(&[
            ("escalation/limit.rs", 3),
            ("escalation/mod.rs", 1),
            ("escalation/plane.rs", 2)
        ])
    );
}

#[test]
fn every_production_task_manager_and_gate_in_the_cli_is_built_from_a_plane() {
    assert_eq!(
        mentions("EscalationPlane").get("main.rs").copied(),
        Some(11),
        "main.rs changed how many places take an EscalationPlane"
    );
    assert_eq!(
        mentions("with_escalation_plane").get("main.rs").copied(),
        Some(1)
    );
    assert_eq!(
        mentions("restore_with_escalations").get("main.rs").copied(),
        Some(1)
    );
    assert_eq!(
        mentions("spawn_escalation_hook").get("main.rs").copied(),
        Some(3)
    );
    assert_eq!(mentions("TaskManager").get("main.rs").copied(), Some(7));
    assert_eq!(mentions("EscalationLog").get("main.rs").copied(), Some(5));
    assert_eq!(mentions("EscalationPlane").get("task.rs").copied(), Some(5));
}

#[test]
fn no_production_use_renames_a_sink_type_to_hide_it() {
    let hidden = [
        "FanoutSink",
        "Escalator",
        "UndeliveredSink",
        "EscalationPlane",
        "LocalRecordSink",
    ];
    let renames: BTreeSet<String> = production()
        .into_iter()
        .flat_map(|scan| scan.renames)
        .filter(|rename| {
            hidden
                .iter()
                .any(|name| rename.ends_with(&format!("renames {name}")))
        })
        .collect();
    assert_eq!(renames, BTreeSet::new());
}

#[test]
fn the_scanner_sees_empty_fan_outs_in_every_shape_and_ignores_cfg_test_items() {
    let seen = scan_source(
        "x.rs",
        "fn a() { FanoutSink(vec![]); } fn b() { FanoutSink(Vec::new()); } fn c() { m!(FanoutSink(vec![])); } fn d() { let s = Vec::new(); x::FanoutSink(s); } fn e() { r#FanoutSink(vec![Box::new(1)]); } use p::FanoutSink as Sneak; type Alias = FanoutSink;",
    );
    let mut shapes = seen.fanouts.clone();
    shapes.sort();
    assert_eq!(
        shapes,
        vec![
            "x.rs: macro-call".to_string(),
            "x.rs: other".to_string(),
            "x.rs: path:s".to_string(),
            "x.rs: vec![..] (non-empty literal)".to_string(),
            "x.rs: vec![] (empty)".to_string(),
        ]
    );
    assert_eq!(seen.found["x.rs: FanoutSink"], 7);
    assert!(seen.renames.contains("x.rs: renames FanoutSink"));
    let hidden = scan_source(
        "x.rs",
        "#[cfg(test)] mod t { fn f() { FanoutSink(vec![]); } } #[cfg(test)] fn h() { FanoutSink(vec![]); } struct S; #[cfg(test)] impl S { fn k(&self) { FanoutSink(vec![]); } }",
    );
    assert!(hidden.fanouts.is_empty());
    assert!(!hidden.found.contains_key("x.rs: FanoutSink"));
}
