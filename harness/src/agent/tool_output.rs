use crate::tools::ToolResult;
use serde_json::Value;
use std::path::{Component, Path};

const READ_MAX_LINES: usize = 200;
const READ_MAX_CHARS: usize = 12_000;
const LIST_MAX_ENTRIES: usize = 200;
const LIST_HIDDEN_DIRS: &[&str] = &["target", ".git"];
const SEARCH_MAX_LINE_CHARS: usize = 200;
const SEARCH_MAX_CHARS: usize = 8_000;
const SEARCH_ENVELOPE_CHARS: usize = 400;
const OTHER_MAX_CHARS: usize = 8_000;

pub(crate) fn render(tool: &str, outcome: &ToolResult<Value>) -> String {
    match outcome {
        Ok(value) => {
            let bounded = match tool {
                "read_file" => bound_read(value),
                "list_directory" => bound_listing(value),
                "search" => bound_search(value),
                _ => None,
            };
            match bounded {
                Some(text) => text,
                None => head_tail(&value.to_string(), OTHER_MAX_CHARS),
            }
        }
        Err(error) => head_tail(&format!("Error: {error}"), OTHER_MAX_CHARS),
    }
}

pub(crate) fn head_tail(text: &str, limit: usize) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.to_string();
    }
    let tail_len = limit / 2;
    let head: String = text.chars().take(limit - tail_len).collect();
    let tail: String = text.chars().skip(total - tail_len).collect();
    format!("{head}\n[... {} characters cut ...]\n{tail}", total - limit)
}

