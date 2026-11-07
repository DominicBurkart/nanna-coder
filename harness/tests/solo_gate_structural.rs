use async_trait::async_trait;
use harness::action_auditor::{ActionAuditLog, ActionGate, RuleActionAuditor};
use harness::backlog::{
    BacklogError, GithubClient, GithubComment, GithubIssue, GithubIssueDetail, GithubPullRequest,
    GithubPullRequestCreated, GithubPullRequestDetail, GithubReview,
};
use harness::effects::EffectClass;
use harness::leases::InMemoryLeaseStore;
use harness::policy::HumanGate;
use harness::pr_tools::{
    GitCommitTool, GitPushBranchTool, GithubIssueCommentTool, GithubIssueReadTool,
    GithubPrCloseTool, GithubPrCommentsTool, GithubPrOpenTool, GithubPrPromoteTool,
};
use harness::task::TaskId;
use harness::tools::{ActionSubject, GitStatusTool, Tool, ToolError, ToolRegistry, ToolResult};
use harness::windows::WindowSet;
use proc_macro2::{TokenStream, TokenTree};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use syn::visit::Visit;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Solo {
    Local,
    Read,
    Comments,
    OpensDraftOnly,
    Closes,
    CiWorkflow,
    GuardedPush,
    RefusedPromote,
    Shell,
}

