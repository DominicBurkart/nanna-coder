mod tools;

use clap::{Parser, Subcommand};
use model::prelude::*;
use std::io::{self, Write};
use tools::{CalculatorTool, EchoTool, ToolRegistry};
use tracing::{error, info};

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let config = OllamaConfig::default();
    let provider = OllamaProvider::new(config)?;

    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(EchoTool::new()));
    tool_registry.register(Box::new(CalculatorTool::new()));

    match cli.command {
        Commands::Chat {
            model,
            prompt,
            tools,
            temperature,
        } => {
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
                interactive_chat(&provider, &tool_registry, &model, tools, temperature).await?;
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
    }

    Ok(())
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
    provider: &OllamaProvider,
    tool_registry: &ToolRegistry,
    model: &str,
    enable_tools: bool,
    temperature: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Starting interactive chat with {} (tools: {})",
        model, enable_tools
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
            println!("✓ Health check passed. Ollama is running and accessible.");
            info!("Health check successful");
        }
        Err(e) => {
            println!("✗ Health check failed: {}", e);
            error!("Health check failed: {}", e);
            return Err(e.into());
        }
    }

    Ok(())
}
