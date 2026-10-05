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

impl CiPolicy {
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
}
