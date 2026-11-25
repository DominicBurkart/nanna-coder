use crate::diagnostics::{new_diagnostics, parse};
use crate::effects::EffectClass;
use crate::exec_sandbox::{ExecRequest, Outcome, SandboxedExec};
use crate::plugins::types::{DiagnosticsParser, ParamKind, ParamSpec, Stack, VerbSpec};
use crate::sandbox_lease::SandboxLease;
use crate::tool_contract::{
    BuildState, Diagnostic, EditFeedback, EditObserver, ErrorCode, Severity, ToolOutcome, TypedTool,
};
use async_trait::async_trait;
use schemars::gen::SchemaGenerator;
use schemars::schema::{Schema, SchemaObject};
use schemars::JsonSchema;
use serde::de::{Deserialize, Deserializer};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_AUTO_CHECK_SECS: u64 = 300;
const TAIL_BYTES: usize = 8 * 1024;
const FEEDBACK_LINES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamValue {
    Text(String),
    List(Vec<String>),
}

impl From<&str> for ParamValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<String> for ParamValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Vec<String>> for ParamValue {
    fn from(value: Vec<String>) -> Self {
        Self::List(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("the verb has an empty argv")]
    EmptyArgv,
    #[error("argv refers to undeclared parameter '{0}'")]
    UnknownPlaceholder(String),
    #[error("required parameter '{0}' is missing")]
    MissingParam(String),
    #[error("parameter '{0}' is not declared by this verb")]
    UnknownParam(String),
    #[error("'{param}' must not start with '-': {value}")]
    OptionLike { param: String, value: String },
    #[error("'{param}' must be one of the allowed values, got: {value}")]
    NotInEnum { param: String, value: String },
    #[error("parameter '{0}' does not accept a list here")]
    ListNotAllowed(String),
    #[error("parameter '{0}' contains a NUL byte")]
    InvalidValue(String),
}

enum Piece<'a> {
    Literal(&'a str),
    Param(&'a str),
}

fn pieces(element: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = element;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let name = after[..end].trim();
        if name.is_empty() {
            out.push(Piece::Literal(&rest[..start + 2 + end + 2]));
        } else {
            if start > 0 {
                out.push(Piece::Literal(&rest[..start]));
            }
            out.push(Piece::Param(name));
        }
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        out.push(Piece::Literal(rest));
    }
    out
}

fn present(value: Option<&ParamValue>) -> Option<&ParamValue> {
    match value {
        Some(ParamValue::Text(text)) if text.is_empty() => None,
        Some(ParamValue::List(items)) if items.is_empty() => None,
        other => other,
    }
}

fn check_value(name: &str, spec: &ParamSpec, value: &ParamValue) -> Result<(), RenderError> {
    let items: Vec<&String> = match value {
        ParamValue::Text(text) => vec![text],
        ParamValue::List(items) => {
            if spec.kind != ParamKind::PathList {
                return Err(RenderError::ListNotAllowed(name.to_string()));
            }
            items.iter().collect()
        }
    };
    for item in items {
        if item.contains('\0') {
            return Err(RenderError::InvalidValue(name.to_string()));
        }
        if spec.kind == ParamKind::Enum {
            if !spec.values.contains(item) {
                return Err(RenderError::NotInEnum {
                    param: name.to_string(),
                    value: item.clone(),
                });
            }
        } else if item.starts_with('-') {
            return Err(RenderError::OptionLike {
                param: name.to_string(),
                value: item.clone(),
            });
        }
    }
    Ok(())
}

pub fn render_argv(
    verb: &VerbSpec,
    params: &BTreeMap<String, ParamValue>,
) -> Result<Vec<String>, RenderError> {
    if verb.argv.is_empty() {
        return Err(RenderError::EmptyArgv);
    }
    for name in params.keys() {
        if !verb.params.contains_key(name) {
            return Err(RenderError::UnknownParam(name.clone()));
        }
    }
    for (name, spec) in &verb.params {
        match present(params.get(name)) {
            Some(value) => check_value(name, spec, value)?,
            None if spec.required => return Err(RenderError::MissingParam(name.clone())),
            None => {}
        }
    }
    let mut argv = Vec::new();
    for element in &verb.argv {
        let parts = pieces(element);
        for part in &parts {
            if let Piece::Param(name) = part {
                if !verb.params.contains_key(*name) {
                    return Err(RenderError::UnknownPlaceholder(name.to_string()));
                }
            }
        }
        if let [Piece::Param(name)] = parts.as_slice() {
            match present(params.get(*name)) {
                Some(ParamValue::Text(text)) => argv.push(text.clone()),
                Some(ParamValue::List(items)) => argv.extend(items.iter().cloned()),
                None => {}
            }
            continue;
        }
        let mut rendered = String::new();
        let mut dropped = false;
        for part in &parts {
            match part {
                Piece::Literal(text) => rendered.push_str(text),
                Piece::Param(name) => match present(params.get(*name)) {
                    Some(ParamValue::Text(text)) => rendered.push_str(text),
                    Some(ParamValue::List(_)) => {
                        return Err(RenderError::ListNotAllowed(name.to_string()))
                    }
                    None => dropped = true,
                },
            }
        }
        if !dropped {
            argv.push(rendered);
        }
    }
    Ok(argv)
}

pub fn params_from_args(args: &Map<String, Value>) -> Result<BTreeMap<String, ParamValue>, String> {
    let mut out = BTreeMap::new();
    for (name, value) in args {
        let converted = match value {
            Value::Null => continue,
            Value::String(text) => ParamValue::Text(text.clone()),
            Value::Number(number) => ParamValue::Text(number.to_string()),
            Value::Bool(flag) => ParamValue::Text(flag.to_string()),
            Value::Array(items) => ParamValue::List(
                items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| format!("'{name}' must be a list of strings"))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(_) => return Err(format!("'{name}' must be a string or list")),
        };
        out.insert(name.clone(), converted);
    }
    Ok(out)
}

pub fn verb_schema(verb: &VerbSpec) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (name, spec) in &verb.params {
        let mut schema = match spec.kind {
            ParamKind::PathList => json!({"type": "array", "items": {"type": "string"}}),
            _ => json!({"type": "string"}),
        };
        if spec.kind == ParamKind::Enum {
            schema["enum"] = json!(spec.values);
        }
        if !spec.description.is_empty() {
            schema["description"] = json!(spec.description);
        }
        properties.insert(name.clone(), schema);
        if spec.required {
            required.push(name.clone());
        }
    }
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

#[derive(Debug, Clone, Default)]
pub struct VerbArgs(pub Map<String, Value>);

impl<'de> Deserialize<'de> for VerbArgs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Map::deserialize(deserializer).map(VerbArgs)
    }
}

