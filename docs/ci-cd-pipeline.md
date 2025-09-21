# CI/CD Pipeline Documentation

## Overview

The Nanna Coder project uses an advanced, parallel CI/CD pipeline that provides comprehensive testing, building, and deployment across multiple platforms and architectures.

## Pipeline Architecture

### Parallel Execution Strategy

The pipeline is designed for maximum parallelization and efficiency:

```
┌─────────────────────────────────────────────────────────────┐
│                    CI/CD Pipeline Matrix                    │
├─────────────────────────────────────────────────────────────┤
│ Test Matrix (Parallel)                                     │
│ ├── Unit Tests (Linux, macOS, Windows × stable, beta)      │
│ ├── Integration Tests (Linux × stable, beta)               │
│ ├── Lint Checks (Linux, macOS, Windows × stable, beta)     │
│ └── Security Checks (Linux × stable, beta)                 │
├─────────────────────────────────────────────────────────────┤
│ Build Matrix (Parallel)                                    │
│ ├── x86_64-linux (Nix)                                     │
│ ├── aarch64-linux (Nix cross-compilation)                  │
│ ├── x86_64-darwin (Cargo)                                  │
│ └── aarch64-darwin (Cargo cross-compilation)               │
├─────────────────────────────────────────────────────────────┤
│ Container Matrix (Parallel)                                │
│ ├── Harness (x86_64, aarch64)                              │
│ ├── Ollama (x86_64, aarch64)                               │
│ ├── Qwen3 Container (x86_64)                               │
│ └── Llama3 Container (x86_64)                              │
├─────────────────────────────────────────────────────────────┤
│ Performance & Maintenance                                  │
│ ├── Benchmarks (main branch)                               │
│ ├── Cache Maintenance                                      │
│ └── CI Summary & Status Aggregation                        │
└─────────────────────────────────────────────────────────────┘
```

## Pipeline Jobs

### 1. Test Matrix (`test-matrix`)

**Purpose**: Comprehensive testing across platforms and Rust versions
**Strategy**: Parallel execution with fail-fast disabled
**Matrix Dimensions**:
- **Operating Systems**: Ubuntu, macOS, Windows
- **Rust Versions**: stable, beta, nightly (limited)
- **Test Types**: unit, integration, lint, security

#### Test Types

##### Unit Tests
- **Scope**: Library and binary unit tests
- **Command**: `cargo nextest run --workspace --lib`
- **Platforms**: All (Linux, macOS, Windows)
- **Rust Versions**: stable, beta, nightly

##### Integration Tests
- **Scope**: Containerized integration testing
- **Command**: `nix run .#container-test`
- **Platforms**: Linux only (container support required)
- **Features**:
  - Pre-built test containers
  - Model integration testing
  - Health checks

##### Lint Checks
- **Scope**: Code quality and formatting
- **Commands**:
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
- **Platforms**: All
- **Standards**: Zero warnings policy

##### Security Checks
- **Scope**: Security audit and compliance
- **Commands**:
  - `cargo audit` (vulnerability scanning)
  - `cargo deny check` (license compliance)
  - `cargo tarpaulin` (code coverage)
- **Platforms**: Linux only
- **Coverage**: Uploaded to Codecov with Rust version flags

#### Platform-Specific Configurations

**Linux (Ubuntu)**:
- Uses Nix for reproducible builds
- Full Cachix binary cache integration
- Container support for integration tests
- All test types supported

**macOS**:
- Uses direct Rust toolchain installation
- Cargo-based tool installation
- No container support (integration tests skipped)
- Unit, lint, and partial security tests

**Windows**:
- Uses direct Rust toolchain installation
- Limited to unit and lint tests
- Security tests skipped (tooling limitations)

### 2. Build Matrix (`build-matrix`)

**Purpose**: Multi-platform binary compilation
**Strategy**: Cross-platform with fallback support
**Targets**: x86_64-linux, aarch64-linux, x86_64-darwin, aarch64-darwin

#### Build Strategies

**Linux Targets** (Ubuntu runner):
- **x86_64-linux**: Native Nix build (`nix build .#nanna-coder`)
- **aarch64-linux**: Nix cross-compilation with fallback