const TOOL_TABLE: &[(&str, Solo)] = &[
    ("AppLogsTool", Solo::Local),
    ("AppStartTool", Solo::Local),
    ("AppStopTool", Solo::Local),
    ("CalculatorTool", Solo::Local),
    ("CargoAuditTool", Solo::Local),
    ("CargoBenchTool", Solo::Local),
    ("CargoBuildTool", Solo::Local),
    ("CargoCheckTool", Solo::Local),
    ("CargoDenyTool", Solo::Local),
    ("CargoRunTool", Solo::Local),
    ("CargoTestTool", Solo::Local),
    ("CiLogsTool", Solo::Read),
    ("CiStatusTool", Solo::Read),
    ("CiTriggerTool", Solo::CiWorkflow),
    ("EchoTool", Solo::Local),
    ("GitCommitTool", Solo::Local),
    ("GitDiffTool", Solo::Local),
    ("GitHubPrStatusTool", Solo::Read),
    ("GitPushBranchTool", Solo::GuardedPush),
    ("GitStatusTool", Solo::Local),
    ("GithubIssueCommentTool", Solo::Comments),
    ("GithubIssueReadTool", Solo::Read),
    ("GithubPrCloseTool", Solo::Closes),
    ("GithubPrCommentsTool", Solo::Read),
    ("GithubPrOpenTool", Solo::OpensDraftOnly),
    ("GithubPrPromoteTool", Solo::RefusedPromote),
    ("ListDirTool", Solo::Local),
    ("QaBrowserTool", Solo::Local),
    ("QaEndpointsTool", Solo::Local),
    ("ReadFileTool", Solo::Local),
    ("RunCommandTool", Solo::Shell),
    ("SandboxDeployTool", Solo::Local),
    ("SandboxTeardownTool", Solo::Local),
    ("SearchTool", Solo::Local),
    ("SqlxMigrateTool", Solo::Local),
    ("StubTool", Solo::Local),
    ("TrunkBuildTool", Solo::Local),
    ("WriteFileTool", Solo::Local),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClientEffect {
    Reads,
    WritesIssueText,
    OpensDraft,
    Closes,
    UnDrafts,
    RunsWorkflow,
}

const GITHUB_CLIENT_METHODS: &[(&str, ClientEffect)] = &[
    ("search_open_issues", ClientEffect::Reads),
    ("open_pull_requests", ClientEffect::Reads),
    ("create_issue", ClientEffect::WritesIssueText),
    ("comment_on_issue", ClientEffect::WritesIssueText),
    ("create_draft_pull_request", ClientEffect::OpensDraft),
    ("get_pull_request", ClientEffect::Reads),
    ("pull_requests_for_head", ClientEffect::Reads),
    ("mark_pull_request_ready", ClientEffect::UnDrafts),
    ("close_pull_request", ClientEffect::Closes),
    ("list_review_comments", ClientEffect::Reads),
    ("list_issue_comments", ClientEffect::Reads),
    ("list_pull_request_reviews", ClientEffect::Reads),
    ("get_issue", ClientEffect::Reads),
];

const ACTIONS_CLIENT_METHODS: &[(&str, ClientEffect)] = &[
    ("dispatch_workflow", ClientEffect::RunsWorkflow),
    ("rerun_workflow", ClientEffect::RunsWorkflow),
    ("list_workflow_runs", ClientEffect::Reads),
    ("get_workflow_run", ClientEffect::Reads),
    ("list_workflow_jobs", ClientEffect::Reads),
    ("get_job_logs", ClientEffect::Reads),
];

const UNDRAFT_METHOD: &str = "mark_pull_request_ready";
const UNDRAFT_SITE: (&str, &str, &str) = ("src/pr_tools.rs", "GithubPrPromoteTool", "execute");

const LITERAL_NEEDLES: &[&str] = &[
    "markPullRequestReadyForReview",
    "mergePullRequest",
    "enablePullRequestAutoMerge",
    "convertPullRequestToDraft",
    "/merge",
    "draft",
];

const LITERAL_SITES: &[(&str, &str, &str, &str)] = &[
    (
        "src/backlog.rs",
        "ReqwestGithubClient",
        "create_draft_pull_request",
        "draft",
    ),
    (
        "src/backlog.rs",
        "ReqwestGithubClient",
        "mark_pull_request_ready",
        "markPullRequestReadyForReview",
    ),
    ("src/pr_tools.rs", "GithubPrOpenTool", "execute", "draft"),
    ("src/tools.rs", "PrStatusData", "to_l0", "draft"),
    ("src/tools.rs", "", "fetch_github_pr_data", "draft"),
];

const GH_ALLOWED: &[(&str, &str)] = &[
    ("auth", "status"),
    ("issue", "list"),
    ("issue", "status"),
    ("issue", "view"),
    ("pr", "checks"),
    ("pr", "diff"),
    ("pr", "list"),
    ("pr", "status"),
    ("pr", "view"),
    ("repo", "view"),
    ("run", "list"),
    ("run", "view"),
];

const GH_REFUSED: &[(&str, &str)] = &[("pr", "merge")];

const PINNED_LIMITS: &[&str] = &[
    "only files under src/ are scanned; tests, benches, examples and build scripts are not",
    "test-only code (cfg(test) items and inline modules) is not scanned",
    "tools are found by the syntax `impl Tool for Type`; a tool registered through a macro, a blanket impl or generated code is not seen",
    "string needles are literal tokens; a needle assembled at run time with format!, concat! or byte arithmetic is not seen",
    "the gh allow-list is read from the arms of the one `match (top, sub)` in `inspect_gh`; a second dispatch elsewhere is not seen",
    "the shell forms are a fixed enumeration; a form not listed here is covered only by the action auditor's own suites",
    "the enumeration shows the harness gives an agent no path to flip draft state; a raw GitHub token held outside the harness, a pull request created outside it and a human click between the draft check and the push are out of scope",
    "the solo check before the un-drafting call is recognised by the identifiers HumanGate, Solo and gate appearing earlier in the same function, not by control flow: an empty or unreachable check passes the pin and is caught only by the behavioural tests",
    "classification forces a review but proves nothing: a new tool or client method that is classified as local or read-only while it un-drafts passes the pin",
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
            match list.path.get_ident().map(|i| i.to_string()).as_deref() {
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

fn flatten(stream: TokenStream, out: &mut Vec<TokenTree>) {
    for token in stream {
        match token {
            TokenTree::Group(group) => flatten(group.stream(), out),
            other => out.push(other),
        }
    }
}

fn string_value(literal: &proc_macro2::Literal) -> Option<String> {
    syn::parse_str::<syn::LitStr>(&literal.to_string())
        .ok()
        .map(|lit| lit.value())
}

#[derive(Default)]
struct Scan {
    impl_type: Vec<String>,
    function: Vec<String>,
    idents_in_fn: Vec<Vec<String>>,
    tool_impls: Vec<String>,
    undraft_sites: Vec<(String, String, Vec<String>)>,
    literals: Vec<(String, String, String)>,
}

impl Scan {
    fn site(&self) -> (String, String) {
        let ty = self.impl_type.last().cloned().unwrap_or_default();
        let function = self.function.last().cloned().unwrap_or_default();
        (ty, function)
    }

    fn note_ident(&mut self, name: &str) {
        if let Some(seen) = self.idents_in_fn.last_mut() {
            seen.push(name.to_string());
        }
        if name == UNDRAFT_METHOD {
            let (ty, function) = self.site();
            let prior = self.idents_in_fn.last().cloned().unwrap_or_default();
            self.undraft_sites.push((ty, function, prior));
        }
    }

    fn enter_impl(&mut self, node: &syn::ItemImpl) {
        let name = type_name(&node.self_ty);
        if let Some((_, path, _)) = &node.trait_ {
            if path.segments.last().is_some_and(|s| s.ident == "Tool") {
                self.tool_impls.push(name.clone());
            }
        }
        self.impl_type.push(name);
        syn::visit::visit_item_impl(self, node);
        self.impl_type.pop();
    }

    fn enter_fn(&mut self, ident: &proc_macro2::Ident, walk: impl FnOnce(&mut Self)) {
        self.function.push(plain(&ident.to_string()).to_string());
        self.idents_in_fn.push(Vec::new());
        walk(self);
        self.idents_in_fn.pop();
        self.function.pop();
    }

    fn note_string(&mut self, literal: &proc_macro2::Literal) {
        if let Some(value) = string_value(literal) {
            self.note_literal(value);
        }
    }

    #[rustfmt::skip]
    fn note_literal(&mut self, value: String) { let (ty, function) = self.site(); self.literals.push((ty, function, value)); }
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| plain(&segment.ident.to_string()).to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

impl<'ast> Visit<'ast> for Scan {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_test_only(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !is_test_only(&node.attrs) {
            self.enter_impl(node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_test_only(&node.attrs) {
            self.enter_fn(&node.sig.ident, |me| syn::visit::visit_item_fn(me, node));
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_test_only(&node.attrs) {
            self.enter_fn(&node.sig.ident, |me| {
                syn::visit::visit_impl_item_fn(me, node)
            });
        }
    }

    fn visit_ident(&mut self, node: &'ast proc_macro2::Ident) {
        let name = plain(&node.to_string()).to_string();
        self.note_ident(&name);
    }

    fn visit_expr_lit(&mut self, node: &'ast syn::ExprLit) {
        if let syn::Lit::Str(text) = &node.lit {
            self.note_literal(text.value());
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let mut tokens = Vec::new();
        flatten(node.tokens.clone(), &mut tokens);
        for token in tokens {
            if let TokenTree::Ident(ident) = &token {
                self.note_ident(plain(&ident.to_string()));
            }
            if let TokenTree::Literal(literal) = &token {
                self.note_string(literal);
            }
        }
    }
}

fn scan(source: &str) -> Scan {
    let mut state = Scan::default();
    state.visit_file(&syn::parse_file(source).unwrap());
    state
}

fn trait_methods(source: &str, name: &str) -> Vec<String> {
    let file = syn::parse_file(source).unwrap();
    let mut found = Vec::new();
    for item in &file.items {
        if let syn::Item::Trait(item) = item {
            if item.ident == name {
                for member in &item.items {
                    if let syn::TraitItem::Fn(method) = member {
                        found.push(method.sig.ident.to_string());
                    }
                }
            }
        }
    }
    found.sort();
    found
}

fn classified(table: &[(&str, ClientEffect)]) -> Vec<String> {
    let mut names: Vec<String> = table.iter().map(|(n, _)| n.to_string()).collect();
    names.sort();
    names
}

fn undrafting_violations(tables: &[&[(&str, ClientEffect)]]) -> Vec<String> {
    let all = tables.iter().flat_map(|table| table.iter());
    let flagged = all.filter(|(_, effect)| *effect == ClientEffect::UnDrafts);
    let undrafting: Vec<&str> = flagged.map(|(name, _)| *name).collect();
    let mut found = Vec::new();
    if undrafting != [UNDRAFT_METHOD] {
        found.push(format!(
            "the classification names {undrafting:?} as un-drafting"
        ));
    }
    found
}

fn client_violations(backlog: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (trait_name, table) in [
        ("GithubClient", GITHUB_CLIENT_METHODS),
        ("GithubActionsClient", ACTIONS_CLIENT_METHODS),
    ] {
        let actual = trait_methods(backlog, trait_name);
        if actual != classified(table) {
            found.push(format!(
                "`{trait_name}` has the unclassified methods {actual:?}"
            ));
        }
    }
    found.extend(undrafting_violations(&[
        GITHUB_CLIENT_METHODS,
        ACTIONS_CLIENT_METHODS,
    ]));
    found
}

fn tool_violations(files: &[(String, String)]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for (_, source) in files {
        seen.extend(scan(source).tool_impls);
    }
    let table: BTreeSet<String> = TOOL_TABLE.iter().map(|(n, _)| n.to_string()).collect();
    seen.difference(&table)
        .map(|name| format!("tool `{name}` is not classified for solo mode"))
        .collect()
}

fn undraft_violations(files: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    let mut calls = Vec::new();
    for (path, source) in files {
        for (ty, function, prior) in scan(source).undraft_sites {
            let definition =
                path == "src/backlog.rs" && (function.is_empty() || function == UNDRAFT_METHOD);
            if !definition {
                calls.push((path.clone(), ty, function, prior));
            }
        }
    }
    match calls.as_slice() {
        [(path, ty, function, prior)] => {
            if (path.as_str(), ty.as_str(), function.as_str()) != UNDRAFT_SITE {
                found.push(format!(
                    "`{UNDRAFT_METHOD}` called from {path} {ty}::{function}"
                ));
            }
            let guarded = ["HumanGate", "Solo", "gate"]
                .iter()
                .all(|needed| prior.iter().any(|ident| ident == needed));
            if !guarded {
                found.push(format!(
                    "`{UNDRAFT_METHOD}` is reached without a HumanGate::Solo check first"
                ));
            }
        }
        other => found.push(format!(
            "`{UNDRAFT_METHOD}` must have exactly one caller, found {}",
            other.len()
        )),
    }
    found
}

fn literal_violations(files: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for (path, source) in files {
        for (ty, function, value) in scan(source).literals {
            for needle in LITERAL_NEEDLES {
                let hit = if *needle == "draft" {
                    value == *needle
                } else {
                    value.contains(needle)
                };
                let site = (path.as_str(), ty.as_str(), function.as_str(), *needle);
                if hit && !LITERAL_SITES.contains(&site) {
                    found.push(format!("{path} {ty}::{function} names `{needle}`"));
                }
            }
        }
    }
    found
}

type Pairs = BTreeSet<(String, String)>;

fn tuple_pairs(pat: &syn::Pat, out: &mut Vec<(String, String)>) {
    match pat {
        syn::Pat::Or(or) => or.cases.iter().for_each(|case| tuple_pairs(case, out)),
        syn::Pat::Tuple(tuple) if tuple.elems.len() == 2 => {
            let text = |p: &syn::Pat| match p {
                syn::Pat::Lit(lit) => match &lit.lit {
                    syn::Lit::Str(s) => Some(s.value()),
                    _ => None,
                },
                _ => None,
            };
            if let (Some(a), Some(b)) = (text(&tuple.elems[0]), text(&tuple.elems[1])) {
                out.push((a, b));
            }
        }
        _ => {}
    }
}

#[derive(Default)]
struct GhArms {
    in_gh: bool,
    allowed: Pairs,
    refused: Pairs,
}

impl<'ast> Visit<'ast> for GhArms {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let was = self.in_gh;
        self.in_gh = node.sig.ident == "inspect_gh";
        syn::visit::visit_item_fn(self, node);
        self.in_gh = was;
    }

    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        if self.in_gh {
            for arm in &node.arms {
                let mut found = Vec::new();
                tuple_pairs(&arm.pat, &mut found);
                let refuses = matches!(&*arm.body, syn::Expr::Return(_)) && arm.guard.is_none();
                for pair in found {
                    if refuses {
                        self.refused.insert(pair);
                    } else {
                        self.allowed.insert(pair);
                    }
                }
            }
        }
        syn::visit::visit_expr_match(self, node);
    }
}

fn pair_set(pairs: &[(&str, &str)]) -> Pairs {
    pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn gh_violations(commands: &str) -> Vec<String> {
    let mut arms = GhArms::default();
    arms.visit_file(&syn::parse_file(commands).unwrap());
    let mut found = Vec::new();
    for pair in arms.allowed.difference(&pair_set(GH_ALLOWED)) {
        found.push(format!(
            "`gh {} {}` is allow-listed but not classified",
            pair.0, pair.1
        ));
    }
    for pair in pair_set(GH_ALLOWED).difference(&arms.allowed) {
        found.push(format!(
            "`gh {} {}` is classified but no longer matched",
            pair.0, pair.1
        ));
    }
    if arms.refused != pair_set(GH_REFUSED) {
        found.push(format!("refused gh pairs are {:?}", arms.refused));
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn relative_name(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap().to_string_lossy();
    relative.replace('\\', "/")
}

fn read_source(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

fn real_files() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    rust_files(&root.join("src"), &mut paths);
    paths
        .into_iter()
        .map(|path| (relative_name(root, &path), read_source(&path)))
        .collect()
}

fn file<'a>(files: &'a [(String, String)], name: &str) -> &'a str {
    files
        .iter()
        .find(|(path, _)| path == name)
        .map(|(_, source)| source.as_str())
        .unwrap()
}

fn with_file(
    files: &[(String, String)],
    name: &str,
    edit: impl Fn(&str) -> String,
) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(path, source)| {
            let text = if path == name {
                edit(source)
            } else {
                source.clone()
            };
            (path.clone(), text)
        })
        .collect()
}

fn replaced(text: &str, old: &str, new: &str) -> String {
    assert!(text.contains(old), "source no longer contains `{old}`");
    text.replacen(old, new, 1)
}

fn appended(files: &[(String, String)], name: &str, addition: &str) -> Vec<(String, String)> {
    with_file(files, name, |s| format!("{s}\n{addition}\n"))
}

fn all_violations(files: &[(String, String)]) -> Vec<String> {
    let mut found = client_violations(file(files, "src/backlog.rs"));
    found.extend(tool_violations(files));
    found.extend(undraft_violations(files));
    found.extend(literal_violations(files));
    found.extend(gh_violations(file(files, "src/action_auditor/commands.rs")));
    found
}

#[test]
fn the_production_tree_classifies_every_tool_client_method_and_draft_flipping_path() {
    assert_eq!(all_violations(&real_files()), Vec::<String>::new());
}

#[test]
fn every_classified_tool_exists_so_the_table_cannot_rot() {
    let mut seen = BTreeSet::new();
    for (_, source) in &real_files() {
        seen.extend(scan(source).tool_impls);
    }
    let stale: Vec<&str> = TOOL_TABLE
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !seen.contains(*name))
        .collect();
    assert_eq!(stale, Vec::<&str>::new());
}

#[test]
fn every_pr_tool_is_classified_with_the_disposition_the_enumeration_exercises() {
    let by_name = |name: &str| TOOL_TABLE.iter().find(|(n, _)| *n == name).unwrap().1;
    assert_eq!(by_name("GithubPrPromoteTool"), Solo::RefusedPromote);
    assert_eq!(by_name("GitPushBranchTool"), Solo::GuardedPush);
    assert_eq!(by_name("GithubPrOpenTool"), Solo::OpensDraftOnly);
    assert_eq!(by_name("GithubPrCloseTool"), Solo::Closes);
    assert_eq!(by_name("GithubIssueCommentTool"), Solo::Comments);
    assert_eq!(by_name("CiTriggerTool"), Solo::CiWorkflow);
    assert_eq!(by_name("RunCommandTool"), Solo::Shell);
    let shell: Vec<_> = TOOL_TABLE
        .iter()
        .filter(|(_, class)| *class == Solo::Shell)
        .collect();
    assert_eq!(shell.len(), 1);
}

#[test]
fn a_new_tool_anywhere_in_src_fails_until_it_is_classified() {
    let files = real_files();
    for (path, addition) in [
        (
            "src/pr_tools.rs",
            "pub struct GithubPrEditTool; #[async_trait] impl Tool for GithubPrEditTool {}",
        ),
        (
            "src/pr_tools.rs",
            "mod extra { impl Tool for GithubPrReadyTool {} }",
        ),
        ("src/ci_tools.rs", "impl Tool for CiMergeTool {}"),
        (
            "src/qa/tools.rs",
            "impl harness::tools::Tool for QaShipTool {}",
        ),
    ] {
        let found = tool_violations(&appended(&files, path, addition));
        assert_eq!(found.len(), 1, "{path}: {addition}: {found:?}");
        assert!(found[0].contains("not classified for solo mode"));
    }
}

#[test]
fn test_gated_tools_are_not_production_tools() {
    let files = real_files();
    for addition in [
        "#[cfg(test)] impl Tool for GithubPrEditTool {}",
        "#[cfg(test)] mod t { impl Tool for GithubPrEditTool {} }",
        "#[cfg(all(test, unix))] mod t { impl Tool for GithubPrEditTool {} }",
    ] {
        let mutated = appended(&files, "src/pr_tools.rs", addition);
        assert!(tool_violations(&mutated).is_empty(), "{addition}");
    }
}

#[test]
fn a_new_or_renamed_client_method_fails_until_it_is_classified() {
    let files = real_files();
    let backlog = file(&files, "src/backlog.rs");
    let added = replaced(
        backlog,
        "pub trait GithubClient: Send + Sync {",
        "pub trait GithubClient: Send + Sync {\n    async fn merge_pull_request(&self, repo: &str, number: u64) -> Result<(), BacklogError>;",
    );
    assert!(client_violations(&added)
        .iter()
        .any(|v| v.contains("`GithubClient` has the unclassified methods")));
    let renamed = replaced(backlog, "mark_pull_request_ready", "make_ready");
    assert!(!client_violations(&renamed).is_empty());
    let actions = replaced(
        backlog,
        "pub trait GithubActionsClient: Send + Sync {",
        "pub trait GithubActionsClient: Send + Sync {\n    async fn cancel_run(&self, repo: &str, run_id: u64) -> Result<(), BacklogError>;",
    );
    assert!(client_violations(&actions)
        .iter()
        .any(|v| v.contains("GithubActionsClient")));
}

#[test]
#[rustfmt::skip]
fn a_second_caller_of_the_un_drafting_method_is_flagged_however_it_is_spelled() {
    let files = real_files();
    for addition in [
        "fn sneaky(c: &dyn GithubClient) { c.mark_pull_request_ready(r, 1); }",
        "fn sneaky() { let f = GithubClient::mark_pull_request_ready; }",
        "fn sneaky() { foo!(c.mark_pull_request_ready(r, 1)); }",
        "impl X { fn go(&self) { self.client.mark_pull_request_ready(r, 1); } }",
    ] {
        let found = undraft_violations(&appended(&files, "src/ci_tools.rs", addition));
        assert!(found.iter().any(|v| v.contains("exactly one caller")), "{addition}: {found:?}");
    }
}

#[test]
#[rustfmt::skip]
fn removing_the_solo_check_before_the_un_drafting_call_is_flagged() {
    let files = real_files();
    let mutated = with_file(&files, "src/pr_tools.rs", |s| {
        replaced(s, "if self.gate == HumanGate::Solo {", "if false {")
    });
    let found = undraft_violations(&mutated);
    assert!(found .iter() .any(|v| v.contains("without a HumanGate::Solo check")), "{found:?}");
}

#[test]
fn moving_the_un_drafting_call_to_another_tool_is_flagged() {
    let files = real_files();
    let mutated = with_file(&files, "src/pr_tools.rs", |s| {
        replaced(
            s,
            "impl Tool for GithubPrPromoteTool",
            "impl Tool for GithubPrPromoteToolX",
        )
    });
    assert!(undraft_violations(&mutated)
        .iter()
        .any(|v| v.contains("called from")));
}

#[test]
fn dangerous_literals_outside_the_listed_sites_are_flagged() {
    let files = real_files();
    for addition in [
        "fn x() { let q = \"mutation { mergePullRequest(input: {}) { clientMutationId } }\"; }",
        "fn x() { let q = \"enablePullRequestAutoMerge\"; }",
        "fn x() { let u = format!(\"/repos/{r}/pulls/{n}/merge\"); }",
        "fn x() { let q = json!({ \"draft\": false }); }",
        "fn x() { foo!(\"convertPullRequestToDraft\"); }",
    ] {
        let mutated = appended(&files, "src/ci_tools.rs", addition);
        assert!(!literal_violations(&mutated).is_empty(), "{addition}");
    }
}

#[test]
fn a_new_allow_listed_gh_command_or_a_dropped_merge_refusal_is_flagged() {
    let files = real_files();
    let commands = file(&files, "src/action_auditor/commands.rs");
    let added = replaced(
        commands,
        "(\"pr\", \"view\") | (\"issue\", \"view\") => GH_PR_VIEW,",
        "(\"pr\", \"view\") | (\"issue\", \"view\") | (\"pr\", \"ready\") => GH_PR_VIEW,",
    );
    assert!(gh_violations(&added)
        .iter()
        .any(|v| v.contains("gh pr ready")));
    let unrefused = replaced(
        commands,
        "(\"pr\", \"merge\") => return Err(\"agents never merge pull requests\".to_string()),",
        "(\"pr\", \"merge\") => GH_PR_VIEW,",
    );
    assert!(!gh_violations(&unrefused).is_empty());
    let dropped = replaced(commands, "(\"pr\", \"checks\") => GH_PR_CHECKS,", "");
    assert!(gh_violations(&dropped)
        .iter()
        .any(|v| v.contains("no longer matched")));
}

#[test]
fn the_documented_blind_spots_are_really_not_seen() {
    let files = real_files();
    for addition in [
        "macro_rules! m { () => { impl Tool for HiddenTool {} } }",
        "fn x() { let q = concat!(\"mergePull\", \"Request\"); }",
        "fn x() { let q = format!(\"{}{}\", \"merge\", \"PullRequest\"); }",
    ] {
        let mutated = appended(&files, "src/ci_tools.rs", addition);
        assert!(tool_violations(&mutated).is_empty(), "{addition}");
        assert!(literal_violations(&mutated).is_empty(), "{addition}");
    }
    assert_eq!(PINNED_LIMITS.len(), 9);
}

#[derive(Default)]
struct Recording {
    calls: Mutex<Vec<String>>,
    listing: Vec<GithubPullRequestDetail>,
    undrafted: Mutex<bool>,
}

fn detail(number: u64, draft: bool) -> GithubPullRequestDetail {
    let body = r"body\n\n<!-- Nanna-Identity: solo-dev -->";
    let json = format!(
        r#"{{"number":{number},"node_id":"PR_{number}","draft":{draft},"state":"open","body":"{body}","html_url":"https://example.invalid/pull/{number}"}}"#
    );
    serde_json::from_str(&json).unwrap()
}

impl Recording {
    #[rustfmt::skip]
    fn new(listing: Vec<GithubPullRequestDetail>) -> Arc<Self> { Arc::new(Self { listing, ..Default::default() }) }

    fn note(&self, call: &str) {
        self.calls.lock().unwrap().push(call.to_string());
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn undrafted(&self) -> bool {
        *self.undrafted.lock().unwrap()
    }
}

fn unavailable(what: &str) -> BacklogError {
    BacklogError::Status {
        url: format!("recording:{what}"),
        status: 404,
    }
}

#[async_trait]
impl GithubClient for Recording {
    async fn search_open_issues(&self, _: &str, _: &str) -> Result<Vec<GithubIssue>, BacklogError> {
        Ok(vec![])
    }

    async fn open_pull_requests(&self, _: &str) -> Result<Vec<GithubPullRequest>, BacklogError> {
        Ok(vec![])
    }

    async fn create_issue(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[String],
    ) -> Result<GithubIssue, BacklogError> {
        Err(unavailable("create_issue"))
    }

    async fn comment_on_issue(&self, _: &str, number: u64, _: &str) -> Result<(), BacklogError> {
        self.note(&format!("comment_on_issue {number}"));
        Ok(())
    }

    async fn create_draft_pull_request(
        &self,
        _: &str,
        _: &str,
        _: &str,
        head: &str,
        _: &str,
    ) -> Result<GithubPullRequestCreated, BacklogError> {
        self.note(&format!("create_draft_pull_request {head}"));
        let json = r#"{"number":9,"html_url":"https://example.invalid/pull/9","node_id":"PR_9"}"#;
        Ok(serde_json::from_str(json).unwrap())
    }

    async fn get_pull_request(
        &self,
        _: &str,
        number: u64,
    ) -> Result<GithubPullRequestDetail, BacklogError> {
        Ok(detail(number, true))
    }

    async fn pull_requests_for_head(
        &self,
        _: &str,
        head: &str,
    ) -> Result<Vec<GithubPullRequestDetail>, BacklogError> {
        self.note(&format!("pull_requests_for_head {head}"));
        Ok(self.listing.clone())
    }

    async fn mark_pull_request_ready(&self, _: &str, number: u64) -> Result<(), BacklogError> {
        self.note(&format!("mark_pull_request_ready {number}"));
        *self.undrafted.lock().unwrap() = true;
        Ok(())
    }

    async fn close_pull_request(&self, _: &str, number: u64) -> Result<(), BacklogError> {
        self.note(&format!("close_pull_request {number}"));
        Ok(())
    }

    async fn list_review_comments(
        &self,
        _: &str,
        _: u64,
    ) -> Result<Vec<GithubComment>, BacklogError> {
        Ok(vec![])
    }

    async fn list_issue_comments(
        &self,
        _: &str,
        _: u64,
    ) -> Result<Vec<GithubComment>, BacklogError> {
        Ok(vec![])
    }

    async fn list_pull_request_reviews(
        &self,
        _: &str,
        _: u64,
    ) -> Result<Vec<GithubReview>, BacklogError> {
        Ok(vec![])
    }

    async fn get_issue(&self, _: &str, _: u64) -> Result<GithubIssueDetail, BacklogError> {
        Err(unavailable("get_issue"))
    }
}

#[rustfmt::skip]
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
}

struct Checkout {
    _bare: tempfile::TempDir,
    work: tempfile::TempDir,
}

fn checkout() -> Checkout {
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "--bare", "-q"]);
    let work = tempfile::tempdir().unwrap();
    let path = work.path();
    git(path, &["init", "-q"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test User"]);
    fs::write(path.join("README.md"), "hello\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "initial"]);
    git(path, &["branch", "-M", "main"]);
    git(
        path,
        &["remote", "add", "origin", "git@github.com:example/repo.git"],
    );
    git(
        path,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            bare.path().to_str().unwrap(),
        ],
    );
    git(path, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(path, &["checkout", "-q", "-b", "feature-solo"]);
    fs::write(path.join("f.txt"), "x\n").unwrap();
    git(path, &["add", "."]);
    git(
        path,
        &["commit", "-q", "-m", "feature\n\nNanna-Identity: solo-dev"],
    );
    Checkout { _bare: bare, work }
}

fn branch_is_on_origin(fixture: &Checkout) -> bool {
    let pushed = Command::new("git")
        .args(["ls-remote", "--heads", "--"])
        .arg(fixture._bare.path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&pushed.stdout).contains("refs/heads/feature-solo")
}

type NamedTool = (&'static str, Box<dyn Tool>, Value);

#[rustfmt::skip]
fn pr_tools(work: &Path, client: &Arc<Recording>, promote: HumanGate, push: HumanGate) -> Vec<NamedTool> {
    let root = work.to_path_buf();
    let shared: Arc<dyn GithubClient> = client.clone();
    let id = "solo-dev";
    let pusher = GitPushBranchTool::new(root.clone(), id).with_gate(push).with_client(shared.clone());
    let promoter = GithubPrPromoteTool::new(root.clone(), id, shared.clone()).with_gate(promote);
    vec![
        ("GitPushBranchTool", Box::new(pusher), json!({})),
        ("GitCommitTool", Box::new(GitCommitTool::new(root.clone(), id)), json!({ "subject": "x" })),
        ("GithubPrOpenTool", Box::new(GithubPrOpenTool::new(root.clone(), id, shared.clone())), json!({ "title": "t", "body": "b" })),
        ("GithubPrCommentsTool", Box::new(GithubPrCommentsTool::new(root.clone(), shared.clone(), vec![])), json!({ "pr_number": 7 })),
        ("GithubPrPromoteTool", Box::new(promoter), json!({ "pr_number": 7 })),
        ("GithubPrCloseTool", Box::new(GithubPrCloseTool::new(root.clone(), id, shared.clone())), json!({ "pr_number": 7, "reason": "#1" })),
        ("GithubIssueReadTool", Box::new(GithubIssueReadTool::new(root.clone(), shared.clone(), vec![])), json!({ "issue_number": 1 })),
        ("GithubIssueCommentTool", Box::new(GithubIssueCommentTool::new(root, id, shared)), json!({ "issue_number": 1, "body": "hi" })),
    ]
}

fn smuggled_args() -> Vec<(&'static str, Value)> {
    vec![
        ("draft", json!(false)),
        ("ready", json!(true)),
        ("state", json!("open")),
        ("merge", json!(true)),
        ("auto_merge", json!(true)),
        ("ready_for_review", json!(true)),
    ]
}

async fn smuggle(
    name: &str,
    tool: &dyn Tool,
    args: &Value,
    key: &str,
    value: Value,
) -> Vec<String> {
    let mut extended = args.clone();
    extended
        .as_object_mut()
        .unwrap()
        .insert(key.to_string(), value);
    let result = tool.execute(extended).await;
    let rejected = matches!(result, Err(ToolError::InvalidArguments { .. }));
    let note = format!("{name} accepted the extra argument `{key}`");
    if rejected {
        vec![]
    } else {
        vec![note]
    }
}

async fn draft_flips(promote: HumanGate, push: HumanGate) -> Vec<String> {
    let mut flips = Vec::new();
    let fixture = checkout();
    let client = Recording::new(vec![detail(7, false)]);
    for (name, tool, args) in pr_tools(fixture.work.path(), &client, promote, push) {
        let outcome = tool.execute(args.clone()).await;
        if name == "GithubPrPromoteTool" && outcome.is_ok() {
            flips.push(format!("{name} promoted"));
        }
        if name == "GitPushBranchTool" && outcome.is_ok() {
            flips.push(format!("{name} pushed over a ready pull request"));
        }
        for (key, value) in smuggled_args() {
            flips.extend(smuggle(name, tool.as_ref(), &args, key, value).await);
        }
    }
    if client.undrafted() {
        flips.push("the client was asked to mark a pull request ready".to_string());
    }
    if branch_is_on_origin(&fixture) {
        flips.push("the branch reached origin although its pull request was ready".to_string());
    }
    flips
}

#[tokio::test]
#[rustfmt::skip]
async fn in_solo_mode_no_pr_tool_can_un_draft_and_the_push_tool_stops_at_a_ready_pull_request() {
    assert_eq!(draft_flips(HumanGate::Solo, HumanGate::Solo).await, Vec::<String>::new());
}

#[tokio::test]
#[rustfmt::skip]
async fn the_enumeration_notices_a_promote_tool_or_push_tool_left_on_the_github_gate() {
    let promote = draft_flips(HumanGate::Github, HumanGate::Solo).await;
    assert!(promote.iter().any(|f| f.contains("promoted")), "{promote:?}");
    assert!(promote .iter() .any(|f| f.contains("mark a pull request ready")), "{promote:?}");
    let push = draft_flips(HumanGate::Solo, HumanGate::Github).await;
    assert!(push.iter() .any(|f| f.contains("pushed over a ready pull request")), "{push:?}");
    assert!(push.iter().any(|f| f.contains("reached origin")), "{push:?}");
}

#[tokio::test]
async fn in_github_mode_promote_and_push_behave_as_before() {
    let fixture = checkout();
    let client = Recording::new(vec![detail(7, false)]);
    let tools = pr_tools(
        fixture.work.path(),
        &client,
        HumanGate::Github,
        HumanGate::Github,
    );
    for (name, tool, args) in tools {
        let outcome = tool.execute(args).await;
        match name {
            "GithubPrPromoteTool" => assert_eq!(outcome.unwrap()["promoted"], json!(true)),
            "GitPushBranchTool" => assert_eq!(outcome.unwrap()["pushed"], json!(true)),
            _ => {}
        }
    }
    assert!(client.undrafted());
    assert!(!client
        .calls()
        .iter()
        .any(|c| c.starts_with("pull_requests_for_head")));
}

#[tokio::test]
#[rustfmt::skip]
async fn solo_mode_lets_the_legitimate_pr_tools_through() {
    let fixture = checkout();
    let client = Recording::new(vec![]);
    for (name, tool, args) in pr_tools(
        fixture.work.path(),
        &client,
        HumanGate::Solo,
        HumanGate::Solo,
    ) {
        let outcome: ToolResult<Value> = tool.execute(args).await;
        match name {
            "GithubPrPromoteTool" => assert!(outcome.is_err()),
            "GitPushBranchTool"
            | "GithubPrOpenTool"
            | "GithubPrCloseTool"
            | "GithubIssueCommentTool" => assert!(outcome.is_ok(), "{name}: {outcome:?}"),
            _ => {}
        }
    }
    let calls = client.calls();
    assert!(calls.contains(&"create_draft_pull_request feature-solo".to_string()), "{calls:?}");
    assert!(calls.contains(&"close_pull_request 7".to_string()), "{calls:?}");
    assert!(calls.contains(&"comment_on_issue 1".to_string()), "{calls:?}");
    assert!(branch_is_on_origin(&fixture));
}

struct ShellStub {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ShellStub {
    fn definition(&self) -> model::types::ToolDefinition {
        GitStatusTool::new(PathBuf::from(".")).definition()
    }

    async fn execute(&self, _: Value) -> ToolResult<Value> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ran": true }))
    }

    fn name(&self) -> &str {
        "run_command"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Workspace
    }
}

fn windows() -> Arc<WindowSet> {
    Arc::new(
        WindowSet::parse(
            "[[window]]\nname = \"w\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\"]\n",
        )
        .unwrap(),
    )
}

#[rustfmt::skip]
fn shell_subject() -> ActionSubject {
    ActionSubject { task_id: TaskId("solo".to_string()), max_effect: EffectClass::Repository, window: None, repo: "example/repo".to_string(), branch: None, pr: None, environment: None, paths: vec![], description: String::new() }
}

fn shell_registry(auditor: RuleActionAuditor, ran: &Arc<AtomicUsize>) -> ToolRegistry {
    let gate = Arc::new(ActionGate::new(
        Arc::new(auditor),
        ActionAuditLog::in_memory(),
    ));
    let subject = shell_subject();
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ShellStub {
        ran: Arc::clone(ran),
    }));
    registry.with_action_gate(gate, subject)
}

fn default_auditor() -> RuleActionAuditor {
    RuleActionAuditor::new(
        windows(),
        Arc::new(InMemoryLeaseStore::default()),
        chrono::Duration::minutes(10),
    )
}

const SHELL_FORMS: &[&str] = &[
    "gh pr ready 5",
    "gh pr ready 5 --repo o/r",
    "gh --repo o/r pr ready 5",
    "gh pr ready --undo 5",
    "gh pr merge 5",
    "gh pr merge 5 --squash --auto",
    "gh pr merge --admin 5",
    "gh pr edit 5 --title x",
    "gh pr edit 5 --base main",
    "gh pr review 5 --approve",
    "gh pr comment 5 --body x",
    "gh pr close 5",
    "gh pr reopen 5",
    "gh api graphql -f query=x",
    "gh api graphql -f query='mutation { markPullRequestReadyForReview(input: {pullRequestId: \"x\"}) { clientMutationId } }'",
    "gh api -X PATCH repos/o/r/pulls/5 -F draft=false",
    "gh api --method PATCH repos/o/r/pulls/5 -f draft=false",
    "gh api -XPATCH repos/o/r/pulls/5 -f state=open",
    "gh api repos/o/r/pulls/5 -X PUT",
    "gh api repos/o/r/pulls/5/merge -X PUT",
    "gh api repos/o/r/pulls/5 -f draft=false",
    "gh api repos/o/r/pulls/5 --input body.json",
    "gh api repos/o/r/pulls/5 -X DELETE",
    "gh api -X POST repos/o/r/pulls/5/requested_reviewers",
    "curl -X PATCH -H 'Authorization: Bearer x' -d '{\"draft\":false}' https://api.github.com/repos/o/r/pulls/5",
    "curl -s https://api.github.com/graphql -d '{\"query\":\"mutation{markPullRequestReadyForReview}\"}'",
    "curl -X PUT https://api.github.com/repos/o/r/pulls/5/merge",
    "wget --method=PATCH --body-data='{\"draft\":false}' https://api.github.com/repos/o/r/pulls/5",
    "bash -c 'gh pr ready 5'",
    "sh -c \"gh pr merge 5\"",
    "env GH_TOKEN=x gh pr ready 5",
    "time gh pr ready 5",
    "nohup gh pr ready 5",
    "timeout 5 gh pr ready 5",
    "xargs gh pr ready",
    "echo 5 | xargs gh pr ready",
    "eval 'gh pr ready 5'",
    "command gh pr ready 5",
    "/usr/bin/gh pr ready 5",
    "cd /tmp && gh pr ready 5",
    "true; gh pr ready 5",
    "(gh pr ready 5)",
    "G=gh; $G pr ready 5",
    "$(printf gh) pr ready 5",
    "git push",
    "git push origin HEAD",
    "git push --force origin HEAD:refs/heads/feature-solo",
    "git push origin HEAD:refs/heads/main",
    "git -C . push",
    "git push --receive-pack=curl origin x",
    "bash -c 'git push origin HEAD'",
];

async fn unrefused_shell_forms(auditor: RuleActionAuditor) -> Vec<String> {
    let ran = Arc::new(AtomicUsize::new(0));
    let registry = shell_registry(auditor, &ran);
    let mut passed = Vec::new();
    for form in SHELL_FORMS {
        let before = ran.load(Ordering::SeqCst);
        let outcome = registry
            .execute("run_command", json!({ "command": form }))
            .await;
        let refused = matches!(outcome, Err(ToolError::ActionDenied(_)));
        if !refused || ran.load(Ordering::SeqCst) != before {
            passed.push(form.to_string());
        }
    }
    passed
}

#[tokio::test]
#[rustfmt::skip]
async fn the_shell_cannot_un_draft_merge_or_push_by_any_enumerated_form() {
    assert_eq!(unrefused_shell_forms(default_auditor()).await, Vec::<String>::new());
}

fn accept_everything(_: &str, _: &Value) -> Result<(), String> {
    Ok(())
}

fn refuse_unless(args: &Value, needles: &[&str]) -> Result<(), String> {
    let command = args["command"].as_str().unwrap_or_default();
    if needles.iter().any(|needle| command.contains(needle)) {
        Ok(())
    } else {
        Err("refused".to_string())
    }
}

fn forget_gh_ready(_: &str, args: &Value) -> Result<(), String> {
    refuse_unless(args, &["ready"])
}

fn forget_merge_and_graphql(_: &str, args: &Value) -> Result<(), String> {
    refuse_unless(args, &["merge", "graphql"])
}

fn forget_push(_: &str, args: &Value) -> Result<(), String> {
    refuse_unless(args, &["push"])
}

fn forget_curl(_: &str, args: &Value) -> Result<(), String> {
    refuse_unless(args, &["curl", "wget"])
}

#[tokio::test]
#[rustfmt::skip]
async fn the_shell_enumeration_notices_an_inspector_that_forgets_a_form() {
    type Inspector = fn(&str, &Value) -> Result<(), String>;
    let mutants: [(Inspector, &str); 5] = [
        (accept_everything, "gh pr ready 5"),
        (forget_gh_ready, "gh pr ready 5"),
        (forget_merge_and_graphql, "gh pr merge 5"),
        (forget_push, "git push"),
        (
            forget_curl,
            "curl -X PUT https://api.github.com/repos/o/r/pulls/5/merge",
        ),
    ];
    for (inspector, expected) in mutants {
        let passed = unrefused_shell_forms(default_auditor().with_inspector(inspector)).await;
        assert!(passed.iter().any(|f| f == expected), "{expected}: {passed:?}");
    }
}

#[test]
#[rustfmt::skip]
fn a_tool_in_pr_tools_cannot_borrow_the_draft_literal_of_another_tool() {
    let files = real_files();
    let addition = "pub struct GithubPrReopenTool; impl Tool for GithubPrReopenTool { async fn execute(&self) { let body = json!({ \"draft\": false }); } }";
    let mutated = appended(&files, "src/pr_tools.rs", addition);
    let found = literal_violations(&mutated);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("GithubPrReopenTool::execute"), "{found:?}");
}

#[test]
fn an_empty_solo_check_still_satisfies_the_identifier_pin() {
    let files = real_files();
    let mutated = with_file(&files, "src/pr_tools.rs", |s| {
        replaced(
            s,
            "if self.gate == HumanGate::Solo {",
            "if self.gate == HumanGate::Solo {} if false {",
        )
    });
    assert!(undraft_violations(&mutated).is_empty());
}

#[tokio::test]
async fn the_recording_client_answers_every_method() {
    let client = Recording::new(vec![]);
    assert!(client
        .search_open_issues("o/r", "q")
        .await
        .unwrap()
        .is_empty());
    assert!(client.open_pull_requests("o/r").await.unwrap().is_empty());
    assert!(client.create_issue("o/r", "t", "b", &[]).await.is_err());
    assert!(client.get_issue("o/r", 1).await.is_err());
    assert!(client
        .list_review_comments("o/r", 1)
        .await
        .unwrap()
        .is_empty());
    assert!(client
        .list_issue_comments("o/r", 1)
        .await
        .unwrap()
        .is_empty());
    assert!(client
        .list_pull_request_reviews("o/r", 1)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(client.get_pull_request("o/r", 4).await.unwrap().number, 4);
    assert!(client.calls().is_empty());
}

struct AcceptsAnything;

#[async_trait]
impl Tool for AcceptsAnything {
    fn definition(&self) -> model::types::ToolDefinition {
        GitStatusTool::new(PathBuf::from(".")).definition()
    }

    async fn execute(&self, _: Value) -> ToolResult<Value> {
        Ok(json!({}))
    }

    fn name(&self) -> &str {
        "accepts_anything"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

#[tokio::test]
#[rustfmt::skip]
async fn the_enumeration_notices_a_tool_that_accepts_a_smuggled_argument() {
    let lenient = smuggle(
        "Lenient",
        &AcceptsAnything,
        &json!({}),
        "draft",
        json!(false),
    )
    .await;
    assert_eq!(lenient, vec!["Lenient accepted the extra argument `draft`".to_string()]);
    let fixture = checkout();
    let client = Recording::new(vec![]);
    let strict = pr_tools(
        fixture.work.path(),
        &client,
        HumanGate::Solo,
        HumanGate::Solo,
    );
    let (name, tool, args) = &strict[1];
    assert!(smuggle(name, tool.as_ref(), args, "merge", json!(true)) .await .is_empty());
}

#[tokio::test]
async fn the_shell_stub_and_the_lenient_stub_describe_themselves() {
    let ran = Arc::new(AtomicUsize::new(0));
    assert_eq!(ShellStub { ran }.definition().function.name, "git_status");
    assert_eq!(AcceptsAnything.definition().function.name, "git_status");
    assert_eq!(AcceptsAnything.name(), "accepts_anything");
    assert_eq!(AcceptsAnything.effect_class(), EffectClass::Repository);
}

#[test]
fn an_unclassified_or_doubled_un_drafting_method_is_reported() {
    let none: &[(&str, ClientEffect)] = &[("read", ClientEffect::Reads)];
    let two: &[(&str, ClientEffect)] = &[
        ("a", ClientEffect::UnDrafts),
        ("mark_pull_request_ready", ClientEffect::UnDrafts),
    ];
    let right: &[(&str, ClientEffect)] = &[("mark_pull_request_ready", ClientEffect::UnDrafts)];
    assert_eq!(undrafting_violations(&[none]).len(), 1);
    assert_eq!(undrafting_violations(&[right, two]).len(), 1);
    assert_eq!(undrafting_violations(&[right, none]).len(), 0);
}

#[test]
fn a_tool_on_a_type_without_a_name_and_odd_gh_arms_are_handled() {
    let files = real_files();
    let tuple = appended(&files, "src/ci_tools.rs", "impl Tool for (A, B) {}");
    assert_eq!(tool_violations(&tuple).len(), 1);
    let odd = "fn inspect_gh() { match (top, sub) { (x, \"view\") => f(), (\"pr\", 1) => g(), _ => h() } }";
    let found = gh_violations(odd);
    assert!(found.iter().any(|v| v.contains("no longer matched")));
}
