# Testing Guide

## Prerequisites

All tests run inside the Nix development shell:

```bash
nix develop
```

Required: `nix`, `jq`, `curl`, `cargo`, `podman`, `vulnix` (all provided by the shell).

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
│   ├── test-dependencies.sh
│   ├── test-environment.sh
│   ├── test-tools-availability.sh
│   ├── test-traditional-security.sh
│   ├── test-behavioral-security.sh
│   └── test-ai-security.sh
├── integration/
│   ├── test-provenance.sh
└──   test-build-system.sh
└── run-all-tests.sh
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

### Legacy script

`./test-agentic-security.sh` is still available for compatibility. It now exits with code 1 (rather than soft-warning) if `podman` or `vulnix` are missing.

### CI/CD Pipeline

- **Unit tests**: `cargo nextest run --workspace --lib`
- **Integration tests**: `nix run .#container-test`
- **Lint**: `cargo clippy`, `cargo fmt`
- **Security**: `cargo audit`, `cargo deny check`, `cargo tarpaulin`

## Security Tooling

### cargo-deny and cargo-audit

Both tools are used together:

| Feature | cargo-deny | cargo-audit |
|---|---|---|
| Vulnerability scanning (RustSec) | ✓ | ✓ |
| License compliance | ✓ | ✗ |
| Dependency bans / duplicates | ✓ | ✗ |
| Lightweight quick scan | ✗ | ✓ |

**Usage**:
```bash
cargo deny check           # full supply-chain check
cargo deny check advisories
cargo audit                # quick vulnerability scan
cargo audit --json
```

Configuration: [`deny.toml`](deny.toml)

### AI Security Tools (requires Ollama)

```bash
nix run .#container-dev        # start Ollama
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

**Not in Nix shell** — run `nix develop` first.

**`podman` or `vulnix` not found** — enter the Nix shell; both are provided automatically.

**Ollama not running** — start it with `nix run .#container-dev`, then retry.

**Behavioral test timed out** — expected on first run; subsequent runs are faster due to caching.

**Tests pass locally but fail in CI** — check the [Actions logs](.github/workflows/ci.yml), verify all tools are available in the CI environment, and review cache status.

## Additional Resources

- [`deny.toml`](deny.toml) - cargo-deny configuration
- [`flake.nix`](flake.nix) - development environment and security tools
- [`.github/workflows/ci.yml`](.github/workflows/ci.yml) - full CI configuration

## Contributing

When adding new tests:
1. Place scripts in `tests/security/` or `tests/integration/`
2. Use helpers from `tests/lib/test-helpers.sh`
3. Make scripts executable and add them to `tests/run-all-tests.sh`
4. Update this document
