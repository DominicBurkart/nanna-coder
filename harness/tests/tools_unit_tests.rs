//! Unit tests for harness tool implementations.
//!
//! ## What is being tested and why
//!
//! `tools.rs` is the widest public surface the agent touches at runtime: every
//! file read/write, directory listing, text search, arithmetic calculation, and
//! git-status inspection goes through one of the tools registered here.  Two
//! classes of behaviour are critical yet had no isolated coverage:
//!
//! 1. **Path-security** – `ReadFileTool`, `WriteFileTool`, `ListDirTool`, and
//!    `SearchTool` all reject paths that escape the workspace root (directory
//!    traversal).  The integration tests exercise the happy path end-to-end but
//!    never deliberately craft `../../etc/passwd`-style inputs.
//!
//! 2. **Argument validation / sad paths** – Every tool returns a structured
//!    `ToolError` on bad input; none of these branches were exercised in
//!    isolation.
//!
//! ## Test style rationale
//!
//! Pure unit tests (no container, no Ollama, no git daemon) keep CI fast and
//! reliable.  Each test is self-contained: it creates its own `TempDir` so
//! tests can run in parallel without interfering.  Async tools are driven with
//! `#[tokio::test]`.  Sync helpers (e.g. `CalculatorTool`) use plain `#[test]`.

use harness::tools::{
    CalculatorTool, EchoTool, ListDirTool, ReadFileTool, SearchTool, Tool, ToolRegistry,
    WriteFileTool,
};
use serde_json::json;
use std::fs;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helper: create a sandboxed workspace with a known file layout
// ---------------------------------------------------------------------------

struct Workspace {
    dir: TempDir,
}

impl Workspace {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        // Populate a minimal file tree the tests can read / search
        fs::write(dir.path().join("hello.txt"), "Hello, world!\n").unwrap();
        fs::write(
            dir.path().join("src.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();
        fs::write(
            dir.path().join("subdir").join("nested.txt"),
            "nested content",
        )
        .unwrap();
        Self { dir }
    }

    fn path_str(&self) -> &str {
        self.dir.path().to_str().expect("utf-8 path")
    }

    fn read_tool(&self) -> ReadFileTool {
        ReadFileTool::new(self.dir.path())
    }

    fn write_tool(&self) -> WriteFileTool {
        WriteFileTool::new(self.dir.path())
    }

    fn list_tool(&self) -> ListDirTool {
        ListDirTool::new(self.dir.path())
    }

    fn search_tool(&self) -> SearchTool {
        SearchTool::new(self.dir.path())
    }
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

#[test]
fn registry_register_and_lookup() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    assert!(registry.get_tool("echo").is_some());
    assert!(registry.get_tool("nonexistent").is_none());
}

#[test]
fn registry_list_tools_includes_registered() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    registry.register(Box::new(CalculatorTool::new()));
    let names = registry.list_tools();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"calculator"));
}

#[test]
fn registry_get_definitions_matches_registered_tools() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    let defs = registry.get_definitions();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].function.name, "echo");
}

// ---------------------------------------------------------------------------
// EchoTool – simplest possible tool, useful as a sanity baseline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn echo_returns_message() {
    let tool = EchoTool::new();
    let result = tool.execute(json!({"message": "ping"})).await.unwrap();
    assert_eq!(result["message"], "ping");
}

