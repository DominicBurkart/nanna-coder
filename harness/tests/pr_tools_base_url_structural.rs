use proc_macro2::{TokenStream, TokenTree};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const SEAM: &str = "register_granted_at";
const ENTRY: &str = "register_granted";
const PUBLIC_ENTRY: &str = "register";
const CLIENT: &str = "ReqwestGithubClient";
const DEFAULT_URL: &str = "GITHUB_API_URL";
const PARAM: &str = "base_url";
const FORBIDDEN_IDENTS: [&str; 14] = [
    "reqwest",
    "hyper",
    "ureq",
    "isahc",
    "surf",
    "attohttpc",
    "curl",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "UnixStream",
    "Url",
    "Uri",
    "TlsConnector",
];
const ALLOWED_MACROS: [&str; 6] = ["format", "json", "matches", "thread_local", "vec", "write"];
const PINNED_ENV_CALLS: [&str; 3] = [
    "git_cmd: std::env::var_os(key)",
    "register_granted_at: std::env::var(\"GITHUB_TOKEN\")",
    "register_granted_at: std::env::var(TRUSTED_PR_COMMENTERS_ENV)",
];

fn plain(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn cfg_holds_outside_tests(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => !path.is_ident("test"),
        syn::Meta::NameValue(_) => true,
        syn::Meta::List(list) => {
            let nested = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .unwrap();
            let holds: Vec<bool> = nested.iter().map(cfg_holds_outside_tests).collect();
            match list
                .path
                .get_ident()
                .map(|ident| ident.to_string())
                .as_deref()
            {
                Some("not") => !holds.iter().all(|held| *held),
                Some("any") => holds.iter().any(|held| *held),
                _ => holds.iter().all(|held| *held),
            }
        }
    }
}

fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .map(|meta| !cfg_holds_outside_tests(&meta))
                .unwrap_or(false)
    })
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn flatten(stream: TokenStream, idents: &mut Vec<String>, macros: &mut Vec<String>) {
    let mut previous: Option<String> = None;
    for token in stream {
        match token {
            TokenTree::Group(group) => {
                flatten(group.stream(), idents, macros);
                previous = None;
            }
            TokenTree::Ident(ident) => {
                let name = plain(&ident.to_string()).to_string();
                idents.push(name.clone());
                previous = Some(name);
            }
            TokenTree::Punct(punct) => {
                if punct.as_char() == '!' {
                    if let Some(name) = previous.take() {
                        macros.push(name);
                    }
                } else {
                    previous = None;
                }
            }
            TokenTree::Literal(_) => previous = None,
        }
    }
}

fn path_segments(path: &syn::Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| plain(&segment.ident.to_string()).to_string())
        .collect()
}

fn expr_shape(expr: &syn::Expr) -> String {
    match expr {
        syn::Expr::Path(path) if path.qself.is_none() && path.path.leading_colon.is_none() => {
            path_segments(&path.path).join("::")
        }
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(text),
            ..
        }) => format!("\"{}\"", text.value()),
        _ => "<expr>".to_string(),
    }
}

fn param_shape(arg: &syn::FnArg) -> String {
    let syn::FnArg::Typed(typed) = arg else {
        return "self".to_string();
    };
    let name = match &*typed.pat {
        syn::Pat::Ident(ident) => plain(&ident.ident.to_string()).to_string(),
        _ => "<pattern>".to_string(),
    };
    let ty = match &*typed.ty {
        syn::Type::Reference(reference) if reference.mutability.is_none() => {
            match &*reference.elem {
                syn::Type::Path(path) => format!("&{}", path_segments(&path.path).join("::")),
                _ => "&<type>".to_string(),
            }
        }
        _ => "<type>".to_string(),
    };
    format!("{name}: {ty}")
}

#[derive(Default)]
struct Calls {
    idents: Vec<String>,
    macros: Vec<String>,
    in_use: bool,
    function: Vec<String>,
    calls: Vec<(String, Vec<String>, Vec<String>)>,
    seam_defs: Vec<(String, bool, Vec<String>, bool)>,
    client_impls: usize,
    env_uses: usize,
}

impl<'ast> Visit<'ast> for Calls {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let name = plain(&node.sig.ident.to_string()).to_string();
        if name == SEAM {
            let params = node.sig.inputs.iter().map(param_shape).collect();
            let private = matches!(node.vis, syn::Visibility::Inherited);
            let plain_sig = node.sig.asyncness.is_none()
                && node.sig.generics.params.is_empty()
                && node.sig.generics.where_clause.is_none();
            self.seam_defs
                .push((name.clone(), private, params, plain_sig));
        }
        self.function.push(name);
        syn::visit::visit_item_fn(self, node);
        self.function.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.function
            .push(plain(&node.sig.ident.to_string()).to_string());
        syn::visit::visit_impl_item_fn(self, node);
        self.function.pop();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if let Some((_, path, _)) = &node.trait_ {
            if path_segments(path)
                .last()
                .is_some_and(|n| n == "GithubClient")
            {
                self.client_impls += 1;
            }
        }
        syn::visit::visit_item_impl(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.in_use = true;
        syn::visit::visit_item_use(self, node);
        self.in_use = false;
    }

