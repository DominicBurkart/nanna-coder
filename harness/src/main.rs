use clap::{Parser, Subcommand};
use harness::entities::ast::WorkspaceScanner;
use harness::entities::git::GitRepository;
use harness::entities::{EntityStore, InMemoryEntityStore};
use harness::tools::ToolRegistry;
use model::prelude::*;
use std::io::{self, Write};
use tracing::{error, info};

// NOTE: `main.rs` binds the workspace entity store to `InMemoryEntityStore`
// concretely today, but the downstream callers accept any `EntityStore` via
// generics (see `AgentLoop<S>` and `interactive_chat`). Issue #193 Phase B
// will introduce `PersistentEntityStore` and swap the binding here.

#[derive(Parser)]
#[command(name = "nanna")]
#[command(about = "A CLI tool for interacting with language models")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
        /// Skip the on-startup pod-ensure check
        #[arg(long)]
        no_ensure_pod: bool,
    },
    /// List available models
    Models,
    /// List available tools
    Tools,
    /// Health check
    Health {
        /// Skip the on-startup pod-ensure check
        #[arg(long)]
        no_ensure_pod: bool,
    },
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
        /// Workspace root the agent operates against. Defaults to cwd.
        #[arg(long)]
        work_dir: Option<std::path::PathBuf>,
        /// Write the structured AgentRunReport JSON here. When set, the
        /// human-readable summary is suppressed and the eval-shape JSON is
        /// the sole output artifact.
        #[arg(long)]
        output_json: Option<std::path::PathBuf>,
        /// Override the Ollama endpoint (default http://localhost:11434).
        #[arg(long)]
        ollama_url: Option<String>,
        /// Skip the on-startup pod-ensure check
        #[arg(long)]
        no_ensure_pod: bool,
    },
    /// Run as an MCP server over stdio
    McpServe {
        /// The model to use for agent tasks
        #[arg(short, long, default_value = "qwen3:0.6b")]
        model: String,
        /// Maximum agent iterations per task
        #[arg(long, default_value = "100")]
        max_iterations: usize,
    },
    /// Delegate a coding task through the MCP Tasks protocol.
    ///
    /// Drives an in-process MCP server as a Tasks client: submits an
    /// `assign_task` tool call augmented with a `task`, polls `tasks/get` to
    /// completion, and prints the `tasks/result` payload. This exercises the
    /// same wire protocol external orchestrators use.
    Delegate {
        /// Description of the coding task to perform
        #[arg(short, long)]
        description: String,
        /// Absolute path to the git repository
        #[arg(short, long)]
        repo_path: std::path::PathBuf,
        /// Branch or ref to base the worktree on
        #[arg(short, long, default_value = "HEAD")]
        branch: String,
        /// The model to use
        #[arg(short, long, default_value = "qwen3:0.6b")]
        model: String,
        /// Maximum agent iterations
        #[arg(long, default_value = "100")]
        max_iterations: usize,
        /// Requested task lifetime in milliseconds
        #[arg(long)]
        ttl_ms: Option<u64>,
    },
    /// Generate a SWE-bench report from JSON results
    SweBenchReport {
        /// Path to the JSON results file
        #[arg(short, long)]
        input: std::path::PathBuf,
        /// Output base directory. The final report is written under
        /// `<output_dir>/<sha>/<bench>/<scenario>/report.md`. Defaults to
        /// `<current_working_directory>/results`.
        #[arg(short, long)]
        output_dir: Option<std::path::PathBuf>,
        /// Optional second JSON file for comparison report
        #[arg(long)]
        compare: Option<std::path::PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Chat {
            model,
            prompt,
            tools,
            temperature,
            no_ensure_pod,
        } => {
            ensure_pod_or_exit(no_ensure_pod).await;
            let provider = OllamaProvider::new(OllamaConfig::default())?;
            let workspace_root = std::env::current_dir()?;
            let tool_registry = create_tool_registry(&workspace_root);
            let entity_store = initialize_workspace(&workspace_root).await;

            if let Some(initial_prompt) = prompt {
                single_chat(
                    &provider,
                    &tool_registry,
                    &model,
                    &initial_prompt,
                    tools,
                    temperature,
                )
                .await?;
            } else {
                interactive_chat(
                    &provider,
                    &tool_registry,
                    &model,
                    tools,
                    temperature,
                    entity_store,
                )
                .await?;
            }
        }
        Commands::Models => {
            let provider = OllamaProvider::new(OllamaConfig::default())?;
            list_models(&provider).await?;
        }
        Commands::Tools => {
            let workspace_root = std::env::current_dir()?;
            let tool_registry = create_tool_registry(&workspace_root);
            list_tools(&tool_registry);
        }
        Commands::Health { no_ensure_pod } => {
            ensure_pod_or_exit(no_ensure_pod).await;
            let provider = OllamaProvider::new(OllamaConfig::default())?;
            health_check(&provider).await?;
        }
        Commands::Agent {
            prompt,
            model,
            max_iterations,
            verbose,
            tools,
            work_dir,
            output_json,
            ollama_url,
            no_ensure_pod,
        } => {
            ensure_pod_or_exit(no_ensure_pod).await;
            let workspace_root = match work_dir {
                Some(p) => p,
                None => std::env::current_dir()?,
            };
            run_agent(
                &prompt,
                &model,
                max_iterations,
                verbose,
                tools,
                &workspace_root,
                output_json.as_deref(),
                ollama_url.as_deref(),
            )
            .await?;
        }
        Commands::McpServe {
            model,
            max_iterations,
        } => {
            run_mcp_server(&model, max_iterations).await?;
        }
        Commands::Delegate {
            description,
            repo_path,
            branch,
            model,
            max_iterations,
            ttl_ms,
        } => {
            run_delegate(
                &description,
                &repo_path,
                &branch,
                &model,
                max_iterations,
                ttl_ms,
            )
            .await?;
        }
        Commands::SweBenchReport {
            input,
            output_dir,
            compare,
        } => {
            let (report_path, comparison_path) =
                generate_swebench_report(&input, output_dir.as_deref(), compare.as_deref())?;
            println!("Report written to: {}", report_path.display());
            if let Some(p) = comparison_path {
                println!("Comparison report written to: {}", p.display());
            }
        }
    }

    Ok(())
}

