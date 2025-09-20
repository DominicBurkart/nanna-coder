# Nanna Coder

A Rust workspace for building AI coding agents with Ollama integration.

## Overview

Nanna Coder provides a strongly-typed, modular framework for building AI coding agents that run locally with Ollama. The project consists of three main components:

- **`model`** - Core library for AI model interaction with strong typing and Ollama integration
- **`image-builder`** - Library for container and deployment tooling
- **`harness`** - CLI application for running and testing the agent system

## Features

- 🦀 **Rust Native** - Memory-safe, performant implementation
- 🔒 **Privacy First** - Local model execution with Ollama (no cloud dependencies)
- 🔧 **Strongly Typed** - Comprehensive type safety with enums instead of strings
- 🛠️ **Tool System** - Extensible tool calling with structured arguments
- 📦 **Modular Design** - Clean separation between model interface and tool execution
- ⚡ **High Context** - 110k token default context window

## Quick Start

### Prerequisites

- Rust 1.70+ with 2024 edition support
- [Ollama](https://ollama.ai/) installed and running
- At least one model pulled (e.g., `ollama pull llama3.1:8b`)

### Installation

```bash
git clone https://github.com/dominicburkart/nanna-coder.git
cd nanna-coder
cargo build --release
```

### Usage

```bash
# Start interactive chat
cargo run --bin harness -- chat --interactive --tools

# Single prompt
cargo run --bin harness -- chat --prompt "Hello, world!" --model llama3.1:8b

# List available models
cargo run --bin harness -- models

# Health check
cargo run --bin harness -- health

# List available tools
cargo run --bin harness -- tools
```

## Architecture

### Model Crate

The `model` crate provides:

- **Types** - Strongly-typed message, tool, and schema definitions
- **Provider Trait** - Abstract interface for different LLM providers
- **Ollama Integration** - Production-ready Ollama client implementation
- **Configuration** - Flexible configuration with validation

Key types:
```rust
pub enum MessageRole { System, User, Assistant, Tool }
pub enum ToolChoice { Auto, None, Required, Specific(String) }
pub enum FinishReason { Stop, ToolCalls, Length, ContentFilter }
```

### Tool System

Tools are strongly typed with JSON schema validation:

```rust
use model::prelude::*;
use harness::tools::*;

let mut registry = ToolRegistry::new();
registry.register(Box::new(EchoTool::new()));
registry.register(Box::new(CalculatorTool::new()));

// Tools are automatically available to the LLM
let tools = registry.get_definitions();
let request = ChatRequest::new("llama3.1:8b", messages).with_tools(tools);
```

### Example Tool Implementation

```rust
#[async_trait]
impl Tool for CalculatorTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "calculate".to_string(),
                description: "Perform basic arithmetic".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some(/* ... */),
                    required: Some(vec!["operation".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        // Strongly typed argument extraction and execution
    }
}
```

## Development

### Running Tests

```bash
# All tests
cargo test --workspace

# Model crate only
cargo test --package model

# With Ollama integration (requires running Ollama)
cargo test -- --ignored
```

### Project Structure

```
nanna-coder/
├── Cargo.toml          # Workspace configuration
├── model/              # Core model interface library
│   ├── src/
│   │   ├── types.rs    # Strongly-typed message and tool definitions
│   │   ├── provider.rs # ModelProvider trait and errors
│   │   ├── config.rs   # Configuration with validation
│   │   ├── ollama.rs   # Ollama integration
│   │   └── lib.rs      # Public API
│   └── Cargo.toml
├── image-builder/      # Container and deployment tooling
├── harness/            # CLI application
│   ├── src/
│   │   ├── main.rs     # CLI interface
│   │   ├── tools.rs    # Tool implementations
│   │   └── lib.rs      # Public API
│   └── Cargo.toml
└── tests/              # Integration tests
    └── integration_tests.rs
```

## Security & Privacy

- **Local Execution** - All model inference happens locally via Ollama
- **No Telemetry** - No data is sent to external services
- **Memory Safety** - Rust's ownership system prevents common vulnerabilities
- **Input Validation** - Comprehensive validation of all inputs

## Compatibility

Designed to be compatible with existing coding agents:

- **OpenAI-style API** - Familiar request/response patterns
- **Tool Calling** - Standard function calling interface
- **Streaming** - Support for real-time response streaming (planned)

## License

This project is licensed under the MIT License - see the LICENSE file for details.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.