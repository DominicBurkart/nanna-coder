use clap::{Parser, Subcommand};
use harness::entities::ast::WorkspaceScanner;
use harness::entities::git::GitRepository;
use harness::entities::{EntityStore, InMemoryEntityStore};
use harness::mcp::handlers;
use harness::output::{ExitCode, JsonEnvelope, OutputFormat};
use harness::task::TaskManager;
use harness::tools::ToolRegistry;
use model::prelude::*;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info};

#[derive(Parser)]
#[command(name = "nanna")]
#[command(about = "Nanna CLI -- manage coding tasks, onboard repos, and interact with models")]
struct Cli {
    /// Output as JSON envelope (version-tagged, machine-readable)
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // \u2500\u2500 Task management (the 6 MVP subcommands) \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
    /// Submit a coding task to be executed
    AssignTask {
        /// Description of the task
        #[arg(short, long)]
        description: String,
        /// Absolute path to the git repository
        #[arg(short, long)]
        repo_path: PathBuf,
        /// Branch or ref to base on (default: HEAD)
        #[arg(short, long, default_value = "HEAD")]
        branch: String,
        /// Model to use
        #[arg(short, long, default_value = "qwen3:0.6b")]
        model: String,
        /// Maximum agent iterations
        #[arg(long, default_value = "100")]
        max_iterations: usize,
    },
    /// Check current status of a task
    PollTask {
        /// Task ID returned by assign-task
        #[arg(short, long)]
        task_id: String,
        /// Block until the task finishes (with optional timeout in seconds)
        #[arg(short, long)]
        wait: Option<Option<u64>>,
    },
    /// Retrieve the final result of a completed/failed task
    GetResult {
        /// Task ID returned by assign-task
        #[arg(short, long)]
        task_id: String,
    },
    /// List all submitted tasks
    ListTasks,
    /// Cancel a pending or running task
    CancelTask {
        /// Task ID returned by assign-task
        #[arg(short, long)]
        task_id: String,
    },
    /// Generate a flake.nix for a pure-Cargo project
    OnboardRepo {
        /// Absolute path to the repository
        #[arg(short, long)]
        repo_path: PathBuf,
    },

    // \u2500\u2500 Legacy / interactive commands \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
    /// Have a conversation with the model
    Chat {
        /// The model to use
        #[arg(short, long, default_value = "llama3.1:8b")]
        model: String,
        /// Initial prompt (if not provided, starts interactive mode)
        #[arg(short, long)]
        prompt: Option<String>,
        /// Enable tool calling
        #[arg(short, long)]
        tools: bool,
        /// Temperature setting (0.0 to 2.0)
        #[arg(long, default_value = "0.7")]
        temperature: f32,
    },
    /// List available models
    Models,
    /// List available tools
    Tools,
    /// Health check
    Health,
    /// Run the autonomous agent with a prompt
    Agent {
        /// The prompt for the agent
        #[arg(short, long)]
        prompt: String,
        /// The model to use
        #[arg(short, long, default_value = "qwen3:0.6b")]
        model: String,
        /// Maximum agent iterations
        #[arg(long, default_value = "100")]
        max_iterations: usize,
        /// Enable verbose output
        #[arg(short, long)]
        verbose: bool,
        /// Enable tool calling
        #[arg(short, long)]
        tools: bool,
    },
    /// Run as an MCP server over stdio
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
}

#[derive(Subcommand)]
enum McpCommands {
    /// Start the MCP JSON-RPC server on stdio
    Serve {
        /// The model to use for agent tasks
        #[arg(short, long, default_value = "qwen3:0.6b")]
        model: String,
        /// Maximum agent iterations per task
        #[arg(long, default_value = "100")]
        max_iterations: usize,
    },
}

/// Returns true when we should use the mock provider (CI-friendly, no Ollama required).
fn use_mock_provider() -> bool {
    std::env::var("NANNA_TEST_MOCK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// A trivial mock provider that always returns a canned response.
/// Used for integration tests in CI where Ollama is unavailable.
struct MockCliProvider;

#[async_trait::async_trait]
impl ModelProvider for MockCliProvider {
    async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
        Ok(ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: MessageRole::Assistant,
                    content: Some("Mock response".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: None,
        })
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        Ok(vec![ModelInfo {
            name: "mock".to_string(),
            size: Some(0),
            digest: None,
            modified_at: None,
        }])
    }

    async fn health_check(&self) -> ModelResult<()> {
        Ok(())
    }

    fn provider_name(&self) -> &'static str {
        "mock"
    }
}

