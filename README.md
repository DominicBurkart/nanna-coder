# Nanna Coder

> AI-powered coding assistant with tool calling and multi-model support, built with Rust and containerized using Nix

[![CI/CD Pipeline](https://github.com/dominicburkart/nanna-coder/actions/workflows/ci.yml/badge.svg)](https://github.com/dominicburkart/nanna-coder/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## 🚀 Features

- 🦀 **Rust Native** - Memory-safe, performant implementation with 2024 edition
- 🔒 **Privacy First** - Local model execution with Ollama (no cloud dependencies)
- 🔧 **Strongly Typed** - Comprehensive type safety with enums instead of strings
- 🛠️ **Tool System** - Extensible tool calling with structured arguments
- 📦 **Containerized Architecture** - Complete isolation using Nix and Podman/Docker
- 🌐 **Cross-Platform** - Native support for x86_64 and ARM64 architectures
- 🎮 **GPU Acceleration** - Support for NVIDIA, AMD, and Intel GPUs
- ⚡ **High Context** - 110k token default context window
- 🔄 **Reproducible Builds** - Deterministic builds with Nix

## 📋 Prerequisites

- [Nix](https://nixos.org/download.html) with flakes enabled (or [Lix](https://lix.systems/))
- [Podman](https://podman.io/) or Docker for containerization
- [direnv](https://direnv.net/) (optional, for automatic environment loading)

### Enabling Nix Flakes

Add to your `~/.config/nix/nix.conf`:
```
experimental-features = nix-command flakes
```

## 🏗️ Quick Start

### 1. Clone and Enter Development Environment

```bash
git clone https://github.com/dominicburkart/nanna-coder.git
cd nanna-coder

# Enter Nix development shell
nix develop

# Or use direnv (if configured)
direnv allow
```

### 2. Build the Project

```bash
# Build everything (workspace + containers)
./scripts/build.sh all

# Or build specific components
./scripts/build.sh workspace    # Rust workspace only
./scripts/build.sh containers   # Container images only
```

### 3. Deploy and Run

```bash
# Start local development stack
./scripts/deploy.sh start

# Or specify deployment type
./scripts/deploy.sh start --type pod --gpu nvidia
```

### 4. Interact with the Assistant

```bash
# Chat mode with tools enabled
nix run .#harness -- chat --tools --model llama3.1:8b

# Single query
nix run .#harness -- chat --prompt "Hello, world!" --model llama3.1:8b

# List available models
nix run .#harness -- models

# Health check
nix run .#harness -- health

# List available tools
nix run .#harness -- tools
```

## 🏗️ Architecture

Nanna Coder is built as a modular Rust workspace with containerized deployment using Nix:

### Core Components

- **`harness/`**: CLI application and main entry point
- **`model/`**: Abstraction layer for different AI model providers
- **`image-builder/`**: Container image building and management utilities

### Container Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Podman Pod / Docker Compose              │
├─────────────────────────┬───────────────────────────────────┤
│   Harness Container     │        Ollama Container          │
│                         │                                   │
│   • CLI Interface       │   • Model Inference               │
│   • Tool Registry       │   • Model Management              │
│   • API Endpoints       │   • GPU Acceleration             │
│   • Business Logic      │   • Model Storage                 │
└─────────────────────────┴───────────────────────────────────┘
```

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

### Key Benefits

- **Complete Isolation**: All dependencies managed by Nix
- **Reproducible Builds**: Deterministic builds across all environments
- **Multi-Architecture**: Native support for different CPU architectures
- **GPU Support**: Seamless GPU acceleration while maintaining isolation
- **Service Orchestration**: Clean separation of concerns between components

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

## 🔧 Development

### Development Environment

The Nix development shell provides:

- Rust toolchain (rustc, cargo, clippy, rustfmt)
- Container tools (podman, buildah, skopeo)
- Development utilities (cargo-watch, cargo-audit, cargo-deny)
- Pre-commit hooks for code quality

### Building and Testing

```bash
# Run all tests
cargo test --workspace

# Run with coverage
cargo tarpaulin --workspace

# Lint and format
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all

# Security audit
cargo audit
cargo deny check
```

### Cross-Compilation

```bash
# Build for multiple architectures
./scripts/build.sh cross

# Build for specific architecture
nix build .#packages.aarch64-linux.nanna-coder
nix build .#packages.x86_64-linux.nanna-coder
```

### GPU Development

```bash
# Detect available GPU
nix run .#detect-gpu

# Build with GPU support
./scripts/build.sh gpu --gpu nvidia

# Run with GPU acceleration
./scripts/deploy.sh start --gpu nvidia
```

## 🐳 Container Usage

### Podman Pod Deployment

```bash
# Create and start pod with GPU support
./scripts/deploy.sh start --type pod --gpu nvidia

# View pod status
podman pod ps

# Access container shell
./scripts/deploy.sh shell harness
```

### Docker Compose Deployment

```bash
# Start services
./scripts/deploy.sh start --type compose

# View logs
./scripts/deploy.sh logs --follow

# Update deployment
./scripts/deploy.sh update
```

### Manual Container Usage

```bash
# Load container images
podman load < result

# Run harness container
podman run -it --rm \
  --env OLLAMA_URL=http://host.containers.internal:11434 \
  nanna-coder-harness:latest \
  harness chat --model llama3.1:8b --tools

# Run Ollama container with GPU
podman run -d \
  --name ollama \
  --publish 11434:11434 \
  --volume ollama_data:/root/.ollama \
  --device nvidia.com/gpu=all \
  nanna-coder-ollama:latest
```

## 🌐 Multi-Platform Support

### Supported Architectures

- **x86_64-linux**: Intel/AMD 64-bit Linux
- **aarch64-linux**: ARM64 Linux (including Apple Silicon via VM)
- **x86_64-darwin**: Intel Mac (with container runtime)
- **aarch64-darwin**: Apple Silicon Mac (with container runtime)

### Cross-Platform Building

```bash
# Build for all supported platforms
nix build .#packages.x86_64-linux.harnessImage
nix build .#packages.aarch64-linux.harnessImage

# Multi-architecture container manifests
podman manifest create nanna-coder-harness:latest
podman manifest add nanna-coder-harness:latest nanna-coder-harness-amd64:latest
podman manifest add nanna-coder-harness:latest nanna-coder-harness-arm64:latest
```

## 🎮 GPU Acceleration

### NVIDIA GPU Support

```bash
# Automatic detection and setup
./scripts/deploy.sh start --gpu nvidia

# Manual container run
podman run --runtime nvidia --gpus all \
  nanna-coder-ollama:latest
```

### AMD GPU Support

```bash
# ROCm support
./scripts/deploy.sh start --gpu amd

# Manual container run
podman run --device /dev/dri --device /dev/kfd \
  nanna-coder-ollama:latest
```

### Intel GPU Support

```bash
# Intel integrated graphics
./scripts/deploy.sh start --gpu intel

# Manual container run
podman run --device /dev/dri \
  nanna-coder-ollama:latest
```

## 📚 Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `OLLAMA_URL` | Ollama service URL | `http://localhost:11434` |
| `RUST_LOG` | Logging level | `info` |
| `GPU_SUPPORT` | GPU support type | `auto` |
| `OLLAMA_MODEL` | Default model | `llama3.1:8b` |

## 🚀 Deployment

### Local Development

```bash
# Quick start for development
nix develop
cargo run --bin harness -- chat --tools
```

### Production Deployment

```bash
# Build production containers
./scripts/build.sh all --env production

# Deploy with orchestration
./scripts/deploy.sh start --type compose --env production

# Monitor deployment
./scripts/deploy.sh status
./scripts/deploy.sh logs --follow
```

## 🔍 Troubleshooting

### Common Issues

**Nix not found**
```bash
# Install Nix with flakes
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install
```

**GPU not detected**
```bash
# Check GPU status
nix run .#detect-gpu

# Verify container runtime
podman info --debug
```

**Container build fails**
```bash
# Clean build cache
nix-collect-garbage
./scripts/build.sh clean

# Rebuild from scratch
./scripts/build.sh all
```

**Ollama connection issues**
```bash
# Check Ollama service
curl http://localhost:11434/api/tags

# Restart services
./scripts/deploy.sh restart
```

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch: `git checkout -b feature/amazing-feature`
3. Make your changes and ensure tests pass: `nix develop --command cargo test --workspace`
4. Commit using conventional commits: `git commit -m "feat: add amazing feature"`
5. Push to the branch: `git push origin feature/amazing-feature`
6. Create a Pull Request

### Development Guidelines

- Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- Write comprehensive tests for new functionality
- Update documentation for API changes
- Ensure all CI checks pass
- Use conventional commit messages

## 🙏 Acknowledgments

- [Ollama](https://ollama.ai/) for the excellent local LLM platform
- [Nix](https://nixos.org/) for reproducible builds and dependency management
- [Podman](https://podman.io/) for secure container execution
- The Rust community for excellent tooling and libraries

---

**Built with ❤️ and Rust 🦀**

Contributions are welcome! Please feel free to submit a Pull Request.