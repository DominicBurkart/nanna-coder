use super::tool_output::head_tail;
use model::config::OllamaConfig;
use model::types::{ChatMessage, MessageRole};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub(super) const CHARS_PER_TOKEN: usize = 3;
const REPLY_RESERVE_TOKENS: u32 = 2_048;
const KEPT_RESULTS: usize = 4;
const PLACEHOLDER_ARGS_CHARS: usize = 200;
const PLACEHOLDER_VALUE_CHARS: usize = 80;
const MIN_CUT_CHARS: usize = 500;
const CUT_MARKER_ALLOWANCE: usize = 64;

pub fn context_budget_chars_from(model: &str, lookup: impl Fn(&str) -> Option<String>) -> usize {
    let config = OllamaConfig::from_lookup(lookup).unwrap_or_else(|error| {
        tracing::error!(%error, "ignoring invalid model settings for the context budget");
        OllamaConfig::default()
    });
    let tokens = config
        .num_ctx_for(model)
        .saturating_sub(REPLY_RESERVE_TOKENS);
    tokens as usize * CHARS_PER_TOKEN
}

pub fn context_budget_chars_from_env(model: &str) -> usize {
    context_budget_chars_from(model, |key| std::env::var(key).ok())
}

#[derive(Debug, Default)]
pub(super) struct HistoryFit {
    plan: Option<usize>,
    masked: HashSet<usize>,
    dropped: HashSet<usize>,
    cut: HashMap<usize, String>,
}

pub(super) struct Fitted {
    pub(super) messages: Vec<ChatMessage>,
    pub(super) chars: usize,
}

pub(super) fn message_chars(message: &ChatMessage) -> usize {
    let content = message.content.as_deref().map_or(0, |c| c.chars().count());
    let calls = message
        .tool_calls
        .as_ref()
        .and_then(|calls| serde_json::to_string(calls).ok())
        .map_or(0, |text| text.chars().count());
    content + calls
}

fn describe_arguments(arguments: &serde_json::Value) -> String {
    let Some(object) = arguments.as_object() else {
        let text = arguments.to_string();
        return text.chars().take(PLACEHOLDER_ARGS_CHARS).collect();
    };
    let mut pairs: Vec<(usize, String)> = object
        .iter()
        .map(|(key, value)| {
            let text = value.to_string();
            let mut short: String = text.chars().take(PLACEHOLDER_VALUE_CHARS).collect();
            if short.len() < text.len() {
                short.push_str("...");
            }
            (text.len(), format!("{key}={short}"))
        })
        .collect();
    pairs.sort();
    let joined: Vec<String> = pairs.into_iter().map(|(_, pair)| pair).collect();
    joined
        .join(", ")
        .chars()
        .take(PLACEHOLDER_ARGS_CHARS)
        .collect()
}

fn placeholders(history: &[ChatMessage]) -> HashMap<usize, String> {
    let mut unanswered: Vec<(&str, String)> = Vec::new();
    let mut found = HashMap::new();
    for (index, message) in history.iter().enumerate() {
        for call in message.tool_calls.iter().flatten() {
            let args = describe_arguments(&call.function.arguments);
            let label = format!("{}({args})", call.function.name);
            unanswered.push((&call.id, label));
        }
        if message.role != MessageRole::Tool {
            continue;
        }
        let position = unanswered
            .iter()
            .position(|(id, _)| message.tool_call_id.as_deref() == Some(*id));
        let label = match position {
            Some(position) => unanswered.remove(position).1,
            None => "an earlier call".to_string(),
        };
        found.insert(
            index,
            format!(
                "[result of {label} removed to save context; call it again if you still need it]"
            ),
        );
    }
    found
}

fn turns(history: &[ChatMessage]) -> Vec<Range<usize>> {
    let mut turns: Vec<Range<usize>> = Vec::new();
    for (index, message) in history.iter().enumerate() {
        match turns.last_mut() {
            Some(turn) if message.role == MessageRole::Tool => turn.end = index + 1,
            _ => turns.push(index..index + 1),
        }
    }
    turns
}

impl HistoryFit {
    pub(super) fn pin_plan(&mut self, index: usize) {
        self.plan = Some(index);
    }