**macOS Targets** (macOS runner):
- **x86_64-darwin**: Native Cargo build (`cargo build --release`)
- **aarch64-darwin**: Cargo cross-compilation (`--target aarch64-apple-darwin`)

#### Artifact Management
- Platform-specific artifact preparation
- Automatic fallback for failed cross-compilation
- Structured artifact naming (`nanna-coder-{target}`)
- Upload with warnings for missing files

### 3. Container Matrix (`build-containers`)

**Purpose**: Multi-architecture container image builds
**Strategy**: Parallel container builds with registry push
**Images**: harness, ollama, qwen3-container, llama3-container
**Architectures**: x86_64, aarch64 (model containers x86_64 only)

#### Container Types

**Base Containers**:
- **Harness**: Application runtime container
- **Ollama**: Model inference server

**Model Containers**:
- **Qwen3**: Pre-loaded with Qwen3 0.6B model
- **Llama3**: Pre-loaded with Llama3 model
- **Optimized**: x86_64 only (ARM64 planned)

#### Registry Management
- **Registry**: GitHub Container Registry (ghcr.io)
- **Tagging Strategy**:
  - Latest: `latest{-arm64}`
  - Commit: `{sha}{-arm64}`
- **Push Policy**: Skip on pull requests
- **Authentication**: GitHub token

### 4. Performance Jobs

#### Benchmarks (`benchmark`)
- **Trigger**: Main branch pushes only
- **Framework**: Cargo bench with Criterion
- **Storage**: GitHub Pages with benchmark-action
- **Alerting**: 200% performance regression threshold
- **Features**:
  - Historical trend analysis
  - Automatic PR comments on regression
  - Performance comparison charts

#### Cache Maintenance (`cache-maintenance`)
- **Trigger**: Main branch pushes only
- **Purpose**: Binary cache optimization
- **Actions**:
  - Push successful builds to Cachix
  - Generate cache analytics
  - Performance reporting
- **Reporting**: GitHub Step Summary integration

### 5. Release Pipeline (`release`)

**Purpose**: Multi-platform release artifact generation
**Trigger**: GitHub release events
**Strategy**: Parallel platform builds with artifact upload

#### Release Targets
- **x86_64-linux**: Primary Linux target
- **aarch64-linux**: ARM64 Linux support
- **x86_64-darwin**: Intel macOS support
- **aarch64-darwin**: Apple Silicon support

#### Release Process
1. **Parallel Build**: Each platform builds independently
2. **Artifact Collection**: Platform-specific binaries
3. **Asset Upload**: Automatic GitHub release asset upload
4. **Naming Convention**: `harness-{target}`

### 6. CI Summary (`ci-summary`)

**Purpose**: Aggregate pipeline status and reporting
**Trigger**: Always runs (even on failures)
**Dependencies**: All major pipeline jobs

#### Summary Features
- **Status Aggregation**: Overall pipeline health
- **Job Matrix Overview**: Individual job status table
- **Artifact Summary**: Build output overview
- **Failure Detection**: Automatic failure reporting
- **GitHub Integration**: Step Summary with rich formatting

## Performance Optimizations

### Caching Strategy

#### Multi-Tier Caching
1. **Cachix Binary Cache**: Persistent, shared across CI runs
2. **Magic Nix Cache**: GitHub Actions automatic caching
3. **Cargo Cache**: Rust dependency caching
4. **Container Cache**: Docker layer caching

#### Cache Configuration
- **Push Filter**: Excludes source tarballs and nixpkgs
- **Cache Keys**: Content-addressed for reproducibility
- **TTL**: 300s for tarball caching
- **Size Management**: 50GB maximum cache size

### Parallel Execution

#### Matrix Optimization
- **Fail-Fast Disabled**: Independent job execution
- **Resource Distribution**: Balanced across runner types
- **Platform Specialization**: Optimal tool usage per platform
- **Selective Testing**: Platform-appropriate test suites

