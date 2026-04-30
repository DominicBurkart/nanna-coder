use harness::agent::eval_case::EvalCase;
use std::path::Path;

const CASES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../evals/cases");

#[test]
fn deserialize_happy_path_001() {
    let path = Path::new(CASES_DIR).join("happy-path-001/task.toml");
    let case = EvalCase::from_toml_file(&path).expect("should parse happy-path-001");
    assert_eq!(case.case.id, "happy-path-001");
    assert_eq!(case.case.name, "Add a function");
    assert!(!case.task.prompt.is_empty());
    assert!(case.expected.build_must_pass);
    assert!(case
        .expected
        .required_symbols
        .contains(&"greet".to_string()));
}

#[test]
fn deserialize_happy_path_002() {
    let path = Path::new(CASES_DIR).join("happy-path-002/task.toml");
    let case = EvalCase::from_toml_file(&path).expect("should parse happy-path-002");
    assert_eq!(case.case.id, "happy-path-002");
    assert!(case.expected.build_must_pass);
    assert_eq!(case.metadata.difficulty, "easy");
}

#[test]
fn deserialize_happy_path_003() {
    let path = Path::new(CASES_DIR).join("happy-path-003/task.toml");
    let case = EvalCase::from_toml_file(&path).expect("should parse happy-path-003");
    assert_eq!(case.case.id, "happy-path-003");
    assert!(case
        .expected
        .files_changed
        .contains(&"src/utils.rs".to_string()));
    assert!(case
        .expected
        .required_symbols
        .contains(&"utils".to_string()));
}

#[test]
fn discover_all_cases() {
    let cases = EvalCase::discover(Path::new(CASES_DIR)).expect("should discover cases");
    let happy: Vec<_> = cases
        .iter()
        .filter(|(c, _)| c.case.id.starts_with("happy-path-"))
        .map(|(c, _)| c.case.id.clone())
        .collect();
    assert_eq!(
        happy,
        vec![
            "happy-path-001".to_string(),
            "happy-path-002".to_string(),
            "happy-path-003".to_string(),
        ]
    );
}

#[test]
fn repo_directories_exist_for_rust_cases() {
    // Rust happy-path fixtures ship a real `repo/` subtree used by the runner.
    // SWE-bench fixtures gitignore `repo/` and materialize it on demand, so
    // they are excluded from this invariant.
    let cases = EvalCase::discover(Path::new(CASES_DIR)).expect("should discover cases");
    let rust_cases: Vec<_> = cases
        .iter()
        .filter(|(c, _)| c.task.language == "rust")
        .collect();
    assert!(!rust_cases.is_empty(), "expected at least one rust case");
    for (_, case_dir) in &rust_cases {
        let repo_dir = case_dir.join("repo");
        assert!(repo_dir.is_dir(), "repo dir should exist for {case_dir:?}");
        assert!(
            repo_dir.join("Cargo.toml").is_file(),
            "Cargo.toml should exist in {repo_dir:?}"
        );
    }
}

#[test]
fn from_toml_str_defaults() {
    let toml_str = r#"
[case]
id = "test-case"
name = "Test"
description = "A test"

[task]
prompt = "Do something"
"#;
    let case = EvalCase::from_toml_str(toml_str).expect("should parse");
    assert_eq!(case.case.id, "test-case");
    assert_eq!(case.task.language, "rust");
    assert!(case.expected.build_must_pass);
    assert_eq!(case.metadata.timeout_secs, 300);
    assert_eq!(case.metadata.difficulty, "medium");
}

#[test]
fn invalid_toml_returns_error() {
    let result = EvalCase::from_toml_str("not valid toml {{{{");
    assert!(result.is_err());
}

#[test]
fn missing_required_field_returns_error() {
    let toml_str = r#"
[case]
id = "incomplete"
name = "Incomplete"
description = "Missing task section"
"#;
    let result = EvalCase::from_toml_str(toml_str);
    assert!(result.is_err());
}
