use crate::tool_contract::ToolOutcome;
use serde_json::Value;

const CONTENT_KEY: &str = "content";

#[derive(Default)]
struct Rendered {
    header: Vec<String>,
    blocks: Vec<(String, String)>,
}

impl Rendered {
    fn add(&mut self, key: &str, value: &Value) {
        match value {
            Value::Null => {}
            Value::String(text) if text.contains('\n') || key == CONTENT_KEY => {
                self.blocks.push((key.to_string(), text.clone()));
            }
            Value::Array(items) if items.is_empty() => {
                self.header.push(format!("{key}: (none)"));
            }
            Value::Array(items) => {
                let lines: Vec<String> = items.iter().map(line_of_item).collect();
                self.blocks.push((key.to_string(), lines.join("\n")));
            }
            Value::Object(map) => {
                for (inner, value) in map {
                    self.add(&format!("{key}.{inner}"), value);
                }
            }
            scalar => self.header.push(format!("{key}: {}", plain(scalar))),
        }
    }
}

fn plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn line_of_item(item: &Value) -> String {
    match item {
        Value::Object(map) => map
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| format!("{key}={}", plain(value)))
            .collect::<Vec<_>>()
            .join(" "),
        other => plain(other),
    }
}

fn status_line(outcome: &ToolOutcome) -> String {
    if outcome.ok {
        return "ok".to_string();
    }
    let code = outcome
        .error_code
        .and_then(|code| serde_json::to_value(code).ok())
        .map(|code| format!(" {}", plain(&code)))
        .unwrap_or_default();
    format!("error{code}: {}", outcome.message)
}

pub fn render_for_model(outcome: &ToolOutcome) -> String {
    let mut rendered = Rendered::default();
    rendered.header.push(status_line(outcome));
    if let Some(hint) = &outcome.hint {
        rendered.header.push(format!("hint: {hint}"));
    }
    match &outcome.data {
        Value::Object(map) => {
            for (key, value) in map {
                rendered.add(key, value);
            }
        }
        Value::Null => {}
        other => rendered.add("result", other),
    }
    rendered
        .blocks
        .sort_by_key(|(key, _)| (key == CONTENT_KEY, key.clone()));
    let mut parts = rendered.header;
    for (key, text) in rendered.blocks {
        parts.push(format!("----- {key} -----\n{text}"));
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_contract::{ErrorCode, ToolOutcome};
    use serde_json::json;

    const SOURCE: &str = "fn main() {\n    println!(\"a\\n{}\", \"b\");\n}";

    #[test]
    fn file_content_reaches_the_model_byte_for_byte_as_the_last_block() {
        let outcome = ToolOutcome::success(json!({
            "path": "src/main.rs",
            "total_lines": 3,
            "truncated": false,
            "content": SOURCE,
        }));

        let text = render_for_model(&outcome);

        assert!(text.ends_with(&format!("----- content -----\n{SOURCE}")));
        assert!(text.starts_with("ok\n"));
        assert!(text.contains("\npath: src/main.rs\n"));
        assert!(text.contains("\ntotal_lines: 3\n"));
        assert!(text.contains("\ntruncated: false\n"));
    }

    #[test]
    fn a_single_line_of_content_is_still_a_raw_block() {
        let outcome = ToolOutcome::success(json!({"content": "say \"hi\""}));

        let text = render_for_model(&outcome);

        assert_eq!(text, "ok\n----- content -----\nsay \"hi\"");
    }

    #[test]
    fn a_failure_shows_its_code_message_and_hint_unescaped() {
        let outcome = ToolOutcome::failure(
            ErrorCode::AmbiguousMatch,
            "\"x\" occurs 18 times, starting on lines 1, 2",
        )
        .with_hint("pass line to choose one");

        let text = render_for_model(&outcome);

        assert_eq!(
            text,
            "error ambiguous_match: \"x\" occurs 18 times, starting on lines 1, 2\nhint: pass line to choose one"
        );
    }

    #[test]
    fn text_lists_are_one_raw_line_each() {
        let outcome = ToolOutcome::success(json!({
            "matches": ["a.rs:1:let s = \"x\";", "a.rs:2:let t = \"y\";"],
            "truncated": true,
        }));

        let text = render_for_model(&outcome);

        assert_eq!(
            text,
            "ok\ntruncated: true\n----- matches -----\na.rs:1:let s = \"x\";\na.rs:2:let t = \"y\";"
        );
    }

    #[test]
    fn records_are_one_line_of_fields_each_and_empty_lists_say_none() {
        let outcome = ToolOutcome::success(json!({
            "entries": [{"name": "src", "kind": "dir"}, {"name": "a.rs", "kind": "file"}],
            "diagnostics": [],
        }));

        let text = render_for_model(&outcome);

        assert_eq!(
            text,
            "ok\ndiagnostics: (none)\n----- entries -----\nkind=dir name=src\nkind=file name=a.rs"
        );
    }

    #[test]
    fn multi_line_values_such_as_build_feedback_become_blocks_before_content() {
        let outcome = ToolOutcome::success(json!({
            "build": "build: pass->fail (1 errors, 1 new)\nsrc/a.rs:3 expected `;`",
            "content": "x",
            "path": "src/a.rs",
        }));

        let text = render_for_model(&outcome);

        assert_eq!(
            text,
            "ok\npath: src/a.rs\n----- build -----\nbuild: pass->fail (1 errors, 1 new)\nsrc/a.rs:3 expected `;`\n----- content -----\nx"
        );
    }

    #[test]
    fn nested_objects_are_flattened_and_non_object_data_is_kept() {
        let nested = ToolOutcome::success(json!({"result": {"lines": 2}}));
        let bare = ToolOutcome::success(json!("line one\nline two"));
        let empty = ToolOutcome::success(json!(null));

        assert_eq!(render_for_model(&nested), "ok\nresult.lines: 2");
        assert_eq!(
            render_for_model(&bare),
            "ok\n----- result -----\nline one\nline two"
        );
        assert_eq!(render_for_model(&empty), "ok");
    }
}
