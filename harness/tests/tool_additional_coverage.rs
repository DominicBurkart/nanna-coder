//! Additional unit tests covering gaps in harness/src/tools.rs.
//!
//! Coverage added:
//! - `CalculatorTool::divide` happy path (non-zero divisor)
//! - `WriteFileTool`: successful write + content verification; overwrite
//! - `ReadFileTool`: `end_line` parameter (range read)
//! - `ListDirTool`: basic listing; glob-pattern filter
//! - `SearchTool`: pattern match; no-match empty results; file/line metadata

use harness::tools::{CalculatorTool, ListDirTool, ReadFileTool, SearchTool, Tool, WriteFileTool};
use serde_json::json;

// --- CalculatorTool::divide (non-zero) ---------------------------------------

#[tokio::test]
async fn calculator_divide_nonzero_returns_correct_quotient() {
    let tool = CalculatorTool::new();
    let result = tool
        .execute(json!({ "operation": "divide", "a": 10.0, "b": 4.0 }))
        .await
        .expect("divide with non-zero divisor should succeed");
    let quotient = result["result"].as_f64().unwrap();
    assert!(
        (quotient - 2.5).abs() < 1e-10,
        "10 / 4 should be 2.5, got {quotient}"
    );
    assert_eq!(result["operation"], "divide");
}

// --- WriteFileTool happy path ------------------------------------------------

#[tokio::test]
async fn write_file_creates_file_with_correct_content() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = WriteFileTool::new(temp_dir.path().to_path_buf());

    let result = tool
        .execute(json!({ "path": "hello.txt", "content": "hello world\nline 2" }))
        .await
        .expect("write_file should succeed");

    assert_eq!(result["success"], true);
    assert_eq!(result["path"], "hello.txt");

    let written = std::fs::read_to_string(temp_dir.path().join("hello.txt"))
        .expect("file should exist after write");
    assert_eq!(written, "hello world\nline 2");
}

#[tokio::test]
async fn write_file_overwrites_existing_content() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("overwrite.txt");
    std::fs::write(&path, "old content").unwrap();

    let tool = WriteFileTool::new(temp_dir.path().to_path_buf());
    tool.execute(json!({ "path": "overwrite.txt", "content": "new content" }))
        .await
        .expect("overwrite should succeed");

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "new content");
}

#[tokio::test]
async fn write_file_reports_bytes_written() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = WriteFileTool::new(temp_dir.path().to_path_buf());

    let content = "12345"; // 5 bytes
    let result = tool
        .execute(json!({ "path": "bytes.txt", "content": content }))
        .await
        .expect("write_file should succeed");

    assert_eq!(
        result["bytes_written"].as_u64().unwrap(),
        5,
        "bytes_written should match content length"
    );
}

// --- ReadFileTool end_line parameter -----------------------------------------

#[tokio::test]
async fn read_file_start_and_end_line_returns_exact_range() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp_dir.path().join("lines.txt"),
        "line1\nline2\nline3\nline4\nline5",
    )
    .unwrap();

    let tool = ReadFileTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": "lines.txt", "start_line": 2, "end_line": 4 }))
        .await
        .expect("range read must succeed");

    assert_eq!(
        result["lines_shown"],
        3,
        "lines 2-4 inclusive = 3 lines"
    );
    let content = result["content"].as_str().unwrap();
    assert!(content.contains("line2"), "should contain line2");
    assert!(content.contains("line3"), "should contain line3");
    assert!(content.contains("line4"), "should contain line4");
    assert!(!content.contains("line1"), "should not contain line1");
    assert!(!content.contains("line5"), "should not contain line5");
}

#[tokio::test]
async fn read_file_end_line_without_start_line_reads_from_beginning() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp_dir.path().join("file.txt"), "a\nb\nc\nd").unwrap();

    let tool = ReadFileTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": "file.txt", "end_line": 2 }))
        .await
        .expect("end_line without start_line must succeed");

    assert_eq!(
        result["lines_shown"],
        2,
        "end_line=2 from start should give 2 lines"
    );
    let content = result["content"].as_str().unwrap();
    assert!(content.contains('a'), "first line should be included");
    assert!(content.contains('b'), "second line should be included");
    assert!(!content.contains('c'), "third line should be excluded");
}

// --- ListDirTool -------------------------------------------------------------

#[tokio::test]
async fn list_dir_returns_files_in_workspace_root() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp_dir.path().join("alpha.txt"), "a").unwrap();
    std::fs::write(temp_dir.path().join("beta.rs"), "b").unwrap();

    let tool = ListDirTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": "." }))
        .await
        .expect("list_dir should succeed");

    let entries = result["entries"].as_array().expect("entries should be array");
    let names: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();

    assert!(names.contains(&"alpha.txt"), "alpha.txt should be listed");
    assert!(names.contains(&"beta.rs"), "beta.rs should be listed");
    assert_eq!(result["count"].as_u64().unwrap(), 2);
}

#[tokio::test]
async fn list_dir_with_glob_pattern_filters_by_extension() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp_dir.path().join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(temp_dir.path().join("readme.md"), "# doc").unwrap();
    std::fs::write(temp_dir.path().join("lib.rs"), "pub mod lib;").unwrap();

    let tool = ListDirTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": ".", "pattern": "*.rs" }))
        .await
        .expect("pattern-filtered list_dir should succeed");

    let entries = result["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 2, "only .rs files should be listed");
    for entry in entries {
        assert!(
            entry["name"].as_str().unwrap().ends_with(".rs"),
            "all entries should be .rs files"
        );
    }
}

// --- SearchTool --------------------------------------------------------------

#[tokio::test]
async fn search_tool_finds_pattern_matches_in_files() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp_dir.path().join("code.rs"),
        "fn hello() {}\nfn world() {}\nconst FOO: i32 = 42;\n",
    )
    .unwrap();

    let tool = SearchTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "pattern": "fn " }))
        .await
        .expect("search should succeed");

    let results = result["results"].as_array().expect("results should be array");
    assert_eq!(results.len(), 2, "should find 2 fn declarations");
    let contents: Vec<&str> = results
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        contents.iter().any(|c| c.contains("hello")),
        "hello fn should be found"
    );
    assert!(
        contents.iter().any(|c| c.contains("world")),
        "world fn should be found"
    );
}

#[tokio::test]
async fn search_tool_returns_empty_results_when_no_match() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp_dir.path().join("data.txt"), "nothing here").unwrap();

    let tool = SearchTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "pattern": "NONEXISTENT_PATTERN_XYZ_12345" }))
        .await
        .expect("search with no matches should succeed (not error)");

    let results = result["results"].as_array().expect("results should be array");
    assert!(
        results.is_empty(),
        "should have no results when pattern does not match"
    );
    assert_eq!(result["count"], 0);
}

#[tokio::test]
async fn search_tool_result_includes_file_and_line_number() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp_dir.path().join("target.txt"),
        "first line\nSEARCH_ME\nthird line",
    )
    .unwrap();

    let tool = SearchTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "pattern": "SEARCH_ME" }))
        .await
        .expect("search should succeed");

    let results = result["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    let hit = &results[0];
    assert_eq!(hit["line"].as_u64().unwrap(), 2, "SEARCH_ME is on line 2");
    assert!(
        hit["file"].as_str().unwrap().contains("target.txt"),
        "file path should reference target.txt"
    );
}
