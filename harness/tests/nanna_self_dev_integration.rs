use harness::agent::{AgentConfig, AgentContext, AgentLoop};
use harness::container::{
    detect_runtime, exec_in_container, load_image_from_path, start_container_with_fallback,
    ContainerConfig, ContainerRuntime,
};
use harness::entities::InMemoryEntityStore;
use harness::tools::{
    ListDirTool, ReadFileTool, RunCommandTool, SearchTool, ToolRegistry, WriteFileTool,
};
use image_builder::build_dev_container;
use model::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

const MAX_ATTEMPTS: usize = 5;
const MAX_TURNS: usize = 32;
const TEST_TIMEOUT: Duration = Duration::from_secs(600);
const E2E_MODEL: &str = "qwen3:0.6b";
const ORIGINAL_HELP_STRING: &str = "A CLI tool for interacting with language models";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[tokio::test]
#[ignore]
async fn test_nanna_self_dev_translate_help_to_french() {
    let root = workspace_root();
    assert!(
        root.join("flake.nix").exists(),
        "workspace root not found at {:?}",
        root
    );

    let image_path = build_dev_container(&root).expect("failed to build nanna dev container image");

    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }

    let image_ref =
        load_image_from_path(&runtime, &image_path).expect("failed to load dev container image");

    let mut last_error: Option<String> = None;

    for attempt in 0..MAX_ATTEMPTS {
        eprintln!("Attempt {}/{}", attempt + 1, MAX_ATTEMPTS);

        let result = timeout(TEST_TIMEOUT, run_single_attempt(&runtime, &image_ref)).await;

        match result {
            Ok(Ok(())) => {
                eprintln!("Test passed on attempt {}", attempt + 1);
                return;
            }
            Ok(Err(e)) => {
                eprintln!("Attempt {} failed: {}", attempt + 1, e);
                last_error = Some(e);
            }
            Err(_) => {
                eprintln!("Attempt {} timed out", attempt + 1);
                last_error = Some("test timed out".to_string());
            }
        }
    }

    panic!(
        "All {} attempts failed. Last error: {}",
        MAX_ATTEMPTS,
        last_error.unwrap_or_default()
    );
}

async fn run_single_attempt(runtime: &ContainerRuntime, image_ref: &str) -> Result<(), String> {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = tempdir.path().to_path_buf();

    let root = workspace_root();
    copy_workspace(&root, &workspace).map_err(|e| format!("workspace copy failed: {}", e))?;

    let container_name = format!("nanna-self-dev-test-{}", uuid::Uuid::new_v4());

    let mut additional_args = vec![format!("-v={}:/workspace", workspace.display())];
    if *runtime == ContainerRuntime::Podman {
        additional_args.push("--userns=keep-id".to_string());
    }

    let config = ContainerConfig {
        base_image: image_ref.to_string(),
        test_image: None,
        container_name: container_name.clone(),
        port_mapping: None,
        model_to_pull: None,
        startup_timeout: Duration::from_secs(5),
        health_check_timeout: Duration::from_secs(5),
        env_vars: vec![],
        additional_args,
    };

    let handle = start_container_with_fallback(&config)
        .await
        .map_err(|e| format!("container start failed: {}", e))?;
    let handle = Arc::new(handle);

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadFileTool::new(workspace.clone())));
    registry.register(Box::new(WriteFileTool::new(workspace.clone())));
    registry.register(Box::new(ListDirTool::new(workspace.clone())));
    registry.register(Box::new(SearchTool::new(workspace.clone())));
    registry.register(Box::new(RunCommandTool::new(
        Arc::clone(&handle),
        Some("/workspace".to_string()),
    )));

    let ollama_config = OllamaConfig::default();
    let provider = OllamaProvider::new(ollama_config).map_err(|e| e.to_string())?;
    let provider: Arc<dyn ModelProvider> = Arc::new(provider);

    let entity_store = InMemoryEntityStore::new();

    let agent_config = AgentConfig {
        max_iterations: MAX_TURNS,
        verbose: true,
        system_prompt: format!(
            "You are a coding assistant working inside a Rust project at /workspace. \
             Your task is to translate the CLI help text in harness/src/main.rs from English to French. \
             Specifically, find the `about` string \"{ORIGINAL_HELP_STRING}\" in the #[command(about = ...)] \
             attribute on the Cli struct in harness/src/main.rs and translate it to French. \
             Also translate any other English user-facing strings in the #[command] and #[arg] \
             doc comments in that file. \
             After making changes, use run_command to verify with `cargo build --workspace` and \
             `cargo test --workspace` inside the container."
        ),
        model_name: E2E_MODEL.to_string(),
    };

    let context = AgentContext {
        user_prompt: "Translate the CLI help text in harness/src/main.rs from English to French."
            .to_string(),
        conversation_history: vec![],
        app_state_id: uuid::Uuid::new_v4().to_string(),
    };

    let mut agent = AgentLoop::with_tools(agent_config, entity_store, provider, registry);

    let result = agent
        .run(context)
        .await
        .map_err(|e| format!("agent failed: {}", e))?;

    if !result.task_completed {
        return Err(format!(
            "agent did not complete task after {} iterations",
            result.iterations
        ));
    }

    let build_result = exec_in_container(
        &handle,
        &["cargo", "build", "--workspace"],
        Some("/workspace"),
    )
    .map_err(|e| format!("exec cargo build failed: {}", e))?;

    if !build_result.success {
        return Err(format!(
            "cargo build failed:\nstdout: {}\nstderr: {}",
            build_result.stdout, build_result.stderr
        ));
    }

    let test_result = exec_in_container(
        &handle,
        &["cargo", "test", "--workspace"],
        Some("/workspace"),
    )
    .map_err(|e| format!("exec cargo test failed: {}", e))?;

    if !test_result.success {
        return Err(format!(
            "cargo test failed:\nstdout: {}\nstderr: {}",
            test_result.stdout, test_result.stderr
        ));
    }

    let help_result = exec_in_container(
        &handle,
        &["cargo", "run", "--bin", "harness", "--", "--help"],
        Some("/workspace"),
    )
    .map_err(|e| format!("exec harness --help failed: {}", e))?;

    let help_output = format!("{}{}", help_result.stdout, help_result.stderr);
    if help_output.contains(ORIGINAL_HELP_STRING) {
        return Err(format!(
            "harness --help still contains original English string {:?}.\nOutput: {}",
            ORIGINAL_HELP_STRING, help_output
        ));
    }

    Ok(())
}

fn copy_workspace(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    let skip = ["target", ".git", "result"];
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if skip.iter().any(|s| *s == name_str.as_ref()) {
            continue;
        }
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_workspace(&entry.path(), &dst.join(&name))?;
        } else {
            std::fs::copy(entry.path(), dst.join(&name))?;
        }
    }
    Ok(())
}