    fn visit_ident(&mut self, node: &'ast proc_macro2::Ident) {
        let name = plain(&node.to_string()).to_string();
        if self.in_use && name == "env" {
            self.env_uses += 1;
        }
        self.idents.push(name);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if let Some(name) = path_segments(&node.path).last() {
            self.macros.push(name.clone());
        }
        syn::visit::visit_macro(self, node);
        let (mut idents, mut macros) = (Vec::new(), Vec::new());
        flatten(node.tokens.clone(), &mut idents, &mut macros);
        self.idents.extend(idents);
        self.macros.extend(macros);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func {
            let function = self.function.last().cloned().unwrap_or_default();
            let args = node.args.iter().map(expr_shape).collect();
            self.calls.push((function, path_segments(&path.path), args));
        }
        syn::visit::visit_expr_call(self, node);
    }
}

fn count(idents: &[String], name: &str) -> usize {
    idents.iter().filter(|ident| *ident == name).count()
}

fn violations(source: &str) -> Vec<String> {
    let file = syn::parse_file(source).unwrap();
    let production: Vec<&syn::Item> = file
        .items
        .iter()
        .filter(|item| !is_test_only(item_attrs(item)))
        .collect();
    let mut calls = Calls::default();
    for item in &production {
        calls.visit_item(item);
    }
    let idents = std::mem::take(&mut calls.idents);
    let macros = std::mem::take(&mut calls.macros);
    let mut found = Vec::new();

    if count(&idents, SEAM) != 2 {
        found.push(format!(
            "`{SEAM}` must appear exactly twice (definition and one call), found {}",
            count(&idents, SEAM)
        ));
    }
    let seam_calls: Vec<_> = calls
        .calls
        .iter()
        .filter(|(_, path, _)| path.last().is_some_and(|n| n == SEAM))
        .collect();
    let expected_url: Vec<String> = vec!["crate".into(), "backlog".into(), DEFAULT_URL.into()];
    match seam_calls.as_slice() {
        [(function, path, args)] => {
            if function != ENTRY || path.len() != 1 {
                found.push(format!("`{SEAM}` called from `{function}` via {path:?}"));
            }
            if args.len() != 5 || args.last().map(String::as_str) != Some(&expected_url.join("::"))
            {
                found.push(format!(
                    "`{SEAM}` last argument is not crate::backlog::{DEFAULT_URL}: {args:?}"
                ));
            }
        }
        other => found.push(format!(
            "`{SEAM}` needs exactly one direct call, found {}",
            other.len()
        )),
    }
    match calls.seam_defs.as_slice() {
        [(_, private, params, plain_sig)] => {
            if !private || !plain_sig {
                found.push(format!(
                    "`{SEAM}` must be a private, non-async, non-generic fn"
                ));
            }
            let last = params.last().map(|p| p.replace(' ', ""));
            if params.len() != 5 || last.as_deref() != Some("base_url:&str") {
                found.push(format!(
                    "`{SEAM}` must end with `base_url: &str`: {params:?}"
                ));
            }
        }
        other => found.push(format!(
            "`{SEAM}` must be defined exactly once, found {}",
            other.len()
        )),
    }
    if count(&idents, ENTRY) != 2 {
        found.push(format!(
            "`{ENTRY}` must appear exactly twice, found {}",
            count(&idents, ENTRY)
        ));
    }
    for (function, path, _) in &calls.calls {
        if path.last().is_some_and(|n| n == ENTRY) && function != PUBLIC_ENTRY {
            found.push(format!("`{ENTRY}` called from `{function}`"));
        }
    }
    if count(&idents, CLIENT) != 1 {
        found.push(format!(
            "`{CLIENT}` must appear exactly once, found {}",
            count(&idents, CLIENT)
        ));
    }
    let constructions: Vec<_> = calls
        .calls
        .iter()
        .filter(|(_, path, _)| path.iter().any(|n| n == CLIENT))
        .collect();
    let expected_ctor: Vec<String> = ["crate", "backlog", CLIENT, "new"]
        .map(String::from)
        .to_vec();
    match constructions.as_slice() {
        [(function, path, args)] => {
            if function != SEAM
                || *path != expected_ctor
                || args != &[PARAM.to_string(), "token".to_string()]
            {
                found.push(format!(
                    "`{CLIENT}` constructed in `{function}` as {path:?}({args:?})"
                ));
            }
        }
        other => found.push(format!(
            "`{CLIENT}` needs exactly one construction, found {}",
            other.len()
        )),
    }
    if count(&idents, PARAM) != 2 {
        found.push(format!(
            "`{PARAM}` must appear exactly twice, found {}",
            count(&idents, PARAM)
        ));
    }
    if count(&idents, DEFAULT_URL) != 1 {
        found.push(format!(
            "`{DEFAULT_URL}` must appear exactly once, found {}",
            count(&idents, DEFAULT_URL)
        ));
    }
    let mut env_calls: Vec<String> = calls
        .calls
        .iter()
        .filter(|(_, path, _)| {
            path.iter().any(|n| n == "env")
                || path
                    .last()
                    .is_some_and(|n| matches!(n.as_str(), "var" | "var_os" | "vars" | "vars_os"))
        })
        .map(|(function, path, args)| {
            format!("{function}: {}({})", path.join("::"), args.join(", "))
        })
        .collect();
    env_calls.sort();
    if env_calls != PINNED_ENV_CALLS {
        found.push(format!("environment reads changed: {env_calls:?}"));
    }
    if calls.env_uses > 0 {
        found.push("`use` of std::env in production code".to_string());
    }
    for name in FORBIDDEN_IDENTS {
        if count(&idents, name) > 0 {
            found.push(format!("production code names `{name}`"));
        }
    }
    let mut used: Vec<&str> = macros.iter().map(String::as_str).collect();
    used.sort_unstable();
    used.dedup();
    for name in used {
        if !ALLOWED_MACROS.contains(&name) {
            found.push(format!("production code invokes unlisted macro `{name}!`"));
        }
    }
    for item in &production {
        if let syn::Item::Mod(module) = item {
            found.push(format!("production module `{}`", module.ident));
        }
    }
    let mut has_path_attr = false;
    for item in &production {
        has_path_attr |= item_attrs(item)
            .iter()
            .any(|attr| attr.path().is_ident("path"));
    }
    if has_path_attr {
        found.push("production code uses a `path` attribute".to_string());
    }
    if calls.client_impls > 0 {
        found.push("production code implements GithubClient".to_string());
    }
    found
}

