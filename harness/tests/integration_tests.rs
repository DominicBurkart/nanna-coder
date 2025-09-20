use harness::tools::{CalculatorTool, EchoTool, Tool, ToolRegistry};
use model::prelude::*;
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn test_model_provider_creation() {
    let config = OllamaConfig::default();
    let provider = OllamaProvider::new(config);
    assert!(provider.is_ok());
    assert_eq!(provider.unwrap().provider_name(), "ollama");
}

#[tokio::test]
async fn test_chat_request_building() {
    let messages = vec![
        ChatMessage::system("You are a helpful assistant"),
        ChatMessage::user("Hello, world!"),
    ];

    let request = ChatRequest::new("test-model", messages)
        .with_temperature(0.5)
        .with_max_tokens(100);

    assert_eq!(request.model, "test-model");
    assert_eq!(request.temperature, Some(0.5));
    assert_eq!(request.max_tokens, Some(100));
    assert_eq!(request.messages.len(), 2);
}

#[tokio::test]
async fn test_tool_registry() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    registry.register(Box::new(CalculatorTool::new()));

    assert_eq!(registry.list_tools().len(), 2);
    assert!(registry.get_tool("echo").is_some());
    assert!(registry.get_tool("calculate").is_some());
    assert!(registry.get_tool("nonexistent").is_none());

    let definitions = registry.get_definitions();
    assert_eq!(definitions.len(), 2);

    let echo_def = definitions
        .iter()
        .find(|d| d.function.name == "echo")
        .unwrap();
    assert_eq!(
        echo_def.function.description,
        "Echo back the provided message"
    );
}

#[tokio::test]
async fn test_echo_tool_execution() {
    let tool = EchoTool::new();
    let args = json!({ "message": "Hello, World!" });
    let result = tool.execute(args).await.unwrap();

    assert_eq!(result["echoed"], "Hello, World!");
    assert!(result["timestamp"].is_string());
}

#[tokio::test]
async fn test_calculator_tool_execution() {
    let tool = CalculatorTool::new();

    let add_args = json!({
        "operation": "add",
        "a": 5.0,
        "b": 3.0
    });
    let result = tool.execute(add_args).await.unwrap();
    assert_eq!(result["result"], 8.0);
    assert_eq!(result["operation"], "add");

    let multiply_args = json!({
        "operation": "multiply",
        "a": 4.0,
        "b": 6.0
    });
    let result = tool.execute(multiply_args).await.unwrap();
    assert_eq!(result["result"], 24.0);

    let divide_by_zero_args = json!({
        "operation": "divide",
        "a": 10.0,
        "b": 0.0
    });
    let result = tool.execute(divide_by_zero_args).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_message_role_serialization() {
    let message = ChatMessage::user("Test message");
    let json = serde_json::to_string(&message).unwrap();
    let deserialized: ChatMessage = serde_json::from_str(&json).unwrap();

    assert_eq!(message.role, deserialized.role);
    assert_eq!(message.content, deserialized.content);
}

#[tokio::test]
async fn test_tool_choice_serialization() {
    let choices = vec![
        ToolChoice::Auto,
        ToolChoice::None,
        ToolChoice::Required,
        ToolChoice::Specific("calculate".to_string()),
    ];

    for choice in choices {
        let json = serde_json::to_string(&choice).unwrap();
        let deserialized: ToolChoice = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            (choice, deserialized),
            (ToolChoice::Auto, ToolChoice::Auto)
                | (ToolChoice::None, ToolChoice::None)
                | (ToolChoice::Required, ToolChoice::Required)
                | (ToolChoice::Specific(_), ToolChoice::Specific(_))
        ));
    }
}

#[tokio::test]
async fn test_config_validation() {
    let valid_config = OllamaConfig::default();
    assert!(valid_config.validate().is_ok());

    let invalid_config = OllamaConfig::new()
        .with_base_url("")
        .with_context_length(0)
        .with_temperature(-1.0);
    assert!(invalid_config.validate().is_err());
}

#[tokio::test]
async fn test_tool_registry_execution() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    registry.register(Box::new(CalculatorTool::new()));

    let echo_result = registry
        .execute("echo", json!({ "message": "test" }))
        .await
        .unwrap();
    assert_eq!(echo_result["echoed"], "test");

    let calc_result = registry
        .execute(
            "calculate",
            json!({
                "operation": "subtract",
                "a": 10.0,
                "b": 3.0
            }),
        )
        .await
        .unwrap();
    assert_eq!(calc_result["result"], 7.0);

    let nonexistent_result = registry.execute("nonexistent", json!({})).await;
    assert!(nonexistent_result.is_err());
}

#[tokio::test]
async fn test_chat_request_with_tools() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    registry.register(Box::new(CalculatorTool::new()));

    let tools = registry.get_definitions();
    let messages = vec![ChatMessage::user("Use the echo tool to say hello")];

    let request = ChatRequest::new("test-model", messages)
        .with_tools(tools)
        .with_temperature(0.7);

    assert!(request.tools.is_some());
    assert_eq!(request.tools.as_ref().unwrap().len(), 2);
    assert_eq!(request.tool_choice, Some(ToolChoice::Auto));
}

