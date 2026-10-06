use crate::deploy::{is_production_env, NON_PRODUCTION_ENVS};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    String,
    Boolean,
    Number,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSpec {
    #[serde(rename = "type")]
    pub kind: InputKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub allowed: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSpec {
    #[serde(default)]
    pub inputs: BTreeMap<String, InputSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CiPolicyError {
    #[error("workflow {0:?} is not allowlisted in the repository's .nanna/deploy.toml")]
    WorkflowNotAllowed(String),
    #[error("workflow {workflow:?} does not declare an input named {input:?}")]
    UnknownInput { workflow: String, input: String },
    #[error("workflow {workflow:?} requires input {input:?}")]
    MissingInput { workflow: String, input: String },
    #[error("{kind} name {name:?} must be a plain ASCII identifier (letters, digits, '_', '-', and inner '.')")]
    InvalidName { kind: &'static str, name: String },
    #[error("workflow {workflow:?} could reach a production environment: {detail}")]
    ProductionReachable { workflow: String, detail: String },
    #[error("input {input:?} of workflow {workflow:?} must be a {expected}")]
    WrongType {
        workflow: String,
        input: String,
        expected: &'static str,
    },
    #[error("input {input:?} of workflow {workflow:?} must be one of {allowed:?}")]
    ValueNotAllowed {
        workflow: String,
        input: String,
        allowed: Vec<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiPolicy {
    #[serde(default)]
    pub workflows: BTreeMap<String, WorkflowSpec>,
}

const ENVIRONMENT_INPUTS: &[&str] = &[
    "env",
    "environment",
    "target",
    "target_env",
    "target_environment",
    "stage",
    "deploy_env",
    "deployment_env",
    "deployment",
    "tier",
];

fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn implies_production(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("prod")
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| matches!(token, "prd" | "live"))
}

fn is_environment_input(name: &str) -> bool {
    ENVIRONMENT_INPUTS
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

impl CiPolicy {
    pub fn ensure_non_production(&self) -> Result<(), CiPolicyError> {
        for (workflow, spec) in &self.workflows {
            if !is_plain_name(workflow) {
                return Err(CiPolicyError::InvalidName {
                    kind: "workflow",
                    name: workflow.clone(),
                });
            }
            let reach = |detail: String| CiPolicyError::ProductionReachable {
                workflow: workflow.clone(),
                detail,
            };
            if implies_production(workflow) {
                return Err(reach("the workflow name implies production".to_string()));
            }
            for (input, input_spec) in &spec.inputs {
                if !is_plain_name(input) {
                    return Err(CiPolicyError::InvalidName {
                        kind: "input",
                        name: input.clone(),
                    });
                }
                if implies_production(input) {
                    return Err(reach(format!("input {input:?} implies production")));
                }
                if is_environment_input(input) {
                    if input_spec.kind != InputKind::String || input_spec.allowed.is_empty() {
                        return Err(reach(format!(
                            "environment input {input:?} must be a string listing its allowed values, each one of {NON_PRODUCTION_ENVS:?}"
                        )));
                    }
                    if let Some(bad) = input_spec
                        .allowed
                        .iter()
                        .find(|value| is_production_env(value))
                    {
                        return Err(reach(format!(
                            "environment input {input:?} allows {bad:?}, which is not one of {NON_PRODUCTION_ENVS:?}"
                        )));
                    }
                } else if input_spec.kind == InputKind::String && input_spec.allowed.is_empty() {
                    return Err(reach(format!(
                        "string input {input:?} must list its allowed values"
                    )));
                } else if let Some(bad) = input_spec
                    .allowed
                    .iter()
                    .find(|value| implies_production(value))
                {
                    return Err(reach(format!(
                        "input {input:?} allows {bad:?}, which implies production"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn deny_all() -> Self {
        Self::default()
    }

    pub fn allowing(workflows: impl IntoIterator<Item = (String, WorkflowSpec)>) -> Self {
        Self {
            workflows: workflows.into_iter().collect(),
        }
    }

    pub fn permits(&self, workflow: &str) -> bool {
        self.workflows.contains_key(workflow)
    }

    pub fn validate_dispatch(&self, workflow: &str, inputs: &Value) -> Result<(), CiPolicyError> {
        let spec = self
            .workflows
            .get(workflow)
            .ok_or_else(|| CiPolicyError::WorkflowNotAllowed(workflow.to_string()))?;
        let empty = serde_json::Map::new();
        let supplied = match inputs {
            Value::Object(map) => map,
            _ => &empty,
        };
        for name in supplied.keys() {
            if !spec.inputs.contains_key(name) {
                return Err(CiPolicyError::UnknownInput {
                    workflow: workflow.to_string(),
                    input: name.clone(),
                });
            }
        }
        for (name, input) in &spec.inputs {
            match supplied.get(name) {
                None if input.required => {
                    return Err(CiPolicyError::MissingInput {
                        workflow: workflow.to_string(),
                        input: name.clone(),
                    })
                }
                None => {}
                Some(value) => check_value(workflow, name, input, value)?,
            }
        }
        Ok(())
    }
}

fn check_value(
    workflow: &str,
    name: &str,
    spec: &InputSpec,
    value: &Value,
) -> Result<(), CiPolicyError> {
    let wrong = |expected: &'static str| CiPolicyError::WrongType {
        workflow: workflow.to_string(),
        input: name.to_string(),
        expected,
    };
    match spec.kind {
        InputKind::Boolean => value.as_bool().map(|_| ()).ok_or_else(|| wrong("boolean")),
        InputKind::Number => value.as_f64().map(|_| ()).ok_or_else(|| wrong("number")),
        InputKind::String => {
            let text = value.as_str().ok_or_else(|| wrong("string"))?;
            if spec.allowed.is_empty() || spec.allowed.iter().any(|a| a == text) {
                Ok(())
            } else {
                Err(CiPolicyError::ValueNotAllowed {
                    workflow: workflow.to_string(),
                    input: name.to_string(),
                    allowed: spec.allowed.clone(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> CiPolicy {
        toml::from_str(
            r#"
[workflows."ci.yml"]

[workflows."deploy-sandbox.yml".inputs.env]
type = "string"
required = true
allowed = ["sandbox", "preview"]

[workflows."deploy-sandbox.yml".inputs.verbose]
type = "boolean"

[workflows."deploy-sandbox.yml".inputs.shards]
type = "number"
"#,
        )
        .unwrap()
    }

    #[test]
    fn deny_all_permits_nothing() {
        let p = CiPolicy::deny_all();
        assert!(!p.permits("ci.yml"));
        assert!(matches!(
            p.validate_dispatch("ci.yml", &json!({})),
            Err(CiPolicyError::WorkflowNotAllowed(_))
        ));
    }

    #[test]
    fn allowlisted_workflow_without_inputs_validates() {
        let p = policy();
        assert!(p.permits("ci.yml"));
        assert!(p.validate_dispatch("ci.yml", &json!({})).is_ok());
        assert!(p.validate_dispatch("ci.yml", &Value::Null).is_ok());
    }

    #[test]
    fn non_allowlisted_workflow_is_rejected() {
        let err = policy()
            .validate_dispatch("release.yml", &json!({}))
            .unwrap_err();
        assert_eq!(err, CiPolicyError::WorkflowNotAllowed("release.yml".into()));
    }

    #[test]
    fn numeric_workflow_ids_are_not_implicitly_allowed() {
        assert!(!policy().permits("12345"));
    }

    #[test]
    fn undeclared_input_is_rejected() {
        let err = policy()
            .validate_dispatch("ci.yml", &json!({"extra": "x"}))
            .unwrap_err();
        assert!(matches!(err, CiPolicyError::UnknownInput { .. }));
    }

    #[test]
    fn required_input_must_be_supplied() {
        let err = policy()
            .validate_dispatch("deploy-sandbox.yml", &json!({}))
            .unwrap_err();
        assert!(matches!(err, CiPolicyError::MissingInput { .. }));
    }

    #[test]
    fn input_types_are_enforced() {
        let p = policy();
        for (inputs, expected) in [
            (json!({"env": 1}), "string"),
            (json!({"env": "sandbox", "verbose": "yes"}), "boolean"),
            (json!({"env": "sandbox", "shards": "4"}), "number"),
        ] {
            match p.validate_dispatch("deploy-sandbox.yml", &inputs) {
                Err(CiPolicyError::WrongType { expected: e, .. }) => assert_eq!(e, expected),
                other => panic!("expected WrongType, got {other:?}"),
            }
        }
    }

    #[test]
    fn string_inputs_are_restricted_to_the_allowed_values() {
        let p = policy();
        let err = p
            .validate_dispatch("deploy-sandbox.yml", &json!({"env": "production"}))
            .unwrap_err();
        assert!(matches!(err, CiPolicyError::ValueNotAllowed { .. }));
        assert!(p
            .validate_dispatch(
                "deploy-sandbox.yml",
                &json!({"env": "preview", "verbose": true, "shards": 4})
            )
            .is_ok());
    }

    #[test]
    fn unknown_toml_fields_are_refused() {
        assert!(toml::from_str::<CiPolicy>("[workflows.\"ci.yml\"]\nsecrets = true\n").is_err());
        assert!(toml::from_str::<CiPolicy>("extra = 1\n").is_err());
    }

    fn parsed(src: &str) -> CiPolicy {
        toml::from_str(src).unwrap()
    }

    fn env_policy(value: &str) -> CiPolicy {
        let mut p = CiPolicy::deny_all();
        p.workflows.insert(
            "deploy.yml".to_string(),
            WorkflowSpec {
                inputs: BTreeMap::from([(
                    "env".to_string(),
                    InputSpec {
                        kind: InputKind::String,
                        required: true,
                        allowed: vec![value.to_string()],
                    },
                )]),
            },
        );
        p
    }

    #[test]
    fn a_policy_limited_to_non_production_environments_loads() {
        assert_eq!(policy().ensure_non_production(), Ok(()));
        for env in NON_PRODUCTION_ENVS {
            assert_eq!(env_policy(env).ensure_non_production(), Ok(()), "{env}");
            assert_eq!(
                env_policy(&env.to_ascii_uppercase()).ensure_non_production(),
                Ok(()),
                "{env}"
            );
        }
    }

    #[test]
    fn an_environment_input_that_accepts_production_is_rejected() {
        for value in [
            "production",
            "Production",
            "PRODUCTION",
            "prod",
            "prd",
            "live",
        ] {
            let err = env_policy(value).ensure_non_production().unwrap_err();
            assert!(
                matches!(err, CiPolicyError::ProductionReachable { .. }),
                "{value}: {err:?}"
            );
        }
    }

    #[test]
    fn environment_values_must_match_the_allowlist_exactly() {
        for value in [
            " staging",
            "staging ",
            "staging.",
            ".staging",
            "./staging",
            "staging/",
            "staging/../production",
            "../staging",
            "staging\\",
            "stag\u{0131}ng",
            "\u{455}taging",
            "staging\u{200b}",
            "STAGING\u{212a}",
            "\u{ff53}taging",
            "%73taging",
            "staging%2f",
            "staging%00",
            "staging\n",
            "staging\0",
            "",
            "*",
            "pr0duction",
            "production\u{200b}",
            "pro\u{200b}duction",
            "\u{0440}roduction",
            "prod%75ction",
            "stage-prod",
        ] {
            let err = env_policy(value).ensure_non_production().unwrap_err();
            assert!(
                matches!(err, CiPolicyError::ProductionReachable { .. }),
                "{value:?}: {err:?}"
            );
        }
    }

    #[test]
    fn any_free_form_string_input_is_rejected_whatever_it_is_called() {
        for name in ["deploy_to", "region", "app_env", "cluster", "suite", "ref"] {
            let src = format!("[workflows.\"ci.yml\".inputs.{name}]\ntype = \"string\"\n");
            assert!(
                matches!(
                    parsed(&src).ensure_non_production(),
                    Err(CiPolicyError::ProductionReachable { .. })
                ),
                "{name}"
            );
        }
        let bounded = parsed(
            "[workflows.\"ci.yml\".inputs.suite]\ntype = \"string\"\nallowed = [\"smoke\", \"full\"]\n\n[workflows.\"ci.yml\".inputs.verbose]\ntype = \"boolean\"\n\n[workflows.\"ci.yml\".inputs.shards]\ntype = \"number\"\n",
        );
        assert_eq!(bounded.ensure_non_production(), Ok(()));
    }

    #[test]
    fn an_environment_input_must_enumerate_its_values() {
        let free_form = parsed("[workflows.\"deploy.yml\".inputs.env]\ntype = \"string\"\n");
        assert!(matches!(
            free_form.ensure_non_production(),
            Err(CiPolicyError::ProductionReachable { .. })
        ));
        let boolean = parsed("[workflows.\"deploy.yml\".inputs.env]\ntype = \"boolean\"\n");
        assert!(matches!(
            boolean.ensure_non_production(),
            Err(CiPolicyError::ProductionReachable { .. })
        ));
    }

    #[test]
    fn environment_input_names_are_recognised_ignoring_ascii_case() {
        for name in [
            "env",
            "ENV",
            "Environment",
            "target",
            "target_env",
            "stage",
            "deploy_env",
            "tier",
        ] {
            let src = format!(
                "[workflows.\"deploy.yml\".inputs.{name}]\ntype = \"string\"\nallowed = [\"production\"]\n"
            );
            assert!(
                matches!(
                    parsed(&src).ensure_non_production(),
                    Err(CiPolicyError::ProductionReachable { .. })
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn names_that_imply_production_are_rejected_for_workflows_and_inputs() {
        for src in [
            "[workflows.\"deploy-production.yml\"]\n",
            "[workflows.\"Prod.yml\"]\n",
            "[workflows.\"release-live.yml\"]\n",
            "[workflows.\"ci.yml\".inputs.to_prod]\ntype = \"boolean\"\n",
            "[workflows.\"ci.yml\".inputs.PRODUCTION]\ntype = \"boolean\"\n",
            "[workflows.\"ci.yml\".inputs.suite]\ntype = \"string\"\nallowed = [\"smoke\", \"Production\"]\n",
            "[workflows.\"ci.yml\".inputs.suite]\ntype = \"string\"\nallowed = [\"deploy-to-prod\"]\n",
        ] {
            assert!(
                matches!(
                    parsed(src).ensure_non_production(),
                    Err(CiPolicyError::ProductionReachable { .. })
                ),
                "{src}"
            );
        }
    }

    #[test]
    fn workflow_and_input_names_must_be_plain_ascii_identifiers() {
        for name in [
            "ci.yml ",
            " ci.yml",
            "ci.yml.",
            "./ci.yml",
            "../ci.yml",
            ".github/workflows/ci.yml",
            "sub/ci.yml",
            "sub\\ci.yml",
            "ci%2eyml",
            "c\u{456}.yml",
            "ci.yml\u{200b}",
            "",
            "..",
            ".",
        ] {
            let mut p = CiPolicy::deny_all();
            p.workflows
                .insert(name.to_string(), WorkflowSpec::default());
            assert!(
                matches!(
                    p.ensure_non_production(),
                    Err(CiPolicyError::InvalidName { .. })
                ),
                "{name:?}"
            );
        }
        let mut p = CiPolicy::deny_all();
        p.workflows.insert(
            "ci.yml".to_string(),
            WorkflowSpec {
                inputs: BTreeMap::from([(
                    "sh ard".to_string(),
                    InputSpec {
                        kind: InputKind::Number,
                        required: false,
                        allowed: vec![],
                    },
                )]),
            },
        );
        assert!(matches!(
            p.ensure_non_production(),
            Err(CiPolicyError::InvalidName { .. })
        ));
    }

    #[test]
    fn a_numeric_workflow_id_is_a_valid_name() {
        let mut p = CiPolicy::deny_all();
        p.workflows
            .insert("12345".to_string(), WorkflowSpec::default());
        assert_eq!(p.ensure_non_production(), Ok(()));
    }
}
