# Testing

All tests run inside the Nix dev shell (`nix develop`).

## Run Tests

```bash
# All tests
./tests/run-all-tests.sh

# Specific suites
./tests/security/test-dependencies.sh
./tests/security/test-environment.sh
./tests/security/test-tools-availability.sh
./tests/security/test-traditional-security.sh
./tests/security/test-behavioral-security.sh
./tests/security/test-ai-security.sh        # requires Ollama
./tests/integration/test-provenance.sh
./tests/integration/test-build-system.sh
```

## CI Commands

- Unit tests: `cargo nextest run --workspace --lib`
- Integration tests: `nix run .#container-test`
- Lint: `cargo clippy` and `cargo fmt`
- Security: `cargo audit`, `cargo deny check`, `cargo tarpaulin`

## Test Structure

```
tests/
├── lib/test-helpers.sh
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

## Security Tools

**cargo-deny** (`deny.toml`):
```bash
cargo deny check             # all checks
cargo deny check advisories  # vulnerabilities only
cargo deny check licenses    # license compliance
cargo deny check bans        # banned dependencies
```

**cargo-audit**:
```bash
cargo audit        # check for vulnerabilities
cargo audit --json # JSON output
```

**AI security tools** (requires Ollama running via `nix run .#container-dev`):
```bash
nix run .#security-judge
nix run .#threat-model-analysis
nix run .#dependency-risk-profile
nix run .#adaptive-vulnix-scan
```

**Provenance / supply chain**:
```bash
nix run .#nix-provenance-validator
vulnix --system
```

## Adding Tests

1. Create test script in the appropriate directory.
2. Use helpers from `tests/lib/test-helpers.sh`.
3. Make it executable: `chmod +x tests/path/to/test.sh`
4. Register it in `tests/run-all-tests.sh`.

## Troubleshooting

| Symptom | Solution |
|---------|----------|
| "Not in Nix development shell" | Run `nix develop` first |
| `podman` not found | Run `nix develop` (podman is provided) |
| `vulnix` not found | Run `nix develop` (vulnix is provided) |
| Ollama not running | `nix run .#container-dev` in a separate terminal, then wait for `curl http://localhost:11434/api/tags` to succeed |
| Behavioral test timed out | Expected on first run; subsequent runs are faster due to caching |

If tests fail: verify prerequisites, confirm you are in the project root inside the Nix shell, and review the specific test output.

## References

- [CI/CD Pipeline](.github/workflows/ci.yml)
- [Nix Flake](flake.nix)
- [cargo-deny config](deny.toml)