#[tokio::test]
async fn test_model_info_serialization() {
    let model_info = ModelInfo {
        name: "test-model".to_string(),
        size: Some(1_000_000_000),
        digest: Some("test-digest".to_string()),
        modified_at: Some("2024-01-01T00:00:00Z".to_string()),
    };

    let json = serde_json::to_string(&model_info).unwrap();
    let deserialized: ModelInfo = serde_json::from_str(&json).unwrap();

    assert_eq!(model_info.name, deserialized.name);
    assert_eq!(model_info.size, deserialized.size);
    assert_eq!(model_info.digest, deserialized.digest);
    assert_eq!(model_info.modified_at, deserialized.modified_at);
}

// This test requires Ollama to be running - it's marked as ignored by default
#[tokio::test]
#[ignore = "requires ollama service"]
async fn test_ollama_health_check() {
    let config = OllamaConfig::default();
    let provider = OllamaProvider::new(config).unwrap();

    let result = timeout(Duration::from_secs(5), provider.health_check()).await;

    match result {
        Ok(Ok(())) => {
            println!("✓ Ollama health check passed");
        }
        Ok(Err(e)) => {
            println!("✗ Ollama health check failed: {}", e);
            println!("  Make sure Ollama is running on localhost:11434");
        }
        Err(_) => {
            println!("✗ Ollama health check timed out");
            println!("  Make sure Ollama is running on localhost:11434");
        }
    }
}

// This test requires Ollama to be running with models - it's marked as ignored by default
#[tokio::test]
#[ignore = "requires ollama service with models"]
async fn test_ollama_list_models() {
    let config = OllamaConfig::default();
    let provider = OllamaProvider::new(config).unwrap();

    let result = timeout(Duration::from_secs(5), provider.list_models()).await;

    match result {
        Ok(Ok(models)) => {
            println!("✓ Found {} models", models.len());
            for model in models {
                println!("  - {}", model.name);
            }
        }
        Ok(Err(e)) => {
            println!("✗ Failed to list models: {}", e);
            println!("  Make sure Ollama is running with models installed");
        }
        Err(_) => {
            println!("✗ List models request timed out");
        }
    }
}