#[tokio::test]
async fn echo_missing_message_returns_error() {
    let tool = EchoTool::new();
    let err = tool.execute(json!({})).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("message") || msg.contains("Invalid") || msg.contains("missing"),
        "expected descriptive error, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// CalculatorTool – pure arithmetic, no I/O
// ---------------------------------------------------------------------------

#[tokio::test]
async fn calculator_addition() {
    let tool = CalculatorTool::new();
    let result = tool
        .execute(json!({"expression": "2 + 3"}))
        .await
        .unwrap();
    // The tool stores the numeric result under a "result" key
    let val = result["result"].as_f64().unwrap();
    assert!((val - 5.0).abs() < 1e-9);
}

#[tokio::test]
async fn calculator_multiplication_and_division() {
    let tool = CalculatorTool::new();

    let r = tool
        .execute(json!({"expression": "6 * 7"}))
        .await
        .unwrap();
    assert!((r["result"].as_f64().unwrap() - 42.0).abs() < 1e-9);

    let r = tool
        .execute(json!({"expression": "10 / 4"}))
        .await
        .unwrap();
    assert!((r["result"].as_f64().unwrap() - 2.5).abs() < 1e-9);
}

#[tokio::test]
async fn calculator_subtraction_negative_result() {
    let tool = CalculatorTool::new();
    let result = tool
        .execute(json!({"expression": "3 - 10"}))
        .await
        .unwrap();
    assert!((result["result"].as_f64().unwrap() - (-7.0)).abs() < 1e-9);
}

#[tokio::test]
async fn calculator_missing_expression_returns_error() {
    let tool = CalculatorTool::new();
    let err = tool.execute(json!({})).await.unwrap_err();
    assert!(
        err.to_string().contains("expression")
            || err.to_string().contains("Invalid")
            || err.to_string().contains("missing"),
        "expected descriptive error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// ReadFileTool – happy path + path traversal rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_file_happy_path() {
    let ws = Workspace::new();
    let tool = ws.read_tool();
    let result = tool
        .execute(json!({"path": "hello.txt"}))
        .await
        .unwrap();
    let content = result["content"].as_str().unwrap();
    assert!(content.contains("Hello, world!"));
}

#[tokio::test]
async fn read_file_nested_path() {
    let ws = Workspace::new();
    let tool = ws.read_tool();
    let result = tool
        .execute(json!({"path": "subdir/nested.txt"}))
        .await
        .unwrap();
    assert!(result["content"]
        .as_str()
        .unwrap()
        .contains("nested content"));
}

#[tokio::test]
async fn read_file_path_traversal_rejected() {
    let ws = Workspace::new();
    let tool = ws.read_tool();
    // Classic traversal attempt escaping the workspace root
    let err = tool
        .execute(json!({"path": "../../etc/passwd"}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("security") || msg.contains("traversal") || msg.contains("outside")
            || msg.contains("allowed") || msg.contains("violation"),
        "expected path-security error, got: {msg}"
    );
}

#[tokio::test]
async fn read_file_absolute_path_outside_workspace_rejected() {
    let ws = Workspace::new();
    let tool = ws.read_tool();
    // Absolute path to a well-known system file
    let err = tool
        .execute(json!({"path": "/etc/hostname"}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("security") || msg.contains("traversal") || msg.contains("outside")
            || msg.contains("allowed") || msg.contains("violation"),
        "expected path-security error for absolute path, got: {msg}"
    );
}

#[tokio::test]
async fn read_file_nonexistent_returns_error() {
    let ws = Workspace::new();
    let tool = ws.read_tool();
    let err = tool
        .execute(json!({"path": "does_not_exist.txt"}))
        .await
        .unwrap_err();
    // Should be an IO error, not a panic
    assert!(!err.to_string().is_empty());
}

#[tokio::test]
async fn read_file_missing_path_argument_returns_error() {
    let ws = Workspace::new();
    let tool = ws.read_tool();
    let err = tool.execute(json!({})).await.unwrap_err();
    assert!(
        err.to_string().contains("path") || err.to_string().contains("Invalid"),
        "expected descriptive error about missing path, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// WriteFileTool – happy path + path traversal rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_file_creates_new_file() {
    let ws = Workspace::new();
    let tool = ws.write_tool();
    tool.execute(json!({"path": "new_file.txt", "content": "created by test"}))
        .await
        .unwrap();
    let on_disk = fs::read_to_string(ws.dir.path().join("new_file.txt")).unwrap();
    assert_eq!(on_disk, "created by test");
}

#[tokio::test]
async fn write_file_overwrites_existing_file() {
    let ws = Workspace::new();
    let tool = ws.write_tool();
    tool.execute(json!({"path": "hello.txt", "content": "overwritten"}))
        .await
        .unwrap();
    let on_disk = fs::read_to_string(ws.dir.path().join("hello.txt")).unwrap();
    assert_eq!(on_disk, "overwritten");
}

#[tokio::test]
async fn write_file_path_traversal_rejected() {
    let ws = Workspace::new();
    let tool = ws.write_tool();
    let err = tool
        .execute(json!({"path": "../../tmp/evil.txt", "content": "should not write"}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("security") || msg.contains("traversal") || msg.contains("outside")
            || msg.contains("allowed") || msg.contains("violation"),
        "expected path-security error, got: {msg}"
    );
}

#[tokio::test]
async fn write_file_absolute_path_outside_workspace_rejected() {
    let ws = Workspace::new();
    let tool = ws.write_tool();
    let err = tool
        .execute(json!({"path": "/tmp/evil_write_test_nanna.txt", "content": "bad"}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("security") || msg.contains("traversal") || msg.contains("outside")
            || msg.contains("allowed") || msg.contains("violation"),
        "expected path-security error for absolute path, got: {msg}"
    );
}

#[tokio::test]
async fn write_file_creates_parent_directories() {
    let ws = Workspace::new();
    let tool = ws.write_tool();
    tool.execute(json!({"path": "a/b/c/deep.txt", "content": "deep"}))
        .await
        .unwrap();
    let on_disk = fs::read_to_string(ws.dir.path().join("a/b/c/deep.txt")).unwrap();
    assert_eq!(on_disk, "deep");
}

#[tokio::test]
async fn write_file_missing_path_argument_returns_error() {
    let ws = Workspace::new();
    let tool = ws.write_tool();
    let err = tool
        .execute(json!({"content": "no path given"}))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("path") || err.to_string().contains("Invalid"),
        "expected descriptive error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// ListDirTool – happy path + path traversal rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_dir_workspace_root() {
    let ws = Workspace::new();
    let tool = ws.list_tool();
    // Listing "." or "" should show the top-level entries
    let result = tool.execute(json!({"path": "."})).await.unwrap();
    let entries = result["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        names.contains(&"hello.txt"),
        "root listing should include hello.txt, got: {names:?}"
    );
    assert!(
        names.contains(&"subdir"),
        "root listing should include subdir, got: {names:?}"
    );
}

#[tokio::test]
async fn list_dir_subdirectory() {
    let ws = Workspace::new();
    let tool = ws.list_tool();
    let result = tool.execute(json!({"path": "subdir"})).await.unwrap();
    let entries = result["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        names.contains(&"nested.txt"),
        "subdir listing should include nested.txt, got: {names:?}"
    );
}

#[tokio::test]
async fn list_dir_path_traversal_rejected() {
    let ws = Workspace::new();
    let tool = ws.list_tool();
    let err = tool
        .execute(json!({"path": "../../"}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("security") || msg.contains("traversal") || msg.contains("outside")
            || msg.contains("allowed") || msg.contains("violation"),
        "expected path-security error, got: {msg}"
    );
}

#[tokio::test]
async fn list_dir_nonexistent_returns_error() {
    let ws = Workspace::new();
    let tool = ws.list_tool();
    let err = tool
        .execute(json!({"path": "no_such_dir"}))
        .await
        .unwrap_err();
    assert!(!err.to_string().is_empty());
}

// ---------------------------------------------------------------------------
// SearchTool – content search within the workspace
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_finds_matching_content() {
    let ws = Workspace::new();
    let tool = ws.search_tool();
    let result = tool
        .execute(json!({"query": "Hello", "path": "."}))
        .await
        .unwrap();
    // Results should mention hello.txt since it contains "Hello, world!"
    let output = serde_json::to_string(&result).unwrap();
    assert!(
        output.contains("hello.txt"),
        "search for 'Hello' should match hello.txt, got: {output}"
    );
}

#[tokio::test]
async fn search_returns_empty_for_no_matches() {
    let ws = Workspace::new();
    let tool = ws.search_tool();
    let result = tool
        .execute(json!({"query": "zzz_nonexistent_string_xyz", "path": "."}))
        .await
        .unwrap();
    let matches = result["matches"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(matches, 0, "no matches expected for nonsense query");
}

#[tokio::test]
async fn search_path_traversal_rejected() {
    let ws = Workspace::new();
    let tool = ws.search_tool();
    let err = tool
        .execute(json!({"query": "root", "path": "../../"}))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("security") || msg.contains("traversal") || msg.contains("outside")
            || msg.contains("allowed") || msg.contains("violation"),
        "expected path-security error for traversal search, got: {msg}"
    );
}

#[tokio::test]
async fn search_missing_query_argument_returns_error() {
    let ws = Workspace::new();
    let tool = ws.search_tool();
    let err = tool.execute(json!({"path": "."})).await.unwrap_err();
    assert!(
        err.to_string().contains("query") || err.to_string().contains("Invalid"),
        "expected descriptive error for missing query, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Tool definitions (JSON schema contract)
// ---------------------------------------------------------------------------

#[test]
fn tool_definitions_have_names_and_descriptions() {
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(EchoTool::new()),
        Box::new(CalculatorTool::new()),
    ];
    for tool in &tools {
        let def = tool.definition();
        assert!(
            !def.function.name.is_empty(),
            "tool '{}' definition name is empty",
            tool.name()
        );
        assert!(
            !def.function.description.is_empty(),
            "tool '{}' definition description is empty",
            tool.name()
        );
    }
}

#[test]
fn workspace_scoped_tool_definitions_have_names_and_descriptions() {
    let ws = Workspace::new();
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(ws.read_tool()),
        Box::new(ws.write_tool()),
        Box::new(ws.list_tool()),
        Box::new(ws.search_tool()),
    ];
    for tool in &tools {
        let def = tool.definition();
        assert!(
            !def.function.name.is_empty(),
            "tool '{}' definition name is empty",
            tool.name()
        );
        assert!(
            !def.function.description.is_empty(),
            "tool '{}' definition description is empty",
            tool.name()
        );
    }
}

// ---------------------------------------------------------------------------
// Invariant: tool name() must equal definition().function.name
// ---------------------------------------------------------------------------

#[test]
fn tool_name_matches_definition_name() {
    let ws = Workspace::new();
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(EchoTool::new()),
        Box::new(CalculatorTool::new()),
        Box::new(ws.read_tool()),
        Box::new(ws.write_tool()),
        Box::new(ws.list_tool()),
        Box::new(ws.search_tool()),
    ];
    for tool in &tools {
        assert_eq!(
            tool.name(),
            tool.definition().function.name,
            "tool name() and definition().function.name must match"
        );
    }
}

// ---------------------------------------------------------------------------
// create_tool_registry helper
// ---------------------------------------------------------------------------

#[test]
fn create_tool_registry_contains_expected_tools() {
    let ws = Workspace::new();
    let registry = harness::tools::create_tool_registry(ws.dir.path());
    // These are the core tools every non-container workspace should have
    for name in &["read_file", "write_file", "list_dir", "search", "calculator"] {
        assert!(
            registry.get_tool(name).is_some(),
            "expected tool '{name}' in default registry"
        );
    }
}