/// Create an `Arc<dyn ModelProvider>` \u2014 mock when `NANNA_TEST_MOCK=1`, else Ollama.
fn create_provider() -> Result<Arc<dyn ModelProvider>, Box<dyn std::error::Error>> {
    if use_mock_provider() {
        Ok(Arc::new(MockCliProvider))
    } else {
        let config = OllamaConfig::default();
        Ok(Arc::new(OllamaProvider::new(config)?))
    }
}

fn emit(format: OutputFormat, code: ExitCode, data: serde_json::Value) -> std::process::ExitCode {
    match format {
        OutputFormat::Json => {
            let envelope = if code == ExitCode::Success {
                JsonEnvelope::success(data)
            } else {
                let msg = data
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| data.to_string());
                let error_code = match code {
                    ExitCode::StateError => "STATE_ERROR",
                    ExitCode::InfraError => "INFRA_ERROR",
                    ExitCode::UserError => "USER_ERROR",
                    ExitCode::Interrupted => "INTERRUPTED",
                    ExitCode::Success => unreachable!(),
                };
                JsonEnvelope::error(error_code, &msg)
            };
            println!("{}", envelope.to_json_string());
        }
        OutputFormat::Human => {
            if code == ExitCode::Success {
                print!("{}", harness::output::render(&data, OutputFormat::Human));
            } else {
                let msg = data.as_str().map(|s| s.to_string()).unwrap_or_else(|| {
                    harness::output::render(&data, OutputFormat::Human)
                        .trim()
                        .to_string()
                });
                eprintln!("Error: {msg}");
            }
        }
    }
    code.process_exit()
}

