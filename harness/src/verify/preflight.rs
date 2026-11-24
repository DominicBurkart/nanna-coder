use std::path::Path;

use super::config::VerifyConfig;
use super::status::{ceiling_violation, ERROR_CHECKS_INVALID, ERROR_ITERATION_BUDGET, ERROR_SCOPE};
use crate::identity::AgentIdentity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub label: &'static str,
    pub message: String,
}

#[doc = r##"Everything a verifier task must satisfy before any worktree or container
exists: the card's ceiling is exactly `workspace` (a higher one would turn
networking on for the dev container), `.nanna/verify.toml` in `repo` loads,
and each declared check fits in `max_iterations`, because every check is one
tool call and the iteration budget bounds tool calls. Anything else is a
[`Refusal`] carrying the task's `error_type` label.

```
use harness::identity::AgentIdentity;
use harness::verify::preflight;

let card = r#"
[identity]
name = "verifier"
description = "Runs the declared checks."
loop = "inner"
model = "m"
system_prompt = { inline = "Verify." }

[scope]
repos = ["github.com/example/repo"]
paths = []
max_effect = "workspace"
tools = ["run_checks"]

[limits]
max_iterations = 1
max_wall_clock_secs = 3600
max_concurrent = 1
"#;
let identity = AgentIdentity::from_toml_str(card, "v.toml").unwrap();
let refusal = preflight(&identity, std::path::Path::new("/nonexistent"), 10).unwrap_err();
assert_eq!(refusal.label, "VerifierChecksInvalid");
```"##]
pub fn preflight(
    identity: &AgentIdentity,
    repo: &Path,
    max_iterations: usize,
) -> Result<VerifyConfig, Refusal> {
    if let Some(message) = ceiling_violation(identity) {
        return Err(Refusal {
            label: ERROR_SCOPE,
            message,
        });
    }
    let config = VerifyConfig::load(repo).map_err(|e| Refusal {
        label: ERROR_CHECKS_INVALID,
        message: e.to_string(),
    })?;
    let declared = config.checks().len();
    if declared > max_iterations {
        return Err(Refusal {
            label: ERROR_ITERATION_BUDGET,
            message: format!(
                "`{}` declares {declared} checks but the task allows {max_iterations} iterations, and each check is one tool call",
                super::VERIFY_FILE
            ),
        });
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectClass;

    fn verifier() -> AgentIdentity {
        let mut identity = crate::identity::example();
        identity.scope.max_effect = EffectClass::Workspace;
        identity.scope.paths = Vec::new();
        identity.scope.tools = vec!["run_checks".parse().unwrap()];
        identity
    }

    fn repo_declaring(checks: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
        let text: String = (0..checks)
            .map(|i| format!("[[check]]\nname = \"c{i}\"\ncommand = \"true\"\n"))
            .collect();
        std::fs::write(dir.path().join(super::super::VERIFY_FILE), text).unwrap();
        dir
    }

    #[test]
    fn a_verifier_within_budget_gets_its_checks() {
        let repo = repo_declaring(2);
        let config = preflight(&verifier(), repo.path(), 2).unwrap();
        assert_eq!(config.checks().len(), 2);
    }

    #[test]
    fn the_iteration_budget_is_exact_at_the_boundary() {
        let repo = repo_declaring(3);
        assert!(preflight(&verifier(), repo.path(), 3).is_ok());
        let refusal = preflight(&verifier(), repo.path(), 2).unwrap_err();
        assert_eq!(refusal.label, ERROR_ITERATION_BUDGET);
        assert!(refusal.message.contains("3 checks"), "{}", refusal.message);
        assert!(
            refusal.message.contains("2 iterations"),
            "{}",
            refusal.message
        );
        assert_eq!(
            preflight(&verifier(), repo.path(), 0).unwrap_err().label,
            ERROR_ITERATION_BUDGET
        );
    }

    #[test]
    fn a_missing_declaration_is_refused_with_the_declaration_label() {
        let empty = tempfile::tempdir().unwrap();
        let refusal = preflight(&verifier(), empty.path(), 10).unwrap_err();
        assert_eq!(refusal.label, ERROR_CHECKS_INVALID);
        assert!(
            refusal.message.contains("verify.toml"),
            "{}",
            refusal.message
        );
    }

    #[test]
    fn every_higher_ceiling_is_refused_before_the_repository_is_even_read() {
        let empty = tempfile::tempdir().unwrap();
        for effect in EffectClass::ALL
            .into_iter()
            .filter(|e| *e != EffectClass::Workspace)
        {
            let mut identity = verifier();
            identity.scope.max_effect = effect;
            let refusal = preflight(&identity, empty.path(), 10).unwrap_err();
            assert_eq!(refusal.label, ERROR_SCOPE, "{effect}");
            assert!(refusal.message.contains(&format!("`{effect}`")), "{effect}");
        }
    }
}
