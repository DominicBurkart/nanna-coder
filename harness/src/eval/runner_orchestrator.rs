//! Container-driven orchestration for `run_eval_case`.
//!
//! Split from `runner.rs` so the container-dependent entry point lives in a
//! file that can be excluded from codecov patch coverage (it cannot execute
//! without a podman/docker runtime, which CI unit jobs do not provide). The
//! pure helpers it depends on — `classify_outcome`, `find_missing_symbols`,
//! `detect_changed_files`, `build_system_prompt`, `copy_dir_all` — remain in
//! `runner.rs` where they contribute to coverage via unit tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::agent::eval_case::EvalCase;
use crate::agent::{AgentConfig, AgentContext, AgentLoop};
use crate::container::{
    detect_runtime, exec_in_container, load_image_from_path, start_container_with_fallback,
    ContainerConfig, ContainerHandle,
};
use crate::entities::InMemoryEntityStore;
use crate::onboarding::{DeterministicOnboarder, Onboarder};
use crate::tools::{
    ListDirTool, ReadFileTool, RunCommandTool, SearchTool, ToolRegistry, WriteFileTool,
    CONTAINER_WORKSPACE_DIR,
};
use model::provider::ModelProvider;

use super::runner::{
    build_system_prompt, classify_outcome, copy_dir_all, detect_changed_files,
    find_missing_symbols, EvalRunConfig, EvalRunResult,
};

/// Execute a single eval case against the live agent.
///
/// `source_repo` is the checked-in fixture (read-only from the runner's
/// perspective). `workspace` is a caller-owned directory into which the
/// fixture is copied before the agent runs; it will be bind-mounted into
/// the dev container at `CONTAINER_WORKSPACE_DIR`. Keeping workspace
/// lifecycle outside the runner lets callers choose between `tempfile`
/// (tests) and fixed paths (report collection).
pub async fn run_eval_case(
    case: &EvalCase,
    source_repo: &std::path::Path,
    workspace: &std::path::Path,
    config: &EvalRunConfig,
    provider: Arc<dyn ModelProvider>,
) -> EvalRunResult {
    let start = Instant::now();
    let case_id = case.case.id.clone();

    if let Err(e) = copy_dir_all(source_repo, workspace) {
        return EvalRunResult::failure(&case_id, start, format!("fixture copy failed: {}", e));
    }

    let runtime = detect_runtime();
    if !runtime.is_available() {
        return EvalRunResult::failure(
            &case_id,
            start,
            "no container runtime (podman/docker) available",
        );
    }

    if !workspace.join("flake.nix").exists() {
        let onboarder = DeterministicOnboarder;
        if let Err(e) = onboarder.onboard(workspace).await {
            return EvalRunResult::failure(&case_id, start, format!("onboarding failed: {}", e));
        }
    }

    let image_path = match image_builder::build_dev_container(workspace) {
        Ok(p) => p,
        Err(e) => {
            return EvalRunResult::failure(
                &case_id,
                start,
                format!("dev container build failed: {}", e),
            )
        }
    };

    let image_ref = match load_image_from_path(&runtime, &image_path) {
        Ok(r) => r,
        Err(e) => {
            return EvalRunResult::failure(&case_id, start, format!("load image failed: {}", e))
        }
    };

    let container_name = format!("nanna-eval-{}-{}", case.case.id, uuid::Uuid::new_v4());
    let additional_args = vec![format!(
        "-v={}:{CONTAINER_WORKSPACE_DIR}",
        workspace.display()
    )];
    let container_config = ContainerConfig {
        base_image: image_ref,
        test_image: None,
        container_name,
        port_mapping: None,
        model_to_pull: None,
        startup_timeout: Duration::from_secs(5),
        health_check_timeout: Duration::from_secs(5),
        env_vars: vec![],
        additional_args,
    };

    let handle = match start_container_with_fallback(&container_config).await {
        Ok(h) => h,
        Err(e) => {
            return EvalRunResult::failure(
                &case_id,
                start,
                format!("container start failed: {}", e),
            )
        }
    };
    let handle = Arc::new(handle);

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadFileTool::new(workspace.to_path_buf())));
    registry.register(Box::new(WriteFileTool::new(workspace.to_path_buf())));
    registry.register(Box::new(ListDirTool::new(workspace.to_path_buf())));
    registry.register(Box::new(SearchTool::new(workspace.to_path_buf())));
    registry.register(Box::new(RunCommandTool::new(
        Arc::clone(&handle),
        Some(CONTAINER_WORKSPACE_DIR.to_string()),
    )));

    let agent_config = AgentConfig {
        max_iterations: config.max_iterations,
        verbose: config.verbose,
        system_prompt: build_system_prompt(case),
        model_name: config.model_name.clone(),
    };
    let context = AgentContext {
        user_prompt: case.task.prompt.clone(),
        conversation_history: vec![],
        app_state_id: uuid::Uuid::new_v4().to_string(),
    };

    let mut agent =
        AgentLoop::with_tools(agent_config, InMemoryEntityStore::new(), provider, registry);

    let agent_timeout = Duration::from_secs(case.metadata.timeout_secs);
    let agent_result = match tokio::time::timeout(agent_timeout, agent.run(context)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return EvalRunResult::failure(&case_id, start, format!("agent error: {}", e))
        }
        Err(_) => {
            return EvalRunResult::failure(
                &case_id,
                start,
                format!("agent timed out after {}s", case.metadata.timeout_secs),
            )
        }
    };

    let files_changed = detect_changed_files(source_repo, workspace);
    let missing_symbols = find_missing_symbols(workspace, &case.expected.required_symbols);

    let build_passed = if case.expected.build_must_pass {
        Some(run_cargo_in_container(&handle, "build"))
    } else {
        None
    };
    let tests_passed = if case.expected.tests_must_pass {
        Some(run_cargo_in_container(&handle, "test"))
    } else {
        None
    };

    let (passed, failure_reason) = classify_outcome(
        agent_result.task_completed,
        build_passed,
        tests_passed,
        &missing_symbols,
    );

    EvalRunResult {
        case_id,
        passed,
        duration: start.elapsed(),
        iterations: agent_result.iterations,
        agent_completed: agent_result.task_completed,
        build_passed,
        tests_passed,
        files_changed,
        missing_symbols,
        failure_reason,
        tool_calls: agent_result.tool_calls_made,
    }
}

fn run_cargo_in_container(handle: &ContainerHandle, subcommand: &str) -> bool {
    exec_in_container(
        handle,
        &["cargo", subcommand],
        Some(CONTAINER_WORKSPACE_DIR),
    )
    .map(|out| out.success)
    .unwrap_or(false)
}