impl JsonSchema for VerbArgs {
    fn schema_name() -> String {
        "VerbArgs".to_string()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        Schema::Object(SchemaObject::default())
    }
}

#[derive(Clone)]
pub enum ExecTarget {
    Ephemeral(Arc<SandboxedExec>),
    Leased(Arc<SandboxLease>),
}

impl ExecTarget {
    fn run(&self, request: &ExecRequest) -> std::io::Result<Outcome> {
        match self {
            Self::Ephemeral(exec) => exec.run(request),
            Self::Leased(lease) => lease.run(request),
        }
    }
}

fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

fn error_diagnostics(verb: &VerbSpec, outcome: &Outcome) -> Vec<Diagnostic> {
    let output = match verb.diagnostics {
        DiagnosticsParser::Regex => format!("{}\n{}", outcome.stdout, outcome.stderr),
        _ => outcome.stdout.clone(),
    };
    parse(
        verb.diagnostics,
        &output,
        verb.diagnostics_pattern.as_deref(),
    )
}

pub struct VerbTool {
    name: String,
    description: String,
    verb: VerbSpec,
    target: ExecTarget,
    env: Vec<(String, String)>,
}

impl VerbTool {
    pub fn new(
        name: impl Into<String>,
        verb: VerbSpec,
        target: ExecTarget,
        env: Vec<(String, String)>,
    ) -> Self {
        let name = name.into();
        let description = format!(
            "Run the {name} verb in the sandbox: {}",
            verb.argv.join(" ")
        );
        Self {
            name,
            description,
            verb,
            target,
            env,
        }
    }

    pub fn args_schema(&self) -> Value {
        verb_schema(&self.verb)
    }

    pub fn verb(&self) -> &VerbSpec {
        &self.verb
    }
}

#[async_trait]
impl TypedTool for VerbTool {
    type Args = VerbArgs;

    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn effect_class(&self) -> EffectClass {
        self.verb.effect
    }

    async fn run(&self, args: VerbArgs) -> ToolOutcome {
        let params = match params_from_args(&args.0) {
            Ok(params) => params,
            Err(message) => return ToolOutcome::failure(ErrorCode::InvalidArgs, message),
        };
        let argv = match render_argv(&self.verb, &params) {
            Ok(argv) => argv,
            Err(error) => return ToolOutcome::failure(ErrorCode::InvalidArgs, error.to_string()),
        };
        let request = ExecRequest {
            argv,
            network: self.verb.network,
            timeout: Some(Duration::from_secs(self.verb.timeout_secs)),
            env: self.env.clone(),
        };
        let target = self.target.clone();
        let outcome = match tokio::task::spawn_blocking(move || target.run(&request)).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => {
                return ToolOutcome::failure(ErrorCode::ExecFailed, error.to_string())
            }
            Err(error) => return ToolOutcome::failure(ErrorCode::ExecFailed, error.to_string()),
        };
        let diagnostics = error_diagnostics(&self.verb, &outcome);
        let data = json!({
            "success": outcome.success,
            "timed_out": outcome.timed_out,
            "stdout_tail": tail(&outcome.stdout, TAIL_BYTES),
            "stderr_tail": tail(&outcome.stderr, TAIL_BYTES),
            "diagnostics": diagnostics,
        });
        if outcome.timed_out {
            return ToolOutcome {
                ok: false,
                data,
                error_code: Some(ErrorCode::Timeout),
                message: format!("{} timed out after {}s", self.name, self.verb.timeout_secs),
                hint: None,
            };
        }
        ToolOutcome::success(data)
    }
}

