use harness::auditor::eval::{default_catalog_dir, AuditorEvalCase};
use harness::auditor::VerdictKind;
use harness::identity::IdentityCatalog;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

fn heldout_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../evals/cases/auditor_heldout")
}

fn spawn_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../evals/cases/auditor_spawn")
}

fn load() -> Vec<AuditorEvalCase> {
    AuditorEvalCase::discover(&heldout_dir()).expect("held-out cases parse")
}

fn has_tag(case: &AuditorEvalCase, tag: &str) -> bool {
    case.metadata.tags.iter().any(|t| t == tag)
}

fn count(cases: &[AuditorEvalCase], tag: &str) -> usize {
    cases.iter().filter(|c| has_tag(c, tag)).count()
}

#[test]
fn heldout_set_has_at_least_forty_cases() {
    assert!(load().len() >= 40);
}

#[test]
fn heldout_set_is_balanced_across_categories() {
    let cases = load();
    assert!(count(&cases, "allow") >= 10, "allow");
    assert!(count(&cases, "block") >= 10, "block");
    assert!(count(&cases, "escalate") >= 10, "escalate");
    assert!(count(&cases, "adversarial") >= 15, "adversarial");
}

#[test]
fn every_case_has_exactly_one_primary_category() {
    for case in load() {
        let primaries = ["allow", "block", "escalate"]
            .iter()
            .filter(|t| has_tag(&case, t))
            .count();
        assert_eq!(
            primaries, 1,
            "{} needs exactly one verdict tag",
            case.case.id
        );
    }
}

#[test]
fn verdict_tag_matches_expected_verdict() {
    for case in load() {
        let want = match case.expected.verdict {
            VerdictKind::Allow => "allow",
            VerdictKind::Block => "block",
            VerdictKind::Escalate => "escalate",
        };
        assert!(
            has_tag(&case, want),
            "{} tag/verdict mismatch",
            case.case.id
        );
    }
}

#[test]
fn adversarial_cases_are_never_expected_to_allow() {
    for case in load().iter().filter(|c| has_tag(c, "adversarial")) {
        assert_ne!(
            case.expected.verdict,
            VerdictKind::Allow,
            "{}",
            case.case.id
        );
    }
}

#[test]
fn case_ids_are_unique_and_match_file_names() {
    let mut seen = HashSet::new();
    for case in load() {
        assert!(seen.insert(case.case.id.clone()), "dup {}", case.case.id);
        assert!(heldout_dir()
            .join(format!("{}.toml", case.case.id))
            .is_file());
    }
}

#[test]
fn heldout_ids_do_not_collide_with_shipped_cases() {
    let shipped: HashSet<String> = AuditorEvalCase::discover(&spawn_dir())
        .expect("shipped cases parse")
        .into_iter()
        .map(|c| c.case.id)
        .collect();
    for case in load() {
        assert!(!shipped.contains(&case.case.id), "{}", case.case.id);
    }
}

#[test]
fn heldout_subtasks_are_not_copies_of_shipped_subtasks() {
    let shipped: HashSet<String> = AuditorEvalCase::discover(&spawn_dir())
        .expect("shipped cases parse")
        .into_iter()
        .map(|c| c.request.to_request().subtask)
        .collect();
    for case in load() {
        assert!(
            !shipped.contains(&case.request.to_request().subtask),
            "{}",
            case.case.id
        );
    }
}

#[test]
fn heldout_catalog_loads_and_has_an_auditor() {
    let catalog = IdentityCatalog::load(heldout_dir().join("catalog")).expect("catalog loads");
    assert!(catalog.get("auditor").is_some());
}

#[test]
fn heldout_catalog_matches_the_shipped_identity_names() {
    let held = IdentityCatalog::load(heldout_dir().join("catalog")).expect("catalog loads");
    let shipped = IdentityCatalog::load(default_catalog_dir()).expect("shipped catalog loads");
    let mut a: Vec<_> = held.names().map(str::to_owned).collect();
    let mut b: Vec<_> = shipped.names().map(str::to_owned).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
}

#[test]
fn only_toml_case_files_and_catalog_live_in_the_directory() {
    for entry in fs::read_dir(heldout_dir()).expect("dir exists") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            assert_eq!(path.file_name().unwrap(), "catalog");
        } else {
            assert_eq!(path.extension().unwrap(), "toml", "{path:?}");
        }
    }
}
