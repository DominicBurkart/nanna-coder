//! Fixture generation for SWE-bench adapter.
//!
//! This test is `#[ignore]`d in normal CI. Run it explicitly to regenerate
//! the checked-in `evals/cases/swebench-*/task.toml` fixtures from
//! `evals/datasets/swebench-verified-sample.jsonl`:
//!
//! ```sh
//! cargo test -p harness --test swebench_fixture_generation -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use harness::agent::eval_case::EvalCase;
use harness::eval::swebench::{adapt_to_eval_case, load_swebench_dataset, SWEBenchTask};

#[derive(serde::Serialize)]
struct TomlEvalCase<'a> {
    case: TomlCaseInfo<'a>,
    task: TomlTaskSpec<'a>,
    expected: TomlExpected<'a>,
    metadata: TomlMetadata<'a>,
}
#[derive(serde::Serialize)]
struct TomlCaseInfo<'a> {
    id: &'a str,
    name: &'a str,
    description: &'a str,
}
#[derive(serde::Serialize)]
struct TomlTaskSpec<'a> {
    prompt: &'a str,
    language: &'a str,
}
#[derive(serde::Serialize)]
struct TomlExpected<'a> {
    files_changed: &'a [String],
    build_must_pass: bool,
    tests_must_pass: bool,
    required_symbols: &'a [String],
}
#[derive(serde::Serialize)]
struct TomlMetadata<'a> {
    difficulty: &'a str,
    tags: &'a [String],
    timeout_secs: u64,
}

fn to_toml(case: &harness::agent::eval_case::EvalCase) -> String {
    let t = TomlEvalCase {
        case: TomlCaseInfo {
            id: &case.case.id,
            name: &case.case.name,
            description: &case.case.description,
        },
        task: TomlTaskSpec {
            prompt: &case.task.prompt,
            language: &case.task.language,
        },
        expected: TomlExpected {
            files_changed: &case.expected.files_changed,
            build_must_pass: case.expected.build_must_pass,
            tests_must_pass: case.expected.tests_must_pass,
            required_symbols: &case.expected.required_symbols,
        },
        metadata: TomlMetadata {
            difficulty: &case.metadata.difficulty,
            tags: &case.metadata.tags,
            timeout_secs: case.metadata.timeout_secs,
        },
    };
    toml::to_string_pretty(&t).unwrap()
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

#[test]
#[ignore = "regenerates vendored fixtures; run explicitly with --ignored"]
fn regenerate_swebench_verified_task_toml_fixtures() {
    let root = repo_root();
    let dataset = root.join("evals/datasets/swebench-verified-sample.jsonl");
    let tasks: Vec<SWEBenchTask> =
        load_swebench_dataset(&dataset).expect("load swebench sample dataset");
    assert!(!tasks.is_empty(), "sample dataset must not be empty");

    let cases_dir = root.join("evals/cases");
    std::fs::create_dir_all(&cases_dir).unwrap();

    for task in &tasks {
        let eval_case = adapt_to_eval_case(task);
        let case_dir = cases_dir.join(format!("swebench-{}", task.instance_id));
        std::fs::create_dir_all(&case_dir).unwrap();
        let task_toml = case_dir.join("task.toml");
        std::fs::write(&task_toml, to_toml(&eval_case)).unwrap();
        eprintln!("wrote {}", task_toml.display());

        // Sanity: the generated TOML must re-parse as an EvalCase.
        let content = std::fs::read_to_string(&task_toml).unwrap();
        EvalCase::from_toml_str(&content)
            .unwrap_or_else(|e| panic!("fixture {} is invalid: {}", task.instance_id, e));
    }
}

#[test]
fn vendored_dataset_parses_and_adapts() {
    // This one DOES run in CI — it verifies the checked-in JSONL stays valid
    // and the adapter is consistent with it, without touching the filesystem
    // outside `target/`.
    let root = repo_root();
    let dataset = root.join("evals/datasets/swebench-verified-sample.jsonl");
    if !dataset.is_file() {
        eprintln!("no vendored dataset at {}; skipping", dataset.display());
        return;
    }
    let tasks = load_swebench_dataset(&dataset).expect("dataset parses");
    assert_eq!(tasks.len(), 5, "expected 5 vendored instances");
    for task in &tasks {
        let case = adapt_to_eval_case(task);
        assert_eq!(case.task.language, "python");
        assert!(!case.case.id.is_empty());
        // Round-trip through TOML must succeed.
        let toml_str = to_toml(&case);
        EvalCase::from_toml_str(&toml_str)
            .unwrap_or_else(|e| panic!("{} failed to round-trip: {}", task.instance_id, e));
    }
}

#[test]
fn vendored_fixtures_stay_in_sync_with_dataset() {
    // If task.toml fixtures exist on disk, they must match what the adapter
    // would produce now. Keeps the checked-in fixtures honest.
    let root = repo_root();
    let dataset = root.join("evals/datasets/swebench-verified-sample.jsonl");
    let cases_dir = root.join("evals/cases");
    if !dataset.is_file() {
        return;
    }
    let tasks = load_swebench_dataset(&dataset).expect("dataset parses");
    for task in &tasks {
        let fixture = cases_dir
            .join(format!("swebench-{}", task.instance_id))
            .join("task.toml");
        if !fixture.is_file() {
            eprintln!("no fixture at {}; skipping", fixture.display());
            continue;
        }
        let adapted = adapt_to_eval_case(task);
        let expected = to_toml(&adapted);
        let actual = std::fs::read_to_string(&fixture).unwrap();
        assert_eq!(
            actual.trim(),
            expected.trim(),
            "fixture {} drifted from adapter output — regenerate with: \
             cargo test -p harness --test swebench_fixture_generation -- \
             --ignored --nocapture",
            fixture.display()
        );
    }
    // Ensure discovery finds every adapted case.
    let discovered = EvalCase::discover(Path::new(&cases_dir)).unwrap();
    let adapted_ids: std::collections::HashSet<String> = tasks
        .iter()
        .map(|t| format!("swebench-{}", t.instance_id))
        .collect();
    let discovered_ids: std::collections::HashSet<String> = discovered
        .iter()
        .filter_map(|(_, p)| p.file_name().and_then(|n| n.to_str()).map(String::from))
        .filter(|n| n.starts_with("swebench-"))
        .collect();
    for id in &adapted_ids {
        assert!(
            discovered_ids.contains(id),
            "EvalCase::discover missed {}",
            id
        );
    }
}