struct CheckSpec {
    argv: Vec<String>,
    verb: VerbSpec,
    env: Vec<(String, String)>,
    cap: Duration,
}

fn run_check(lease: &SandboxLease, spec: &CheckSpec) -> (BuildState, Option<String>) {
    let request = ExecRequest {
        argv: spec.argv.clone(),
        network: spec.verb.network,
        timeout: Some(spec.cap),
        env: spec.env.clone(),
    };
    let outcome = match lease.run(&request) {
        Ok(outcome) => outcome,
        Err(error) => return (BuildState::NotRun, Some(error.to_string())),
    };
    if outcome.timed_out {
        return (BuildState::TimedOut, None);
    }
    if outcome.success {
        return (BuildState::Pass, None);
    }
    let mut errors: Vec<Diagnostic> = error_diagnostics(&spec.verb, &outcome)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    if errors.is_empty() {
        let message = outcome
            .stderr
            .lines()
            .chain(outcome.stdout.lines())
            .find(|line| !line.trim().is_empty())
            .unwrap_or("check failed")
            .trim()
            .to_string();
        errors.push(Diagnostic {
            file: String::new(),
            line: 0,
            col: 0,
            severity: Severity::Error,
            code: None,
            message,
        });
    }
    (BuildState::Fail { errors }, None)
}

fn describe(d: &Diagnostic) -> String {
    if d.file.is_empty() {
        d.message.clone()
    } else {
        format!("{}:{} {}", d.file, d.line, d.message)
    }
}

pub fn feedback_for(
    previous: &BuildState,
    current: &BuildState,
    unrunnable: Option<&str>,
    cap: Duration,
) -> EditFeedback {
    let errors_of = |state: &BuildState| match state {
        BuildState::Fail { errors } => errors.clone(),
        _ => Vec::new(),
    };
    let (header, shown, improved) = match current {
        BuildState::NotRun => (
            format!(
                "build: check could not run ({})",
                unrunnable.unwrap_or("unknown reason")
            ),
            Vec::new(),
            false,
        ),
        BuildState::TimedOut => (
            format!("build: check timed out ({}s)", cap.as_secs()),
            Vec::new(),
            false,
        ),
        BuildState::Pass => match previous {
            BuildState::Fail { .. } => ("build: fail->pass".to_string(), Vec::new(), true),
            _ => ("build: pass".to_string(), Vec::new(), false),
        },
        BuildState::Fail { errors } => {
            let before = errors_of(previous);
            let fresh = new_diagnostics(&before, errors);
            let count = errors.len();
            let plural = if count == 1 { "error" } else { "errors" };
            let (header, improved) = match previous {
                BuildState::Pass => (
                    format!("build: pass->fail ({count} {plural}, {} new)", fresh.len()),
                    false,
                ),
                BuildState::Fail { errors: old } => {
                    let tail = if fresh.is_empty() {
                        String::new()
                    } else {
                        format!(", {} new", fresh.len())
                    };
                    (
                        format!("build: still failing ({count} {plural}{tail})"),
                        count < old.len(),
                    )
                }
                _ => (format!("build: fail ({count} {plural})"), false),
            };
            let shown = if fresh.is_empty() {
                errors.clone()
            } else {
                fresh
            };
            (header, shown, improved)
        }
    };
    let mut text = header;
    for diagnostic in shown.iter().take(FEEDBACK_LINES) {
        text.push('\n');
        text.push_str(&describe(diagnostic));
    }
    EditFeedback {
        text,
        build: current.clone(),
        improved,
    }
}

struct CheckerState {
    baseline: Option<tokio::task::JoinHandle<(BuildState, Option<String>)>>,
    last: BuildState,
}

pub struct AutoChecker {
    lease: Arc<SandboxLease>,
    spec: Arc<CheckSpec>,
    sources: Vec<glob::Pattern>,
    manifests: Vec<glob::Pattern>,
    state: tokio::sync::Mutex<CheckerState>,
}

impl AutoChecker {
    pub fn new(
        lease: Arc<SandboxLease>,
        stack: &Stack,
        check: &VerbSpec,
    ) -> Result<Self, RenderError> {
        let argv = render_argv(check, &BTreeMap::new())?;
        let compile = |globs: &[String]| -> Vec<glob::Pattern> {
            globs
                .iter()
                .filter_map(|g| glob::Pattern::new(g).ok())
                .collect()
        };
        Ok(Self {
            lease,
            spec: Arc::new(CheckSpec {
                argv,
                verb: check.clone(),
                env: stack
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                cap: Duration::from_secs(DEFAULT_AUTO_CHECK_SECS),
            }),
            sources: compile(&stack.source_globs),
            manifests: compile(&stack.detect),
            state: tokio::sync::Mutex::new(CheckerState {
                baseline: None,
                last: BuildState::NotRun,
            }),
        })
    }