fn bound_read(value: &Value) -> Option<String> {
    let content = value.get("content")?.as_str()?;
    let total = value.get("total_lines")?.as_u64()?;
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.len() <= READ_MAX_LINES && content.chars().count() <= READ_MAX_CHARS {
        return Some(value.to_string());
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    for line in lines.iter().take(READ_MAX_LINES) {
        let cost = line.chars().count() + usize::from(!kept.is_empty());
        if used + cost > READ_MAX_CHARS {
            break;
        }
        used += cost;
        kept.push(line);
    }
    let line_was_cut = kept.is_empty();
    let shown = if line_was_cut {
        kept.push(lines[0]);
        lines[0].chars().take(READ_MAX_CHARS).collect()
    } else {
        kept.join("\n")
    };
    let last: u64 = kept
        .last()?
        .split_whitespace()
        .next()
        .and_then(|number| number.parse().ok())?;
    let next = last + 1;
    let continuation = if next <= total {
        let end = total.min(next + READ_MAX_LINES as u64 - 1);
        format!("call read_file with start_line={next} end_line={end}")
    } else {
        format!("line {last} was cut at {READ_MAX_CHARS} characters; use search to inspect it")
    };
    let mut bounded = value.clone();
    bounded["content"] = Value::String(shown);
    bounded["lines_shown"] = kept.len().into();
    bounded["continuation"] = continuation.into();
    Some(bounded.to_string())
}

fn is_hidden_component(component: Component<'_>) -> bool {
    LIST_HIDDEN_DIRS
        .iter()
        .any(|hidden| component.as_os_str() == *hidden)
}

fn is_hidden_entry(entry: &Value) -> bool {
    let is_dir = entry["is_dir"].as_bool().unwrap_or(false);
    let Some(path) = entry["path"].as_str().or_else(|| entry["name"].as_str()) else {
        return false;
    };
    let mut components: Vec<Component<'_>> = Path::new(path).components().collect();
    if !is_dir {
        components.pop();
    }
    components.into_iter().any(is_hidden_component)
}

fn bound_listing(value: &Value) -> Option<String> {
    let entries = value.get("entries")?.as_array()?;
    let listed = value.get("path").and_then(Value::as_str).unwrap_or(".");
    let asked_for_hidden = Path::new(listed).components().any(is_hidden_component);
    let visible: Vec<&Value> = entries
        .iter()
        .filter(|entry| asked_for_hidden || !is_hidden_entry(entry))
        .collect();
    if visible.len() == entries.len() && visible.len() <= LIST_MAX_ENTRIES {
        return Some(value.to_string());
    }
    let total = visible.len();
    let shown: Vec<Value> = visible
        .into_iter()
        .take(LIST_MAX_ENTRIES)
        .cloned()
        .collect();
    let mut bounded = value.clone();
    bounded["count"] = shown.len().into();
    if shown.len() < total {
        bounded["total_entries"] = total.into();
        bounded["continuation"] = "list a subdirectory with recursive=false".into();
    }
    bounded["entries"] = Value::Array(shown);
    Some(bounded.to_string())
}

fn bound_search(value: &Value) -> Option<String> {
    let results = value.get("results")?.as_array()?;
    let mut shown: Vec<Value> = Vec::new();
    let mut used = SEARCH_ENVELOPE_CHARS;
    let mut cut_line = false;
    for result in results {
        let mut result = result.clone();
        if let Some(content) = result.get("content").and_then(Value::as_str) {
            if content.chars().count() > SEARCH_MAX_LINE_CHARS {
                let short: String = content.chars().take(SEARCH_MAX_LINE_CHARS).collect();
                result["content"] = short.into();
                cut_line = true;
            }
        }
        used += result.to_string().chars().count() + 1;
        if used > SEARCH_MAX_CHARS {
            break;
        }
        shown.push(result);
    }
    if !cut_line && shown.len() == results.len() {
        let text = value.to_string();
        return (text.chars().count() <= SEARCH_MAX_CHARS).then_some(text);
    }
    let mut bounded = value.clone();
    bounded["count"] = shown.len().into();
    bounded["total_matches"] = results.len().into();
    bounded["continuation"] = "narrow pattern or file_pattern".into();
    bounded["results"] = Value::Array(shown);
    let text = bounded.to_string();
    (text.chars().count() <= SEARCH_MAX_CHARS).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{create_tool_registry, ToolError};
    use serde_json::json;

    async fn rendered(root: &std::path::Path, tool: &str, args: Value) -> String {
        let outcome = create_tool_registry(root).execute(tool, args).await;
        assert!(outcome.is_ok(), "{outcome:?}");
        render(tool, &outcome)
    }

    fn parsed(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn numbered_file(root: &std::path::Path, name: &str, lines: usize, width: usize) {
        let body: String = (1..=lines)
            .map(|n| format!("line{n:05} {}\n", "x".repeat(width)))
            .collect();
        std::fs::write(root.join(name), body).unwrap();
    }

    #[tokio::test]
    async fn read_file_result_is_windowed_with_a_start_line_hint() {
        let dir = tempfile::tempdir().unwrap();
        numbered_file(dir.path(), "big.rs", 1000, 5);
        let out = parsed(&rendered(dir.path(), "read_file", json!({"path": "big.rs"})).await);
        let content = out["content"].as_str().unwrap();
        assert_eq!(content.lines().count(), 200);
        assert!(content.contains("line00001"));
        assert!(content.contains("line00200"));
        assert!(!content.contains("line00201"));
        assert_eq!(out["lines_shown"], 200);
        assert_eq!(out["total_lines"], 1000);
        assert_eq!(
            out["continuation"],
            "call read_file with start_line=201 end_line=400"
        );
    }

    #[tokio::test]
    async fn read_file_hint_continues_from_the_requested_range() {
        let dir = tempfile::tempdir().unwrap();
        numbered_file(dir.path(), "big.rs", 650, 5);
        let args = json!({"path": "big.rs", "start_line": 301, "end_line": 650});
        let out = parsed(&rendered(dir.path(), "read_file", args).await);
        assert_eq!(out["lines_shown"], 200);
        assert_eq!(
            out["continuation"],
            "call read_file with start_line=501 end_line=650"
        );
    }

    #[tokio::test]
    async fn read_file_window_stops_at_the_character_cap_on_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        numbered_file(dir.path(), "wide.rs", 150, 190);
        let out = parsed(&rendered(dir.path(), "read_file", json!({"path": "wide.rs"})).await);
        let content = out["content"].as_str().unwrap();
        assert!(content.chars().count() <= 12_000, "{}", content.len());
        let kept = out["lines_shown"].as_u64().unwrap();
        assert!(kept > 0 && kept < 150);
        assert!(content.ends_with(&"x".repeat(190)));
        assert_eq!(
            out["continuation"],
            format!("call read_file with start_line={} end_line=150", kept + 1)
        );
    }

    #[tokio::test]
    async fn read_file_cuts_one_oversized_line_and_moves_past_it() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("{}\nsecond\n", "é".repeat(30_000));
        std::fs::write(dir.path().join("min.js"), body).unwrap();
        let out = parsed(&rendered(dir.path(), "read_file", json!({"path": "min.js"})).await);
        assert_eq!(out["content"].as_str().unwrap().chars().count(), 12_000);
        assert_eq!(out["lines_shown"], 1);
        assert_eq!(
            out["continuation"],
            "call read_file with start_line=2 end_line=2"
        );
    }

    #[tokio::test]
    async fn short_results_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        numbered_file(dir.path(), "small.rs", 3, 1);
        for (tool, args) in [
            ("read_file", json!({"path": "small.rs"})),
            ("list_directory", json!({})),
            ("search", json!({"pattern": "line"})),
        ] {
            let outcome = create_tool_registry(dir.path()).execute(tool, args).await;
            let raw = outcome.as_ref().unwrap().to_string();
            assert_eq!(render(tool, &outcome), raw, "{tool}");
        }
    }

    #[tokio::test]
    async fn recursive_listing_skips_target_and_git_and_caps_entries() {
        let dir = tempfile::tempdir().unwrap();
        for sub in [
            "target/debug",
            ".git/objects",
            ".github",
            "src",
            "src/target",
        ] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        for file in [
            "target/debug/bin",
            ".git/objects/ab",
            "src/target/nested",
            ".github/ci.yml",
            "src/target_utils.rs",
        ] {
            std::fs::write(dir.path().join(file), "x").unwrap();
        }
        let args = json!({"recursive": true});
        let few = parsed(&rendered(dir.path(), "list_directory", args.clone()).await);
        let mut paths: Vec<&str> = few["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        paths.sort_unstable();
        assert_eq!(paths, [".github/ci.yml", "src/target_utils.rs"]);
        assert_eq!(few["count"], 2);
        for n in 0..300 {
            std::fs::write(dir.path().join(format!("src/f{n}.rs")), "x").unwrap();
        }
        let many = parsed(&rendered(dir.path(), "list_directory", args).await);
        assert_eq!(many["entries"].as_array().unwrap().len(), 200);
        assert_eq!(many["count"], 200);
        assert_eq!(many["total_entries"], 302);
        assert_eq!(
            many["continuation"],
            "list a subdirectory with recursive=false"
        );
    }

    #[tokio::test]
    async fn flat_listing_hides_target_and_git_but_an_explicit_listing_of_target_works() {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["target/debug", ".git", "src"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        std::fs::write(dir.path().join("target/debug/bin"), "x").unwrap();
        let top = parsed(&rendered(dir.path(), "list_directory", json!({})).await);
        assert_eq!(top["count"], 1);
        assert_eq!(top["entries"][0]["name"], "src");
        let args = json!({"path": "target", "recursive": true});
        let inside = parsed(&rendered(dir.path(), "list_directory", args).await);
        assert_eq!(inside["count"], 1);
    }

    #[tokio::test]
    async fn search_lines_and_total_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..50)
            .map(|_| format!("needle {}\n", "ü".repeat(1000)))
            .collect();
        std::fs::write(dir.path().join("a.txt"), body).unwrap();
        let text = rendered(dir.path(), "search", json!({"pattern": "needle"})).await;
        assert!(text.chars().count() <= 8_000, "{}", text.chars().count());
        let out = parsed(&text);
        let results = out["results"].as_array().unwrap();
        assert!(!results.is_empty() && results.len() < 50);
        for result in results {
            assert_eq!(result["content"].as_str().unwrap().chars().count(), 200);
        }
        assert_eq!(out["count"], results.len());
        assert_eq!(out["total_matches"], 50);
        assert_eq!(out["continuation"], "narrow pattern or file_pattern");
    }

    #[test]
    fn error_results_are_head_tail_truncated() {
        let message = format!("HEAD{}TAIL", "é".repeat(20_000));
        let outcome: ToolResult<Value> = Err(ToolError::ExecutionFailed { message });
        let full = format!("Error: {}", outcome.as_ref().unwrap_err());
        let text = render("read_file", &outcome);
        assert!(text.starts_with("Error: "));
        assert!(text.contains("HEAD"));
        assert!(text.ends_with("TAIL"));
        let cut = full.chars().count() - 8_000;
        assert!(text.contains(&format!("[... {cut} characters cut ...]")));
        assert!(text.chars().count() < 8_100);
    }

    #[test]
    fn other_tool_results_are_head_tail_truncated() {
        let outcome: ToolResult<Value> = Ok(json!({"diff": "d".repeat(50_000)}));
        let text = render("git_diff", &outcome);
        assert!(text.contains("characters cut"));
        assert!(text.chars().count() < 8_100);
    }
}
