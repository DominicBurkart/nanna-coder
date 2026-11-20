use std::process::Command;

fn nanna(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nanna"))
        .args(args)
        .env_remove("NANNA_MODEL")
        .output()
        .unwrap()
}

#[test]
fn a_configured_model_is_printed_without_any_network_call() {
    let out = nanna(&["default-model", "--model", "qwen3:0.6b"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "qwen3:0.6b");
}

#[test]
fn nanna_model_is_the_fallback_for_the_configured_model() {
    let out = Command::new(env!("CARGO_BIN_EXE_nanna"))
        .args(["default-model"])
        .env("NANNA_MODEL", "from-env:1b")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "from-env:1b");
}
