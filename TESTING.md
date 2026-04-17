# Testing

## Prerequisites

All tests must be run from within the Nix development shell:

```bash
nix develop
```

Required dependencies (all provided by the Nix shell):

- **nix** - Nix package manager
- **jq** - JSON processor
- **curl** - HTTP client
- **cargo** - Rust build tool
- **podman** - Container runtime (integration tests)
- **vulnix** - Nix vulnerability scanner (security tests)

## Running Tests

```bash
# All test suites
./tests/run-all-tests.sh

# Individual suites
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
│   └── test-helpers.sh
├── security/
│   ├── test-dependencies.sh
│   ├── test-environment.sh
│   ├── test-tools-availability.sh
│   ├── test-traditional-security.sh
│   ├── test-behavioral-security.sh
│   └── test-ai-security.sh
├── integration/
│   ├── test-provenance.sh
│   └── test-build-system.sh
└── run-all-tests.sh
```

## Test Philosophy

1. **Fail Fast** - Exit immediately on critical failures (dependencies, environment)
2. **Modular** - Focused, single-responsibility scripts
3. **Reproducible** - Hermetic Nix environment
4. **Comprehensive** - Security tests cover traditional tools, AI analysis, and supply chain
5. **CI/CD Ready** - Designed to run in GitHub Actions

## Contributing Tests

1. Create scripts in the appropriate directory (`tests/security/`, `tests/integration/`, etc.)
2. Use shared helpers from `tests/lib/test-helpers.sh`
3. Make scripts executable: `chmod +x tests/path/to/test.sh`
4. Add your test to `tests/run-all-tests.sh`
5. Update this file
6. Ensure tests pass locally before submitting a PR

## CI/CD Pipeline

- **Unit tests**: `cargo nextest run --workspace --lib`
- **Integration tests**: `nix run .#container-test`
- **Lint**: `cargo clippy` and `cargo fmt`
- **Security**: `cargo audit`, `cargo deny check`, `cargo tarpaulin`

## Security Tooling

### cargo-deny

Configuration: `deny.toml`

```bash
cargo deny check             # All checks
cargo deny check advisories  # Vulnerabilities only
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
nix run .#container-dev  # Start Ollama service
curl http://localhost:11434/api/tags  # Verify readiness
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

**Not in Nix shell** — Run `nix develop` first.

**`podman` or `vulnix` not available** — Ensure you're in the Nix shell (`nix develop`); both are provided automatically.

**Ollama not running** — Start it with `nix run .#container-dev`, then wait for `curl http://localhost:11434/api/tags` to respond.

**Behavioral test timed out** — Expected on the first run; subsequent runs are faster due to caching.

**Tests fail locally but pass in CI (or vice versa)** — Check GitHub Actions logs, verify all required tools are present, and review cache status.

## Additional Resources

- [CI/CD Pipeline](.github/workflows/ci.yml)
- [Nix Flake](flake.nix)
- [cargo-deny Configuration](deny.toml)
