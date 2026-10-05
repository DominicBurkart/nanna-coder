use super::template::{DeployTemplate, RiskClass};
use super::DeployError;
use std::path::{Path, PathBuf};

/// Starter `.nanna/deploy.toml` for a full-stack Rust repository under `class`.
///
/// The rollout section is the least cautious one the class accepts; the
/// target, health and rollback sections are placeholders to edit. The result
/// always parses.
///
/// ```
/// use harness::deploy::{starter_template, DeployTemplate, RiskClass, Strategy};
///
/// let template = DeployTemplate::parse(&starter_template(RiskClass::Core)).unwrap();
/// assert_eq!(template.rollout.strategy, Strategy::Gradual);
/// assert_eq!(template.rollout.steps.len(), 7);
/// ```
pub fn starter_template(class: RiskClass) -> String {
    let (strategy, steps, min_step_duration) = match class {
        RiskClass::Unused => ("instant", "[100]", "0m"),
        RiskClass::Internal => ("gradual", "[100]", "30m"),
        RiskClass::Edge => ("gradual", "[10, 50, 100]", "8h"),
        RiskClass::Core => ("gradual", "[1, 5, 10, 25, 50, 75, 100]", "1d"),
    };
    format!(
        "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\", \"staging\", \"production\"]\n\n[risk]\nclass = \"{class}\"\n\n[rollout]\nstrategy = \"{strategy}\"\nsteps = {steps}\nmin_step_duration = \"{min_step_duration}\"\nwindows = \"business-hours\"\n\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"30m\"\n\n[rollback]\nautomatic = true\non_breach = \"rollback\"\nretain_for = \"1d\"\n\n[shadow]\nenabled = false\nmirror_percent = 0\ncompare = [\"status\", \"latency\"]\nmax_divergence = 0.05\n"
    )
}

/// Write [`starter_template`] to `<repo>/.nanna/deploy.toml` and return that path.
///
/// Creates `.nanna/` when missing and refuses to overwrite an existing template.
///
/// ```
/// use harness::deploy::{init, DeployError, RiskClass};
///
/// let repo = tempfile::tempdir().unwrap();
/// let path = init(repo.path(), RiskClass::Edge).unwrap();
/// assert_eq!(path, repo.path().join(".nanna/deploy.toml"));
/// assert!(matches!(init(repo.path(), RiskClass::Core), Err(DeployError::AlreadyExists { .. })));
/// ```
pub fn init(repo: &Path, class: RiskClass) -> Result<PathBuf, DeployError> {
    let path = DeployTemplate::path_in(repo);
    if path.exists() {
        return Err(DeployError::AlreadyExists { path });
    }
    let dir = path.parent().expect("template path has a parent directory");
    std::fs::create_dir_all(dir).map_err(|source| io_error(dir, source))?;
    std::fs::write(&path, starter_template(class)).map_err(|source| io_error(&path, source))?;
    Ok(path)
}

fn io_error(path: &Path, source: std::io::Error) -> DeployError {
    DeployError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_starter_parses_and_plans_for_production() {
        for class in RiskClass::ALL {
            let template = DeployTemplate::parse(&starter_template(class)).unwrap();
            assert_eq!(template.highest_risk(), class);
            let plan = template.plan("production").unwrap();
            assert_eq!(plan.steps.last().unwrap().traffic_percent, 100);
        }
    }

    #[test]
    fn init_writes_the_starter_into_a_fresh_nanna_dir() {
        let repo = tempfile::tempdir().unwrap();
        let path = init(repo.path(), RiskClass::Core).unwrap();
        assert_eq!(path, repo.path().join(".nanna").join("deploy.toml"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            starter_template(RiskClass::Core)
        );
        assert!(DeployTemplate::load_from_repo(repo.path()).is_ok());
    }

    #[test]
    fn init_refuses_to_overwrite() {
        let repo = tempfile::tempdir().unwrap();
        let path = init(repo.path(), RiskClass::Edge).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let err = init(repo.path(), RiskClass::Core).unwrap_err();
        assert!(matches!(&err, DeployError::AlreadyExists { path: p } if *p == path));
        assert!(err.to_string().contains("refusing to overwrite"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn init_reports_unusable_config_dir() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join(".nanna"), "not a directory").unwrap();
        let err = init(repo.path(), RiskClass::Unused).unwrap_err();
        assert!(matches!(err, DeployError::Io { .. }));
        assert!(err.to_string().contains(".nanna"));
    }

    #[cfg(unix)]
    #[test]
    fn init_reports_unwritable_config_dir() {
        use std::os::unix::fs::PermissionsExt;
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".nanna");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = init(repo.path(), RiskClass::Unused);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = result.unwrap_err();
        assert!(matches!(err, DeployError::Io { .. }));
        assert!(err.to_string().contains("deploy.toml"));
    }
}
