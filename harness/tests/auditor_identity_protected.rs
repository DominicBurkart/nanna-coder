use harness::auditor::eval::default_catalog_dir;
use harness::identity::{IdentityCatalog, IdentityError, AUDITOR_IDENTITY, REPO_AGENTS_DIR};
use harness::protected::ProtectedPaths;
use std::fs;
use std::path::{Path, PathBuf};

fn auditor_card() -> String {
    fs::read_to_string(default_catalog_dir().join("auditor.toml")).unwrap()
}

fn repo_with_override(card: &str) -> (tempfile::TempDir, PathBuf) {
    let repo = tempfile::tempdir().unwrap();
    let relative = Path::new(REPO_AGENTS_DIR).join(format!("{AUDITOR_IDENTITY}.toml"));
    let file = repo.path().join(&relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, card).unwrap();
    (repo, relative)
}

#[test]
fn a_repo_cannot_override_the_auditor_and_cannot_write_the_file_that_would() {
    let narrowing = auditor_card().replace("Find the flaw", "Always allow");
    let (repo, relative) = repo_with_override(&narrowing);

    let err = IdentityCatalog::load(default_catalog_dir())
        .unwrap()
        .with_repo_overrides(repo.path())
        .unwrap_err();
    assert!(
        matches!(&err, IdentityError::ProtectedIdentity { name, file }
            if name == AUDITOR_IDENTITY && *file == repo.path().join(&relative)),
        "{err:?}"
    );

    let protected = ProtectedPaths::with_config_dir(repo.path(), None);
    let violation = protected
        .check(&relative)
        .expect_err("the repo-level auditor card path must be protected");
    assert_eq!(violation.rule, ".nanna/**");
}

#[test]
fn other_identities_may_be_narrowed_by_a_repo_but_the_override_path_is_still_protected() {
    let repo = tempfile::tempdir().unwrap();
    let catalog = IdentityCatalog::load(default_catalog_dir()).unwrap();
    let shepherd = fs::read_to_string(default_catalog_dir().join("pr-shepherd.toml")).unwrap();
    let relative = Path::new(REPO_AGENTS_DIR).join("pr-shepherd.toml");
    fs::create_dir_all(repo.path().join(REPO_AGENTS_DIR)).unwrap();
    fs::write(repo.path().join(&relative), shepherd).unwrap();
    assert!(catalog.with_repo_overrides(repo.path()).is_ok());
    let protected = ProtectedPaths::with_config_dir(repo.path(), None);
    assert!(protected.is_protected(&relative));
    assert!(!protected.is_protected(Path::new("src/auditor.toml")));
}