    pub fn with_cap_secs(mut self, secs: u64) -> Self {
        if let Some(spec) = Arc::get_mut(&mut self.spec) {
            spec.cap = Duration::from_secs(secs);
        }
        self
    }

    pub fn start_baseline(&self) {
        let lease = Arc::clone(&self.lease);
        let spec = Arc::clone(&self.spec);
        let handle = tokio::task::spawn_blocking(move || run_check(&lease, &spec));
        if let Ok(mut state) = self.state.try_lock() {
            state.baseline = Some(handle);
        }
    }

    pub fn is_relevant(&self, path: &str) -> bool {
        let options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };
        let name = path.rsplit('/').next().unwrap_or(path);
        self.sources
            .iter()
            .any(|pattern| pattern.matches_with(path, options))
            || self
                .manifests
                .iter()
                .any(|pattern| pattern.matches_with(path, options) || pattern.matches(name))
    }
}

#[async_trait]
impl EditObserver for AutoChecker {
    async fn after_edits(&self, touched: &[String]) -> Option<EditFeedback> {
        if !touched.iter().any(|path| self.is_relevant(path)) {
            return None;
        }
        let mut state = self.state.lock().await;
        if let Some(handle) = state.baseline.take() {
            if let Ok((baseline, _)) = handle.await {
                state.last = baseline;
            }
        }
        let previous = state.last.clone();
        let lease = Arc::clone(&self.lease);
        let spec = Arc::clone(&self.spec);
        let (current, reason) = tokio::task::spawn_blocking(move || run_check(&lease, &spec))
            .await
            .unwrap_or((BuildState::NotRun, Some("check task failed".to_string())));
        state.last = current.clone();
        Some(feedback_for(
            &previous,
            &current,
            reason.as_deref(),
            self.spec.cap,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectClass;
    use crate::plugins::types::{DiagnosticsParser, ParamKind, ParamSpec, VerbSpec};
    use std::collections::BTreeMap;

    fn param(kind: ParamKind, required: bool) -> ParamSpec {
        ParamSpec {
            kind,
            description: "p".to_string(),
            required,
            values: Vec::new(),
        }
    }

    fn verb(argv: &[&str], params: Vec<(&str, ParamSpec)>) -> VerbSpec {
        VerbSpec {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            params: params
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            effect: EffectClass::Workspace,
            network: false,
            timeout_secs: 30,
            diagnostics: DiagnosticsParser::None,
            diagnostics_pattern: None,
        }
    }

    fn text(value: &str) -> ParamValue {
        ParamValue::Text(value.to_string())
    }

    fn list(values: &[&str]) -> ParamValue {
        ParamValue::List(values.iter().map(|s| s.to_string()).collect())
    }

    fn params(items: Vec<(&str, ParamValue)>) -> BTreeMap<String, ParamValue> {
        items.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_whole_element_placeholder_expands_a_string_to_one_item() {
        let v = verb(
            &["cargo", "check", "{{package}}"],
            vec![("package", param(ParamKind::String, false))],
        );
        let argv = render_argv(&v, &params(vec![("package", text("core"))])).unwrap();
        assert_eq!(argv, strings(&["cargo", "check", "core"]));
    }

    #[test]
    fn a_whole_element_placeholder_expands_a_list_to_many_items() {
        let v = verb(
            &["rustfmt", "{{files}}", "--check"],
            vec![("files", param(ParamKind::PathList, true))],
        );
        let argv = render_argv(&v, &params(vec![("files", list(&["a.rs", "b.rs"]))])).unwrap();
        assert_eq!(argv, strings(&["rustfmt", "a.rs", "b.rs", "--check"]));
    }

    #[test]
    fn an_embedded_placeholder_substitutes_text() {
        let v = verb(
            &["go", "test", "-run={{name}}", "./{{dir}}/..."],
            vec![
                ("name", param(ParamKind::String, true)),
                ("dir", param(ParamKind::Path, true)),
            ],
        );
        let argv = render_argv(
            &v,
            &params(vec![("name", text("TestX")), ("dir", text("pkg"))]),
        )
        .unwrap();
        assert_eq!(argv, strings(&["go", "test", "-run=TestX", "./pkg/..."]));
    }

    #[test]
    fn an_absent_optional_param_drops_its_element() {
        let v = verb(
            &["cargo", "check", "{{package}}", "--features={{features}}"],
            vec![
                ("package", param(ParamKind::String, false)),
                ("features", param(ParamKind::String, false)),
            ],
        );
        assert_eq!(
            render_argv(&v, &BTreeMap::new()).unwrap(),
            strings(&["cargo", "check"])
        );
        assert_eq!(
            render_argv(&v, &params(vec![("package", text(""))])).unwrap(),
            strings(&["cargo", "check"])
        );
    }

    #[test]
    fn an_unknown_placeholder_is_an_error() {
        let v = verb(&["x", "{{ghost}}"], vec![]);
        assert_eq!(
            render_argv(&v, &BTreeMap::new()).unwrap_err(),
            RenderError::UnknownPlaceholder("ghost".to_string())
        );
        let embedded = verb(&["x", "a{{ghost}}b"], vec![]);
        assert!(matches!(
            render_argv(&embedded, &BTreeMap::new()),
            Err(RenderError::UnknownPlaceholder(_))
        ));
    }

    #[test]
    fn a_missing_required_param_is_an_error() {
        let v = verb(
            &["x", "{{target}}"],
            vec![("target", param(ParamKind::String, true))],
        );
        assert_eq!(
            render_argv(&v, &BTreeMap::new()).unwrap_err(),
            RenderError::MissingParam("target".to_string())
        );
    }

    #[test]
    fn a_supplied_param_the_verb_does_not_declare_is_an_error() {
        let v = verb(&["x"], vec![]);
        assert_eq!(
            render_argv(&v, &params(vec![("extra", text("1"))])).unwrap_err(),
            RenderError::UnknownParam("extra".to_string())
        );
    }

    #[test]
    fn option_like_values_are_rejected() {
        let v = verb(
            &["cargo", "check", "{{package}}"],
            vec![("package", param(ParamKind::String, false))],
        );
        for bad in ["--manifest-path=/x", "-p", "-"] {
            assert!(
                matches!(
                    render_argv(&v, &params(vec![("package", text(bad))])),
                    Err(RenderError::OptionLike { .. })
                ),
                "{bad}"
            );
        }
        let files = verb(
            &["fmt", "{{files}}"],
            vec![("files", param(ParamKind::PathList, true))],
        );
        assert!(matches!(
            render_argv(&files, &params(vec![("files", list(&["ok.rs", "--evil"]))])),
            Err(RenderError::OptionLike { .. })
        ));
    }

    #[test]
    fn an_enum_param_takes_only_listed_values_and_may_be_option_shaped() {
        let mut spec = param(ParamKind::Enum, false);
        spec.values = strings(&["--release", "--debug"]);
        let v = verb(&["cargo", "build", "{{profile}}"], vec![("profile", spec)]);
        assert_eq!(
            render_argv(&v, &params(vec![("profile", text("--release"))])).unwrap(),
            strings(&["cargo", "build", "--release"])
        );
        assert!(matches!(
            render_argv(&v, &params(vec![("profile", text("--other"))])),
            Err(RenderError::NotInEnum { .. })
        ));
    }

    #[test]
    fn list_values_for_scalar_params_and_embedded_lists_are_rejected() {
        let v = verb(
            &["x", "{{one}}"],
            vec![("one", param(ParamKind::String, true))],
        );
        assert!(matches!(
            render_argv(&v, &params(vec![("one", list(&["a", "b"]))])),
            Err(RenderError::ListNotAllowed(_))
        ));
        let embedded = verb(
            &["x", "--f={{many}}"],
            vec![("many", param(ParamKind::PathList, true))],
        );
        assert!(matches!(
            render_argv(&embedded, &params(vec![("many", list(&["a"]))])),
            Err(RenderError::ListNotAllowed(_))
        ));
    }

    #[test]
    fn shell_metacharacters_stay_single_literal_items() {
        let v = verb(
            &["echo", "{{msg}}"],
            vec![("msg", param(ParamKind::String, true))],
        );
        let argv = render_argv(&v, &params(vec![("msg", text("a; rm -rf / $(x) `y`"))])).unwrap();
        assert_eq!(argv, strings(&["echo", "a; rm -rf / $(x) `y`"]));
    }

    #[test]
    fn an_empty_argv_is_an_error_and_unclosed_braces_are_literal() {
        assert_eq!(
            render_argv(&verb(&[], vec![]), &BTreeMap::new()).unwrap_err(),
            RenderError::EmptyArgv
        );
        let v = verb(&["x", "{{open"], vec![]);
        assert_eq!(
            render_argv(&v, &BTreeMap::new()).unwrap(),
            strings(&["x", "{{open"])
        );
    }

    #[test]
    fn json_arguments_convert_to_params() {
        let mut map = serde_json::Map::new();
        map.insert("a".to_string(), serde_json::json!("s"));
        map.insert("b".to_string(), serde_json::json!(["x", "y"]));
        map.insert("c".to_string(), serde_json::json!(7));
        map.insert("d".to_string(), serde_json::json!(true));
        map.insert("e".to_string(), serde_json::Value::Null);
        let converted = params_from_args(&map).unwrap();
        assert_eq!(converted.get("a"), Some(&text("s")));
        assert_eq!(converted.get("b"), Some(&list(&["x", "y"])));
        assert_eq!(converted.get("c"), Some(&text("7")));
        assert_eq!(converted.get("d"), Some(&text("true")));
        assert!(!converted.contains_key("e"));
        map.insert("f".to_string(), serde_json::json!({"k": 1}));
        assert!(params_from_args(&map).is_err());
        let mut mixed = serde_json::Map::new();
        mixed.insert("l".to_string(), serde_json::json!(["a", 1]));
        assert!(params_from_args(&mixed).is_err());
    }

    #[test]
    fn the_tool_schema_describes_the_verb_params() {
        let mut spec = param(ParamKind::Enum, false);
        spec.values = strings(&["a", "b"]);
        let v = verb(
            &["x"],
            vec![
                ("package", param(ParamKind::String, true)),
                ("files", param(ParamKind::PathList, false)),
                ("mode", spec),
            ],
        );
        let schema = verb_schema(&v);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], serde_json::json!(["package"]));
        assert_eq!(schema["properties"]["package"]["type"], "string");
        assert_eq!(schema["properties"]["files"]["type"], "array");
        assert_eq!(schema["properties"]["files"]["items"]["type"], "string");
        assert_eq!(
            schema["properties"]["mode"]["enum"],
            serde_json::json!(["a", "b"])
        );
    }

    use crate::exec_sandbox::tests::{fake_config, fake_helper, fake_log};
    use crate::exec_sandbox::{ExecProfile, SandboxedExec};
    use crate::sandbox_lease::SandboxLease;
    use crate::tool_contract::{BuildState, EditObserver};
    use crate::tools::Tool;

    struct Rig {
        helper_dir: tempfile::TempDir,
        scratch: tempfile::TempDir,
        work: tempfile::TempDir,
    }

    fn rig() -> Rig {
        Rig {
            helper_dir: tempfile::tempdir().unwrap(),
            scratch: tempfile::tempdir().unwrap(),
            work: tempfile::tempdir().unwrap(),
        }
    }

    fn exec(rig: &Rig) -> SandboxedExec {
        SandboxedExec::new(
            fake_config(fake_helper(rig.helper_dir.path())),
            rig.work.path().to_path_buf(),
            ExecProfile::Generic,
        )
    }

    fn lease(rig: &Rig) -> Arc<SandboxLease> {
        Arc::new(SandboxLease::allocate_in(exec(rig), rig.scratch.path()).unwrap())
    }

    const PATTERN: &str = r"(?m)^(?P<file>[^:\n]+):(?P<line>\d+): (?P<message>.+)$";

    fn check_verb(script: &str) -> VerbSpec {
        let mut v = verb(&["/bin/sh", "-c", script], vec![]);
        v.diagnostics = DiagnosticsParser::Regex;
        v.diagnostics_pattern = Some(PATTERN.to_string());
        v
    }

    const READ_ERRORS: &str =
        "out=$(cat errors.txt 2>/dev/null); touch read.marker; if [ -n \"$out\" ]; then echo \"$out\"; exit 1; fi";

    fn stack_with(source_globs: &[&str], detect: &[&str]) -> Stack {
        Stack {
            language: "x".to_string(),
            package_manager: "x".to_string(),
            build_system: "x".to_string(),
            detect: detect.iter().map(|s| s.to_string()).collect(),
            source_globs: source_globs.iter().map(|s| s.to_string()).collect(),
            test_globs: Vec::new(),
            protected: Vec::new(),
            env: BTreeMap::new(),
            verbs: BTreeMap::new(),
        }
    }

    fn checker(rig: &Rig, script: &str) -> AutoChecker {
        AutoChecker::new(
            lease(rig),
            &stack_with(&["**/*.rs"], &["Cargo.toml"]),
            &check_verb(script),
        )
        .unwrap()
    }

    fn set_errors(rig: &Rig, count: usize) {
        let body: String = (1..=count)
            .map(|i| format!("src/a.rs:{i}: problem {i}\n"))
            .collect();
        std::fs::write(rig.work.path().join("errors.txt"), body).unwrap();
    }

    fn touched(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    fn runs(rig: &Rig) -> usize {
        fake_log(rig.helper_dir.path())
            .iter()
            .filter(|l| l.starts_with("run "))
            .count()
    }

    #[tokio::test]
    async fn edits_to_unrelated_files_trigger_no_check() {
        let rig = rig();
        let checker = checker(&rig, READ_ERRORS);
        assert!(checker
            .after_edits(&touched(&["README.md", "docs/a.txt"]))
            .await
            .is_none());
        assert!(checker.after_edits(&[]).await.is_none());
        assert_eq!(runs(&rig), 0);
    }

    #[tokio::test]
    async fn source_files_and_build_manifests_trigger_a_check() {
        let rig = rig();
        let checker = checker(&rig, READ_ERRORS);
        assert!(checker.is_relevant("src/deep/mod.rs"));
        assert!(checker.is_relevant("lib.rs"));
        assert!(checker.is_relevant("Cargo.toml"));
        assert!(checker.is_relevant("crates/x/Cargo.toml"));
        assert!(!checker.is_relevant("README.md"));
        assert!(checker
            .after_edits(&touched(&["Cargo.toml"]))
            .await
            .is_some());
        assert_eq!(runs(&rig), 1);
    }

    #[tokio::test]
    async fn a_clean_tree_reports_build_pass() {
        let rig = rig();
        let checker = checker(&rig, READ_ERRORS);
        let feedback = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        assert_eq!(feedback.text, "build: pass");
        assert_eq!(feedback.build, BuildState::Pass);
        assert!(!feedback.improved);
    }

    #[tokio::test]
    async fn pass_to_fail_lists_new_errors_capped_at_eight_lines() {
        let rig = rig();
        let checker = checker(&rig, READ_ERRORS);
        checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        set_errors(&rig, 3);
        let feedback = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        let lines: Vec<&str> = feedback.text.lines().collect();
        assert_eq!(lines[0], "build: pass->fail (3 errors, 3 new)");
        assert_eq!(
            &lines[1..],
            [
                "src/a.rs:1 problem 1",
                "src/a.rs:2 problem 2",
                "src/a.rs:3 problem 3"
            ]
        );
        assert_eq!(feedback.build.error_count(), 3);
        assert!(!feedback.improved);

        set_errors(&rig, 12);
        let busy = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        assert_eq!(busy.text.lines().count(), 9);
    }

    #[tokio::test]
    async fn fail_to_pass_is_an_improvement() {
        let rig = rig();
        set_errors(&rig, 2);
        let checker = checker(&rig, READ_ERRORS);
        checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        std::fs::remove_file(rig.work.path().join("errors.txt")).unwrap();
        let feedback = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        assert_eq!(feedback.text, "build: fail->pass");
        assert!(feedback.improved);
        assert_eq!(feedback.build, BuildState::Pass);
    }

    #[tokio::test]
    async fn still_failing_improves_only_when_the_error_count_drops() {
        let rig = rig();
        set_errors(&rig, 3);
        let checker = checker(&rig, READ_ERRORS);
        checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        set_errors(&rig, 2);
        let fewer = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        assert_eq!(
            fewer.text.lines().next().unwrap(),
            "build: still failing (2 errors)"
        );
        assert!(fewer.improved);
        let same = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        assert!(!same.improved);
        set_errors(&rig, 4);
        let worse = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        assert_eq!(
            worse.text.lines().next().unwrap(),
            "build: still failing (4 errors, 2 new)"
        );
        assert!(!worse.improved);
    }

    #[tokio::test]
    async fn the_baseline_runs_in_the_background_and_the_first_check_awaits_it() {
        let rig = rig();
        set_errors(&rig, 2);
        let checker = checker(&rig, READ_ERRORS);
        checker.start_baseline();
        let marker = rig.work.path().join("read.marker");
        let started = std::time::Instant::now();
        while !marker.exists() {
            assert!(started.elapsed() < std::time::Duration::from_secs(20));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        std::fs::remove_file(rig.work.path().join("errors.txt")).unwrap();
        let feedback = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        assert_eq!(feedback.text, "build: fail->pass");
        assert!(feedback.improved);
    }

    #[tokio::test]
    async fn a_check_over_the_cap_is_killed_and_reported_as_timed_out() {
        let rig = rig();
        let checker = checker(&rig, "sleep 30").with_cap_secs(1);
        let started = std::time::Instant::now();
        let feedback = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        assert_eq!(feedback.text, "build: check timed out (1s)");
        assert_eq!(feedback.build, BuildState::TimedOut);
        assert!(!feedback.improved);
        assert!(started.elapsed() < std::time::Duration::from_secs(15));
        assert!(fake_log(rig.helper_dir.path())
            .iter()
            .any(|l| l.starts_with("kill ")));
    }

    #[tokio::test]
    async fn a_failing_check_with_no_parsed_diagnostics_still_reports_an_error() {
        let rig = rig();
        let checker = checker(&rig, "echo boom >&2; exit 1");
        let feedback = checker.after_edits(&touched(&["src/a.rs"])).await.unwrap();
        assert_eq!(feedback.text, "build: fail (1 error)\nboom");
        assert_eq!(feedback.build.error_count(), 1);
    }

    #[tokio::test]
    async fn an_unrunnable_check_is_reported_without_a_verdict() {
        let rig = rig();
        let mut config = fake_config(rig.helper_dir.path().join("missing-helper"));
        config.timeout = std::time::Duration::from_secs(5);
        let exec = SandboxedExec::new(config, rig.work.path().to_path_buf(), ExecProfile::Generic);
        let lease = Arc::new(SandboxLease::allocate_in(exec, rig.scratch.path()).unwrap());
        let checker =
            AutoChecker::new(lease, &stack_with(&["**/*.rs"], &[]), &check_verb("true")).unwrap();
        let feedback = checker.after_edits(&touched(&["a.rs"])).await.unwrap();
        assert!(feedback.text.starts_with("build: check could not run"));
        assert_eq!(feedback.build, BuildState::NotRun);
    }

    #[test]
    fn a_check_verb_with_a_required_param_cannot_drive_the_checker() {
        let rig = rig();
        let mut v = check_verb("true");
        v.argv.push("{{needed}}".to_string());
        v.params
            .insert("needed".to_string(), param(ParamKind::String, true));
        let result = AutoChecker::new(lease(&rig), &stack_with(&[], &[]), &v);
        assert!(matches!(result, Err(RenderError::MissingParam(_))));
    }

    fn echo_tool(rig: &Rig, v: VerbSpec) -> crate::tool_contract::Typed<VerbTool> {
        crate::tool_contract::Typed(VerbTool::new(
            "say",
            v,
            ExecTarget::Ephemeral(Arc::new(exec(rig))),
            Vec::new(),
        ))
    }

    #[tokio::test]
    async fn a_verb_tool_runs_the_rendered_argv_and_reports_the_result() {
        let rig = rig();
        let v = verb(
            &["/bin/echo", "{{msg}}"],
            vec![("msg", param(ParamKind::String, true))],
        );
        let tool = echo_tool(&rig, v);
        let wire = tool
            .execute(serde_json::json!({"msg": "hello; $(x)"}))
            .await
            .unwrap();
        assert_eq!(wire["ok"], true);
        assert_eq!(wire["data"]["success"], true);
        assert_eq!(wire["data"]["timed_out"], false);
        assert_eq!(wire["data"]["stdout_tail"], "hello; $(x)\n");
        assert_eq!(wire["data"]["diagnostics"], serde_json::json!([]));
        assert_eq!(tool.name(), "say");
        assert_eq!(tool.effect_class(), EffectClass::Workspace);
    }

    #[tokio::test]
    async fn a_verb_tool_rejects_bad_arguments_without_running_anything() {
        let rig = rig();
        let v = verb(
            &["/bin/echo", "{{msg}}"],
            vec![("msg", param(ParamKind::String, true))],
        );
        let tool = echo_tool(&rig, v);
        for args in [
            serde_json::json!({}),
            serde_json::json!({"msg": "--flag"}),
            serde_json::json!({"msg": "ok", "other": "x"}),
            serde_json::json!({"msg": {"nested": 1}}),
            serde_json::json!("not an object"),
        ] {
            let wire = tool.execute(args.clone()).await.unwrap();
            assert_eq!(wire["ok"], false, "{args}");
            assert_eq!(wire["error_code"], "invalid_args", "{args}");
        }
        assert_eq!(runs(&rig), 0);
    }

    #[tokio::test]
    async fn a_verb_tool_attaches_parsed_diagnostics_and_failure_status() {
        let rig = rig();
        let v = check_verb("echo 'src/a.rs:4: broken' >&2; exit 2");
        let tool = echo_tool(&rig, v);
        let wire = tool.execute(serde_json::json!({})).await.unwrap();
        assert_eq!(wire["ok"], true);
        assert_eq!(wire["data"]["success"], false);
        assert_eq!(wire["data"]["diagnostics"][0]["file"], "src/a.rs");
        assert_eq!(wire["data"]["diagnostics"][0]["line"], 4);
        assert_eq!(wire["data"]["diagnostics"][0]["message"], "broken");
    }

    #[tokio::test]
    async fn a_verb_tool_that_times_out_is_a_timeout_failure_with_data() {
        let rig = rig();
        let mut v = verb(&["/bin/sleep", "30"], vec![]);
        v.timeout_secs = 1;
        let tool = echo_tool(&rig, v);
        let wire = tool.execute(serde_json::json!({})).await.unwrap();
        assert_eq!(wire["ok"], false);
        assert_eq!(wire["error_code"], "timeout");
        assert_eq!(wire["data"]["timed_out"], true);
    }

    #[test]
    fn a_verb_tool_exposes_the_verb_schema_for_the_integrator() {
        let rig = rig();
        let v = verb(
            &["x", "{{msg}}"],
            vec![("msg", param(ParamKind::String, true))],
        );
        let tool = VerbTool::new(
            "say",
            v.clone(),
            ExecTarget::Ephemeral(Arc::new(exec(&rig))),
            Vec::new(),
        );
        assert_eq!(tool.args_schema(), verb_schema(&v));
        assert_eq!(tool.verb(), &v);
    }

    #[test]
    fn feedback_text_covers_each_transition() {
        let err = |n: u32| Diagnostic {
            file: format!("f{n}.rs"),
            line: n,
            col: 1,
            severity: crate::tool_contract::Severity::Error,
            code: None,
            message: format!("m{n}"),
        };
        let fail = |ns: &[u32]| BuildState::Fail {
            errors: ns.iter().map(|n| err(*n)).collect(),
        };
        let cap = std::time::Duration::from_secs(300);
        let pass_fail = feedback_for(&BuildState::Pass, &fail(&[1]), None, cap);
        assert_eq!(
            pass_fail.text,
            "build: pass->fail (1 error, 1 new)\nf1.rs:1 m1"
        );
        let fixed = feedback_for(&fail(&[1]), &BuildState::Pass, None, cap);
        assert_eq!(
            (fixed.text.as_str(), fixed.improved),
            ("build: fail->pass", true)
        );
        let same = feedback_for(&fail(&[1, 2]), &fail(&[1, 2]), None, cap);
        assert_eq!(
            same.text,
            "build: still failing (2 errors)\nf1.rs:1 m1\nf2.rs:2 m2"
        );
        let timeout = feedback_for(&BuildState::Pass, &BuildState::TimedOut, None, cap);
        assert_eq!(timeout.text, "build: check timed out (300s)");
        let fresh = feedback_for(&BuildState::NotRun, &fail(&[1]), None, cap);
        assert_eq!(fresh.text, "build: fail (1 error)\nf1.rs:1 m1");
    }
}
