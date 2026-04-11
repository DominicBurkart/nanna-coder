# Testing Guide

## Prerequisites

All tests must be run from within the Nix development shell, which provides all required dependencies (nix, jq, curl, cargo, podman, vulnix).

```bash
nix develop
```

## Quick Start

```bash
# Run all test suites
./tests/run-all-tests.sh
```

## Test Structure

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

### Individual suites

```bash
./tests/security/test-dependencies.sh
./tests/security/test-environment.sh
./tests/security/test-tools-availability.sh
./tests/security/test-traditional-security.sh
./tests/security/test-behavioral-security.sh
./tests/security/test-ai-security.sh
./tests/integration/test-provenance.sh
./tests/integration/test-build-system.sh
```

### Legacy monolithic script

```bash
./test-agentic-security.sh
```

This script requires `podman` and `vulnix` to be available (exits 1 if missing).

### CI/CD pipeline

- **Unit tests**: `cargo nextest run --workspace --lib`
- **Integration tests**: `nix run .#container-test`
- **Lint**: `cargo clippy` and `cargo fmt`
- **Security**: `cargo audit`, `cargo deny check`, `cargo tarpaulin`

## Security Tooling

### cargo-deny and cargo-audit

Both tools are retained for complementary coverage:

- **cargo-deny** ([`deny.toml`](deny.toml)): vulnerability checking, license compliance, banned crates, duplicate detection, source validation.
- **cargo-audit**: lightweight focused vulnerability scan with detailed RustSec reports.

```bash
cargo deny check             # all checks
cargo deny check advisories  # vulnerabilities only
cargo deny check licenses    # license compliance only
cargo audit                  # vulnerability scan
cargo audit --json           # JSON output
```

### AI security tools (requires Ollama)

```bash
nix run .#container-dev      # start Ollama
nix run .#security-judge
nix run .#threat-model-analysis
nix run .#dependency-risk-profile
nix run .#adaptive-vulnix-scan
```

### Provenance and supply chain

```bash
nix run .#nix-provenance-validator
vulnix --system
```

## Troubleshooting

### "Not in Nix development shell"
Run `nix develop` before executing tests.

### "podman not available" / "vulnix not available"
Ensure you are in the Nix development shell — both tools are provided automatically:
```bash
nix develop
command -v podman
command -v vulnix
```

### "Ollama not running"
```bash
# Terminal 1
nix run .#container-dev
# Terminal 2
curl http://localhost:11434/api/tags
./tests/security/test-ai-security.sh
```

### "Behavioral security test timed out"
Expected on first run (2-3 minutes). Subsequent runs are faster due to caching.

### Tests pass locally but fail in CI
Check GitHub Actions workflow logs, verify all required tools are present in CI, and review cache status.

## Adding New Tests

1. Create scripts in `tests/security/` or `tests/integration/`
2. Use helpers from `tests/lib/test-helpers.sh`
3. Make executable: `chmod +x tests/path/to/test.sh`
4. Register in `tests/run-all-tests.sh`
5. Update this file

## Test Philosophy

- **Fail Fast**: exit immediately on critical failures (missing dependencies, bad environment)
- **Modular**: single-responsibility scripts
- **Reproducible**: hermetic Nix environment
- **Comprehensive**: traditional tools + AI analysis + supply chain validation

## Additional Resources

- [`.github/workflows/ci.yml`](.github/workflows/ci.yml) - CI configuration
- [`flake.nix`](flake.nix) - Development environment and security tools
- [`deny.toml`](deny.toml) - Security and compliance rules
