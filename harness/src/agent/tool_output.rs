use crate::tools::ToolResult;
use serde_json::Value;
use std::path::{Component, Path};

const READ_MAX_LINES: usize = 200;
const READ_MAX_CHARS: usize = 12_000;
const LIST_MAX_ENTRIES: usize = 200;
const LIST_HIDDEN_DIRS: &[&str] = &["target", ".git"];
const SEARCH_MAX_LINE_CHARS: usize = 200;
const SEARCH_MAX_CHARS: usize = 8_000;
const SEARCH_HINT_CHARS: usize = 200;
const OTHER_MAX_CHARS: usize = 8_000;

pub(crate) fn render(tool: &str, outcome: &ToolResult<Value>) -> String {
    match outcome {
        Ok(value) => {
            let plain = match tool {
                "read_file" => plain_read(value),
                "list_directory" => plain_listing(value),
                "search" => plain_search(value),
                "edit_file" => plain_edit(value),
                "git_diff" => plain_diff(value),
                _ => plain_command(value),
            };
            match plain {
                Some(text) if matches!(tool, "read_file" | "list_directory") => text,
                Some(text) => head_tail(&text, OTHER_MAX_CHARS),
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

fn without_gutter(content: &str, count: usize) -> Option<(u64, Vec<&str>)> {
    let lines: Vec<&str> = content.split('\n').take(count).collect();
    let first: u64 = lines
        .first()?
        .trim_start()
        .split_once("  ")?
        .0
        .parse()
        .ok()?;
    let raw = lines
        .iter()
        .zip(first..)
        .map(|(line, number)| line.strip_prefix(&format!("{number:>6}  ")))
        .collect::<Option<Vec<&str>>>()?;
    Some((first, raw))
}

fn plain_read(value: &Value) -> Option<String> {
    let path = value.get("path")?.as_str()?;
    let content = value.get("content")?.as_str()?;
    let total = value.get("total_lines")?.as_u64()?;
    let count = value.get("lines_shown")?.as_u64()? as usize;
    if total == 0 {
        return Some(format!("{path} is empty (0 lines)."));
    }
    if count == 0 {
        return Some(format!(
            "{path} has {total} lines; none are in the requested range."
        ));
    }
    let (first, lines) = without_gutter(content, count)?;
    let mut kept = 0;
    let mut used = 0;
    for line in lines.iter().take(READ_MAX_LINES) {
        let cost = line.chars().count() + usize::from(kept > 0);
        if used + cost > READ_MAX_CHARS {
            break;
        }
        used += cost;
        kept += 1;
    }
    let line_was_cut = kept == 0;
    let (shown, kept) = if line_was_cut {
        (lines[0].chars().take(READ_MAX_CHARS).collect(), 1)
    } else {
        (lines[..kept].join("\n"), kept)
    };
    let last = first + kept as u64 - 1;
    let mut text = format!("{path}, lines {first}-{last} of {total}:\n{shown}");
    if line_was_cut || kept < lines.len() {
        let next = last + 1;
        let hint = if next <= total {
            let end = total.min(next + READ_MAX_LINES as u64 - 1);
            format!("call read_file with start_line={next} end_line={end}")
        } else {
            format!("line {last} was cut at {READ_MAX_CHARS} characters; use search to inspect it")
        };
        text.push_str(&format!("\n[{hint}]"));
    }
    Some(text)
}

fn plain_edit(value: &Value) -> Option<String> {
    let path = value.get("path")?.as_str()?;
    let replacements = value.get("replacements")?.as_u64()?;
    let mut text = format!("edited {path}: {replacements} replacement(s)");
    for side in ["before", "after"] {
        let snippet = value.get(side)?.as_str()?;
        if snippet.is_empty() {
            text.push_str(&format!("\n{side}: no lines"));
            continue;
        }
        let (first, lines) = without_gutter(snippet, usize::MAX)?;
        let last = first + lines.len() as u64 - 1;
        let shown = lines.join("\n");
        text.push_str(&format!("\n{side} (lines {first}-{last}):\n{shown}"));
    }
    Some(text)
}

fn plain_diff(value: &Value) -> Option<String> {
    let diff = value.get("diff")?.as_str()?;
    if !diff.is_empty() {
        return Some(diff.to_string());
    }
    let staged = value.get("staged")?.as_bool()?;
    let path = value.get("path")?.as_str()?;
    let kind = if staged { "staged" } else { "unstaged" };
    Some(format!("no {kind} changes in {path}"))
}

fn plain_command(value: &Value) -> Option<String> {
    let stdout = value.get("stdout")?.as_str()?;
    let stderr = value.get("stderr")?.as_str()?;
    let success = value.get("success")?.as_bool()?;
    let mut parts = Vec::new();
    if let Some(command) = value.get("command").and_then(Value::as_str) {
        parts.push(format!("$ {command}"));
    }
    parts.push(format!("success: {success}"));
    if value.get("timed_out").and_then(Value::as_bool) == Some(true) {
        parts.push("timed out: true".to_string());
    }
    for (label, output) in [("stdout", stdout), ("stderr", stderr)] {
        if !output.is_empty() {
            let output = output.strip_suffix('\n').unwrap_or(output);
            parts.push(format!("{label}:\n{output}"));
        }
    }
    Some(parts.join("\n"))
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

fn listed_path(listed: &str, entry: &Value) -> Option<String> {
    let mut path = match entry["path"].as_str() {
        Some(path) => path.to_string(),
        None => {
            let name = entry["name"].as_str()?;
            let base = listed.trim_start_matches("./").trim_end_matches('/');
            match base {
                "" | "." => name.to_string(),
                _ => format!("{base}/{name}"),
            }
        }
    };
    if entry["is_dir"].as_bool() == Some(true) {
        path.push('/');
    }
    Some(path)
}

fn plain_listing(value: &Value) -> Option<String> {
    let entries = value.get("entries")?.as_array()?;
    let listed = value.get("path").and_then(Value::as_str).unwrap_or(".");
    let asked_for_hidden = Path::new(listed).components().any(is_hidden_component);
    let visible: Vec<&Value> = entries
        .iter()
        .filter(|entry| asked_for_hidden || !is_hidden_entry(entry))
        .collect();
    if visible.is_empty() {
        return Some(format!("no entries in {listed}"));
    }
    let mut lines = visible
        .iter()
        .take(LIST_MAX_ENTRIES)
        .map(|entry| listed_path(listed, entry))
        .collect::<Option<Vec<String>>>()?;
    if lines.len() < visible.len() {
        lines.push(format!(
            "[showing {} of {} entries; list a subdirectory with recursive=false]",
            lines.len(),
            visible.len()
        ));
    }
    Some(lines.join("\n"))
}

fn plain_search(value: &Value) -> Option<String> {
    let results = value.get("results")?.as_array()?;
    if results.is_empty() {
        let pattern = value.get("pattern")?.as_str()?;
        return Some(format!("no matches for pattern `{pattern}`"));
    }
    let mut lines: Vec<String> = Vec::new();
    let mut used = SEARCH_HINT_CHARS;
    let mut cut_line = false;
    for result in results {
        let file = result.get("file")?.as_str()?;
        let number = result.get("line")?.as_u64()?;
        let content = result.get("content")?.as_str()?;
        let short: String = content.chars().take(SEARCH_MAX_LINE_CHARS).collect();
        let cut = short.len() < content.len();
        let line = format!("{file}:{number}: {short}");
        used += line.chars().count() + 1;
        if used > SEARCH_MAX_CHARS {
            break;
        }
        cut_line |= cut;
        lines.push(line);
    }
    if cut_line || lines.len() < results.len() {
        let cut_note = if cut_line {
            format!(", lines cut at {SEARCH_MAX_LINE_CHARS} characters")
        } else {
            String::new()
        };
        lines.push(format!(
            "[showing {} of {} matches{cut_note}; narrow pattern or file_pattern]",
            lines.len(),
            results.len()
        ));
    }
    Some(lines.join("\n"))
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

    fn numbered_file(root: &std::path::Path, name: &str, lines: usize, width: usize) {
        let body: String = (1..=lines)
            .map(|n| format!("line{n:05} {}\n", "x".repeat(width)))
            .collect();
        std::fs::write(root.join(name), body).unwrap();
    }

    fn hint(text: &str) -> &str {
        text.rsplit('\n').next().unwrap()
    }

    fn body(text: &str) -> Vec<&str> {
        let lines: Vec<&str> = text.split('\n').collect();
        let end = lines.len() - usize::from(hint(text).starts_with("[call "));
        lines[1..end].to_vec()
    }

    fn has_number_gutter(text: &str) -> bool {
        regex::Regex::new(r"(?m)^\s*\d+  ").unwrap().is_match(text)
    }

    const QUOTED: &str = "#[cfg(feature = \"cli\")]\nfn main() {\n    println!(\"a\\n\");\n\n}\n";

    #[tokio::test]
    async fn read_file_shows_a_header_and_the_file_bytes_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), QUOTED).unwrap();
        let text = rendered(dir.path(), "read_file", json!({"path": "main.rs"})).await;
        let shown = QUOTED.strip_suffix('\n').unwrap();
        assert_eq!(text, format!("main.rs, lines 1-5 of 5:\n{shown}"));
        assert!(!has_number_gutter(&text), "{text}");
    }

    #[tokio::test]
    async fn read_file_range_header_names_the_lines_shown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "one\n  two\n\n    4  four\nfive\n",
        )
        .unwrap();
        let args = json!({"path": "a.txt", "start_line": 2, "end_line": 4});
        let text = rendered(dir.path(), "read_file", args).await;
        assert_eq!(text, "a.txt, lines 2-4 of 5:\n  two\n\n    4  four");
    }

    #[tokio::test]
    async fn read_file_says_when_there_is_nothing_to_show() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();
        std::fs::write(dir.path().join("two.txt"), "a\nb\n").unwrap();
        let empty = rendered(dir.path(), "read_file", json!({"path": "empty.txt"})).await;
        assert_eq!(empty, "empty.txt is empty (0 lines).");
        let args = json!({"path": "two.txt", "start_line": 9});
        let past = rendered(dir.path(), "read_file", args).await;
        assert_eq!(
            past,
            "two.txt has 2 lines; none are in the requested range."
        );
    }

    #[tokio::test]
    async fn text_copied_from_a_crlf_read_edits_the_file_and_keeps_crlf() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("w.txt"), "say \"one\"\r\ntwo\r\nthree\r\n").unwrap();
        let text = rendered(dir.path(), "read_file", json!({"path": "w.txt"})).await;
        assert!(!text.contains('\r'), "{text:?}");
        let copied = body(&text)[..2].join("\n");
        let args = json!({"path": "w.txt", "old_string": copied, "new_string": "1\n2"});
        rendered(dir.path(), "edit_file", args).await;
        assert_eq!(
            std::fs::read(dir.path().join("w.txt")).unwrap(),
            b"1\r\n2\r\nthree\r\n"
        );
    }

    #[tokio::test]
    async fn read_file_result_is_windowed_with_a_start_line_hint() {
        let dir = tempfile::tempdir().unwrap();
        numbered_file(dir.path(), "big.rs", 1000, 5);
        let text = rendered(dir.path(), "read_file", json!({"path": "big.rs"})).await;
        assert!(text.starts_with("big.rs, lines 1-200 of 1000:\nline00001 xxxxx\n"));
        assert_eq!(body(&text).len(), 200);
        assert_eq!(body(&text)[199], "line00200 xxxxx");
        assert_eq!(
            hint(&text),
            "[call read_file with start_line=201 end_line=400]"
        );
    }

    #[tokio::test]
    async fn read_file_hint_continues_from_the_requested_range() {
        let dir = tempfile::tempdir().unwrap();
        numbered_file(dir.path(), "big.rs", 650, 5);
        let args = json!({"path": "big.rs", "start_line": 301, "end_line": 650});
        let text = rendered(dir.path(), "read_file", args).await;
        assert!(text.starts_with("big.rs, lines 301-500 of 650:\nline00301 "));
        assert_eq!(body(&text).len(), 200);
        assert_eq!(
            hint(&text),
            "[call read_file with start_line=501 end_line=650]"
        );
    }

    #[tokio::test]
    async fn read_file_window_stops_at_the_character_cap_on_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        numbered_file(dir.path(), "wide.rs", 150, 190);
        let text = rendered(dir.path(), "read_file", json!({"path": "wide.rs"})).await;
        let content = body(&text).join("\n");
        assert!(content.chars().count() <= 12_000, "{}", content.len());
        let kept = body(&text).len();
        assert!(kept > 0 && kept < 150);
        assert!(content.ends_with(&"x".repeat(190)));
        assert!(text.starts_with(&format!("wide.rs, lines 1-{kept} of 150:\n")));
        assert_eq!(
            hint(&text),
            format!("[call read_file with start_line={} end_line=150]", kept + 1)
        );
    }

    #[tokio::test]
    async fn read_file_cuts_one_oversized_line_and_moves_past_it() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("{}\nsecond\n", "é".repeat(30_000));
        std::fs::write(dir.path().join("min.js"), long).unwrap();
        let text = rendered(dir.path(), "read_file", json!({"path": "min.js"})).await;
        assert!(text.starts_with("min.js, lines 1-1 of 2:\n"));
        assert_eq!(body(&text)[0].chars().count(), 12_000);
        assert_eq!(hint(&text), "[call read_file with start_line=2 end_line=2]");
        std::fs::write(dir.path().join("one.js"), "é".repeat(30_000)).unwrap();
        let text = rendered(dir.path(), "read_file", json!({"path": "one.js"})).await;
        assert_eq!(
            hint(&text),
            "[line 1 was cut at 12000 characters; use search to inspect it]"
        );
    }

    #[tokio::test]
    async fn search_results_are_path_line_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), QUOTED).unwrap();
        let text = rendered(dir.path(), "search", json!({"pattern": "feature"})).await;
        assert_eq!(text, "main.rs:1: #[cfg(feature = \"cli\")]");
        let none = rendered(dir.path(), "search", json!({"pattern": "absent"})).await;
        assert_eq!(none, "no matches for pattern `absent`");
    }

    #[tokio::test]
    async fn listings_are_one_workspace_path_per_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/bin")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "x").unwrap();
        for path in ["src", "./src/"] {
            let text = rendered(dir.path(), "list_directory", json!({"path": path})).await;
            let mut lines: Vec<&str> = text.split('\n').collect();
            lines.sort_unstable();
            assert_eq!(lines, ["src/bin/", "src/lib.rs"], "{path}");
        }
        let args = json!({"path": "src", "recursive": true});
        let text = rendered(dir.path(), "list_directory", args).await;
        assert_eq!(text, "src/lib.rs");
        let args = json!({"path": "src/bin"});
        let empty = rendered(dir.path(), "list_directory", args).await;
        assert_eq!(empty, "no entries in src/bin");
    }

    #[tokio::test]
    async fn a_listing_is_capped_by_entries_not_by_characters() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..200 {
            std::fs::write(dir.path().join(format!("{n:0>60}.rs")), "x").unwrap();
        }
        let text = rendered(dir.path(), "list_directory", json!({})).await;
        assert!(text.chars().count() > 8_000);
        assert_eq!(text.split('\n').count(), 200);
        assert!(!text.contains("characters cut"));
    }

    #[tokio::test]
    async fn an_edit_result_shows_the_lines_without_numbers_or_escapes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), QUOTED).unwrap();
        let args = json!({"path": "main.rs", "old_string": "fn main()", "new_string": "fn run()"});
        let text = rendered(dir.path(), "edit_file", args).await;
        assert_eq!(
            text,
            "edited main.rs: 1 replacement(s)\n\
             before (lines 1-3):\n\
             #[cfg(feature = \"cli\")]\nfn main() {\n    println!(\"a\\n\");\n\
             after (lines 1-3):\n\
             #[cfg(feature = \"cli\")]\nfn run() {\n    println!(\"a\\n\");"
        );
    }

    #[tokio::test]
    async fn an_edit_that_empties_a_file_renders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        let args = json!({"path": "a.txt", "old_string": "x\n", "new_string": ""});
        let text = rendered(dir.path(), "edit_file", args).await;
        assert_eq!(
            text,
            "edited a.txt: 1 replacement(s)\nbefore (lines 1-1):\nx\nafter: no lines"
        );
    }

    #[test]
    fn a_git_diff_is_shown_as_it_is() {
        let diff = "diff --git a/a b/a\n-say \"x\"\n+say \"y\"\n";
        let outcome: ToolResult<Value> =
            Ok(json!({"diff": diff, "staged": false, "path": "(all files)", "has_changes": true}));
        assert_eq!(render("git_diff", &outcome), diff);
        let outcome: ToolResult<Value> =
            Ok(json!({"diff": "", "staged": true, "path": "a", "has_changes": false}));
        assert_eq!(render("git_diff", &outcome), "no staged changes in a");
    }

    #[test]
    fn command_output_is_shown_as_it_is() {
        let outcome: ToolResult<Value> = Ok(json!({
            "stdout": "running 1 test\n",
            "stderr": "error: expected `\"`\n --> a.rs:1\n",
            "success": false,
            "timed_out": true,
            "command": "cargo test --offline",
        }));
        assert_eq!(
            render("cargo_test", &outcome),
            "$ cargo test --offline\nsuccess: false\ntimed out: true\n\
             stdout:\nrunning 1 test\n\
             stderr:\nerror: expected `\"`\n --> a.rs:1"
        );
        let quiet: ToolResult<Value> = Ok(json!({"stdout": "", "stderr": "", "success": true}));
        assert_eq!(render("run_command", &quiet), "success: true");
    }

    #[test]
    fn long_command_output_keeps_its_head_and_tail() {
        let outcome: ToolResult<Value> = Ok(json!({
            "stdout": format!("HEAD{}", "o".repeat(30_000)),
            "stderr": format!("{}TAIL", "e".repeat(30_000)),
            "success": false,
            "command": "cargo build",
        }));
        let text = render("cargo_build", &outcome);
        assert!(text.starts_with("$ cargo build\nsuccess: false\nstdout:\nHEAD"));
        assert!(text.ends_with("TAIL"));
        assert!(text.contains("characters cut"));
        assert!(text.chars().count() < 8_100);
    }

    #[test]
    fn results_of_other_tools_stay_json() {
        let value = json!({"path": "a", "bytes_written": 1, "success": true});
        let outcome: ToolResult<Value> = Ok(value.clone());
        assert_eq!(render("write_file", &outcome), value.to_string());
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
        let few = rendered(dir.path(), "list_directory", args.clone()).await;
        let mut paths: Vec<&str> = few.split('\n').collect();
        paths.sort_unstable();
        assert_eq!(paths, [".github/ci.yml", "src/target_utils.rs"]);
        for n in 0..300 {
            std::fs::write(dir.path().join(format!("src/f{n}.rs")), "x").unwrap();
        }
        let many = rendered(dir.path(), "list_directory", args).await;
        assert_eq!(many.split('\n').count(), 201);
        assert_eq!(
            hint(&many),
            "[showing 200 of 302 entries; list a subdirectory with recursive=false]"
        );
    }

    #[tokio::test]
    async fn flat_listing_hides_target_and_git_but_an_explicit_listing_of_target_works() {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["target/debug", ".git", "src"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        std::fs::write(dir.path().join("target/debug/bin"), "x").unwrap();
        let top = rendered(dir.path(), "list_directory", json!({})).await;
        assert_eq!(top, "src/");
        let args = json!({"path": "target", "recursive": true});
        let inside = rendered(dir.path(), "list_directory", args).await;
        assert_eq!(inside, "target/debug/bin");
    }

    #[tokio::test]
    async fn search_lines_and_total_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let long: String = (0..50)
            .map(|_| format!("needle {}\n", "ü".repeat(1000)))
            .collect();
        std::fs::write(dir.path().join("a.txt"), long).unwrap();
        let text = rendered(dir.path(), "search", json!({"pattern": "needle"})).await;
        assert!(text.chars().count() <= 8_000, "{}", text.chars().count());
        let mut lines: Vec<&str> = text.split('\n').collect();
        let last = lines.pop().unwrap();
        assert!(!lines.is_empty() && lines.len() < 50);
        for (index, line) in lines.iter().enumerate() {
            let content = line
                .strip_prefix(&format!("a.txt:{}: ", index + 1))
                .expect(line);
            assert_eq!(content.chars().count(), 200);
        }
        assert_eq!(
            last,
            format!(
                "[showing {} of 50 matches, lines cut at 200 characters; narrow pattern or file_pattern]",
                lines.len()
            )
        );
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
