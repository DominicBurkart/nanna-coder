# Developer Experience Guide

## Overview

This guide covers the optimized developer experience for the Nanna Coder project, including development utilities, workflows, and best practices.

## Quick Start

```bash
# Enter development environment
nix develop

# Quick development check
dev-check

# Start development containers
container-dev

# Run tests in watch mode
dev-test watch
```

## Development Environment

### Automatic Setup

When entering the development shell with `nix develop`, the following is automatically configured:

#### Git Hooks
- **Pre-commit hooks** with comprehensive checks:
  - Code formatting (cargo fmt)
  - Linting (cargo clippy)
  - Tests (cargo nextest)
  - Security audit (cargo audit)
  - License compliance (cargo deny)
  - Coverage validation (cargo tarpaulin)

#### Development Aliases
- **File navigation**: `ll`, `la`, `l`, `..`, `...`
- **Cargo shortcuts**: `cb`, `ct`, `cc`, `cf`, `cn`
- **Git shortcuts**: `gs`, `ga`, `gc`, `gp`, `gl`, `gd`
- **Nix shortcuts**: `nb`, `nr`, `nd`, `nf`
- **Project shortcuts**: `dt`, `db`, `dc`

### Development Tools

#### Core Tools (Auto-installed)
- **cargo-watch**: Incremental compilation with file watching
- **cargo-nextest**: Better test runner with parallel execution
- **cargo-audit**: Security vulnerability scanning
- **cargo-deny**: License and dependency compliance
- **cargo-tarpaulin**: Code coverage analysis
- **cargo-expand**: Macro expansion debugging
- **cargo-udeps**: Unused dependency detection
- **cargo-machete**: Remove unused dependencies
- **cargo-outdated**: Check for outdated dependencies

#### Container Tools
- **Podman**: Rootless container runtime (preferred)
- **Buildah**: Container image building
- **Skopeo**: Container image management

## Development Utilities

### Core Development Commands

#### `dev-check`
Quick syntax and format validation.
```bash
dev-check
```
**Features:**
- Fast formatting check
- Clippy linting
- Compilation verification
- Early error detection

#### `dev-build`
Fast incremental development build.
```bash
dev-build
```
**Features:**
- Uses cargo-watch for file monitoring
- Incremental compilation
- Real-time feedback

#### `dev-test`
Comprehensive test runner with multiple modes.
```bash
# Run all tests with full validation
dev-test

# Run only unit tests
dev-test unit

# Run only integration tests
dev-test integration

# Run tests in watch mode
dev-test watch
```
**Features:**
- Multiple test types
- Watch mode for continuous testing
- Comprehensive validation (clippy, format, audit, deny)

#### `dev-clean`
Clean development artifacts and containers.
```bash
dev-clean
```
**Features:**
- Cleans Cargo artifacts
- Removes target directory
- Prunes container images (24h old)
- Optional Nix store cleanup

#### `dev-reset`
Complete development environment reset.
```bash
dev-reset
```
**Features:**
- Full cleanup
- Updates flake inputs
- Rebuilds development shell
- Warms common builds

### Container Development Commands

#### `container-dev`
Start development containers.
```bash
container-dev
```
**Features:**
- Docker/Podman compose support
- Automatic service orchestration
- Health checks

#### `container-test`
Run containerized integration tests.
```bash
container-test
```
**Features:**
- Loads test containers
- Runs integration tests
- Automatic cleanup

#### `container-stop`
Stop all development containers.
```bash
container-stop
```

#### `container-logs`
View container logs.
```bash
container-logs
```

### Cache Management Commands

#### `cache-warm`
Pre-warm frequently used builds.
```bash
cache-warm
```
**Features:**
- Parallel build execution
- Core package caching
- Container image preparation
- Performance analytics

## Development Workflows

### Daily Development Workflow

1. **Start Development Session**
   ```bash
   nix develop
   dev-check  # Quick health check
   ```

2. **Start Development Services**
   ```bash
   container-dev  # Start containers if needed
   ```

3. **Development Loop**
   ```bash
   # Make changes...
   dev-check      # Quick validation
   dev-test unit  # Run relevant tests
   ```

4. **Pre-commit Preparation**
   ```bash
   dev-test       # Full validation
   git add .
   git commit     # Pre-commit hooks run automatically
   ```

### Testing Workflow

