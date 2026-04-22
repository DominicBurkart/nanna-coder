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
#[command(name = "harness")]
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
    McpServe {
        /// The model to use for agent tasks
        #[arg(short, long, default_value = "qwen3:0.6b")]
        model: String,
        /// Maximum agent iterations per task
        #[arg(long, default_value = "100")]
        max_iterations: usize,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let config = OllamaConfig::default();
    let provider = OllamaProvider::new(config)?;

    let workspace_root = std::env::current_dir()?;
    let tool_registry = create_tool_registry(&workspace_root);

    match cli.command {
        Commands::Chat {
            model,
            prompt,
            tools,
            temperature,
        } => {
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
            list_models(&provider).await?;
        }
        Commands::Tools => {
            list_tools(&tool_registry);
        }
        Commands::Health => {
            health_check(&provider).await?;
        }
        Commands::Agent {
            prompt,
            model,
            max_iterations,
            verbose,
            tools,
        } => {
            run_agent(
                &prompt,
                &model,
                max_iterations,
                verbose,
                tools,
                &workspace_root,
            )
            .await?;
        }
        Commands::McpServe {
            model,
            max_iterations,
        } => {
            run_mcp_server(&model, max_iterations).await?;
        }
    }

    Ok(())
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

/// Shared settings for the CLI chat helpers (`single_chat` / `interactive_chat`).
///
/// Controls both model behaviour (tools / temperature) and surface output
/// (`tool_header` / `tool_result_prefix`) so the one-shot and interactive
/// modes can keep their distinct formatting while sharing the tool-call loop.
struct ChatTurnConfig<'a> {
    model: &'a str,
    enable_tools: bool,
    temperature: f32,
    tool_header: &'a str,
    tool_result_prefix: &'a str,
    /// Whether to print a blank line after the tool-call block (interactive
    /// mode does; one-shot mode does not).
    trailing_tool_newline: bool,
    /// Whether to append the final tool-call-free assistant message to
    /// `messages` so it persists across user turns.
    push_final_assistant: bool,
}

/// Drive a single chat turn to completion, looping through any tool-call
/// round-trips the model requests. Appends assistant and tool messages to
/// `messages` and returns once the model produces a final (tool-call-free)
/// response.
async fn run_chat_turn(
    provider: &OllamaProvider,
    tool_registry: &ToolRegistry,
    cfg: &ChatTurnConfig<'_>,
    messages: &mut Vec<ChatMessage>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let mut request =
            ChatRequest::new(cfg.model, messages.clone()).with_temperature(cfg.temperature);

        if cfg.enable_tools {
            request = request.with_tools(tool_registry.get_definitions());
        }

        let response = provider.chat(request).await?;
        let choice = &response.choices[0];

        if let Some(content) = &choice.message.content {
            println!("Assistant: {}", content);
        }

        let Some(tool_calls) = &choice.message.tool_calls else {
            if cfg.push_final_assistant {
                messages.push(choice.message.clone());
            }
            return Ok(());
        };

        // Push the assistant message once for the entire response batch,
        // before iterating over individual tool calls.
        messages.push(choice.message.clone());

        println!("{}", cfg.tool_header);
        for tool_call in tool_calls {
            println!(
                "  Calling {}: {:?}",
                tool_call.function.name, tool_call.function.arguments
            );

            let content = match tool_registry
                .execute(
                    &tool_call.function.name,
                    tool_call.function.arguments.clone(),
                )
                .await
            {
                Ok(result) => {
                    println!("{}{}", cfg.tool_result_prefix, result);
                    result.to_string()
                }
                Err(e) => {
                    error!("Tool execution failed: {}", e);
                    format!("Error: {}", e)
                }
            };

            messages.push(ChatMessage::tool_response(tool_call.id.clone(), content));
        }

        if cfg.trailing_tool_newline {
            println!();
        }
    }
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
    let cfg = ChatTurnConfig {
        model,
        enable_tools,
        temperature,
        tool_header: "\nTool calls:",
        tool_result_prefix: "  Result: ",
        trailing_tool_newline: false,
        push_final_assistant: false,
    };
    run_chat_turn(provider, tool_registry, &cfg, &mut messages).await
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
    let cfg = ChatTurnConfig {
        model,
        enable_tools,
        temperature,
        tool_header: "\n[Tool calls]",
        tool_result_prefix: "  -> ",
        trailing_tool_newline: true,
        push_final_assistant: true,
    };

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

        run_chat_turn(provider, tool_registry, &cfg, &mut messages).await?;
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

async fn run_agent(
    prompt: &str,
    model: &str,
    max_iterations: usize,
    verbose: bool,
    tools: bool,
    workspace_root: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use harness::agent::{AgentConfig, AgentContext, AgentLoop};
    use std::sync::Arc;

    let config = OllamaConfig::default();
    let provider = Arc::new(OllamaProvider::new(config)?);
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
    use harness::task::TaskManager;
    use std::sync::Arc;

    let config = OllamaConfig::default();
    let provider = Arc::new(OllamaProvider::new(config)?);
    let task_manager = Arc::new(TaskManager::default());

    info!(
        "Starting Nanna MCP server (model: {}, max_iterations: {})",
        model, max_iterations
    );

    let server = NannaMcpServer::new(task_manager, provider, model.to_string(), max_iterations);

    let reader = tokio::io::BufReader::new(tokio::io::stdin());
    let writer = tokio::io::stdout();
    server.serve(reader, writer).await?;
    Ok(())
}
