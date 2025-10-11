# Container Loading Guide

This document explains how nanna-coder loads and manages container images built with nix2container.

## Table of Contents

- [Overview](#overview)
- [Loading Methods](#loading-methods)
- [Configuration](#configuration)
- [Troubleshooting](#troubleshooting)
- [Fork Compatibility](#fork-compatibility)
- [Development Workflow](#development-workflow)

## Overview

nanna-coder uses [nix2container](https://github.com/nlewo/nix2container) to build OCI-compatible container images. Unlike traditional Docker builds, nix2container:

- **Doesn't create tarballs** in the Nix store (saves space and time)
- **Uses skopeo** internally for efficient image handling
- **Supports layer caching** to skip unchanged layers during pushes
- **Provides reproducible builds** through Nix's deterministic system

## Loading Methods

### Recommended: `copyToDockerDaemon`

The **official and recommended** method for loading nix2container images:

```bash
# Load a single image
nix run .#ollamaImage.copyToDockerDaemon

# Verify it loaded
docker images | grep nanna-coder-ollama
```

**How it works:**
1. nix2container builds a JSON description (not a tarball)
2. `copyToDockerDaemon` uses skopeo with the `nix:` transport
3. Skopeo loads the image directly into the Docker daemon
4. No intermediate files are created

**Benefits:**
- Official nix2container approach
- Fast (no tar extraction)
- Handles all format complexities internally
- Works with both Docker and Podman

### CI/CD Usage

In CI workflows, we use the same method with explicit error handling:

```yaml
- name: Load container image
  run: |
    if ! nix run .#ollamaImage.copyToDockerDaemon; then
      echo "❌ Failed to load image"
      echo "Check Docker daemon: docker info"
      exit 1
    fi

    # Verify image loaded
    docker image inspect nanna-coder-ollama:latest
```

See: `.github/workflows/ci.yml:517-551` for the full implementation.

### Alternative: Manual Load (Not Recommended)

For debugging only:

```bash
# Build the image
nix build .#ollamaImage

# Check the output format
file result  # Should show "JSON data"

# Manual load (requires understanding nix2container internals)
docker load < result  # May not work correctly
```

⚠️ **Warning:** Manual `docker load` may fail because nix2container uses a custom JSON format optimized for skopeo. Always prefer `copyToDockerDaemon`.

## Configuration

Container loading is configured via `nix/container-config.nix`:

```nix
{
  # Loading configuration
  loading = {
    # Primary method (do not change unless you know what you're doing)
    method = "copyToDockerDaemon";

    # Retry configuration for transient failures
    retries = {
      maxAttempts = 3;
      delaySeconds = 5;
    };

    # Timeout for loading operations (seconds)
    timeoutSeconds = 300;

    # Enable verbose logging
    verbose = false;
  };
}
```

### Environment Variables

You can override configuration via environment variables:

```bash
# Use a custom registry
export CONTAINER_REGISTRY=quay.io

# Enable verbose loading
export CONTAINER_LOAD_VERBOSE=true

# Then build/load as normal
nix run .#ollamaImage.copyToDockerDaemon
```

## Troubleshooting

### Image Not Loading

**Symptom:** `copyToDockerDaemon` fails or times out

**Solutions:**
1. Check Docker daemon is running:
   ```bash
   docker info
   ```

2. Ensure you have enough disk space:
   ```bash
   df -h
   ```

3. Verify the image built successfully:
   ```bash
   nix build .#ollamaImage --print-out-paths
   ```

4. Check skopeo is available:
   ```bash
   which skopeo
   ```

### Image Loaded But Not Visible

**Symptom:** `copyToDockerDaemon` succeeds but `docker images` doesn't show it

**Solution:**
Images are loaded with their configured names from `nix/container-config.nix`:

```bash
# Check for the exact name
docker images | grep nanna-coder-ollama

# List all images to find it
docker images --all
```

### Fork-Specific Issues

**Symptom:** CI fails with "image not found" in a fork

**Cause:** Hardcoded repository names in old code

**Solution:** Ensure you're on the latest version with configurable repositories:
```bash
git pull origin main
# Repository now uses $GITHUB_REPOSITORY automatically
```

See: [Fork Compatibility](#fork-compatibility) below.

### Permission Errors

**Symptom:** "permission denied" when loading

**Solutions:**
1. **Docker socket permissions:**
   ```bash
   sudo usermod -aG docker $USER
   newgrp docker
   ```

2. **Podman rootless:**
   ```bash
   # Use podman instead of docker
   alias docker=podman
   ```

3. **SELinux issues:**
   ```bash
   # Temporarily disable (not recommended for production)
   sudo setenforce 0
   ```

## Fork Compatibility

As of commit `8bcfff5`, nanna-coder is fully fork-compatible:

### How It Works

1. **Registry Configuration** (`nix/container-config.nix`):
   ```nix
   registry = {
     # Reads from GITHUB_REPOSITORY env var (auto-set in GitHub Actions)
     repository = getEnv "GITHUB_REPOSITORY" "local/nanna-coder";
   };
   ```

2. **CI Workflow** (`.github/workflows/ci.yml`):
   ```yaml
   # Uses ${{ github.repository }} dynamically
   REPO_LOWERCASE=$(echo "${{ github.repository }}" | tr '[:upper:]' '[:lower:]')
   docker tag ${IMAGE_NAME}:latest ghcr.io/${REPO_LOWERCASE}/ollama:latest
   ```

### Testing in Your Fork

1. **Fork the repository** on GitHub

2. **Enable GitHub Actions** in your fork settings

3. **Push a commit:**
   ```bash
   git push origin your-branch
   ```

4. **Check the workflow:**
   - Images will be tagged with your fork's repository name
   - No code changes needed!

### Using Custom Registries

```bash
# Build with custom registry
export CONTAINER_REGISTRY=docker.io
export GITHUB_REPOSITORY=yourusername/nanna-coder

nix build .#ollamaImage
```

The image will be built for `docker.io/yourusername/nanna-coder/ollama:latest`.

## Development Workflow

### Quick Iteration

```bash
# 1. Make code changes
vim harness/src/main.rs

# 2. Rebuild container (uses caching for unchanged layers)
nix build .#harnessImage

# 3. Load into Docker
nix run .#harnessImage.copyToDockerDaemon

# 4. Test immediately
docker run -it nanna-coder-harness:latest --help
```

### Layer Caching

nix2container's layer caching means:
- **Dependencies layer:** Rebuilt only when Cargo.lock changes
- **Source layer:** Rebuilt only when Rust code changes
- **Other layers:** Skipped entirely if unchanged

This makes iteration **much faster** than traditional Docker builds.

### Pre-built Images

For faster CI, pre-load images in the integration test step:

```yaml
- name: Pre-build test containers
  run: |
    nix build .#ollamaImage
    nix run .#ollamaImage.copyToDockerDaemon
```

The image is then cached for subsequent test runs.

## Model Containers

### Understanding Model Hashes

Model containers use content hashing for reproducibility:

```nix
"qwen3" = {
  name = "qwen3:0.6b";
  hash = "sha256-2EaXyBr1C+6wNyLzcWblzB52iV/2G26dSa5MFqpYJLc=";  # Real hash
  # ...
};

"llama3" = {
  name = "llama3:8b";
  hash = "sha256-0000000000000000000000000000000000000000000=";  # Placeholder
  # ...
};
```

**Real hash (qwen3):**
- Fully reproducible builds
- Cached by Nix store based on content
- Guaranteed same output every time

**Placeholder hash (llama3, mistral, gemma):**
- Development mode
- Creates lightweight stub during build
- Downloads model on-demand when container runs
- Faster iteration, no large downloads during development

### Enabling Production Caching for Models

To convert a placeholder model to production:

```bash
# 1. Build the model container
nix build .#llama3-container

# 2. Run it to download the model
docker run -it nanna-coder-ollama-llama3:latest

# 3. Calculate the actual hash
nix hash path /path/to/model/directory

# 4. Update nix/containers.nix with real hash
# 5. Rebuild - now fully cached and reproducible
```

## Advanced Topics

### Skopeo Internals

When you run `copyToDockerDaemon`, this happens:

```bash
# Internally executes something like:
skopeo copy \
  nix:/nix/store/xxx-image.json \
  containers-storage:nanna-coder-ollama:latest
```

The `nix:` transport is a skopeo plugin that understands nix2container's JSON format.

### Multi-Arch Builds

Currently, nanna-coder supports:
- ✅ x86_64-linux (primary)
- ⚠️ aarch64-linux (experimental, cross-compilation issues)
- ✅ x86_64-darwin (macOS, no containers)
- ✅ aarch64-darwin (Apple Silicon, no containers)

For aarch64-linux containers, use QEMU:
```yaml
- name: Set up QEMU
  uses: docker/setup-qemu-action@v3
  with:
    platforms: arm64
```

### Custom Loading Scripts

If you need custom loading logic, see the helper utility:

```bash
# From flake.nix
nix run .#load-ollama-image
```

Source: `flake.nix:332-348`

## References

- [nix2container GitHub](https://github.com/nlewo/nix2container)
- [Skopeo Documentation](https://github.com/containers/skopeo)
- [OCI Image Specification](https://github.com/opencontainers/image-spec)
- [Issue #3: Container Loading Redesign](https://github.com/DominicBurkart/nanna-coder/issues/3)

## See Also

- `nix/container-config.nix` - Centralized configuration
- `nix/containers.nix` - Container definitions
- `.github/workflows/ci.yml` - CI implementation
- `ARCHITECTURE.md` - Overall system architecture
