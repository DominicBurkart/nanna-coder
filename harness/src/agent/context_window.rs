use model::types::{ChatMessage, MessageRole};

pub const DEFAULT_CONTEXT_CHARS: usize = 36_000;
pub const DROPPED_RESULT: &str =
    "[earlier result dropped to fit the context window; call the tool again if you need it]";
pub const CUT_RESULT: &str =
    "[result cut to fit the context window; ask for a smaller range or a narrower search]";

fn message_chars(message: &ChatMessage) -> usize {
    let content = message
        .content
        .as_deref()
        .map_or(0, |text| text.chars().count());
    let calls = message.tool_calls.as_ref().map_or(0, |calls| {
        calls
            .iter()
            .map(|call| {
                call.function.name.chars().count()
                    + call.function.arguments.to_string().chars().count()
            })
            .sum()
    });
    content + calls
}

pub fn conversation_chars(messages: &[ChatMessage]) -> usize {
    messages.iter().map(message_chars).sum()
}

fn turn_starts(messages: &[ChatMessage]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == MessageRole::Assistant)
        .map(|(index, _)| index)
        .collect()
}

pub fn fit_context(messages: &[ChatMessage], budget: usize) -> Vec<ChatMessage> {
    let mut sent = messages.to_vec();
    let Some(&latest) = turn_starts(&sent).last() else {
        return sent;
    };

    for index in 0..latest {
        if conversation_chars(&sent) <= budget {
            return sent;
        }
        let message = &mut sent[index];
        if message.role == MessageRole::Tool && message_chars(message) > DROPPED_RESULT.len() {
            message.content = Some(DROPPED_RESULT.to_string());
        }
    }

    while conversation_chars(&sent) > budget {
        let starts = turn_starts(&sent);
        if starts.len() < 2 {
            break;
        }
        sent.drain(starts[0]..starts[1]);
    }

    cut_latest_results(&mut sent, budget);
    sent
}

fn cut_latest_results(sent: &mut [ChatMessage], budget: usize) {
    let total = conversation_chars(sent);
    if total <= budget {
        return;
    }
    let Some(&latest) = turn_starts(sent).last() else {
        return;
    };
    let results: Vec<usize> = (latest..sent.len())
        .filter(|&index| sent[index].role == MessageRole::Tool)
        .collect();
    if results.is_empty() {
        return;
    }
    let in_results: usize = results.iter().map(|&i| message_chars(&sent[i])).sum();
    let share = budget.saturating_sub(total - in_results) / results.len();
    let keep = share.saturating_sub(CUT_RESULT.chars().count());
    for index in results {
        if message_chars(&sent[index]) <= share {
            continue;
        }
        let content = sent[index].content.take().unwrap_or_default();
        let mut cut: String = content.chars().take(keep).collect();
        cut.push_str(CUT_RESULT);
        sent[index].content = Some(cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::{ChatMessage, FunctionCall, MessageRole, ToolCall};

    fn call(id: &str) -> ChatMessage {
        ChatMessage {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: id.to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "a"}),
                },
            }]),
            tool_call_id: None,
        }
    }

    fn conversation(results: &[usize]) -> Vec<ChatMessage> {
        let mut messages = vec![
            ChatMessage::system("SYSTEM RULES"),
            ChatMessage::user("THE TASK"),
        ];
        for (index, size) in results.iter().enumerate() {
            let id = format!("call_{index}");
            messages.push(call(&id));
            messages.push(ChatMessage::tool_response(&id, "x".repeat(*size)));
        }
        messages
    }

    fn contents(messages: &[ChatMessage]) -> Vec<String> {
        messages
            .iter()
            .map(|message| message.content.clone().unwrap_or_default())
            .collect()
    }

    fn assert_every_result_follows_its_call(messages: &[ChatMessage]) {
        let mut open: Vec<String> = Vec::new();
        for message in messages {
            if let Some(calls) = &message.tool_calls {
                open = calls.iter().map(|call| call.id.clone()).collect();
            }
            if message.role == MessageRole::Tool {
                let id = message.tool_call_id.clone().unwrap();
                assert!(open.contains(&id), "orphan tool result {id}");
            }
        }
    }

    #[test]
    fn a_conversation_within_budget_is_sent_unchanged() {
        let messages = conversation(&[100, 100]);

        let sent = fit_context(&messages, 10_000);

        assert_eq!(contents(&sent), contents(&messages));
    }

    #[test]
    fn old_tool_results_are_dropped_before_the_task_or_the_latest_result() {
        let messages = conversation(&[44_000, 500]);

        let sent = fit_context(&messages, 4_000);

        let text = contents(&sent);
        assert_eq!(text[0], "SYSTEM RULES");
        assert_eq!(text[1], "THE TASK");
        assert_eq!(text[3], DROPPED_RESULT);
        assert_eq!(text[5], "x".repeat(500));
        assert!(conversation_chars(&sent) <= 4_000);
    }

    #[test]
    fn whole_old_turns_go_when_stubs_alone_are_not_enough() {
        let messages = conversation(&[400; 80]);

        let sent = fit_context(&messages, 3_000);

        assert!(conversation_chars(&sent) <= 3_000);
        assert_eq!(contents(&sent)[0], "SYSTEM RULES");
        assert_eq!(contents(&sent)[1], "THE TASK");
        assert_eq!(contents(&sent).last().unwrap(), &"x".repeat(400));
        assert!(sent.len() < messages.len());
        assert_every_result_follows_its_call(&sent);
    }

    #[test]
    fn an_oversized_latest_result_is_cut_and_the_task_survives() {
        let messages = conversation(&[117_000]);

        let sent = fit_context(&messages, 4_000);

        let text = contents(&sent);
        assert_eq!(text[0], "SYSTEM RULES");
        assert_eq!(text[1], "THE TASK");
        assert!(text[3].ends_with(CUT_RESULT));
        assert!(conversation_chars(&sent) <= 4_000);
    }

    #[test]
    fn every_result_of_an_oversized_latest_turn_shares_the_room() {
        let mut messages = conversation(&[]);
        let mut turn = call("call_a");
        turn.tool_calls.as_mut().unwrap().push(ToolCall {
            id: "call_b".to_string(),
            function: FunctionCall {
                name: "grep".to_string(),
                arguments: serde_json::json!({}),
            },
        });
        messages.push(turn);
        messages.push(ChatMessage::tool_response("call_a", "a".repeat(30_000)));
        messages.push(ChatMessage::tool_response("call_b", "b".repeat(30_000)));

        let sent = fit_context(&messages, 4_000);

        let text = contents(&sent);
        assert!(text[3].starts_with('a') && text[3].ends_with(CUT_RESULT));
        assert!(text[4].starts_with('b') && text[4].ends_with(CUT_RESULT));
        assert!(conversation_chars(&sent) <= 4_000);
    }
}
