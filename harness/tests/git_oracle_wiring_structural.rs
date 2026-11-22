use syn::visit::Visit;

#[derive(Default)]
struct Wiring {
    inside: bool,
    calls_git_command_with_process_names: bool,
    other_command_new: bool,
}

fn callee(call: &syn::ExprCall) -> Option<String> {
    match &*call.func {
        syn::Expr::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

impl<'ast> Visit<'ast> for Wiring {
    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let entered = node.sig.ident == "object_exists" && !self.inside;
        self.inside |= entered;
        syn::visit::visit_impl_item_fn(self, node);
        if entered {
            self.inside = false;
        }
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if self.inside {
            let name = callee(node).unwrap_or_default();
            let last_is_process_names = node.args.last().is_some_and(|arg| {
                matches!(arg, syn::Expr::Call(inner) if callee(inner).as_deref() == Some("process_git_env_names") && inner.args.is_empty())
            });
            if name == "git_command" && node.args.len() == 3 && last_is_process_names {
                self.calls_git_command_with_process_names = true;
            }
            if name == "new" {
                self.other_command_new = true;
            }
        }
        syn::visit::visit_expr_call(self, node);
    }
}

fn wiring_of(source: &str) -> Wiring {
    let file = syn::parse_file(source).expect("source parses");
    let mut wiring = Wiring::default();
    for item in &file.items {
        if let syn::Item::Impl(item_impl) = item {
            let is_system_git =
                matches!(&*item_impl.self_ty, syn::Type::Path(p) if p.path.is_ident("SystemGit"));
            if is_system_git {
                wiring.visit_item_impl(item_impl);
            }
        }
    }
    wiring
}

#[test]
fn system_git_builds_its_command_only_through_git_command_with_the_process_git_names() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/hub/oracle.rs");
    let wiring = wiring_of(&std::fs::read_to_string(path).expect("oracle.rs reads"));
    assert!(wiring.calls_git_command_with_process_names);
    assert!(!wiring.other_command_new);
}

#[test]
fn the_scan_rejects_a_call_without_the_process_names_or_a_second_command() {
    let missing = "impl GitRunner for SystemGit { fn object_exists(&self) -> bool { run(git_command(d, h, Vec::new())) } }";
    assert!(!wiring_of(missing).calls_git_command_with_process_names);
    let renamed = "impl GitRunner for SystemGit { fn object_exists(&self) -> bool { run(git_command(d, h, other())) } }";
    assert!(!wiring_of(renamed).calls_git_command_with_process_names);
    let bypass = "impl GitRunner for SystemGit { fn object_exists(&self) -> bool { run(Command::new(\"git\")) } }";
    assert!(wiring_of(bypass).other_command_new);
    let parenthesised = "impl GitRunner for SystemGit { fn object_exists(&self) -> bool { run((git_command)(d, h, process_git_env_names())) } }";
    assert!(!wiring_of(parenthesised).calls_git_command_with_process_names);
    let good = "impl GitRunner for SystemGit { fn object_exists(&self) -> bool { run(git_command(d, h, process_git_env_names())) } }";
    let wiring = wiring_of(good);
    assert!(wiring.calls_git_command_with_process_names && !wiring.other_command_new);
}