fn real_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pr_tools.rs");
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn production_registration_can_only_ever_target_the_github_api() {
    assert_eq!(violations(&real_source()), Vec::<String>::new());
}

#[test]
fn the_default_base_url_is_the_public_github_api() {
    assert_eq!(harness::backlog::GITHUB_API_URL, "https://api.github.com");
    let client = harness::backlog::ReqwestGithubClient::github(None);
    assert!(format!("{client:?}").contains("base_url: \"https://api.github.com\""));
}

#[test]
fn no_other_file_can_name_the_seam() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "tests", "benches", "examples"] {
        rust_files(&root.join(dir), &mut files);
    }
    let this = root.join("tests/pr_tools_base_url_structural.rs");
    let owner = root.join("src/pr_tools.rs");
    assert!(files.len() > 50);
    for file in files.iter().filter(|f| **f != this && **f != owner) {
        let parsed = syn::parse_file(&fs::read_to_string(file).unwrap()).unwrap();
        let mut calls = Calls::default();
        calls.visit_file(&parsed);
        let idents = calls.idents;
        assert_eq!(count(&idents, SEAM), 0, "{}", file.display());
    }
}

fn mutate(replacements: &[(&str, &str)]) -> Vec<String> {
    let mut source = real_source();
    for (from, to) in replacements {
        assert!(source.contains(from), "{from}");
        source = source.replacen(from, to, 1);
    }
    violations(&source)
}

fn append(extra: &str) -> Vec<String> {
    let source = real_source();
    let at = source.find("#[cfg(test)]\nmod tests").unwrap();
    violations(&format!("{}{extra}\n{}", &source[..at], &source[at..]))
}

fn must_fail(found: Vec<String>, needle: &str) {
    assert!(
        found.iter().any(|v| v.contains(needle)),
        "{needle}: {found:?}"
    );
}

#[test]
fn a_changed_default_is_rejected() {
    must_fail(
        mutate(&[(
            "crate::backlog::GITHUB_API_URL,\n    );",
            "\"http://127.0.0.1:1\",\n    );",
        )]),
        "last argument",
    );
    must_fail(
        mutate(&[(
            "crate::backlog::GITHUB_API_URL,\n    );",
            "&std::env::var(\"NANNA_BASE\").unwrap_or_default(),\n    );",
        )]),
        "last argument",
    );
}

