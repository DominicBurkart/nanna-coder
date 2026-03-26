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

fn example_repo_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/integration/repo")
}

#[tokio::test]
#[ignore]
async fn test_dev_container_fibonacci_to_primes() {
    let repo_path = example_repo_path();
    assert!(
        repo_path.exists(),
        "Example repo not found at {:?}",
        repo_path
    );

    let image_path = build_dev_container(&repo_path).expect("failed to build dev container image");

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

    let repo_path = example_repo_path();
    copy_dir_all(&repo_path, &workspace).map_err(|e| format!("copy failed: {}", e))?;

    let container_name = format!("nanna-dev-container-test-{}", uuid::Uuid::new_v4());

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
        system_prompt: "You are a coding assistant. Modify this Rust project to compute the first 10 prime numbers instead of Fibonacci numbers. Update src/lib.rs to implement a `primes(n: usize) -> Vec<u64>` function that returns the first n prime numbers. Update src/main.rs to call `primes(10)` and print the result. Update tests/fib_test.rs to test that primes(10) returns [2, 3, 5, 7, 11, 13, 17, 19, 23, 29]. Use run_command to run `cargo test` and `cargo run` to verify your changes work.".to_string(),
        model_name: E2E_MODEL.to_string(),
    };

    let context = AgentContext {
        user_prompt: "Modify the fibonacci project to compute prime numbers instead.".to_string(),
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

    let test_result = exec_in_container(&handle, &["cargo", "test"], Some("/workspace"))
        .map_err(|e| format!("exec cargo test failed: {}", e))?;

    if !test_result.success {
        return Err(format!(
            "cargo test failed:\nstdout: {}\nstderr: {}",
            test_result.stdout, test_result.stderr
        ));
    }

    let run_result = exec_in_container(&handle, &["cargo", "run"], Some("/workspace"))
        .map_err(|e| format!("exec cargo run failed: {}", e))?;

    if !run_result.success {
        return Err(format!(
            "cargo run failed:\nstdout: {}\nstderr: {}",
            run_result.stdout, run_result.stderr
        ));
    }

    let expected_primes = "[2, 3, 5, 7, 11, 13, 17, 19, 23, 29]";
    if !run_result.stdout.contains(expected_primes) {
        return Err(format!(
            "cargo run output does not contain expected primes {}.\nActual stdout: {}",
            expected_primes, run_result.stdout
        ));
    }

    Ok(())
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}
