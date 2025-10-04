# Testing Guide for Nanna Coder

This document provides comprehensive instructions for running tests locally in the nanna-coder project.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Quick Start](#quick-start)
- [Test Structure](#test-structure)
- [Running Tests](#running-tests)
- [Security Tooling](#security-tooling)
- [Troubleshooting](#troubleshooting)

## Prerequisites

### Required Dependencies

All tests must be run from within the Nix development shell. The following dependencies are required:

- **nix** - Nix package manager (required)
- **jq** - JSON processor (required)
- **curl** - HTTP client (required)
- **cargo** - Rust build tool (required)
- **podman** - Container runtime (required for integration tests)
- **vulnix** - Nix vulnerability scanner (required for security tests)

### Entering the Development Shell

```bash
# Enter the Nix development shell
nix develop

# Verify you're in the shell
echo $IN_NIX_SHELL  # Should output "1" or "pure"
```

## Quick Start

### Run All Tests

From the project root directory (inside the Nix shell):

```bash
# Run all test suites
./tests/run-all-tests.sh
```

### Run Specific Test Suites

```bash
# Dependency checks
./tests/security/test-dependencies.sh

# Environment validation
./tests/security/test-environment.sh

# Security tool availability
./tests/security/test-tools-availability.sh

# Traditional security checks (cargo-deny, cargo-audit)
./tests/security/test-traditional-security.sh

# Behavioral security testing
./tests/security/test-behavioral-security.sh

# AI-powered security analysis (requires Ollama)
./tests/security/test-ai-security.sh

# Provenance validation
./tests/integration/test-provenance.sh

# Build system integration
./tests/integration/test-build-system.sh
```

## Test Structure

The test suite is organized into modular components:

```
tests/
├── lib/
│   └── test-helpers.sh          # Shared test utilities
├── security/
│   ├── test-dependencies.sh     # Dependency verification
│   ├── test-environment.sh      # Environment setup validation
│   ├── test-tools-availability.sh    # Security tools availability
│   ├── test-traditional-security.sh  # cargo-deny, cargo-audit
│   ├── test-behavioral-security.sh   # Behavioral tests
│   └── test-ai-security.sh           # AI-powered security analysis
├── integration/
│   ├── test-provenance.sh       # Supply chain validation
│   └── test-build-system.sh     # Build system checks
└── run-all-tests.sh             # Main test runner
```

## Running Tests

### Legacy Test Script

The original monolithic test script is still available for compatibility:

```bash
./test-agentic-security.sh
```

**Note**: This script has been enhanced with stricter dependency checks. It will now fail (exit 1) if `podman` or `vulnix` are not available, rather than issuing soft warnings.

### Modular Test Execution

For more granular control, use the modular test scripts:

```bash
# Run only dependency checks
cd /path/to/nanna-coder
nix develop
./tests/security/test-dependencies.sh

# Run only traditional security tests
./tests/security/test-traditional-security.sh
```

### CI/CD Pipeline

The CI pipeline runs tests automatically on push and pull requests:

- **Unit tests**: `cargo nextest run --workspace --lib`
- **Integration tests**: `nix run .#container-test`
- **Lint checks**: `cargo clippy` and `cargo fmt`
- **Security checks**: `cargo audit`, `cargo deny check`, `cargo tarpaulin`

## Security Tooling

### cargo-deny vs cargo-audit

The project uses **both** `cargo-deny` and `cargo-audit` for comprehensive security coverage:

#### cargo-deny

**Purpose**: Multi-faceted supply chain security and compliance

**Features**:
- ✅ Vulnerability checking (via RustSec Advisory Database)
- ✅ License compliance validation
- ✅ Dependency graph analysis
- ✅ Ban specific crates or versions
- ✅ Check for duplicate dependencies
- ✅ Source validation

**Configuration**: `deny.toml` in project root

**Usage**:
```bash
cargo deny check           # Run all checks
cargo deny check advisories  # Only vulnerability checks
cargo deny check licenses    # Only license compliance
cargo deny check bans        # Only banned dependencies
```

#### cargo-audit

**Purpose**: Focused vulnerability scanning

**Features**:
- ✅ Vulnerability checking (via RustSec Advisory Database)
- ✅ Lightweight and fast
- ✅ Detailed vulnerability reports
- ✅ Integration with `Cargo.lock`

**Usage**:
```bash
cargo audit               # Check for vulnerabilities
cargo audit --json        # JSON output
```

#### Why Use Both?

**Recommendation**: **Keep both tools**

1. **cargo-deny** provides comprehensive supply chain security including license compliance, which is critical for enterprise deployments and open-source compliance
2. **cargo-audit** is lighter weight and provides detailed vulnerability-specific reporting
3. They complement each other:
   - Use `cargo-deny` for pre-commit hooks and full compliance checks
   - Use `cargo-audit` for quick vulnerability scans during development
   - Both share the same vulnerability database (RustSec) but present information differently

**Redundancy Analysis**:
- Vulnerability checking: Both use RustSec (redundant but provides validation)
- License checking: **Only cargo-deny**
- Ban checking: **Only cargo-deny**
- Duplicate detection: **Only cargo-deny**

**Conclusion**: While there is some redundancy in vulnerability checking, `cargo-deny` provides essential features that `cargo-audit` does not. Both tools should be retained.

### AI Security Tools

When Ollama is running, additional AI-powered security analysis is available:

```bash
# Start Ollama service
nix run .#container-dev

# Wait for service to be ready
curl http://localhost:11434/api/tags

# Run AI security analysis
nix run .#security-judge
nix run .#threat-model-analysis
nix run .#dependency-risk-profile
nix run .#adaptive-vulnix-scan
```

### Provenance and Supply Chain

```bash
# Validate Nix package provenance
nix run .#nix-provenance-validator

# Check for Nix-specific vulnerabilities
vulnix --system
```

## Troubleshooting

### Common Issues

#### "Not in Nix development shell"

**Solution**: Run `nix develop` before executing tests

```bash
nix develop
./tests/run-all-tests.sh
```

#### "podman not available"

The test suite now requires podman for container-based tests.

**Solution**: Ensure you're in the Nix development shell (podman is provided automatically):

```bash
nix develop
command -v podman  # Should output: /nix/store/.../bin/podman
```

#### "vulnix not available"

The test suite now requires vulnix for Nix vulnerability scanning.

**Solution**: Ensure you're in the Nix development shell:

```bash
nix develop
command -v vulnix  # Should output: /nix/store/.../bin/vulnix
```

#### "Ollama not running"

AI-powered tests require Ollama to be running.

**Solution**:

```bash
# Terminal 1: Start Ollama
nix run .#container-dev

# Terminal 2: Wait for readiness, then run tests
curl http://localhost:11434/api/tags
./tests/security/test-ai-security.sh
```

#### "Behavioral security test timed out"

Behavioral tests can take 2-3 minutes.

**Solution**: This is expected for the first run. Subsequent runs should be faster due to caching.

### Test Failures

If tests fail:

1. Check that all prerequisites are installed (run dependency check)
2. Ensure you're in the project root directory
3. Verify you're in the Nix development shell
4. Check network connectivity (some tests download data)
5. Review the specific test output for detailed error messages

### CI/CD Debugging

If tests pass locally but fail in CI:

1. Check the GitHub Actions workflow logs
2. Verify the CI environment has all required tools
3. Check for platform-specific issues (Linux vs macOS vs Windows)
4. Review cache status (cache misses can cause timeouts)

## Additional Resources

- [CI/CD Pipeline](.github/workflows/ci.yml) - Full CI configuration
- [Nix Flake](flake.nix) - Development environment and security tools
- [cargo-deny Configuration](deny.toml) - Security and compliance rules
- [AGENTIC-SECURITY.md](AGENTIC-SECURITY.md) - AI-powered security architecture (if available)

## Contributing

When adding new tests:

1. Create test scripts in the appropriate directory (`tests/security/`, `tests/integration/`, etc.)
2. Use the shared test helpers from `tests/lib/test-helpers.sh`
3. Make scripts executable: `chmod +x tests/path/to/test.sh`
4. Add your test to `tests/run-all-tests.sh`
5. Update this TESTING.md document
6. Ensure tests pass locally before submitting a PR

## Test Philosophy

This project follows these testing principles:

1. **Fail Fast**: Tests exit immediately on critical failures (dependencies, environment)
2. **Modular**: Tests are split into focused, single-responsibility scripts
3. **Reproducible**: All tests run in a hermetic Nix environment
4. **Comprehensive**: Security tests cover traditional tools, AI analysis, and supply chain validation
5. **CI/CD Ready**: All tests are designed to run in GitHub Actions

---

For questions or issues with testing, please open a GitHub issue or consult the CI/CD logs.