    pub(super) fn fit(&mut self, history: &[ChatMessage], budget: usize) -> Fitted {
        let placeholders = placeholders(history);
        self.mask_old_results(history, &placeholders, budget);
        self.drop_old_turns(history, &placeholders, budget);
        self.cut_large_results(history, &placeholders, budget);
        let messages = self.view(history, &placeholders);
        let chars = messages.iter().map(message_chars).sum();
        if chars > budget {
            tracing::warn!(
                chars,
                budget,
                "the history does not fit the context budget and nothing more can be cut"
            );
        }
        Fitted { messages, chars }
    }

    fn shown(
        &self,
        history: &[ChatMessage],
        placeholders: &HashMap<usize, String>,
        index: usize,
    ) -> ChatMessage {
        let mut message = history[index].clone();
        if self.masked.contains(&index) {
            message.content = placeholders.get(&index).cloned();
        } else if let Some(cut) = self.cut.get(&index) {
            message.content = Some(cut.clone());
        }
        message
    }

    fn view(
        &self,
        history: &[ChatMessage],
        placeholders: &HashMap<usize, String>,
    ) -> Vec<ChatMessage> {
        (0..history.len())
            .filter(|index| !self.dropped.contains(index))
            .map(|index| self.shown(history, placeholders, index))
            .collect()
    }

    fn excess(
        &self,
        history: &[ChatMessage],
        placeholders: &HashMap<usize, String>,
        budget: usize,
    ) -> usize {
        let chars: usize = self
            .view(history, placeholders)
            .iter()
            .map(message_chars)
            .sum();
        chars.saturating_sub(budget)
    }

    fn results(&self, history: &[ChatMessage]) -> Vec<usize> {
        (0..history.len())
            .filter(|index| history[*index].role == MessageRole::Tool)
            .filter(|index| !self.dropped.contains(index))
            .collect()
    }

    fn mask_old_results(
        &mut self,
        history: &[ChatMessage],
        placeholders: &HashMap<usize, String>,
        budget: usize,
    ) {
        let results = self.results(history);
        let old = results.len().saturating_sub(KEPT_RESULTS);
        for &index in &results[..old] {
            if self.excess(history, placeholders, budget) == 0 {
                return;
            }
            let current = message_chars(&self.shown(history, placeholders, index));
            let shorter = placeholders
                .get(&index)
                .is_some_and(|text| text.chars().count() < current);
            if shorter {
                self.masked.insert(index);
            }
        }
    }

    fn drop_old_turns(
        &mut self,
        history: &[ChatMessage],
        placeholders: &HashMap<usize, String>,
        budget: usize,
    ) {
        let results = self.results(history);
        let kept = &results[results.len().saturating_sub(KEPT_RESULTS)..];
        let task = history.iter().position(|m| m.role == MessageRole::User);
        let mut turns = turns(history);
        turns.pop();
        for turn in turns {
            if self.excess(history, placeholders, budget) == 0 {
                return;
            }
            let needed = turn.clone().any(|index| {
                history[index].role == MessageRole::System
                    || Some(index) == task
                    || Some(index) == self.plan
                    || kept.contains(&index)
            });
            if !needed {
                self.dropped.extend(turn);
            }
        }
    }

