use crate::effects::EffectClass;
use crate::scope::DenialReason;
use crate::tools::{Tool, ToolError, ToolResult};
use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema as ModelSchema, SchemaType, ToolDefinition};
use schemars::gen::SchemaSettings;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnknownTool,
    InvalidArgs,
    NotFound,
    OutOfScope,
    AmbiguousMatch,
    NoMatch,
    FileExists,
    Io,
    Timeout,
    Denied,
    ExecFailed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ToolOutcome {
    pub fn success(data: Value) -> Self {
        Self {
            ok: true,
            data,
            error_code: None,
            message: String::new(),
            hint: None,
        }
    }

    pub fn failure(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: Value::Null,
            error_code: Some(code),
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl From<&ToolError> for ToolOutcome {
    fn from(error: &ToolError) -> Self {
        let code = match error {
            ToolError::InvalidArguments { .. } | ToolError::Serialization(_) => {
                ErrorCode::InvalidArgs
            }
            ToolError::ExecutionFailed { .. } => ErrorCode::ExecFailed,
            ToolError::NotFound { .. } => ErrorCode::NotFound,
            ToolError::PathSecurityViolation { .. } => ErrorCode::OutOfScope,
            ToolError::ScopeDenied(denial) => match denial.reason {
                DenialReason::ToolNotInScope => ErrorCode::UnknownTool,
                DenialReason::PathOutsideScope { .. } => ErrorCode::OutOfScope,
                DenialReason::ProtectedPath { .. } => ErrorCode::Denied,
            },
            ToolError::Io(_) => ErrorCode::Io,
            ToolError::ProtectedPath(_)
            | ToolError::ProductionHeld { .. }
            | ToolError::ActionDenied(_)
            | ToolError::Budget(_) => ErrorCode::Denied,
        };
        Self::failure(code, error.to_string())
    }
}

#[async_trait]
pub trait TypedTool: Send + Sync {
    type Args: DeserializeOwned + JsonSchema + Send;

    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn effect_class(&self) -> EffectClass;
    fn parameters_schema(&self) -> Value {
        schema_for_args::<Self::Args>()
    }
    async fn run(&self, args: Self::Args) -> ToolOutcome;
}

pub struct Typed<T>(pub T);

pub fn schema_for_args<A: JsonSchema>() -> Value {
    let settings = SchemaSettings::draft07().with(|settings| {
        settings.inline_subschemas = true;
    });
    let root = settings.into_generator().into_root_schema_for::<A>();
    let mut schema = serde_json::to_value(root).unwrap_or_else(|_| json!({"type": "object"}));
    if let Some(object) = schema.as_object_mut() {
        object.remove("$schema");
        object.remove("title");
        object.remove("definitions");
        object.insert("type".to_string(), json!("object"));
        object.insert("additionalProperties".to_string(), json!(false));
    }
    schema
}

#[async_trait]
impl<T: TypedTool> Tool for Typed<T> {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: self.0.name().to_string(),
                description: self.0.description().to_string(),
                raw_parameters: Some(self.0.parameters_schema()),
                parameters: ModelSchema {
                    schema_type: SchemaType::Object,
                    properties: None,
                    required: None,
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let outcome = match serde_json::from_value::<T::Args>(args) {
            Ok(parsed) => self.0.run(parsed).await,
            Err(error) => ToolOutcome::failure(ErrorCode::InvalidArgs, error.to_string()),
        };
        Ok(serde_json::to_value(outcome)?)
    }

    fn name(&self) -> &str {
        self.0.name()
    }

    fn effect_class(&self) -> EffectClass {
        self.0.effect_class()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Diagnostic {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub severity: Severity,
    pub code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BuildState {
    NotRun,
    Pass,
    Fail { errors: Vec<Diagnostic> },
    TimedOut,
}

impl BuildState {
    pub fn is_passing(&self) -> bool {
        matches!(self, BuildState::Pass)
    }

    pub fn error_count(&self) -> usize {
        match self {
            BuildState::Fail { errors } => errors.len(),
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishClaim {
    pub summary: String,
    pub files_changed: Vec<String>,
}

pub type FinishSlot = std::sync::Arc<std::sync::Mutex<Option<FinishClaim>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditFeedback {
    pub text: String,
    pub build: BuildState,
    pub improved: bool,
}

#[async_trait]
pub trait EditObserver: Send + Sync {
    async fn after_edits(&self, touched: &[String]) -> Option<EditFeedback>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{DenialReason, ScopeDenial};
    use crate::tools::{Tool, ToolError};
    use async_trait::async_trait;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct EchoArgs {
        /// Text to echo back.
        text: String,
        times: Option<u32>,
    }

    struct EchoTyped;

    #[async_trait]
    impl TypedTool for EchoTyped {
        type Args = EchoArgs;

        fn name(&self) -> &str {
            "echo_typed"
        }

        fn description(&self) -> &str {
            "Echo text back."
        }

        fn effect_class(&self) -> EffectClass {
            EffectClass::None
        }

        async fn run(&self, args: EchoArgs) -> ToolOutcome {
            ToolOutcome::success(json!({"text": args.text, "times": args.times}))
        }
    }

    #[test]
    fn a_successful_outcome_serialises_ok_and_data_only() {
        let wire = serde_json::to_value(ToolOutcome::success(json!({"lines": 3}))).unwrap();

        assert_eq!(wire["ok"], true);
        assert_eq!(wire["data"]["lines"], 3);
        assert!(wire.get("error_code").is_none());
        assert!(wire.get("message").is_none());
        assert!(wire.get("hint").is_none());
    }

    #[test]
    fn a_failed_outcome_serialises_code_message_and_hint() {
        let outcome = ToolOutcome::failure(ErrorCode::NoMatch, "old text not found")
            .with_hint("call read_file and copy the text without line numbers");

        let wire = serde_json::to_value(outcome).unwrap();

        assert_eq!(wire["ok"], false);
        assert_eq!(wire["error_code"], "no_match");
        assert_eq!(wire["message"], "old text not found");
        assert_eq!(
            wire["hint"],
            "call read_file and copy the text without line numbers"
        );
        assert!(wire.get("data").is_none());
    }

    #[test]
    fn error_codes_use_snake_case_names() {
        let names: Vec<String> = [
            ErrorCode::UnknownTool,
            ErrorCode::InvalidArgs,
            ErrorCode::NotFound,
            ErrorCode::OutOfScope,
            ErrorCode::AmbiguousMatch,
            ErrorCode::NoMatch,
            ErrorCode::FileExists,
            ErrorCode::Io,
            ErrorCode::Timeout,
            ErrorCode::Denied,
            ErrorCode::ExecFailed,
        ]
        .iter()
        .map(|code| {
            serde_json::to_value(code)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();

        assert_eq!(
            names,
            [
                "unknown_tool",
                "invalid_args",
                "not_found",
                "out_of_scope",
                "ambiguous_match",
                "no_match",
                "file_exists",
                "io",
                "timeout",
                "denied",
                "exec_failed"
            ]
        );
    }

    #[test]
    fn a_scope_denial_for_a_tool_outside_the_scope_is_an_unknown_tool() {
        let denial = ScopeDenial {
            identity: "rust-implementer".to_string(),
            tool: "file_manager.write_file".to_string(),
            reason: DenialReason::ToolNotInScope,
        };

        let outcome = ToolOutcome::from(&ToolError::ScopeDenied(denial));

        assert!(!outcome.ok);
        assert_eq!(outcome.error_code, Some(ErrorCode::UnknownTool));
    }

    #[test]
    fn a_path_outside_the_scope_is_out_of_scope() {
        let denial = ScopeDenial {
            identity: "rust-implementer".to_string(),
            tool: "read_file".to_string(),
            reason: DenialReason::PathOutsideScope {
                access: crate::scope::PathAccess::Read,
                path: "test/main.rs".to_string(),
            },
        };

        let outcome = ToolOutcome::from(&ToolError::ScopeDenied(denial));

        assert_eq!(outcome.error_code, Some(ErrorCode::OutOfScope));
        assert!(outcome.message.contains("test/main.rs"));
    }

    #[test]
    fn invalid_arguments_and_missing_files_map_to_their_codes() {
        let invalid = ToolOutcome::from(&ToolError::InvalidArguments {
            message: "bad".to_string(),
        });
        let missing = ToolOutcome::from(&ToolError::NotFound {
            name: "x".to_string(),
        });
        let io = ToolOutcome::from(&ToolError::Io(std::io::Error::other("disk")));

        assert_eq!(invalid.error_code, Some(ErrorCode::InvalidArgs));
        assert_eq!(missing.error_code, Some(ErrorCode::NotFound));
        assert_eq!(io.error_code, Some(ErrorCode::Io));
    }

    #[test]
    fn a_typed_tool_schema_forbids_extra_properties_and_lists_required_fields() {
        let definition = Typed(EchoTyped).definition();

        let schema = definition.function.raw_parameters.expect("raw schema");

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["text"]));
        assert_eq!(schema["properties"]["text"]["type"], "string");
        assert!(schema.get("$schema").is_none());
        assert!(schema.get("title").is_none());
        assert_eq!(definition.function.name, "echo_typed");
        assert_eq!(definition.function.description, "Echo text back.");
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    struct DynamicArgs {}

    struct DynamicTool;

    #[async_trait]
    impl TypedTool for DynamicTool {
        type Args = DynamicArgs;

        fn name(&self) -> &str {
            "check"
        }

        fn description(&self) -> &str {
            "Run the stack check."
        }

        fn effect_class(&self) -> EffectClass {
            EffectClass::Workspace
        }

        fn parameters_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {"package": {"type": "string"}},
                "required": []
            })
        }

        async fn run(&self, _args: DynamicArgs) -> ToolOutcome {
            ToolOutcome::success(json!({}))
        }
    }

    #[test]
    fn a_typed_tool_can_supply_its_own_parameter_schema() {
        let definition = Typed(DynamicTool).definition();

        let schema = definition.function.raw_parameters.expect("raw schema");

        assert_eq!(schema["properties"]["package"]["type"], "string");
        assert_eq!(schema["additionalProperties"], false);
    }

    #[tokio::test]
    async fn a_typed_tool_runs_with_valid_arguments() {
        let result = Typed(EchoTyped)
            .execute(json!({"text": "hi", "times": 2}))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["data"]["text"], "hi");
        assert_eq!(result["data"]["times"], 2);
    }

    #[tokio::test]
    async fn an_unknown_argument_is_rejected_and_named() {
        let result = Typed(EchoTyped)
            .execute(json!({"text": "hi", "prepend": true}))
            .await
            .unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("prepend"));
    }

    #[tokio::test]
    async fn a_missing_required_argument_is_rejected_and_named() {
        let result = Typed(EchoTyped).execute(json!({})).await.unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("text"));
    }

    #[test]
    fn a_typed_tool_reports_its_name_and_effect_class() {
        let typed = Typed(EchoTyped);

        assert_eq!(Tool::name(&typed), "echo_typed");
        assert_eq!(typed.effect_class(), EffectClass::None);
    }

    #[test]
    fn a_finish_slot_starts_empty_and_holds_the_claim() {
        let slot = FinishSlot::default();
        assert!(slot.lock().unwrap().is_none());

        *slot.lock().unwrap() = Some(FinishClaim {
            summary: "added the attribute".to_string(),
            files_changed: vec!["src/main.rs".to_string()],
        });

        let claim = slot.lock().unwrap().clone().unwrap();
        assert_eq!(claim.files_changed, ["src/main.rs"]);
    }

    struct FixedObserver;

    #[async_trait]
    impl EditObserver for FixedObserver {
        async fn after_edits(&self, touched: &[String]) -> Option<EditFeedback> {
            Some(EditFeedback {
                text: format!("build: pass ({} file)", touched.len()),
                build: BuildState::Pass,
                improved: true,
            })
        }
    }

    #[tokio::test]
    async fn an_edit_observer_reports_text_state_and_whether_the_build_improved() {
        let observer: Box<dyn EditObserver> = Box::new(FixedObserver);

        let feedback = observer.after_edits(&["a.rs".to_string()]).await.unwrap();

        assert_eq!(feedback.text, "build: pass (1 file)");
        assert!(feedback.build.is_passing());
        assert!(feedback.improved);
    }

    #[test]
    fn a_diagnostic_serialises_with_its_location() {
        let diagnostic = Diagnostic {
            file: "src/main.rs".to_string(),
            line: 12,
            col: 5,
            severity: Severity::Error,
            code: Some("E0308".to_string()),
            message: "mismatched types".to_string(),
        };

        let wire = serde_json::to_value(&diagnostic).unwrap();

        assert_eq!(wire["file"], "src/main.rs");
        assert_eq!(wire["line"], 12);
        assert_eq!(wire["severity"], "error");
        assert_eq!(wire["code"], "E0308");
    }

    #[test]
    fn build_state_reports_pass_and_failure_counts() {
        let failing = BuildState::Fail {
            errors: vec![Diagnostic {
                file: "a.rs".to_string(),
                line: 1,
                col: 1,
                severity: Severity::Error,
                code: None,
                message: "boom".to_string(),
            }],
        };

        assert!(BuildState::Pass.is_passing());
        assert!(!failing.is_passing());
        assert_eq!(failing.error_count(), 1);
        assert_eq!(BuildState::Pass.error_count(), 0);
        assert_eq!(BuildState::NotRun.error_count(), 0);
        assert_eq!(BuildState::TimedOut.error_count(), 0);
    }
}
