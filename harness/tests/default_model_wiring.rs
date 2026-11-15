const MAIN: &str = include_str!("../src/main.rs");

fn function_body(name: &str) -> &'static str {
    let marker = format!("async fn {name}(");
    let start = MAIN
        .find(&marker)
        .unwrap_or_else(|| panic!("{name} not found"));
    let rest = &MAIN[start..];
    let end = rest[1..]
        .find("\nasync fn ")
        .into_iter()
        .chain(rest[1..].find("\nfn "))
        .chain(rest[1..].find("\n#[cfg(test)]"))
        .min()
        .map_or(rest.len(), |at| at + 1);
    &rest[..end]
}

fn call_arguments<'a>(body: &'a str, callee: &str) -> &'a str {
    let marker = format!("{callee}(");
    let start = body
        .find(&marker)
        .unwrap_or_else(|| panic!("{callee} is not called"))
        + marker.len();
    let mut depth = 1usize;
    for (offset, ch) in body[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &body[start..start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced call to {callee}");
}

#[test]
fn mcp_serve_resolves_the_default_model_and_gives_the_auditors_that_binding() {
    let body = function_body("run_mcp_server");
    assert!(
        body.contains("startup_auditor_model("),
        "startup resolution is not called"
    );
    let manager = call_arguments(body, "build_mcp_task_manager");
    assert!(manager.contains("&auditor_name"), "{manager}");
    let gate = call_arguments(body, "serve_spawn_gate");
    assert!(gate.contains("&auditor_name"), "{gate}");
    assert!(!manager.contains("&model") && !gate.contains("&model") && !gate.contains("(model"));
}

#[test]
fn delegate_resolves_the_default_model_and_gives_the_auditors_that_binding() {
    let body = function_body("run_delegate");
    assert!(
        body.contains("startup_auditor_model("),
        "startup resolution is not called"
    );
    assert!(call_arguments(body, "build_delegate_task_manager").contains("auditor_name"));
    assert!(call_arguments(body, "delegate_spawn_gate").contains("auditor_name"));
}

#[test]
fn startup_resolution_uses_the_default_registry_and_the_real_provider() {
    for name in ["run_mcp_server", "run_delegate"] {
        let args = call_arguments(function_body(name), "startup_auditor_model");
        assert!(args.contains("DEFAULT_REGISTRY_URL"), "{name}: {args}");
        assert!(args.contains("provider.as_ref()"), "{name}: {args}");
        assert!(args.contains("configured_model("), "{name}: {args}");
    }
}

#[test]
fn backlog_sync_audits_through_its_helper() {
    assert!(MAIN.contains("backlog_audit_model(audit_model, &model)"));
}

#[test]
fn no_startup_path_resolves_the_default_model_before_the_audit_model_is_known() {
    let helper = function_body("startup_auditor_model_with");
    let explicit = helper
        .find("explicit_name(explicit)")
        .expect("explicit check");
    let resolve = helper.find("startup_model(").expect("startup_model call");
    assert!(explicit < resolve);
}

#[test]
fn the_device_probe_runs_off_the_async_executor() {
    let body = function_body("startup_model");
    let probe = body
        .find("spawn_blocking(probe)")
        .expect("probe on the blocking pool");
    assert!(probe < body.find("resolve_default_model(").expect("resolution"));
}