// This test runs Ollama in an isolated container with qwen3:0.6b and tests communication
#[tokio::test]
#[ignore = "requires container runtime"]
async fn test_containerized_ollama_qwen3_communication() {
    use std::process::Command;
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    // Container configuration
    let container_name = "nanna-coder-ollama-test";
    let ollama_port = "11435"; // Use different port to avoid conflicts
    let qwen3_model = "qwen3:0.6b";

    println!("🚀 Starting containerized Ollama integration test...");

    // Clean up any existing container
    let _ = Command::new("podman")
        .args(["rm", "-f", container_name])
        .output();

    // Start Ollama container
    println!("📦 Starting Ollama container...");
    let container_start = Command::new("podman")
        .args([
            "run",
            "-d",
            "--name",
            container_name,
            "-p",
            &format!("{}:11434", ollama_port),
            "--rm",
            "ollama/ollama:latest",
        ])
        .output()
        .expect("Failed to start Ollama container");

    if !container_start.status.success() {
        panic!(
            "Failed to start Ollama container: {}",
            String::from_utf8_lossy(&container_start.stderr)
        );
    }

    println!("⏳ Waiting for Ollama to be ready...");
    sleep(Duration::from_secs(10)).await;

    // Pull qwen3:0.6b model
    println!("📥 Pulling qwen3:0.6b model...");
    let model_pull = Command::new("podman")
        .args(["exec", container_name, "ollama", "pull", qwen3_model])
        .output()
        .expect("Failed to pull model");

    if !model_pull.status.success() {
        let _ = Command::new("podman")
            .args(["rm", "-f", container_name])
            .output();
        panic!(
            "Failed to pull qwen3:0.6b model: {}",
            String::from_utf8_lossy(&model_pull.stderr)
        );
    }

    println!("✅ Model pulled successfully");

    // Configure provider to use the containerized Ollama
    let config = OllamaConfig::new()
        .with_base_url(format!("http://localhost:{}", ollama_port))
        .with_timeout(Duration::from_secs(60));

    let provider = match OllamaProvider::new(config) {
        Ok(p) => p,
        Err(e) => {
            let _ = Command::new("podman")
                .args(["rm", "-f", container_name])
                .output();
            panic!("Failed to create provider: {}", e);
        }
    };

    // Test 1: Health check
    println!("🏥 Testing health check...");
    let health_result = timeout(Duration::from_secs(10), provider.health_check()).await;
    match health_result {
        Ok(Ok(())) => println!("✅ Health check passed"),
        Ok(Err(e)) => {
            let _ = Command::new("podman")
                .args(["rm", "-f", container_name])
                .output();
            panic!("Health check failed: {}", e);
        }
        Err(_) => {
            let _ = Command::new("podman")
                .args(["rm", "-f", container_name])
                .output();
            panic!("Health check timed out");
        }
    }

    // Test 2: List models (should include qwen3:0.6b)
    println!("📋 Testing model listing...");
    let models_result = timeout(Duration::from_secs(10), provider.list_models()).await;
    match models_result {
        Ok(Ok(models)) => {
            let qwen3_found = models.iter().any(|m| m.name.contains("qwen3"));
            if !qwen3_found {
                let _ = Command::new("podman")
                    .args(["rm", "-f", container_name])
                    .output();
                panic!("qwen3:0.6b model not found in model list");
            }
            println!("✅ Model listing passed - qwen3:0.6b found");
        }
        Ok(Err(e)) => {
            let _ = Command::new("podman")
                .args(["rm", "-f", container_name])
                .output();
            panic!("Failed to list models: {}", e);
        }
        Err(_) => {
            let _ = Command::new("podman")
                .args(["rm", "-f", container_name])
                .output();
            panic!("Model listing timed out");
        }
    }

    // Test 3: Chat with qwen3:0.6b
    println!("💬 Testing chat with qwen3:0.6b...");
    let messages = vec![ChatMessage::user(
        "Say 'Hello from qwen3!' in exactly those words.",
    )];
    let chat_request = ChatRequest::new(qwen3_model, messages).with_temperature(0.1); // Low temperature for deterministic response

    let chat_result = timeout(Duration::from_secs(30), provider.chat(chat_request)).await;
    match chat_result {
        Ok(Ok(response)) => {
            if let Some(content) = &response.choices[0].message.content {
                println!("✅ Chat response received: {}", content);

                // Verify we got a reasonable response
                if content.trim().is_empty() {
                    let _ = Command::new("podman")
                        .args(["rm", "-f", container_name])
                        .output();
                    panic!("Received empty response from qwen3:0.6b");
                }
            } else {
                let _ = Command::new("podman")
                    .args(["rm", "-f", container_name])
                    .output();
                panic!("No content in chat response");
            }
        }
        Ok(Err(e)) => {
            let _ = Command::new("podman")
                .args(["rm", "-f", container_name])
                .output();
            panic!("Chat request failed: {}", e);
        }
        Err(_) => {
            let _ = Command::new("podman")
                .args(["rm", "-f", container_name])
                .output();
            panic!("Chat request timed out");
        }
    }

    // Test 4: Chat with tools enabled
    println!("🔧 Testing chat with tools enabled...");
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Box::new(EchoTool::new()));
    tool_registry.register(Box::new(CalculatorTool::new()));

    let tool_messages = vec![ChatMessage::user(
        "Calculate 7 plus 13 using the calculator tool.",
    )];
    let tool_request = ChatRequest::new(qwen3_model, tool_messages)
        .with_tools(tool_registry.get_definitions())
        .with_temperature(0.1);

    let tool_result = timeout(Duration::from_secs(30), provider.chat(tool_request)).await;
    match tool_result {
        Ok(Ok(response)) => {
            let choice = &response.choices[0];

            // Check if we got tool calls or a direct response
            if let Some(tool_calls) = &choice.message.tool_calls {
                println!("✅ Tool calls received: {} calls", tool_calls.len());

                // Verify at least one tool call was made
                if tool_calls.is_empty() {
                    let _ = Command::new("podman")
                        .args(["rm", "-f", container_name])
                        .output();
                    panic!("Expected tool calls but got none");
                }
            } else if let Some(content) = &choice.message.content {
                // Some models might respond directly without tool calls
                println!("✅ Direct response received (no tool calls): {}", content);
            } else {
                let _ = Command::new("podman")
                    .args(["rm", "-f", container_name])
                    .output();
                panic!("No tool calls or content in response");
            }
        }
        Ok(Err(e)) => {
            let _ = Command::new("podman")
                .args(["rm", "-f", container_name])
                .output();
            panic!("Tool-enabled chat request failed: {}", e);
        }
        Err(_) => {
            let _ = Command::new("podman")
                .args(["rm", "-f", container_name])
                .output();
            panic!("Tool-enabled chat request timed out");
        }
    }

    // Cleanup: Stop and remove container
    println!("🧹 Cleaning up container...");
    let cleanup_result = Command::new("podman")
        .args(["rm", "-f", container_name])
        .output();

    match cleanup_result {
        Ok(output) => {
            if output.status.success() {
                println!("✅ Container cleaned up successfully");
            } else {
                eprintln!(
                    "⚠️  Warning: Failed to clean up container: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        Err(e) => {
            eprintln!("⚠️  Warning: Failed to execute cleanup: {}", e);
        }
    }

    println!("🎉 Containerized Ollama integration test completed successfully!");
}