    fn cut_large_results(
        &mut self,
        history: &[ChatMessage],
        placeholders: &HashMap<usize, String>,
        budget: usize,
    ) {
        loop {
            let excess = self.excess(history, placeholders, budget);
            if excess == 0 {
                return;
            }
            let largest = self
                .results(history)
                .into_iter()
                .filter(|index| !self.masked.contains(index))
                .map(|index| {
                    let shown = self.shown(history, placeholders, index);
                    (message_chars(&shown), index)
                })
                .filter(|(chars, _)| *chars > MIN_CUT_CHARS + CUT_MARKER_ALLOWANCE)
                .max();
            let Some((chars, index)) = largest else {
                return;
            };
            let target = chars
                .saturating_sub(excess + CUT_MARKER_ALLOWANCE)
                .max(MIN_CUT_CHARS);
            let original = history[index].content.as_deref().unwrap_or_default();
            self.cut.insert(index, head_tail(original, target));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentConfig, AgentContext, AgentLoop};
    use crate::entities::InMemoryEntityStore;
    use crate::tools::{EchoTool, ToolRegistry};
    use async_trait::async_trait;
    use model::provider::{ModelError, ModelProvider, ModelResult};
    use model::types::{
        ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, MessageRole, ModelInfo,
        ToolCall,
    };
    use std::sync::{Arc, Mutex};

    fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args,
            },
        }
    }

    fn assistant_calls(calls: Vec<ToolCall>) -> ChatMessage {
        ChatMessage {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(calls),
            tool_call_id: None,
        }
    }

    fn result_of(letter: char, size: usize) -> String {
        std::iter::repeat_n(letter, size).collect()
    }

    fn read_turn(history: &mut Vec<ChatMessage>, letter: char, size: usize) {
        let args = serde_json::json!({"path": format!("{letter}.rs")});
        history.push(assistant_calls(vec![call("call_0", "read_file", args)]));
        history.push(ChatMessage::tool_response(
            "call_0",
            result_of(letter, size),
        ));
    }

    fn conversation(turns: &str, size: usize) -> Vec<ChatMessage> {
        let mut history = vec![
            ChatMessage::system("SYSTEM"),
            ChatMessage::user("TASK"),
            ChatMessage::user("PLAN"),
        ];
        for letter in turns.chars() {
            read_turn(&mut history, letter, size);
        }
        history
    }

    fn pinned() -> HistoryFit {
        let mut fit = HistoryFit::default();
        fit.pin_plan(2);
        fit
    }

    fn tool_contents(messages: &[ChatMessage]) -> Vec<String> {
        messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .filter_map(|m| m.content.clone())
            .collect()
    }

    fn size_of(messages: &[ChatMessage]) -> usize {
        messages
            .iter()
            .map(|m| {
                m.content.as_deref().map_or(0, |c| c.chars().count())
                    + m.tool_calls.as_ref().map_or(0, |calls| {
                        serde_json::to_string(calls).unwrap().chars().count()
                    })
            })
            .sum()
    }

    fn assert_every_result_follows_its_call(messages: &[ChatMessage]) {
        let mut open = 0;
        for message in messages {
            if message.role == MessageRole::Tool {
                assert!(open > 0, "orphan tool result: {messages:?}");
                open -= 1;
            } else {
                assert_eq!(open, 0, "unanswered tool call: {messages:?}");
                open = message.tool_calls.as_ref().map_or(0, Vec::len);
            }
        }
    }

    #[test]
    fn budget_follows_num_ctx() {
        let none = |_: &str| None;
        assert_eq!(context_budget_chars_from("qwen2.5:0.5b", none), 43_008);
        assert_eq!(context_budget_chars_from("gemma4:e4b", none), 190_464);
        let set = |value: &'static str| {
            move |key: &str| (key == "NANNA_NUM_CTX").then(|| value.to_string())
        };
        assert_eq!(
            context_budget_chars_from("qwen2.5:0.5b", set("32768")),
            92_160
        );
        assert_eq!(
            context_budget_chars_from("gemma4:e4b", set("8192")),
            190_464
        );
        assert_eq!(context_budget_chars_from("qwen2.5:0.5b", set("2048")), 0);
        assert_eq!(
            context_budget_chars_from("qwen2.5:0.5b", set("nope")),
            43_008
        );
    }

    #[test]
    fn a_history_inside_the_budget_is_sent_unchanged() {
        let history = conversation("abcdef", 100);
        let fitted = pinned().fit(&history, 100_000);
        assert_eq!(
            serde_json::to_value(&fitted.messages).unwrap(),
            serde_json::to_value(&history).unwrap()
        );
        assert_eq!(fitted.chars, size_of(&history));
    }

    #[test]
    fn context_fit_keeps_system_task_and_plan_messages() {
        let history = conversation("abcdefgh", 1_000);
        for budget in [6_000, 2_000, 0] {
            let fitted = pinned().fit(&history, budget);
            let head: Vec<&str> = fitted
                .messages
                .iter()
                .take(3)
                .filter_map(|m| m.content.as_deref())
                .collect();
            assert_eq!(head, ["SYSTEM", "TASK", "PLAN"], "budget {budget}");
            assert!(size_of(&fitted.messages) < size_of(&history));
            assert_every_result_follows_its_call(&fitted.messages);
        }
    }

    #[test]
    fn context_fit_masks_oldest_results_first_and_keeps_last_four() {
        let history = conversation("abcdefg", 1_000);
        let budget = size_of(&history) - 1_000;
        let fitted = pinned().fit(&history, budget);
        let results = tool_contents(&fitted.messages);
        assert_eq!(results.len(), 7);
        assert!(results[0].contains("read_file"), "{}", results[0]);
        assert!(results[0].contains("a.rs"), "{}", results[0]);
        assert!(results[0].len() < 300);
        assert!(results[1].contains("b.rs"), "{}", results[1]);
        assert_eq!(results[2], result_of('c', 1_000));
        let tight = pinned().fit(&history, 5_000);
        let results = tool_contents(&tight.messages);
        for (index, letter) in "abc".chars().enumerate() {
            assert!(results[index].contains(&format!("{letter}.rs")));
            assert!(results[index].len() < 300);
        }
        for (index, letter) in "defg".chars().enumerate() {
            assert_eq!(results[index + 3], result_of(letter, 1_000));
        }
        assert!(tight.chars <= 5_000);
        assert_eq!(tight.chars, size_of(&tight.messages));
    }

    #[test]
    fn masked_results_stay_masked_on_the_next_turn() {
        let mut fit = pinned();
        let mut history = conversation("abcdef", 1_000);
        let first = fit.fit(&history, size_of(&history) - 500);
        let masked = tool_contents(&first.messages)[0].clone();
        assert!(masked.contains("a.rs") && masked.len() < 300);
        read_turn(&mut history, 'g', 10);
        let second = fit.fit(&history, 1_000_000);
        let results = tool_contents(&second.messages);
        assert_eq!(results[0], masked);
        assert_eq!(results[1], result_of('b', 1_000));
    }

    #[test]
    fn a_placeholder_names_its_own_call_when_ids_repeat_and_bounds_the_arguments() {
        let mut history = conversation("", 0);
        history.push(assistant_calls(vec![
            call("call_0", "search", serde_json::json!({"pattern": "first"})),
            call(
                "call_0",
                "write_file",
                serde_json::json!({"path": "w.rs", "content": result_of('w', 5_000)}),
            ),
        ]));
        history.push(ChatMessage::tool_response("call_0", result_of('x', 2_000)));
        history.push(ChatMessage::tool_response("call_0", result_of('y', 2_000)));
        for letter in "abcd".chars() {
            read_turn(&mut history, letter, 10);
        }
        let fitted = pinned().fit(&history, size_of(&history) - 3_000);
        let results = tool_contents(&fitted.messages);
        assert!(results[0].contains("search") && results[0].contains("first"));
        assert!(results[1].contains("write_file") && results[1].contains("w.rs"));
        assert!(results[1].len() < 400, "{}", results[1].len());
    }

    #[test]
    fn context_fit_drops_whole_old_turns_once_masking_is_not_enough() {
        let history = conversation("abcdefghij", 200);
        let mut fit = pinned();
        let fitted = fit.fit(&history, 1_500);
        assert!(fitted.chars <= 1_500, "{}", fitted.chars);
        assert_every_result_follows_its_call(&fitted.messages);
        let results = tool_contents(&fitted.messages);
        assert!(results.len() < 10);
        assert_eq!(results.last().unwrap(), &result_of('j', 200));
        assert_eq!(results[results.len() - 4], result_of('g', 200));
        let again = fit.fit(&history, 1_000_000);
        assert_eq!(again.messages.len(), fitted.messages.len());
    }

    #[test]
    fn context_fit_cuts_an_oversized_result_when_nothing_else_is_left() {
        let history = conversation("a", 20_000);
        let mut fit = pinned();
        let fitted = fit.fit(&history, 5_000);
        assert!(fitted.chars <= 5_000, "{}", fitted.chars);
        let results = tool_contents(&fitted.messages);
        assert!(results[0].starts_with("aaaa") && results[0].ends_with("aaaa"));
        assert!(results[0].contains("characters cut"));
        let again = fit.fit(&history, 1_000_000);
        assert_eq!(tool_contents(&again.messages), results);
    }

    #[test]
    fn context_fit_terminates_when_the_protected_messages_alone_exceed_the_budget() {
        let history = conversation("abcde", 3_000);
        let fitted = pinned().fit(&history, 0);
        assert!(fitted.chars > 0);
        assert_every_result_follows_its_call(&fitted.messages);
        assert_eq!(tool_contents(&fitted.messages).len(), 4);
        assert!(tool_contents(&fitted.messages)
            .iter()
            .all(|r| r.len() < 600));
    }

    struct Recorder {
        script: Mutex<Vec<ChatResponse>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    #[async_trait]
    impl ModelProvider for Recorder {
        async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
            self.requests.lock().unwrap().push(request);
            let mut script = self.script.lock().unwrap();
            if script.is_empty() {
                return Err(ModelError::Unknown {
                    message: "script exhausted".to_string(),
                });
            }
            Ok(script.remove(0))
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "recorder"
        }
    }

    fn reply(message: ChatMessage, finish_reason: FinishReason) -> ChatResponse {
        ChatResponse {
            choices: vec![Choice {
                message,
                finish_reason: Some(finish_reason),
            }],
            usage: None,
        }
    }

    fn says(text: &str) -> ChatResponse {
        reply(ChatMessage::assistant(text), FinishReason::Stop)
    }

    fn echoes(letter: char) -> ChatResponse {
        let args = serde_json::json!({"message": result_of(letter, 1_000)});
        reply(
            assistant_calls(vec![call("call_0", "echo", args)]),
            FinishReason::ToolCalls,
        )
    }

    async fn requests_of(state_machine: bool, budget: Option<usize>) -> Vec<ChatRequest> {
        let mut script: Vec<ChatResponse> = "abcdefgh".chars().map(echoes).collect();
        script.push(says("done"));
        if state_machine {
            script.insert(0, says("PLAN"));
            script.push(says("COMPLETE"));
        }
        let provider = Arc::new(Recorder {
            script: Mutex::new(script),
            requests: Mutex::new(Vec::new()),
        });
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let config = AgentConfig {
            system_prompt: "SYSTEM".to_string(),
            context_budget_chars: budget,
            ..Default::default()
        };
        let mut agent = AgentLoop::with_tools(
            config,
            InMemoryEntityStore::new(),
            provider.clone(),
            registry,
        );
        let context = AgentContext {
            user_prompt: "TASK".to_string(),
            conversation_history: vec![ChatMessage::user("TASK")],
            app_state_id: "fit".to_string(),
        };
        let result = if state_machine {
            agent.run(context).await
        } else {
            agent.run_tool_loop(context).await
        };
        let result = result.unwrap();
        assert!(result
            .tool_calls_made
            .iter()
            .all(|record| !record.result.contains("removed to save context")));
        let requests = provider.requests.lock().unwrap();
        requests
            .iter()
            .filter(|r| r.tools.is_some())
            .cloned()
            .collect()
    }

    #[tokio::test]
    async fn both_loops_send_a_fitted_history_and_keep_the_full_one() {
        for state_machine in [false, true] {
            let schemas =
                serde_json::to_string(&vec![crate::tools::Tool::definition(&EchoTool::new())])
                    .unwrap()
                    .chars()
                    .count();
            let budget = 14_000 + schemas;
            let requests = requests_of(state_machine, Some(budget)).await;
            assert_eq!(requests.len(), 9);
            for request in &requests {
                assert!(
                    size_of(&request.messages) + schemas <= budget,
                    "state_machine={state_machine}: {}",
                    size_of(&request.messages)
                );
            }
            let last = &requests[8].messages;
            assert!(tool_contents(last)[0].contains("removed to save context"));
            assert!(tool_contents(last)[7].len() > 1_000);
            if state_machine {
                assert!(last
                    .iter()
                    .any(|m| m.content.as_deref().is_some_and(|c| c.ends_with("PLAN"))));
            }
        }
    }

    #[tokio::test]
    async fn the_tool_schemas_count_against_the_budget() {
        let unfitted = requests_of(false, None).await;
        let history_alone = size_of(&unfitted[8].messages);
        assert!(!tool_contents(&unfitted[8].messages)[0].contains("removed"));
        let fitted = requests_of(false, Some(history_alone + 10)).await;
        assert!(tool_contents(&fitted[8].messages)[0].contains("removed to save context"));
    }
}