#### Unit Testing
```bash
# Quick unit tests
dev-test unit

# Watch mode for TDD
dev-test watch
```

#### Integration Testing
```bash
# Full integration tests
dev-test integration

# Containerized integration tests
container-test
```

#### Performance Testing
```bash
# Code coverage
cargo tarpaulin --skip-clean --ignore-tests

# Benchmark tests
cargo bench
```

### Debugging Workflow

#### Macro Expansion
```bash
# Expand macros for debugging
cargo expand

# Expand specific module
cargo expand module_name
```

#### Dependency Analysis
```bash
# Check for unused dependencies
cargo udeps

# Remove unused dependencies
cargo machete

# Check for outdated dependencies
cargo outdated
```

### Container Development Workflow

#### Local Container Testing
```bash
# Build and test locally
nix build .#qwen3-container
container-test

# Check container logs
container-logs
```

#### Multi-Model Testing
```bash
# Test with different models
nix build .#llama3-container
nix build .#mistral-container
# Test each configuration
```

## Performance Optimization

### Build Performance

#### Cache Utilization
```bash
# Check cache status
cache-analytics

# Warm cache for faster builds
cache-warm

# Setup binary cache
setup-cache
```

#### Incremental Builds
- Use `dev-build` for file watching
- Leverage cargo incremental compilation
- Pre-warm dependencies with `cache-warm`

### Test Performance

#### Parallel Testing
```bash
# Use nextest for parallel execution
cargo nextest run --workspace

# Set test thread count
cargo nextest run --test-threads=4
```

#### Selective Testing
```bash
# Run specific test patterns
cargo nextest run test_pattern

# Run tests for specific package
cargo nextest run -p harness
```

## Troubleshooting

### Common Issues

#### Build Failures
1. **Check formatting**: `cargo fmt --all`
2. **Fix clippy warnings**: `cargo clippy --workspace --fix`
3. **Clean and rebuild**: `dev-clean && dev-build`

#### Container Issues
1. **Check runtime**: `podman --version` or `docker --version`
2. **Restart services**: `container-stop && container-dev`
3. **Check logs**: `container-logs`

#### Cache Issues
1. **Check cache status**: `cache-analytics`
2. **Reconfigure cache**: `setup-cache`
3. **Clear and rebuild**: `dev-reset`

### Debug Commands

#### Environment Debugging
```bash
# Check development environment
echo $RUST_TOOLCHAIN_PATH
echo $NIX_PATH

# Verify tools
which cargo-watch
which cargo-nextest
```

#### Container Debugging
```bash
# List containers
podman ps -a

# Inspect container
podman inspect container-name

# Execute commands in container
podman exec -it container-name bash
```

## IDE Integration

### VS Code Setup

#### Recommended Extensions
- **rust-analyzer**: Rust language server
- **CodeLLDB**: Debugging support
- **Better TOML**: Cargo.toml syntax highlighting
- **Nix IDE**: Nix language support

#### Settings Configuration
```json
{
  "rust-analyzer.server.path": "rust-analyzer",
  "rust-analyzer.cargo.features": "all",
  "rust-analyzer.checkOnSave.command": "clippy",
  "rust-analyzer.cargo.buildScripts.enable": true
}
```

### Neovim Setup

#### Required Plugins
- **nvim-lspconfig**: LSP configuration
- **rust-tools.nvim**: Enhanced Rust support
- **nvim-cmp**: Completion engine
- **telescope.nvim**: Fuzzy finder

## Best Practices

### Code Quality
1. **Always run `dev-check` before committing**
2. **Use `dev-test watch` for TDD workflow**
3. **Keep dependencies up to date with `cargo outdated`**
4. **Run security audits with `cargo audit`**

### Performance
1. **Use `cache-warm` for faster builds**
2. **Leverage incremental compilation with `dev-build`**
3. **Use `cargo nextest` for faster test execution**
4. **Profile with `cargo bench` for performance-critical code**

### Container Development
1. **Use pre-built test containers when possible**
2. **Clean up containers regularly with `dev-clean`**
3. **Monitor resource usage with `container-logs`**
4. **Test with multiple model configurations**

### Git Workflow
1. **Let pre-commit hooks enforce quality**
2. **Use meaningful commit messages**
3. **Test thoroughly before pushing**
4. **Leverage branch protection with required checks**

---

For additional help or feature requests, consult the project documentation or create an issue in the repository.