fn format_from(cli: &Cli) -> OutputFormat {
    if cli.json {
        OutputFormat::Json
    } else {
        OutputFormat::Human
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Install Ctrl+C handler so we can return exit code 130.
    let fmt_for_ctrlc = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fmt_clone = fmt_for_ctrlc.clone();
    let _ = ctrlc::set_handler(move || {
        // If JSON mode was set we can't easily detect from here, so just exit 130.
        if fmt_clone.load(std::sync::atomic::Ordering::Relaxed) {
            let envelope = JsonEnvelope::error("INTERRUPTED", "Received SIGINT");
            // Best-effort print
            let _ = writeln!(io::stdout(), "{}", envelope.to_json_string());
        } else {
            let _ = writeln!(io::stderr(), "Interrupted");
        }
        std::process::exit(130);
    });

    let cli = Cli::parse();
    let fmt = format_from(&cli);

    // Store whether JSON mode is active for the Ctrl+C handler.
    fmt_for_ctrlc.store(cli.json, std::sync::atomic::Ordering::Relaxed);

    match cli.command {
        // \u2500\u2500 6 MVP subcommands \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
        Commands::AssignTask {
            description,
            repo_path,
            branch,
            model,
            max_iterations,
        } => {
            let provider = match create_provider() {
                Ok(p) => p,
                Err(e) => {
                    return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
                }
            };
            let task_manager = Arc::new(TaskManager::default());
            let params = serde_json::json!({
                "description": description,
                "repo_path": repo_path.to_string_lossy(),
                "branch": branch,
                "model": model,
                "max_iterations": max_iterations,
            });
            match handlers::handle_assign_task(
                &params,
                &task_manager,
                &provider,
                &model,
                max_iterations,
            )
            .await
            {
                Ok(data) => emit(fmt, ExitCode::Success, data),
                Err(e) => emit(fmt, ExitCode::UserError, serde_json::json!(e)),
            }
        }

        Commands::PollTask { task_id, wait } => {
            // poll-task does not need a model provider.
            let task_manager = Arc::new(TaskManager::default());
            let params = serde_json::json!({ "task_id": task_id });

            if let Some(timeout_secs) = wait {
                // --wait: block until terminal state or timeout
                let deadline = timeout_secs
                    .map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));
                loop {
                    match handlers::handle_poll_task(&params, &task_manager).await {
                        Ok(data) => {
                            let status = data["status"].as_str().unwrap_or("");
                            if status == "Completed" || status == "Failed" {
                                return emit(fmt, ExitCode::Success, data);
                            }
                            if let Some(dl) = deadline {
                                if std::time::Instant::now() >= dl {
                                    return emit(
                                        fmt,
                                        ExitCode::StateError,
                                        serde_json::json!("Timed out waiting for task to finish"),
                                    );
                                }
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                        Err(e) => {
                            return emit(fmt, ExitCode::StateError, serde_json::json!(e));
                        }
                    }
                }
            } else {
                match handlers::handle_poll_task(&params, &task_manager).await {
                    Ok(data) => emit(fmt, ExitCode::Success, data),
                    Err(e) => emit(fmt, ExitCode::StateError, serde_json::json!(e)),
                }
            }
        }

        Commands::GetResult { task_id } => {
            let task_manager = Arc::new(TaskManager::default());
            let params = serde_json::json!({ "task_id": task_id });
            match handlers::handle_get_result(&params, &task_manager).await {
                Ok(data) => emit(fmt, ExitCode::Success, data),
                Err(e) => emit(fmt, ExitCode::StateError, serde_json::json!(e)),
            }
        }

        Commands::ListTasks => {
            let task_manager = Arc::new(TaskManager::default());
            match handlers::handle_list_tasks(&task_manager).await {
                Ok(data) => emit(fmt, ExitCode::Success, data),
                Err(e) => emit(fmt, ExitCode::InfraError, serde_json::json!(e)),
            }
        }

        Commands::CancelTask { task_id } => {
            let task_manager = Arc::new(TaskManager::default());
            let params = serde_json::json!({ "task_id": task_id });
            match handlers::handle_cancel_task(&params, &task_manager).await {
                Ok(data) => emit(fmt, ExitCode::Success, data),
                Err(e) => emit(fmt, ExitCode::StateError, serde_json::json!(e)),
            }
        }

        Commands::OnboardRepo { repo_path } => {
            let params =
                serde_json::json!({ "repo_path": repo_path.to_string_lossy().to_string() });
            match handlers::handle_onboard_repo(&params).await {
                Ok(data) => emit(fmt, ExitCode::Success, data),
                Err(e) => emit(fmt, ExitCode::UserError, serde_json::json!(e)),
            }
        }

        // \u2500\u2500 Legacy interactive commands (require provider) \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
        Commands::Chat {
            model,
            prompt,
            tools,
            temperature,
        } => {
            let provider = match create_provider() {
                Ok(p) => p,
                Err(e) => {
                    return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
                }
            };
            let workspace_root = match std::env::current_dir() {
                Ok(p) => p,
                Err(e) => {
                    return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
                }
            };
            let tool_registry = create_tool_registry(&workspace_root);

            if let Some(initial_prompt) = prompt {
                let entity_store = initialize_workspace(&workspace_root).await;
                let _ = entity_store; // used for interactive mode only
                if let Err(e) = single_chat(
                    &*provider,
                    &tool_registry,
                    &model,
                    &initial_prompt,
                    tools,
                    temperature,
                )
                .await
                {
                    return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
                }
            } else {
                let entity_store = initialize_workspace(&workspace_root).await;
                if let Err(e) = interactive_chat(
                    &*provider,
                    &tool_registry,
                    &model,
                    tools,
                    temperature,
                    entity_store,
                )
                .await
                {
                    return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
                }
            }
            ExitCode::Success.process_exit()
        }

        Commands::Models => {
            let provider = match create_provider() {
                Ok(p) => p,
                Err(e) => {
                    return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
                }
            };
            if let Err(e) = list_models(&*provider).await {
                return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
            }
            ExitCode::Success.process_exit()
        }

        Commands::Tools => {
            let workspace_root = std::env::current_dir().unwrap_or_default();
            let tool_registry = create_tool_registry(&workspace_root);
            list_tools(&tool_registry);
            ExitCode::Success.process_exit()
        }

        Commands::Health => {
            let provider = match create_provider() {
                Ok(p) => p,
                Err(e) => {
                    return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
                }
            };
            if let Err(e) = health_check(&*provider).await {
                return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
            }
            ExitCode::Success.process_exit()
        }

        Commands::Agent {
            prompt,
            model,
            max_iterations,
            verbose,
            tools,
        } => {
            let workspace_root = std::env::current_dir().unwrap_or_default();
            if let Err(e) = run_agent(
                &prompt,
                &model,
                max_iterations,
                verbose,
                tools,
                &workspace_root,
            )
            .await
            {
                return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
            }
            ExitCode::Success.process_exit()
        }

        Commands::Mcp {
            command:
                McpCommands::Serve {
                    model,
                    max_iterations,
                },
        } => {
            if let Err(e) = run_mcp_server(&model, max_iterations).await {
                return emit(fmt, ExitCode::InfraError, serde_json::json!(e.to_string()));
            }
            ExitCode::Success.process_exit()
        }
    }
}

fn create_tool_registry(workspace_root: &std::path::Path) -> ToolRegistry {
    harness::tools::create_tool_registry(workspace_root)
}

async fn initialize_workspace(workspace_root: &std::path::Path) -> InMemoryEntityStore {
    let mut store = InMemoryEntityStore::new();

    if let Some(git_repo) = GitRepository::detect(workspace_root) {
        info!(
            "Detected git repository: {} ({})",
            git_repo.current_branch.as_deref().unwrap_or("unknown"),
            git_repo.head_commit.as_deref().unwrap_or("unknown")
        );
        if let Err(e) = store.store(Box::new(git_repo)).await {
            error!("Failed to store git repository entity: {}", e);
        }
    }

    let scanner = WorkspaceScanner::new();
    match scanner.scan_workspace(workspace_root, &mut store).await {
        Ok(count) => {
            info!("Scanned {} files in workspace", count);
        }
        Err(e) => {
            error!("Failed to scan workspace: {}", e);
        }
    }

    store
}

async fn single_chat(
    provider: &dyn ModelProvider,
    tool_registry: &ToolRegistry,
    model: &str,
    prompt: &str,
    enable_tools: bool,
    temperature: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut messages = vec![ChatMessage::user(prompt)];

    loop {
        let mut request = ChatRequest::new(model, messages.clone()).with_temperature(temperature);

        if enable_tools {
            let tool_definitions = tool_registry.get_definitions();
            request = request.with_tools(tool_definitions);
        }

        let response = provider.chat(request).await?;
        let choice = &response.choices[0];

        if let Some(content) = &choice.message.content {
            println!("Assistant: {}", content);
        }

        if let Some(tool_calls) = &choice.message.tool_calls {
            println!("\nTool calls:");
            for tool_call in tool_calls {
                println!(
                    "  Calling {}: {:?}",
                    tool_call.function.name, tool_call.function.arguments
                );

                match tool_registry
                    .execute(
                        &tool_call.function.name,
                        tool_call.function.arguments.clone(),
                    )
                    .await
                {
                    Ok(result) => {
                        println!("  Result: {}", result);
                        messages.push(choice.message.clone());
                        messages.push(ChatMessage::tool_response(
                            tool_call.id.clone(),
                            result.to_string(),
                        ));
                    }
                    Err(e) => {
                        error!("Tool execution failed: {}", e);
                        messages.push(choice.message.clone());
                        messages.push(ChatMessage::tool_response(
                            tool_call.id.clone(),
                            format!("Error: {}", e),
                        ));
                    }
                }
            }

            continue;
        }

        break;
    }

    Ok(())
}

async fn interactive_chat(
    provider: &dyn ModelProvider,
    tool_registry: &ToolRegistry,
    model: &str,
    enable_tools: bool,
    temperature: f32,
    entity_store: InMemoryEntityStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let entity_count = entity_store
        .query(&harness::entities::EntityQuery::default())
        .await?
        .len();
    println!(
        "Starting interactive chat with {} (tools: {}, entities: {})",
        model, enable_tools, entity_count
    );
    println!("Type 'quit' or 'exit' to end the conversation.\n");

    let mut messages = vec![];

    loop {
        print!("You: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        if input == "quit" || input == "exit" {
            println!("Goodbye!");
            break;
        }

        messages.push(ChatMessage::user(input));

        loop {
            let mut request =
                ChatRequest::new(model, messages.clone()).with_temperature(temperature);

            if enable_tools {
                let tool_definitions = tool_registry.get_definitions();
                request = request.with_tools(tool_definitions);
            }

            let response = provider.chat(request).await?;
            let choice = &response.choices[0];

            if let Some(content) = &choice.message.content {
                println!("Assistant: {}", content);
            }

            if let Some(tool_calls) = &choice.message.tool_calls {
                println!("\n[Tool calls]");
                for tool_call in tool_calls {
                    println!(
                        "  Calling {}: {:?}",
                        tool_call.function.name, tool_call.function.arguments
                    );

                    match tool_registry
                        .execute(
                            &tool_call.function.name,
                            tool_call.function.arguments.clone(),
                        )
                        .await
                    {
                        Ok(result) => {
                            println!("  -> {}", result);
                            messages.push(choice.message.clone());
                            messages.push(ChatMessage::tool_response(
                                tool_call.id.clone(),
                                result.to_string(),
                            ));
                        }
                        Err(e) => {
                            error!("Tool execution failed: {}", e);
                            messages.push(choice.message.clone());
                            messages.push(ChatMessage::tool_response(
                                tool_call.id.clone(),
                                format!("Error: {}", e),
                            ));
                        }
                    }
                }
                println!();
                continue;
            }

            messages.push(choice.message.clone());
            break;
        }
    }

    Ok(())
}

async fn list_models(provider: &dyn ModelProvider) -> Result<(), Box<dyn std::error::Error>> {
    println!("Available models:");
    let models = provider.list_models().await?;

    if models.is_empty() {
        println!("  No models found. Make sure Ollama is running and has models installed.");
    } else {
        for model in models {
            println!(
                "  - {} ({})",
                model.name,
                model
                    .size
                    .map(|s| format!("{:.1} GB", s as f64 / 1_000_000_000.0))
                    .unwrap_or_else(|| "unknown size".to_string())
            );
        }
    }

    Ok(())
}

fn list_tools(tool_registry: &ToolRegistry) {
    println!("Available tools:");
    let tools = tool_registry.list_tools();

    if tools.is_empty() {
        println!("  No tools registered.");
    } else {
        for tool_name in tools {
            if let Some(tool) = tool_registry.get_tool(tool_name) {
                let def = tool.definition();
                println!("  - {}: {}", def.function.name, def.function.description);
            }
        }
    }
}

async fn health_check(provider: &dyn ModelProvider) -> Result<(), Box<dyn std::error::Error>> {
    println!("Performing health check...");

    match provider.health_check().await {
        Ok(()) => {
            println!("Health check passed. Ollama is running and accessible.");
            info!("Health check successful");
        }
        Err(e) => {
            println!("Health check failed: {}", e);
            error!("Health check failed: {}", e);
            return Err(e.into());
        }
    }

    Ok(())
}

async fn run_agent(
    prompt: &str,
    model: &str,
    max_iterations: usize,
    verbose: bool,
    tools: bool,
    workspace_root: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use harness::agent::{AgentConfig, AgentContext, AgentLoop};

    let provider = create_provider()?;
    let entity_store = initialize_workspace(workspace_root).await;

    let agent_config = AgentConfig {
        max_iterations,
        verbose,
        system_prompt: "You are a helpful coding assistant. Use the available tools to accomplish tasks. When you have completed the task, respond with a summary.".to_string(),
        model_name: model.to_string(),
    };

    let context = AgentContext {
        user_prompt: prompt.to_string(),
        conversation_history: vec![ChatMessage::user(prompt)],
        app_state_id: "cli".to_string(),
    };

    if verbose {
        println!("Starting agent with model: {}", model);
        println!("Prompt: {}", prompt);
        println!("Max iterations: {}", max_iterations);
        println!("Tools enabled: {}", tools);
    }

    let mut agent = if tools {
        let tool_registry = create_tool_registry(workspace_root);
        AgentLoop::with_tools(agent_config, entity_store, provider, tool_registry)
    } else {
        AgentLoop::with_llm(agent_config, entity_store, provider)
    };

    let result = agent.run(context).await?;

    println!("\n--- Agent Result ---");
    println!("Completed: {}", result.task_completed);
    println!("Iterations: {}", result.iterations);
    println!("Final state: {:?}", result.final_state);

    if verbose {
        println!("\n--- Conversation History ---");
        for msg in agent.conversation_history() {
            println!("[{:?}] {}", msg.role, msg.content.as_deref().unwrap_or(""));
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    println!(
                        "  Tool call: {} ({:?})",
                        tc.function.name, tc.function.arguments
                    );
                }
            }
        }
    } else if let Some(last) = agent.conversation_history().last() {
        if let Some(content) = &last.content {
            println!("\nAgent: {}", content);
        }
    }

    Ok(())
}

async fn run_mcp_server(
    model: &str,
    max_iterations: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use harness::mcp::NannaMcpServer;

    let provider = create_provider()?;
    let task_manager = Arc::new(TaskManager::default());

    info!(
        "Starting Nanna MCP server (model: {}, max_iterations: {})",
        model, max_iterations
    );

    let server = NannaMcpServer::new(task_manager, provider, model.to_string(), max_iterations);

    server.run_stdio().await?;
    Ok(())
}
