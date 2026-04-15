# Testing Guide

## Contributing

When adding new tests:

1. Create test scripts in the appropriate directory (`tests/security/`, `tests/integration/`, etc.)
2. Use shared helpers from `tests/lib/test-helpers.sh`
3. Make scripts executable: `chmod +x tests/path/to/test.sh`
4. Add your test to `tests/run-all-tests.sh`
5. Update this document
6. Ensure tests pass locally before submitting a PR

## Test Philosophy

1. **Fail Fast**: Exit immediately on critical failures (missing dependencies, bad environment)
2. **Modular**: Focused, single-responsibility scripts
3. **Reproducible**: Hermetic Nix environment
4. **Comprehensive**: Security tests cover traditional tools, AI analysis, and supply chain
5. **CI/CD Ready**: All tests run in GitHub Actions

## Prerequisites

All tests must run inside the Nix development shell:

```bash
nix develop
echo $IN_NIX_SHELL  # Should output "1" or "pure"
```

Required tools (all provided by `nix develop`):

- **nix**, **jq**, **curl**, **cargo**, **podman**, **vulnix**

## Quick Start

```bash
# Run all test suites
./tests/run-all-tests.sh

# Run individual suites
./tests/security/test-dependencies.sh
./tests/security/test-environment.sh
./tests/security/test-tools-availability.sh
./tests/security/test-traditional-security.sh
./tests/security/test-behavioral-security.sh
./tests/security/test-ai-security.sh
./tests/integration/test-provenance.sh
./tests/integration/test-build-system.sh
```

## Test Structure

```
tests/
├── lib/
│   └── test-helpers.sh          # Shared test utilities
├── security/
│   ├── test-dependencies.sh     # Dependency verification
│   ├── test-environment.sh      # Environment setup validation
│   ├── test-tools-availability.sh
│   ├── test-traditional-security.sh  # cargo-deny, cargo-audit
│   ├── test-behavioral-security.sh
│   └── test-ai-security.sh
├── integration/
│   ├── test-provenance.sh       # Supply chain validation
│   └── test-build-system.sh
└── run-all-tests.sh
```

## CI/CD Pipeline

- **Unit tests**: `cargo nextest run --workspace --lib`
- **Integration tests**: `nix run .#container-test`
- **Lint**: `cargo clippy`, `cargo fmt`
- **Security**: `cargo audit`, `cargo deny check`, `cargo tarpaulin`

## Security Tooling

### cargo-deny

Configuration: `deny.toml`

```bash
cargo deny check             # All checks
cargo deny check advisories  # Vulnerability checks only
cargo deny check licenses    # License compliance only
cargo deny check bans        # Banned dependencies only
```

### cargo-audit

```bash
cargo audit         # Check for vulnerabilities
cargo audit --json  # JSON output
```

### AI Security Tools (requires Ollama)

```bash
# Terminal 1: start Ollama
nix run .#container-dev

# Terminal 2: run AI security analysis
curl http://localhost:11434/api/tags  # wait for readiness
nix run .#security-judge
nix run .#threat-model-analysis
nix run .#dependency-risk-profile
nix run .#adaptive-vulnix-scan
```

### Provenance and Supply Chain

```bash
nix run .#nix-provenance-validator
vulnix --system
```

## Troubleshooting

All common issues (missing `podman`, `vulnix`, Ollama not running, behavioral test timeouts) are solved by ensuring you are inside the Nix development shell:

```bash
nix develop
```

The shell provides `podman`, `vulnix`, and all other required tools. Behavioral security tests can take 2-3 minutes on the first run; subsequent runs are faster due to caching.

If tests pass locally but fail in CI:

1. Check GitHub Actions workflow logs
2. Verify the CI environment has all required tools
3. Check for platform-specific issues
4. Review cache status (cache misses can cause timeouts)

## Additional Resources

- [CI/CD Pipeline](.github/workflows/ci.yml)
- [Nix Flake](flake.nix)
- [cargo-deny Configuration](deny.toml)