#[test]
fn a_second_call_is_rejected_however_it_is_spelled() {
    let call = "fn sneak(r: &mut ToolRegistry, p: &Path) { SEAM(r, p, \"x\", &|_| true, \"http://127.0.0.1:1\"); }";
    must_fail(
        append(&call.replace("SEAM", "register_granted_at")),
        "exactly twice",
    );
    must_fail(
        append(&call.replace("SEAM", "r#register_granted_at")),
        "exactly twice",
    );
    must_fail(
        append("use self::register_granted_at as go;"),
        "exactly twice",
    );
    must_fail(append("const GO: fn(&mut ToolRegistry, &Path, &str, &dyn Fn(&str) -> bool, &str) = register_granted_at;"), "exactly twice");
    must_fail(
        append("macro_rules! go { ($r:expr) => { register_granted_at($r, p, \"x\", &|_| true, \"http://x\") }; }"),
        "exactly twice",
    );
    must_fail(
        append("#[cfg(any(test, unix))]\nfn sneak(r: &mut ToolRegistry, p: &Path) { register_granted_at(r, p, \"x\", &|_| true, \"http://x\"); }"),
        "exactly twice",
    );
    must_fail(
        append("fn sneak(r: &mut ToolRegistry, p: &Path) { register_granted(r, p, \"x\", &|_| true); }"),
        "`register_granted` called from `sneak`",
    );
}

#[test]
fn the_seam_cannot_be_widened_or_renamed_around() {
    must_fail(
        mutate(&[(
            "\nfn register_granted_at(",
            "\npub(crate) fn register_granted_at(",
        )]),
        "private",
    );
    must_fail(
        mutate(&[("\nfn register_granted_at(", "\npub fn register_granted_at(")]),
        "private",
    );
    must_fail(
        mutate(&[(
            "\nfn register_granted_at(",
            "\nasync fn register_granted_at(",
        )]),
        "non-async",
    );
    must_fail(
        mutate(&[("    base_url: &str,\n) {", "    base_url_: &str,\n) {")]),
        "base_url: &str",
    );
    must_fail(
        mutate(&[(
            "::new(base_url, token)",
            "::new(&std::env::var(\"X\").unwrap(), token)",
        )]),
        "constructed in",
    );
}

#[test]
fn another_client_or_environment_read_is_rejected() {
    must_fail(
        append(
            "fn sneak() { let _ = crate::backlog::ReqwestGithubClient::new(\"http://x\", None); }",
        ),
        "ReqwestGithubClient",
    );
    must_fail(
        append("use crate::backlog::ReqwestGithubClient as C;"),
        "ReqwestGithubClient",
    );
    must_fail(
        append("fn sneak() -> Option<String> { std::env::var(\"NANNA_GITHUB_API_URL\").ok() }"),
        "environment reads changed",
    );
    must_fail(
        append("use std::env::var as v;\nfn sneak() { let _ = v(\"X\"); }"),
        "`use` of std::env",
    );
    must_fail(append("use std::env;"), "`use` of std::env");
    must_fail(
        append("fn sneak() -> &'static str { env!(\"NANNA_BASE\") }"),
        "unlisted macro `env!`",
    );
    must_fail(
        append("fn sneak() -> &'static str { include_str!(\"base.txt\") }"),
        "unlisted macro `include_str!`",
    );
    must_fail(append("fn sneak(c: reqwest::Client) {}"), "names `reqwest`");
    must_fail(
        append("struct Own;\n#[async_trait]\nimpl GithubClient for Own {}"),
        "implements GithubClient",
    );
    must_fail(
        append("fn sneak(s: std::net::TcpStream) {}"),
        "names `TcpStream`",
    );
}

#[test]
fn child_modules_and_unlisted_macros_are_rejected() {
    must_fail(append("mod elsewhere;"), "production module `elsewhere`");
    must_fail(
        append("#[path = \"../../elsewhere.rs\"]\nmod elsewhere;"),
        "production module `elsewhere`",
    );
    must_fail(
        append("mod inline { pub fn go() { super::register_granted_at(); } }"),
        "production module `inline`",
    );
    must_fail(
        append("fn sneak() { paste::paste! { [<register_ granted_at>](); } }"),
        "unlisted macro `paste!`",
    );
    must_fail(
        append("macro_rules! go { () => {}; }"),
        "unlisted macro `macro_rules!`",
    );
}

#[test]
fn test_gated_items_are_ignored_and_never_production() {
    assert!(append(
        "#[cfg(test)]\nfn only_in_tests() { register_granted_at(a, b, c, d, \"http://x\"); }"
    )
    .is_empty());
}