fn generate_swebench_report(
    input: &std::path::Path,
    output_dir: Option<&std::path::Path>,
    compare: Option<&std::path::Path>,
) -> Result<(std::path::PathBuf, Option<std::path::PathBuf>), Box<dyn std::error::Error>> {
    use harness::eval::swebench_report::SweBenchReport;
    use harness::eval::swebench_results::SweBenchRunResult;

    let json = std::fs::read_to_string(input)?;
    let run_result: SweBenchRunResult = serde_json::from_str(&json)?;

    let owned_default;
    let base_dir: &std::path::Path = match output_dir {
        Some(p) => p,
        None => {
            owned_default = std::env::current_dir()?.join("results");
            owned_default.as_path()
        }
    };

    let report = SweBenchReport::new("SWE-bench Report", run_result);
    let report_path = report.write_to_directory(base_dir)?;

    let comparison_path = if let Some(compare_path) = compare {
        let compare_json = std::fs::read_to_string(compare_path)?;
        let compare_result: SweBenchRunResult = serde_json::from_str(&compare_json)?;
        Some(report.write_comparison_to_directory(&compare_result, base_dir)?)
    } else {
        None
    };

    Ok((report_path, comparison_path))
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

    // Surface repo-level agent guidance (AGENTS.md / CLAUDE.md) into the
    // entity store as a `ContextEntity` so tool-accessible retrieval paths
    // (RAG, entity queries) can discover it the same way as conversation
    // history and tool-call records. See issue #231.
    store_repo_guidance_entity(workspace_root, &mut store).await;

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

async fn store_repo_guidance_entity(
    workspace_root: &std::path::Path,
    store: &mut InMemoryEntityStore,
) {
    use harness::entities::context::types::ContextEntity;

    match harness::agent::agents_md::load(workspace_root) {
        Ok(Some(doc)) => {
            let mut entity = ContextEntity::new(
                format!("repo-guidance:{}", doc.source.filename()),
                Vec::new(),
                Vec::new(),
                doc.body.clone(),
                "n/a".to_string(),
            );
            entity
                .metadata
                .tags
                .push(format!("agents-md:{}", doc.source.filename()));
            if doc.truncated {
                entity.metadata.tags.push("truncated".to_string());
            }
            if let Err(e) = store.store(Box::new(entity)).await {
                error!("Failed to store AGENTS.md entity: {}", e);
            } else {
                info!(
                    path = %doc.path.display(),
                    source = doc.source.filename(),
                    "Stored repo-level agent guidance entity"
                );
            }
        }
        Ok(None) => {}
        Err(e) => {
            error!(
                error = %e,
                "Failed to read AGENTS.md / CLAUDE.md; skipping entity injection"
            );
        }
    }
}

/// Records a task failure caused by a tool execution error.
///
/// Appends both the assistant message that triggered the failing tool call and
/// a `tool_response` message carrying the error text to `messages`, so the
/// conversation history accurately reflects what happened. This mirrors the
/// successful-execution path and is called from every site that handles an
/// `Err` arm after [`ToolRegistry::execute`].
fn record_task_failure(
    messages: &mut Vec<ChatMessage>,
    assistant_message: ChatMessage,
    tool_call_id: String,
    error: &dyn std::error::Error,
) {
    error!("Tool execution failed: {}", error);
    messages.push(assistant_message);
    messages.push(ChatMessage::tool_response(
        tool_call_id,
        format!("Error: {}", error),
    ));
}

async fn single_chat(
    provider: &OllamaProvider,
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
                        record_task_failure(
                            &mut messages,
                            choice.message.clone(),
                            tool_call.id.clone(),
                            &e,
                        );
                    }
                }
            }

            continue;
        }

        break;
    }

    Ok(())
}

