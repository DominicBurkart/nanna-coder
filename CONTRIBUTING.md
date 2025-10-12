# Contributing to Nanna Coder

Thank you for your interest in contributing to Nanna Coder! This document provides guidelines and information for contributors.

## Development Environment Setup

### Prerequisites

- **Nix with flakes** - For reproducible development environment
- **Git** - Version control
- **Cargo/Rust** - Will be provided by Nix

### Getting Started

1. Clone the repository:
   ```bash
   git clone https://github.com/DominicBurkart/nanna-coder.git
   cd nanna-coder
   ```

2. Enter the Nix development shell:
   ```bash
   nix develop
   ```

3. Install pre-commit hooks (automatic on first test run):
   ```bash
   cargo test --no-run
   ```

## Pre-Commit Hooks

This project uses **cargo-husky** to manage git hooks automatically. When you run `cargo test` for the first time, comprehensive pre-commit hooks will be installed.

### What Gets Checked

The pre-commit hooks run the following checks automatically:

#### Rust Checks
- ✅ `cargo fmt --check` - Code formatting
- ✅ `cargo clippy -- -D warnings` - Linting and best practices
- ✅ `cargo test --all-features` - All tests pass
- ✅ `cargo audit` - Security vulnerabilities
- ✅ `cargo deny check` - License compliance and dependency validation
- ✅ `cargo doc --no-deps` - Documentation builds successfully

#### Shell Script Checks
- ✅ `shellcheck` - Shell script linting
- ✅ `shfmt` - Shell script formatting (2-space indent)

#### Nix File Checks
- ✅ `nixfmt --check` - Nix file formatting (RFC 166 style)

#### YAML File Checks
- ✅ `yamllint` - YAML file linting
- ✅ `actionlint` - GitHub Actions workflow validation

#### Markdown Checks
- ✅ `markdownlint` - Markdown linting (when available)

#### TOML File Checks
- ✅ `taplo fmt --check` - TOML file formatting

#### Security Checks
- ✅ Claude security review (optional, if CLI available)
- ✅ Code coverage comparison with main branch

### Configuration Files

The following configuration files control linter behavior:

- `.shellcheckrc` - ShellCheck configuration
- `.yamllint.yml` - YAML linting rules
- `.markdownlint.yaml` - Markdown linting rules
- `taplo.toml` - TOML formatting configuration
- `.cargo-husky/hooks/pre-commit` - Pre-commit hook script

### Bypassing Hooks

**⚠️ Not Recommended**: In rare cases where you need to bypass hooks:

```bash
git commit --no-verify -m "your message"
```

**Please only use this for**:
- Emergency hotfixes
- Work-in-progress commits on feature branches
- When explicitly requested by maintainers

**Never use `--no-verify` for commits to `main` or `develop` branches.**

## Running Checks Manually

You can run any check manually before committing:

### Rust Checks
```bash
# Format code
cargo fmt

# Run linter
cargo clippy --workspace --all-targets -- -D warnings

# Run tests
cargo test --workspace --all-features

# Security audit
cargo audit

# License/dependency check
cargo deny check

# Build docs
cargo doc --no-deps --workspace
```

### Non-Rust File Checks
```bash
# Shell scripts
shellcheck scripts/**/*.sh
shfmt -i 2 -ci -d scripts/**/*.sh

# Nix files
nixfmt --check **/*.nix

# YAML files
yamllint .
actionlint .github/workflows/*.yml

# Markdown files
markdownlint .

# TOML files
taplo fmt --check
```

### Run All Checks
```bash
# This will run all pre-commit checks
.cargo-husky/hooks/pre-commit
```

## Continuous Integration

All pre-commit checks are also enforced in CI via GitHub Actions. The CI pipeline includes:

- **Test Matrix**: Unit, integration, lint, and security tests across multiple OS and Rust versions
- **Build Matrix**: Multi-platform builds (x86_64/aarch64 for Linux/macOS)
- **Container Builds**: Docker/Podman container images
- **Lint Jobs**: Shell, Nix, YAML, Markdown, and TOML linting
- **Security Scans**: Trivy container scanning, cargo-audit, cargo-deny

See `.github/workflows/ci.yml` for the complete CI configuration.

## Code Style

### Rust
- Follow standard Rust formatting (`rustfmt`)
- Address all `clippy` warnings
- Write comprehensive tests for new functionality
- Document public APIs with doc comments
- Use `thiserror` for custom error types
- Prefer `anyhow` for application-level error handling

### Shell Scripts
- Use `#!/usr/bin/env bash` shebang
- 2-space indentation
- Quote all variables
- Use `set -e` for error handling
- Add comments for complex logic

### Nix
- Use RFC 166 style (nixfmt-rfc-style)
- Keep expressions modular
- Document non-obvious functionality

### Commit Messages
Follow semantic commit format:
```
type(scope): brief description

Longer explanation if needed
```

**Types**: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `ci`

**Examples**:
- `feat(agent): add support for GPT-4 model`
- `fix(tests): handle timeout in integration tests`
- `docs(readme): update installation instructions`

## Testing

### Running Tests
```bash
# All tests
cargo test --workspace --all-features

# Specific package
cargo test --package harness

# With coverage
cargo tarpaulin --skip-clean --ignore-tests

# Integration tests
cargo test --test integration_test
```

### Writing Tests
- Place unit tests in the same file as the code being tested
- Use `tests/` directory for integration tests
- Use `#[should_panic]` for expected error cases
- Consider property-based testing with `proptest` for complex logic

## Pull Request Process

1. **Create a feature branch**:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make your changes**:
   - Write code
   - Add tests
   - Update documentation
   - Ensure all checks pass locally

3. **Commit your changes**:
   ```bash
   git add .
   git commit -m "feat(scope): description"
   ```
   (Pre-commit hooks will run automatically)

4. **Push to your fork**:
   ```bash
   git push origin feature/your-feature-name
   ```

5. **Create a Pull Request**:
   - Provide clear description of changes
   - Reference any related issues
   - Ensure CI passes
   - Request review from maintainers

## Getting Help

- **Issues**: [GitHub Issues](https://github.com/DominicBurkart/nanna-coder/issues)
- **Discussions**: [GitHub Discussions](https://github.com/DominicBurkart/nanna-coder/discussions)
- **Documentation**: See `docs/` directory

## License

By contributing to Nanna Coder, you agree that your contributions will be licensed under the MIT License.
