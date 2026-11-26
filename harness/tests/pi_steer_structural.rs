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
        if name.ends_with("_tests.rs") {
            continue;
        }
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

fn text_of(name: &str) -> String {
    let all = production();
    let found = all.iter().find(|(file, _)| file == name);
    found.map(|(_, text)| text.clone()).unwrap()
}

fn function(text: &str, header: &str) -> String {
    let after = text.split(header).nth(1).unwrap();
    let end = after
        .find("\nfn ")
        .or_else(|| after.find("\nasync fn "))
        .unwrap_or(after.len());
    after[..end].to_string()
}

#[test]
fn one_pi_steer_doorbell_is_built_in_production_and_only_by_the_binary() {
    assert_eq!(sites("PiSteerDoorbell::new("), ["main.rs"]);
    let main = text_of("main.rs");
    assert_eq!(main.matches("let bell = pi_doorbell(pi_route);").count(), 1);
    let factory = function(&main, "fn pi_doorbell(");
    assert!(factory.contains("enabled.then("), "{factory}");
}

#[test]
fn the_production_hub_open_gets_the_pi_doorbell_for_the_pi_route_and_the_executor_shares_it() {
    let main = text_of("main.rs");
    let builder = function(&main, "async fn build_mcp_task_manager_with_budget(");
    assert_eq!(
        builder
            .matches("opening = opening.with_doorbell(bell);")
            .count(),
        1
    );
    assert_eq!(builder.matches("if let Some(bell) = bell {").count(), 1);
    let serve = main
        .split("let bell = pi_doorbell(pi_route);")
        .nth(1)
        .unwrap();
    let serve = serve.split("let telemetry").next().unwrap();
    assert_eq!(serve.matches("bell.clone(),").count(), 1);
    assert_eq!(
        serve.matches("pi_route_from_env(pi_route, bell)").count(),
        1
    );
    let executor = function(&main, "fn pi_executor(");
    assert_eq!(executor.matches("executor.with_doorbells(bell)").count(), 1);
    assert_eq!(sites(".with_doorbells("), ["main.rs"]);
}

#[test]
fn delegate_and_backlog_sync_are_given_no_doorbell() {
    let main = text_of("main.rs");
    assert_eq!(main.matches("pi_route_from_env(pi_route, None)").count(), 1);
    let delegate = main.split("async fn run_delegate(").nth(1).unwrap();
    assert!(delegate.contains("pi_route_from_env(pi_route, None)"));
    assert!(!delegate.contains("pi_doorbell("));
    assert_eq!(sites("pi_doorbell("), ["main.rs", "main.rs"]);
}

#[test]
fn the_steer_line_is_written_only_by_the_executor_and_only_from_the_doorbell_text() {
    assert_eq!(sites("\"steer\""), ["harness_adapter/executor.rs"]);
    let mut callers = sites("doorbell_text(");
    callers.retain(|name| name != "hub/doorbell.rs");
    assert_eq!(callers, ["harness_adapter/executor.rs"]);
    let executor = text_of("harness_adapter/executor.rs");
    let line = function(&executor, "fn steer_line(");
    assert!(line.contains("doorbell_text(count)"), "{line}");
    assert_eq!(executor.matches("send_lines(&mut stdin").count(), 2);
}

#[test]
fn the_steer_doorbell_and_its_channel_carry_a_count_and_never_text() {
    let steer = text_of("harness_adapter/steer.rs");
    for banned in [
        "String",
        "&str",
        "format!",
        "to_string",
        "Vec<u8>",
        "Box<dyn",
    ] {
        assert!(!steer.contains(banned), "{banned} in steer.rs");
    }
    assert_eq!(steer.matches("watch::Sender<usize>").count(), 1);
    assert_eq!(steer.matches("watch::Receiver<usize>").count(), 1);
    assert_eq!(steer.matches("slot.bell.send_replace(unread);").count(), 1);
    assert_eq!(steer.matches("send_replace(").count(), 1);
}

#[test]
fn the_steer_arm_runs_inside_the_one_drive_loop_and_is_coalesced_per_turn() {
    let executor = text_of("harness_adapter/executor.rs");
    let drive = executor.split("async fn drive(").nth(1).unwrap();
    for needed in [
        "coalescer.turn_start()",
        "coalescer.arm()",
        "coalescer.on_ring(count)",
        "next_steer(&mut steer)",
        "drop(steer);",
    ] {
        assert_eq!(drive.matches(needed).count(), 1, "{needed}");
    }
}
