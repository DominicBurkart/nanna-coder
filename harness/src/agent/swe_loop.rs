use super::{is_refusal, DEFAULT_MODEL, FILE_EDIT_TOOLS};
use crate::action_auditor::ActionDenied;
use crate::effects::EffectRecord;
use crate::entities::context::types::ToolCallRecord;
use crate::tool_contract::{EditObserver, ErrorCode, FinishClaim, FinishSlot, ToolOutcome};
use crate::tools::{ToolError, ToolRegistry};
use model::provider::ModelProvider;
use model::types::{ChatMessage, ChatRequest, ToolCall, Usage};
use serde_json::{json, Value};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

pub const DEFAULT_MAX_INACTIVITY_SECS: u64 = 2700;
pub const MAX_INACTIVITY_ENV: &str = "NANNA_MAX_INACTIVITY_SECS";
const DEFAULT_MAX_ITERATIONS: usize = 200;
const FINISH_TOOL: &str = "finish";

pub fn resolve_inactivity(
    card_limit: Option<u64>,
    env: Option<&str>,
    task_override: Option<u64>,
) -> Option<Duration> {
    let env_value = env.and_then(|raw| raw.trim().parse::<u64>().ok());
    let base = card_limit
        .or(env_value)
        .unwrap_or(DEFAULT_MAX_INACTIVITY_SECS);
    let base = (base > 0).then_some(base);
    let effective = match (base, task_override) {
        (Some(card), Some(task)) if task > 0 => Some(card.min(task)),
        (None, Some(task)) if task > 0 => Some(task),
        (base, _) => base,
    };
    effective.map(Duration::from_secs)
}