#### Build Optimization
- **Incremental Compilation**: Cargo incremental builds
- **Parallel Jobs**: Maximum CPU utilization
- **Cross-Compilation**: Nix-based for Linux, Cargo for macOS
- **Container Parallelization**: Simultaneous multi-image builds

## Monitoring and Observability

### Status Reporting

#### GitHub Integration
- **Step Summary**: Rich markdown reporting
- **PR Comments**: Automated feedback
- **Status Checks**: Required for branch protection
- **Artifact Links**: Direct download access

#### Metrics Collection
- **Build Times**: Per-job execution tracking
- **Cache Hit Rates**: Binary cache effectiveness
- **Test Coverage**: Code coverage trends
- **Performance Benchmarks**: Regression detection

### Alerting

#### Failure Notifications
- **Email**: Automatic GitHub notifications
- **Status Badges**: README integration
- **PR Blocks**: Required checks for merging
- **Performance Alerts**: Benchmark regression detection

## Security Considerations

### Supply Chain Security

#### Dependency Management
- **Cargo Audit**: Vulnerability scanning
- **Cargo Deny**: License compliance
- **Pinned Dependencies**: Reproducible builds
- **SBOM Generation**: Software bill of materials

#### Container Security
- **Trivy Scanning**: Container vulnerability analysis
- **SARIF Upload**: Security findings integration
- **Base Image Updates**: Regular security patches
- **Multi-stage Builds**: Minimal attack surface

### Access Control

#### GitHub Security
- **Token Permissions**: Minimal required permissions
- **Secret Management**: Encrypted secrets storage
- **Branch Protection**: Required status checks
- **Code Review**: Mandatory PR reviews

#### Registry Security
- **GHCR Integration**: GitHub-native container registry
- **Image Signing**: Container image verification
- **Access Control**: Organization-level permissions
- **Vulnerability Scanning**: Automatic security analysis

## Configuration Files

### Main Pipeline
- **File**: `.github/workflows/ci.yml`
- **Triggers**: Push, PR, Release
- **Jobs**: 6 major job types
- **Matrix Size**: ~30 parallel jobs

### Dependencies
- **Nix Flake**: `flake.nix` (build configuration)
- **Cargo Config**: `Cargo.toml` (Rust configuration)
- **GitHub Actions**: Pinned action versions

## Troubleshooting

### Common Issues

#### Cache Misses
1. **Check Cachix Configuration**: Ensure auth token is set
2. **Verify Cache Keys**: Content-addressed cache validation
3. **Review Push Filters**: Exclude patterns verification
4. **Monitor Cache Size**: Storage limit management

#### Cross-Compilation Failures
1. **Target Validation**: Ensure target is supported
2. **Dependency Compatibility**: Cross-compilation support
3. **Fallback Strategy**: Native compilation backup
4. **Tool Availability**: Cross-compilation toolchain

#### Container Build Issues
1. **Registry Authentication**: GitHub token permissions
2. **Base Image Updates**: Dependency availability
3. **Multi-arch Support**: Platform compatibility
4. **Resource Limits**: Memory and disk usage

### Debug Commands

#### Local Reproduction
```bash
# Reproduce test issues
nix develop --command cargo nextest run
nix run .#dev-check

# Reproduce build issues
nix build .#nanna-coder
nix flake check

# Reproduce container issues
nix build .#qwen3-container
nix run .#container-test
```

#### CI Investigation
```bash
# Check cache status
nix run .#cache-analytics

# Verify binary cache
nix path-info --json .#nanna-coder

# Debug flake configuration
nix flake show
nix flake metadata
```

## Performance Targets

### Build Performance
- **Cache Hit Rate**: >85% for CI builds
- **Total Pipeline Time**: <20 minutes for full matrix
- **Container Build Time**: <10 minutes per image
- **Binary Build Time**: <5 minutes per target

### Resource Utilization
- **Parallel Jobs**: ~30 concurrent jobs
- **Runner Distribution**: Balanced across Ubuntu/macOS/Windows
- **Cache Storage**: <50GB total usage
- **Artifact Size**: <100MB per platform

---

For additional information about the CI/CD pipeline, consult the workflow files or create an issue for pipeline improvements.