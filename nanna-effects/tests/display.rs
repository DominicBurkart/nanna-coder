use nanna_effects::Touch;

#[test]
fn display_renders_file_line_module_and_asset() {
    let touch = Touch::new("db.users", "api::auth", "api/src/auth.rs", 12);
    assert_eq!(
        touch.to_string(),
        "api/src/auth.rs:12 (api::auth) touches db.users"
    );
    assert_eq!(
        format!("{touch}"),
        "api/src/auth.rs:12 (api::auth) touches db.users"
    );
}

#[test]
fn display_reflects_every_field() {
    let base = Touch::new("a", "m", "f.rs", 1);
    assert_eq!(base.to_string(), "f.rs:1 (m) touches a");
    assert_eq!(
        Touch::new("b", "m", "f.rs", 1).to_string(),
        "f.rs:1 (m) touches b"
    );
    assert_eq!(
        Touch::new("a", "n", "f.rs", 1).to_string(),
        "f.rs:1 (n) touches a"
    );
    assert_eq!(
        Touch::new("a", "m", "g.rs", 1).to_string(),
        "g.rs:1 (m) touches a"
    );
    assert_eq!(
        Touch::new("a", "m", "f.rs", 2).to_string(),
        "f.rs:2 (m) touches a"
    );
}

#[test]
fn display_handles_empty_fields_and_max_line() {
    let touch = Touch::new("", "", "", u32::MAX);
    assert_eq!(touch.to_string(), format!(":{} () touches ", u32::MAX));
}