async fn interactive_chat<S: EntityStore + Send>(
    provider: &OllamaProvider,
    tool_registry: &ToolRegistry,
    model: &str,
    enable_tools: bool,
    temperature: f32,
    entity_store: S,
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
                            record_task_failure(
                                &mut messages,
                                choice.message.clone(),
                                tool_call.id.clone(),
                                &e,
                            );
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

async fn list_models(provider: &OllamaProvider) -> Result<(), Box<dyn std::error::Error>> {
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

async fn health_check(provider: &OllamaProvider) -> Result<(), Box<dyn std::error::Error>> {
    println!("Performing health check...");

    match provider.health_check().await {
        Ok(()) => {
            println!("\u{2713} Health check passed. Ollama is running and accessible.");
            info!("Health check successful");
        }
        Err(e) => {
            println!("\u{2717} Health check failed: {}", e);
            error!("Health check failed: {}", e);
            return Err(e.into());
        }
    }

    Ok(())
}

/// Default system prompt used when an onboarded repo does not supply any
/// repo-level guidance. Kept in a `const` so the `AGENTS.md` loader and the
/// task-dispatch path (`harness/src/task.rs`) share a single source of truth.
const DEFAULT_SESSION_SYSTEM_PROMPT: &str = "You are a helpful coding assistant. Use the available tools to accomplish tasks. When you have completed the task, respond with a summary.";

/// Build the system prompt for a session, appending any repo-level guidance
/// discovered under `workspace_root` (closes #231).
///
/// Precedence is enforced by [`harness::agent::agents_md::load`]: `AGENTS.md`
/// wins over `CLAUDE.md`. Missing files produce no injection and no error.
/// Read errors are logged and swallowed so a broken guidance file never blocks
/// a session from starting.
fn build_session_system_prompt(workspace_root: &std::path::Path) -> String {
    match harness::agent::agents_md::load(workspace_root) {
        Ok(Some(doc)) => {
            info!(
                path = %doc.path.display(),
                source = doc.source.filename(),
                truncated = doc.truncated,
                "Loaded repo-level agent guidance into session system prompt"
            );
            format!(
                "{}\n\n{}",
                DEFAULT_SESSION_SYSTEM_PROMPT,
                harness::agent::agents_md::format_system_prompt_fragment(&doc)
            )
        }
        Ok(None) => DEFAULT_SESSION_SYSTEM_PROMPT.to_string(),
        Err(e) => {
            error!(
                error = %e,
                "Failed to read AGENTS.md / CLAUDE.md; continuing without repo guidance"
            );
            DEFAULT_SESSION_SYSTEM_PROMPT.to_string()
        }
    }
}

/// Run `pod::ensure_running`; on failure, print to stderr and exit with
/// code 3 so the eval-side caller (or a CI step) can distinguish pod
/// bring-up failures from agent-loop failures.
///
/// `async` to match `pod::ensure_running` — the probe and post-bring-up
/// health wait both run on the Tokio executor. The previous sync wrapper
/// used `std::thread::sleep` inside `wait_for_ollama` and stalled the
/// runtime for up to 60s.
async fn ensure_pod_or_exit(no_ensure_pod_flag: bool) {
    let cfg = harness::pod::EnsureConfig::from_env_and_flag(no_ensure_pod_flag);
    match harness::pod::ensure_running(&cfg).await {
        Ok(_) => {}
        Err(e) => {
            eprintln!("nanna: pod ensure failed: {e}");
            std::process::exit(3);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_agent(
    prompt: &str,
    model: &str,
    max_iterations: usize,
    verbose: bool,
    tools: bool,
    workspace_root: &std::path::Path,
    output_json: Option<&std::path::Path>,
    ollama_url: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use harness::agent::{AgentConfig, AgentContext, AgentLoop, AgentRunReport};
    use std::sync::Arc;

    let mut config = OllamaConfig::default();
    if let Some(url) = ollama_url {
        config = config.with_base_url(url.to_string());
    }
    let provider = Arc::new(OllamaProvider::new(config)?);
    let entity_store = initialize_workspace(workspace_root).await;

    let agent_config = AgentConfig {
        max_iterations,
        verbose,
        system_prompt: build_session_system_prompt(workspace_root),
        model_name: model.to_string(),
    };

    // Match the eval-runner shape: prompt enters via `user_prompt`, not via
    // pre-seeded `conversation_history`. `run_tool_loop` builds the user
    // message from `user_prompt` itself.
    let context = AgentContext {
        user_prompt: prompt.to_string(),
        conversation_history: vec![],
        app_state_id: "cli".to_string(),
    };

    if verbose {
        eprintln!("Starting agent with model: {}", model);
        eprintln!("Prompt: {}", prompt);
        eprintln!("Max iterations: {}", max_iterations);
        eprintln!("Tools enabled: {}", tools);
    }

    let mut agent = if tools {
        let tool_registry = create_tool_registry(workspace_root);
        AgentLoop::with_tools(agent_config, entity_store, provider, tool_registry)
    } else {
        AgentLoop::with_llm(agent_config, entity_store, provider)
    };

    let result = match agent.run_tool_loop(context).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("nanna: agent error: {e}");
            std::process::exit(2);
        }
    };

    if let Some(path) = output_json {
        let report = AgentRunReport::from(&result);
        report.write_to_path(path)?;
        return Ok(());
    }

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
    use harness::task::TaskManager;
    use std::sync::Arc;

    let config = OllamaConfig::default();
    let provider = Arc::new(OllamaProvider::new(config)?);
    let task_manager = Arc::new(TaskManager::default());

    info!(
        "Starting Nanna MCP server (model: {}, max_iterations: {})",
        model, max_iterations
    );

    let server = Arc::new(NannaMcpServer::new(
        task_manager,
        provider,
        model.to_string(),
        max_iterations,
    ));

    let reader = tokio::io::BufReader::new(tokio::io::stdin());
    let writer = tokio::io::stdout();
    server.serve(reader, writer).await?;
    Ok(())
}

/// Delegate a coding task through the MCP Tasks protocol by driving an
/// in-process server as a client over an in-memory duplex. This dogfoods the
/// exact wire protocol external orchestrators use, without spawning a child
/// process.
async fn run_delegate(
    description: &str,
    repo_path: &std::path::Path,
    branch: &str,
    model: &str,
    max_iterations: usize,
    ttl_ms: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    use harness::mcp::client::NannaMcpClient;
    use harness::mcp::NannaMcpServer;
    use harness::task::TaskManager;
    use std::sync::Arc;

    let config = OllamaConfig::default();
    let provider = Arc::new(OllamaProvider::new(config)?);
    let task_manager = Arc::new(TaskManager::default());
    let server = Arc::new(NannaMcpServer::new(
        task_manager,
        provider,
        model.to_string(),
        max_iterations,
    ));

    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_side);
    let serve_handle = tokio::spawn(async move {
        let _ = server
            .serve(tokio::io::BufReader::new(server_read), server_write)
            .await;
    });

    let (client_read, client_write) = tokio::io::split(client_side);
    let mut client = NannaMcpClient::new(tokio::io::BufReader::new(client_read), client_write);

    client.initialize().await?;
    let arguments = serde_json::json!({
        "description": description,
        "repo_path": repo_path.to_string_lossy(),
        "branch": branch,
        "model": model,
        "max_iterations": max_iterations,
    });
    let task_id = client.submit_task(arguments, ttl_ms).await?;
    info!("Delegated task {task_id}; awaiting completion...");

    let result = client.wait_result(&task_id).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);

    drop(client);
    let _ = serve_handle.await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::eval::swebench_results::{
        SweBenchInstanceResult, SweBenchRunConfig, SweBenchRunResult, TokenUsage,
    };

    fn fixture_run(scenario: &str) -> SweBenchRunResult {
        SweBenchRunResult {
            config: SweBenchRunConfig {
                commit_sha: "abc123".to_string(),
                bench_name: "swebench_verified".to_string(),
                scenario: scenario.to_string(),
                model_name: Some("gemma4:e4b".to_string()),
                timestamp: chrono::Utc::now(),
            },
            instances: vec![SweBenchInstanceResult {
                instance_id: "django__django-11099".to_string(),
                resolved: true,
                orchestrator_token_usage: TokenUsage {
                    prompt_tokens: 100,
                    completion_tokens: 50,
                    total_tokens: 150,
                },
                worker_token_usage: None,
                wall_time_secs: 12.0,
                error: None,
            }],
        }
    }

    #[test]
    fn generate_report_compare_path_writes_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let a_path = dir.path().join("a.json");
        let b_path = dir.path().join("b.json");
        std::fs::write(
            &a_path,
            serde_json::to_string(&fixture_run("nanna_only")).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &b_path,
            serde_json::to_string(&fixture_run("claude_plus_nanna")).unwrap(),
        )
        .unwrap();

        let out = dir.path().join("out");
        let (report_path, comparison_path) =
            generate_swebench_report(&a_path, Some(out.as_path()), Some(b_path.as_path()))
                .expect("generate should succeed");

        assert!(report_path.exists(), "report.md missing");
        let comparison_path = comparison_path.expect("comparison path returned");
        assert!(comparison_path.exists(), "comparison.md missing");
        assert!(comparison_path
            .to_string_lossy()
            .contains("nanna_only_vs_claude_plus_nanna"));
    }

    #[test]
    fn generate_report_returns_err_on_bad_json() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{not valid json").unwrap();

        let out = dir.path().join("out");
        let result = generate_swebench_report(&bad, Some(out.as_path()), None);
        assert!(result.is_err(), "expected error on malformed JSON");
    }

    #[test]
    fn generate_report_returns_err_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does_not_exist.json");
        let out = dir.path().join("out");
        let result = generate_swebench_report(&missing, Some(out.as_path()), None);
        assert!(result.is_err(), "expected error on missing input file");
    }

    #[test]
    fn generate_report_returns_err_on_missing_compare_file() {
        let dir = tempfile::tempdir().unwrap();
        let a_path = dir.path().join("a.json");
        std::fs::write(&a_path, serde_json::to_string(&fixture_run("a")).unwrap()).unwrap();
        let out = dir.path().join("out");
        let missing_compare = dir.path().join("missing.json");
        let result = generate_swebench_report(
            &a_path,
            Some(out.as_path()),
            Some(missing_compare.as_path()),
        );
        assert!(result.is_err(), "expected error on missing compare file");
    }
}