pub fn no_progress_classification(elapsed: Duration, window: Duration) -> String {
    format!(
        "no file edit succeeded and the build state did not improve for {}s (inactivity window {}s). \
         Raise the window with the identity card field `max_inactivity_secs` under [limits], the \
         daemon environment variable {MAX_INACTIVITY_ENV}, or lower it per task with the \
         `max_inactivity_secs` argument of assign_task (a task may only lower the card limit). \
         Partial changes remain available through get_task_changes.",
        elapsed.as_secs(),
        window.as_secs()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopLimits {
    pub max_iterations: usize,
    pub max_inactivity: Option<Duration>,
}

impl Default for LoopLimits {
    fn default() -> Self {
        Self {
            max_iterations: DEFAULT_MAX_ITERATIONS,
            max_inactivity: resolve_inactivity(
                None,
                std::env::var(MAX_INACTIVITY_ENV).ok().as_deref(),
                None,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopOutcome {
    Completed(FinishClaim),
    Blocked(String),
    NoProgress {
        last_error: Option<ErrorCode>,
        last_message: String,
    },
    Inactive {
        elapsed: Duration,
        window: Duration,
    },
    MaxIterations,
    Escalated {
        tool: String,
    },
}

#[derive(Debug, Clone)]
pub struct SweRun {
    pub outcome: LoopOutcome,
    pub ledger: Vec<ToolCallRecord>,
    pub iterations: usize,
    pub conversation: Vec<ChatMessage>,
    pub usage: Usage,
}

#[derive(Debug, thiserror::Error)]
pub enum SweError {
    #[error("LLM call failed: {0}")]
    Model(String),
    #[error("Empty response from model")]
    EmptyResponse,
}

pub struct SweLoop {
    provider: Arc<dyn ModelProvider>,
    registry: ToolRegistry,
    system_prompt: String,
    finish: FinishSlot,
    observer: Option<Arc<dyn EditObserver>>,
    limits: LoopLimits,
    model_name: String,
}

struct TurnEntry {
    call: ToolCall,
    outcome: ToolOutcome,
    legacy_raw: Option<String>,
}

enum TurnEnd {
    Continue,
    Finished(FinishClaim),
    Escalated(String),
    Inactive,
}

struct Clock {
    window: Option<Duration>,
    last_progress: Instant,
}

impl Clock {
    fn new(window: Option<Duration>) -> Self {
        Self {
            window,
            last_progress: Instant::now(),
        }
    }

    fn reset(&mut self) {
        self.last_progress = Instant::now();
    }

    fn deadline(&self) -> Option<Instant> {
        self.window.map(|window| self.last_progress + window)
    }

    fn expired(&self) -> bool {
        self.deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    async fn bound<T>(&self, work: impl Future<Output = T>) -> Option<T> {
        match self.deadline() {
            Some(deadline) => tokio::time::timeout_at(deadline, work).await.ok(),
            None => Some(work.await),
        }
    }

    fn inactive(&self) -> LoopOutcome {
        LoopOutcome::Inactive {
            elapsed: self.last_progress.elapsed(),
            window: self.window.unwrap_or_default(),
        }
    }
}

fn is_unknown_tool(error: &ToolError) -> bool {
    match error {
        ToolError::NotFound { .. } => true,
        ToolError::ScopeDenied(denial) => {
            matches!(denial.reason, crate::scope::DenialReason::ToolNotInScope)
        }
        _ => false,
    }
}

fn parse_typed_outcome(value: &Value) -> Option<ToolOutcome> {
    if value.get("ok").is_some_and(Value::is_boolean) {
        serde_json::from_value(value.clone()).ok()
    } else {
        None
    }
}

fn attach_build_feedback(outcome: &mut ToolOutcome, text: &str) {
    match &mut outcome.data {
        Value::Object(map) => {
            map.insert("build".to_string(), Value::String(text.to_string()));
        }
        Value::Null => outcome.data = json!({"build": text}),
        other => {
            let previous = other.take();
            outcome.data = json!({"result": previous, "build": text});
        }
    }
}

fn touched_path(args: &Value) -> Option<String> {
    ["path", "file_path"]
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::to_string)
}

fn changed_content(outcome: &ToolOutcome) -> bool {
    outcome.ok && outcome.data.get("changed") != Some(&Value::Bool(false))
}

impl SweLoop {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        registry: ToolRegistry,
        system_prompt: impl Into<String>,
        finish: FinishSlot,
        limits: LoopLimits,
    ) -> Self {
        Self {
            provider,
            registry,
            system_prompt: system_prompt.into(),
            finish,
            observer: None,
            limits,
            model_name: DEFAULT_MODEL.to_string(),
        }
    }

    pub fn with_observer(mut self, observer: Arc<dyn EditObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn with_model_name(mut self, model_name: impl Into<String>) -> Self {
        self.model_name = model_name.into();
        self
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub async fn run(&mut self, user_prompt: &str) -> Result<SweRun, SweError> {
        let mut conversation = Vec::new();
        if !self.system_prompt.is_empty() {
            conversation.push(ChatMessage::system(&self.system_prompt));
        }
        conversation.push(ChatMessage::user(user_prompt));

        let tool_defs = self.registry.get_definitions();
        let mut clock = Clock::new(self.limits.max_inactivity);
        let mut ledger: Vec<ToolCallRecord> = Vec::new();
        let mut usage = Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        let mut iterations = 0;
        let mut nudged = false;
        let mut last_failure: Option<(ErrorCode, String)> = None;

        let outcome = loop {
            if iterations >= self.limits.max_iterations {
                break LoopOutcome::MaxIterations;
            }
            if clock.expired() {
                break clock.inactive();
            }
            let request = ChatRequest::new(&self.model_name, conversation.clone())
                .with_tools(tool_defs.clone());
            let response = match clock.bound(self.provider.chat(request)).await {
                Some(result) => result.map_err(|error| SweError::Model(error.to_string()))?,
                None => break clock.inactive(),
            };
            iterations += 1;
            if let Some(reported) = &response.usage {
                usage.prompt_tokens += reported.prompt_tokens;
                usage.completion_tokens += reported.completion_tokens;
                usage.total_tokens += reported.total_tokens;
            }
            let Some(choice) = response.choices.into_iter().next() else {
                return Err(SweError::EmptyResponse);
            };
            let message = choice.message;
            let calls = message.tool_calls.clone().unwrap_or_default();
            let text = message.content.clone().unwrap_or_default();
            conversation.push(message);

            if calls.is_empty() {
                if is_refusal(&text) {
                    break LoopOutcome::Blocked(text.trim()[7..].trim().to_string());
                }
                if nudged {
                    break LoopOutcome::NoProgress {
                        last_error: last_failure.as_ref().map(|(code, _)| *code),
                        last_message: last_failure
                            .as_ref()
                            .map(|(_, message)| message.clone())
                            .unwrap_or_default(),
                    };
                }
                nudged = true;
                conversation.push(ChatMessage::user(nudge_text(last_failure.as_ref())));
                continue;
            }
            nudged = false;

            match self
                .run_turn(
                    calls,
                    &mut clock,
                    &mut conversation,
                    &mut ledger,
                    &mut last_failure,
                )
                .await
            {
                TurnEnd::Continue => {}
                TurnEnd::Finished(claim) => break LoopOutcome::Completed(claim),
                TurnEnd::Escalated(tool) => break LoopOutcome::Escalated { tool },
                TurnEnd::Inactive => break clock.inactive(),
            }
        };

        Ok(SweRun {
            outcome,
            ledger,
            iterations,
            conversation,
            usage,
        })
    }

    async fn execute_call(&self, call: &ToolCall) -> (ToolOutcome, Option<String>, bool) {
        let result = self
            .registry
            .execute(&call.function.name, call.function.arguments.clone())
            .await;
        match result {
            Ok(value) => match parse_typed_outcome(&value) {
                Some(outcome) => (outcome, None, false),
                None => (
                    ToolOutcome::success(value.clone()),
                    Some(value.to_string()),
                    false,
                ),
            },
            Err(error) => {
                let escalated = matches!(
                    error,
                    ToolError::ActionDenied(ActionDenied::Escalate { .. })
                );
                let outcome = if is_unknown_tool(&error) {
                    let mut names = self.registry.list_tools();
                    names.sort_unstable();
                    ToolOutcome::failure(ErrorCode::UnknownTool, error.to_string())
                        .with_hint(format!("valid tools: {}", names.join(", ")))
                } else {
                    ToolOutcome::from(&error)
                };
                (outcome, None, escalated)
            }
        }
    }

    async fn run_turn(
        &self,
        calls: Vec<ToolCall>,
        clock: &mut Clock,
        conversation: &mut Vec<ChatMessage>,
        ledger: &mut Vec<ToolCallRecord>,
        last_failure: &mut Option<(ErrorCode, String)>,
    ) -> TurnEnd {
        let mut entries: Vec<TurnEntry> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let mut last_edit: Option<usize> = None;
        let mut end = TurnEnd::Continue;

        for call in calls {
            let Some((outcome, legacy_raw, escalated)) =
                clock.bound(self.execute_call(&call)).await
            else {
                end = TurnEnd::Inactive;
                break;
            };
            let name = call.function.name.as_str();
            if !outcome.ok {
                *last_failure = Some((
                    outcome.error_code.unwrap_or(ErrorCode::ExecFailed),
                    outcome.message.clone(),
                ));
            }
            if FILE_EDIT_TOOLS.contains(&name) && outcome.ok {
                if let Some(path) = touched_path(&call.function.arguments) {
                    if !touched.contains(&path) {
                        touched.push(path);
                    }
                }
                last_edit = Some(entries.len());
                if changed_content(&outcome) {
                    clock.reset();
                }
            }
            let finished = name == FINISH_TOOL && outcome.ok;
            let tool = call.function.name.clone();
            entries.push(TurnEntry {
                call,
                outcome,
                legacy_raw,
            });
            if escalated {
                end = TurnEnd::Escalated(tool);
                break;
            }
            if finished {
                if let Some(claim) = self.finish.lock().ok().and_then(|mut slot| slot.take()) {
                    end = TurnEnd::Finished(claim);
                    break;
                }
            }
        }

        if let (Some(observer), Some(index), TurnEnd::Continue) = (&self.observer, last_edit, &end)
        {
            match clock.bound(observer.after_edits(&touched)).await {
                Some(Some(feedback)) => {
                    attach_build_feedback(&mut entries[index].outcome, &feedback.text);
                    entries[index].legacy_raw = None;
                    if feedback.improved {
                        clock.reset();
                    }
                }
                Some(None) => {}
                None => end = TurnEnd::Inactive,
            }
        }

        for entry in entries {
            let content = entry
                .legacy_raw
                .clone()
                .unwrap_or_else(|| serde_json::to_string(&entry.outcome).unwrap_or_default());
            conversation.push(ChatMessage::tool_response(
                entry.call.id.clone(),
                content.clone(),
            ));
            let effect = self
                .registry
                .effect_class_of(&entry.call.function.name)
                .map(EffectRecord::new);
            ledger.push(ToolCallRecord {
                tool_name: entry.call.function.name,
                arguments: entry.call.function.arguments,
                call_id: entry.call.id,
                result: content,
                effect,
                seq: ledger.len() as u64,
                outcome: Some(entry.outcome),
            });
        }
        end
    }
}

fn nudge_text(last_failure: Option<&(ErrorCode, String)>) -> String {
    let failure = match last_failure {
        Some((code, _)) => {
            let code = serde_json::to_value(code)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default();
            format!("Your last failed call returned error_code `{code}`; fix it or ")
        }
        None => String::new(),
    };
    format!(
        "{failure}Do not describe what you will do. Keep calling tools until the work is done, \
         then call `finish`. If the task cannot be done, answer with exactly one sentence \
         starting with `CANNOT:` that says why."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_support::{
        multi_call_response, plain_response, scoped_write_registry, scripted_registry,
        tool_call_response, DelayedProvider, MockProvider, ScriptedTool,
    };
    use crate::effects::EffectClass;
    use crate::tool_contract::{BuildState, EditFeedback};
    use crate::tools::ToolError;
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn slot() -> FinishSlot {
        Arc::new(Mutex::new(None))
    }

    fn limits() -> LoopLimits {
        LoopLimits {
            max_iterations: 20,
            max_inactivity: None,
        }
    }

    fn ok_outcome(data: Value) -> crate::tools::ToolResult<Value> {
        Ok(serde_json::to_value(ToolOutcome::success(data)).unwrap())
    }

    fn err_outcome(code: ErrorCode, message: &str) -> crate::tools::ToolResult<Value> {
        Ok(serde_json::to_value(ToolOutcome::failure(code, message)).unwrap())
    }

    fn finish_tool(slot: &FinishSlot) -> ScriptedTool {
        let slot = slot.clone();
        ScriptedTool::new(
            "finish",
            EffectClass::None,
            vec![ok_outcome(json!({"finished": true}))],
        )
        .on_run(move || {
            *slot.lock().unwrap() = Some(FinishClaim {
                summary: "done".to_string(),
                files_changed: vec!["a.rs".to_string()],
            });
        })
    }

    fn claim() -> FinishClaim {
        FinishClaim {
            summary: "done".to_string(),
            files_changed: vec!["a.rs".to_string()],
        }
    }

    fn edit_tool(results: Vec<crate::tools::ToolResult<Value>>) -> ScriptedTool {
        ScriptedTool::new("edit_file", EffectClass::Workspace, results)
    }

    fn read_tool() -> ScriptedTool {
        ScriptedTool::new("read_file", EffectClass::None, vec![])
    }

    fn edit_args() -> Value {
        json!({"path": "src/lib.rs", "old_string": "a", "new_string": "b"})
    }

    struct ScriptedObserver {
        calls: Mutex<Vec<Vec<String>>>,
        feedback: Mutex<Vec<Option<EditFeedback>>>,
    }

    impl ScriptedObserver {
        fn new(feedback: Vec<Option<EditFeedback>>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                feedback: Mutex::new(feedback),
            })
        }
    }

    #[async_trait]
    impl EditObserver for ScriptedObserver {
        async fn after_edits(&self, touched: &[String]) -> Option<EditFeedback> {
            self.calls.lock().unwrap().push(touched.to_vec());
            let mut feedback = self.feedback.lock().unwrap();
            if feedback.is_empty() {
                None
            } else {
                feedback.remove(0)
            }
        }
    }

    fn feedback(text: &str, improved: bool) -> Option<EditFeedback> {
        Some(EditFeedback {
            text: text.to_string(),
            build: BuildState::Pass,
            improved,
        })
    }

    #[tokio::test]
    async fn finish_with_a_filled_slot_completes_with_the_claim() {
        let finish = slot();
        let registry = scripted_registry(vec![finish_tool(&finish)]);
        let provider = MockProvider::new(vec![tool_call_response("finish", json!({}))]);
        let mut swe = SweLoop::new(provider, registry, "sys", finish, limits());

        let run = swe.run("do it").await.unwrap();

        assert_eq!(run.outcome, LoopOutcome::Completed(claim()));
        assert_eq!(run.ledger.len(), 1);
        assert_eq!(run.iterations, 1);
    }

    #[tokio::test]
    async fn a_cannot_reply_blocks_with_the_reason() {
        let registry = scripted_registry(vec![read_tool()]);
        let provider = MockProvider::new(vec![plain_response("CANNOT: no such crate")]);
        let mut swe = SweLoop::new(provider, registry, "", slot(), limits());

        let run = swe.run("do it").await.unwrap();

        assert_eq!(
            run.outcome,
            LoopOutcome::Blocked("no such crate".to_string())
        );
    }

    #[tokio::test]
    async fn one_nudge_names_the_last_failed_error_code_then_a_second_stop_is_no_progress() {
        let registry = scripted_registry(vec![edit_tool(vec![err_outcome(
            ErrorCode::NoMatch,
            "old text not found",
        )])]);
        let provider = MockProvider::new(vec![
            tool_call_response("edit_file", edit_args()),
            plain_response("I will fix it"),
            plain_response("still thinking"),
        ]);
        let mut swe = SweLoop::new(provider, registry, "", slot(), limits());

        let run = swe.run("do it").await.unwrap();

        assert_eq!(
            run.outcome,
            LoopOutcome::NoProgress {
                last_error: Some(ErrorCode::NoMatch),
                last_message: "old text not found".to_string()
            }
        );
        let nudges: Vec<&str> = run
            .conversation
            .iter()
            .filter(|message| message.role == model::types::MessageRole::User)
            .filter_map(|message| message.content.as_deref())
            .filter(|text| text.contains("no_match"))
            .collect();
        assert_eq!(nudges.len(), 1);
        assert!(nudges[0].contains("finish"));
        assert!(nudges[0].contains("CANNOT:"));
    }

    #[tokio::test]
    async fn a_nudged_model_can_still_finish() {
        let finish = slot();
        let registry = scripted_registry(vec![finish_tool(&finish)]);
        let provider = MockProvider::new(vec![
            plain_response("thinking"),
            tool_call_response("finish", json!({})),
        ]);
        let mut swe = SweLoop::new(provider, registry, "", finish, limits());

        let run = swe.run("do it").await.unwrap();

        assert_eq!(run.outcome, LoopOutcome::Completed(claim()));
    }

    #[tokio::test]
    async fn two_turns_that_both_use_call_0_get_distinct_increasing_seq() {
        let registry = scripted_registry(vec![read_tool()]);
        let provider = MockProvider::new(vec![
            tool_call_response("read_file", json!({"path": "a"})),
            tool_call_response("read_file", json!({"path": "b"})),
            plain_response("CANNOT: stop"),
        ]);
        let mut swe = SweLoop::new(provider, registry, "", slot(), limits());

        let run = swe.run("do it").await.unwrap();

        let seqs: Vec<u64> = run.ledger.iter().map(|record| record.seq).collect();
        assert_eq!(seqs, vec![0, 1]);
        assert!(run.ledger.iter().all(|record| record.call_id == "call_0"));
        assert!(run
            .ledger
            .iter()
            .all(|record| record.outcome.as_ref().is_some_and(|outcome| outcome.ok)));
    }

    #[tokio::test]
    async fn seq_continues_across_calls_within_one_turn() {
        let registry = scripted_registry(vec![read_tool()]);
        let provider = MockProvider::new(vec![
            multi_call_response(vec![
                ("read_file", json!({"path": "a"})),
                ("read_file", json!({"path": "b"})),
            ]),
            plain_response("CANNOT: stop"),
        ]);
        let mut swe = SweLoop::new(provider, registry, "", slot(), limits());

        let run = swe.run("do it").await.unwrap();

        let seqs: Vec<u64> = run.ledger.iter().map(|record| record.seq).collect();
        assert_eq!(seqs, vec![0, 1]);
    }

    #[tokio::test]
    async fn a_legacy_error_becomes_a_synthesized_outcome_and_compact_json_for_the_model() {
        let legacy = ScriptedTool::new(
            "legacy",
            EffectClass::None,
            vec![Err(ToolError::ExecutionFailed {
                message: "boom".to_string(),
            })],
        );
        let registry = scripted_registry(vec![legacy]);
        let provider = MockProvider::new(vec![
            tool_call_response("legacy", json!({})),
            plain_response("CANNOT: x"),
        ]);
        let mut swe = SweLoop::new(provider, registry, "", slot(), limits());

        let run = swe.run("do it").await.unwrap();

        let record = &run.ledger[0];
        let outcome = record.outcome.as_ref().unwrap();
        assert!(!outcome.ok);
        assert_eq!(outcome.error_code, Some(ErrorCode::ExecFailed));
        assert!(!record.result.starts_with("Error:"));
        let parsed: ToolOutcome = serde_json::from_str(&record.result).unwrap();
        assert_eq!(&parsed, outcome);
        assert!(!record.result.contains('\n'));
    }

    #[tokio::test]
    async fn a_legacy_success_keeps_its_raw_json_for_the_model() {
        let legacy = ScriptedTool::new(
            "legacy",
            EffectClass::None,
            vec![Ok(json!({"lines": ["x"]}))],
        );
        let registry = scripted_registry(vec![legacy]);
        let provider = MockProvider::new(vec![
            tool_call_response("legacy", json!({})),
            plain_response("CANNOT: x"),
        ]);
        let mut swe = SweLoop::new(provider, registry, "", slot(), limits());

        let run = swe.run("do it").await.unwrap();

        let record = &run.ledger[0];
        assert_eq!(record.result, json!({"lines": ["x"]}).to_string());
        let outcome = record.outcome.as_ref().unwrap();
        assert!(outcome.ok);
        assert_eq!(outcome.data, json!({"lines": ["x"]}));
    }

    #[tokio::test]
    async fn an_unknown_tool_lists_the_sorted_valid_tools_and_is_recorded_as_a_denial() {
        let root = tempfile::tempdir().unwrap();
        let registry = scoped_write_registry(root.path());
        let provider = MockProvider::new(vec![
            tool_call_response("file_manager.write_file", json!({})),
            plain_response("CANNOT: x"),
        ]);
        let mut swe = SweLoop::new(provider, registry, "", slot(), limits());

        let run = swe.run("do it").await.unwrap();

        let outcome = run.ledger[0].outcome.as_ref().unwrap();
        assert_eq!(outcome.error_code, Some(ErrorCode::UnknownTool));
        assert_eq!(
            outcome.hint.as_deref(),
            Some("valid tools: edit_file, write_file")
        );
        assert_eq!(swe.registry().denial_count(), 1);
    }

    #[tokio::test]
    async fn an_unscoped_unknown_tool_is_also_an_unknown_tool_error() {
        let registry = scripted_registry(vec![read_tool(), edit_tool(vec![])]);
        let provider = MockProvider::new(vec![
            tool_call_response("bogus", json!({})),
            plain_response("CANNOT: x"),
        ]);
        let mut swe = SweLoop::new(provider, registry, "", slot(), limits());

        let run = swe.run("do it").await.unwrap();

        let outcome = run.ledger[0].outcome.as_ref().unwrap();
        assert_eq!(outcome.error_code, Some(ErrorCode::UnknownTool));
        assert_eq!(
            outcome.hint.as_deref(),
            Some("valid tools: edit_file, read_file")
        );
    }

    #[tokio::test]
    async fn the_observer_runs_once_per_turn_and_feedback_lands_on_the_last_edit_only() {
        let registry = scripted_registry(vec![edit_tool(vec![
            ok_outcome(json!({"replaced": 1})),
            ok_outcome(json!({"replaced": 2})),
        ])]);
        let observer = ScriptedObserver::new(vec![feedback("build: pass -> fail", false)]);
        let provider = MockProvider::new(vec![
            multi_call_response(vec![
                (
                    "edit_file",
                    json!({"path": "src/a.rs", "old_string": "a", "new_string": "b"}),
                ),
                (
                    "edit_file",
                    json!({"path": "src/b.rs", "old_string": "a", "new_string": "b"}),
                ),
            ]),
            plain_response("CANNOT: x"),
        ]);
        let mut swe =
            SweLoop::new(provider, registry, "", slot(), limits()).with_observer(observer.clone());

        let run = swe.run("do it").await.unwrap();

        assert_eq!(
            *observer.calls.lock().unwrap(),
            vec![vec!["src/a.rs".to_string(), "src/b.rs".to_string()]]
        );
        let first = run.ledger[0].outcome.as_ref().unwrap();
        let last = run.ledger[1].outcome.as_ref().unwrap();
        assert!(first.data.get("build").is_none());
        assert_eq!(last.data["build"], "build: pass -> fail");
        assert!(run.ledger[1].result.contains("build: pass -> fail"));
    }

    #[tokio::test]
    async fn the_observer_is_not_called_for_failed_edits_or_reads() {
        let registry = scripted_registry(vec![
            edit_tool(vec![err_outcome(ErrorCode::NoMatch, "nope")]),
            read_tool(),
        ]);
        let observer = ScriptedObserver::new(vec![]);
        let provider = MockProvider::new(vec![
            multi_call_response(vec![
                ("edit_file", edit_args()),
                ("read_file", json!({"path": "a"})),
            ]),
            plain_response("CANNOT: x"),
        ]);
        let mut swe =
            SweLoop::new(provider, registry, "", slot(), limits()).with_observer(observer.clone());

        swe.run("do it").await.unwrap();

        assert!(observer.calls.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_model_call_ends_inactive_at_the_window() {
        let registry = scripted_registry(vec![read_tool()]);
        let provider = DelayedProvider::new(
            Duration::from_secs(86_400),
            vec![plain_response("never arrives")],
        );
        let window = Duration::from_secs(100);
        let mut swe = SweLoop::new(
            provider,
            registry,
            "",
            slot(),
            LoopLimits {
                max_iterations: 5,
                max_inactivity: Some(window),
            },
        );

        let run = swe.run("do it").await.unwrap();

        match run.outcome {
            LoopOutcome::Inactive { elapsed, window: w } => {
                assert_eq!(w, window);
                assert!(elapsed >= window);
                assert!(elapsed < window + Duration::from_secs(5));
            }
            other => panic!("expected inactive, got {other:?}"),
        }
    }

    fn windowed(window: u64) -> LoopLimits {
        LoopLimits {
            max_iterations: 20,
            max_inactivity: Some(Duration::from_secs(window)),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn reads_never_reset_the_inactivity_clock() {
        let finish = slot();
        let registry = scripted_registry(vec![read_tool(), finish_tool(&finish)]);
        let provider = DelayedProvider::new(
            Duration::from_secs(60),
            vec![
                tool_call_response("read_file", json!({"path": "a"})),
                tool_call_response("finish", json!({})),
            ],
        );
        let mut swe = SweLoop::new(provider, registry, "", finish, windowed(100));

        let run = swe.run("do it").await.unwrap();

        assert!(matches!(run.outcome, LoopOutcome::Inactive { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_edit_does_not_reset_the_inactivity_clock() {
        let finish = slot();
        let registry = scripted_registry(vec![
            edit_tool(vec![err_outcome(ErrorCode::NoMatch, "nope")]),
            finish_tool(&finish),
        ]);
        let provider = DelayedProvider::new(
            Duration::from_secs(60),
            vec![
                tool_call_response("edit_file", edit_args()),
                tool_call_response("finish", json!({})),
            ],
        );
        let mut swe = SweLoop::new(provider, registry, "", finish, windowed(100));

        let run = swe.run("do it").await.unwrap();

        assert!(matches!(run.outcome, LoopOutcome::Inactive { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn a_successful_content_changing_edit_resets_the_inactivity_clock() {
        let finish = slot();
        let registry = scripted_registry(vec![
            edit_tool(vec![ok_outcome(json!({"replaced": 1}))]),
            finish_tool(&finish),
        ]);
        let provider = DelayedProvider::new(
            Duration::from_secs(60),
            vec![
                tool_call_response("edit_file", edit_args()),
                tool_call_response("finish", json!({})),
            ],
        );
        let mut swe = SweLoop::new(provider, registry, "", finish, windowed(100));

        let run = swe.run("do it").await.unwrap();

        assert_eq!(run.outcome, LoopOutcome::Completed(claim()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_no_change_edit_does_not_reset_but_an_improved_build_does() {
        let unchanged = || ok_outcome(json!({"changed": false}));

        let finish = slot();
        let registry = scripted_registry(vec![edit_tool(vec![unchanged()]), finish_tool(&finish)]);
        let provider = DelayedProvider::new(
            Duration::from_secs(60),
            vec![
                tool_call_response("edit_file", edit_args()),
                tool_call_response("finish", json!({})),
            ],
        );
        let mut swe = SweLoop::new(provider, registry, "", finish, windowed(100));
        let without = swe.run("do it").await.unwrap();
        assert!(matches!(without.outcome, LoopOutcome::Inactive { .. }));

        let finish = slot();
        let registry = scripted_registry(vec![edit_tool(vec![unchanged()]), finish_tool(&finish)]);
        let provider = DelayedProvider::new(
            Duration::from_secs(60),
            vec![
                tool_call_response("edit_file", edit_args()),
                tool_call_response("finish", json!({})),
            ],
        );
        let observer = ScriptedObserver::new(vec![feedback("build: fail -> pass", true)]);
        let mut swe =
            SweLoop::new(provider, registry, "", finish, windowed(100)).with_observer(observer);
        let with = swe.run("do it").await.unwrap();
        assert_eq!(with.outcome, LoopOutcome::Completed(claim()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_disabled_window_never_expires() {
        let finish = slot();
        let registry = scripted_registry(vec![finish_tool(&finish)]);
        let provider = DelayedProvider::new(
            Duration::from_secs(1_000_000),
            vec![tool_call_response("finish", json!({}))],
        );
        let mut swe = SweLoop::new(provider, registry, "", finish, limits());

        let run = swe.run("do it").await.unwrap();

        assert_eq!(run.outcome, LoopOutcome::Completed(claim()));
    }

    #[tokio::test]
    async fn max_iterations_ends_the_loop() {
        let registry = scripted_registry(vec![read_tool()]);
        let provider = MockProvider::new(vec![
            tool_call_response("read_file", json!({})),
            tool_call_response("read_file", json!({})),
            tool_call_response("read_file", json!({})),
        ]);
        let mut swe = SweLoop::new(
            provider,
            registry,
            "",
            slot(),
            LoopLimits {
                max_iterations: 2,
                max_inactivity: None,
            },
        );

        let run = swe.run("do it").await.unwrap();

        assert_eq!(run.outcome, LoopOutcome::MaxIterations);
        assert_eq!(run.iterations, 2);
    }

    #[tokio::test]
    async fn a_model_failure_is_an_error() {
        let registry = scripted_registry(vec![read_tool()]);
        let provider = MockProvider::new(vec![]);
        let mut swe = SweLoop::new(provider, registry, "", slot(), limits());

        assert!(matches!(swe.run("x").await, Err(SweError::Model(_))));
    }

    #[test]
    fn resolve_inactivity_defaults_to_the_card_default() {
        assert_eq!(
            resolve_inactivity(None, None, None),
            Some(Duration::from_secs(2700))
        );
    }

    #[test]
    fn resolve_inactivity_prefers_the_card_over_the_env() {
        assert_eq!(
            resolve_inactivity(Some(600), Some("900"), None),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            resolve_inactivity(None, Some("900"), None),
            Some(Duration::from_secs(900))
        );
    }

    #[test]
    fn resolve_inactivity_ignores_an_unparseable_env_value() {
        assert_eq!(
            resolve_inactivity(None, Some("soon"), None),
            Some(Duration::from_secs(2700))
        );
    }

    #[test]
    fn resolve_inactivity_zero_disables() {
        assert_eq!(resolve_inactivity(Some(0), None, None), None);
        assert_eq!(resolve_inactivity(None, Some("0"), None), None);
    }

    #[test]
    fn a_task_override_may_only_lower() {
        assert_eq!(
            resolve_inactivity(Some(600), None, Some(60)),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            resolve_inactivity(Some(600), None, Some(6000)),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            resolve_inactivity(Some(600), None, Some(0)),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            resolve_inactivity(Some(0), None, Some(30)),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn the_no_progress_text_names_the_card_field_env_var_and_task_argument() {
        let text = no_progress_classification(Duration::from_secs(2701), Duration::from_secs(2700));

        assert!(text.contains("max_inactivity_secs"));
        assert!(text.contains("NANNA_MAX_INACTIVITY_SECS"));
        assert!(text.contains("assign_task"));
        assert!(text.contains("2701"));
        assert!(text.contains("2700"));
    }
}
