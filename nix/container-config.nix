/** Centralized container configuration for nanna-coder

This module provides configurable settings for container builds,
eliminating hardcoded values and enabling fork compatibility.

# Overview

- **Fork-compatible**: Uses $GITHUB_REPOSITORY env var automatically
- **Registry-agnostic**: Supports any OCI-compatible registry
- **Environment-driven**: Override via env vars or Nix inputs

# Quick Start

```nix
# In your flake.nix:
containerConfig = import ./nix/container-config.nix { lib = pkgs.lib; };

# Access configuration:
containerConfig.images.ollama
=> "nanna-coder-ollama"

containerConfig.registry.url
=> "ghcr.io"

containerConfig.helpers.ollamaRef "v1.0.0"
=> "ghcr.io/dominicburkart/nanna-coder/nanna-coder-ollama:v1.0.0"
```

# Configuration via Environment Variables

```bash
# Use custom registry
export CONTAINER_REGISTRY=docker.io
export GITHUB_REPOSITORY=myorg/nanna-coder
nix build .#ollamaImage

# Result uses:
# docker.io/myorg/nanna-coder/nanna-coder-ollama:latest
```

# Fork Compatibility

In GitHub Actions forks, GITHUB_REPOSITORY is automatically set:

```nix
# Fork: username/nanna-coder
containerConfig.registry.repository
=> "username/nanna-coder"

containerConfig.validation.isFork
=> true
```

# Loading Containers

Recommended method (official nix2container approach):

```bash
# Load any image using copyToDockerDaemon
nix run .#ollamaImage.copyToDockerDaemon

# Verify loaded
docker images | grep nanna-coder-ollama

# Run container
docker run -d -p 11434:11434 nanna-coder-ollama:latest
```

# See Also

- Container definitions: nix/containers.nix
- Loading utilities: flake.nix (load-ollama-image)
- CI usage: .github/workflows/ci.yml:529-563
- nix2container: https://github.com/nlewo/nix2container
*/

{ lib }:

let
  /** Get environment variable with fallback

  Reads environment variable at evaluation time. Returns default if unset.

  # Example

  ```nix
  getEnv "GITHUB_REPOSITORY" "local/nanna-coder"
  => "dominicburkart/nanna-coder"  # if GITHUB_REPOSITORY is set

  getEnv "UNSET_VAR" "default-value"
  => "default-value"
  ```
  */
  getEnv = var: default:
    let val = builtins.getEnv var;
    in if val != "" then val else default;

  /** Parse repository name from GitHub format

  Splits "owner/repo" format into components or uses default for local builds.

  # Example

  ```nix
  parseRepository "dominicburkart/nanna-coder"
  => {
    owner = "dominicburkart";
    name = "nanna-coder";
    full = "dominicburkart/nanna-coder";
  }

  parseRepository "local-build"
  => {
    owner = "local";
    name = "local-build";
    full = "local-build";
  }
  ```
  */
  parseRepository = repoString:
    let
      parts = lib.splitString "/" repoString;
      hasOwner = (builtins.length parts) == 2;
    in {
      owner = if hasOwner then builtins.elemAt parts 0 else "local";
      name = if hasOwner then builtins.elemAt parts 1 else repoString;
      full = repoString;
    };

in rec {
  /** Registry configuration

  Container registry settings with automatic fork detection.

  # Attributes

  - `url`: Registry URL (default: ghcr.io)
  - `repository`: Full repository name (default: from $GITHUB_REPOSITORY)
  - `repo`: Parsed repository components

  # Example

  ```nix
  registry.url
  => "ghcr.io"

  registry.repository
  => "dominicburkart/nanna-coder"

  registry.repo.owner
  => "dominicburkart"
  ```

  # Override via Environment

  ```bash
  export CONTAINER_REGISTRY=docker.io
  export GITHUB_REPOSITORY=myuser/my-fork
  ```
  */
  registry = {
    # Container registry URL (e.g., ghcr.io, docker.io, quay.io)
    url = getEnv "CONTAINER_REGISTRY" "ghcr.io";

    # Repository in format "owner/name" (e.g., "dominicburkart/nanna-coder")
    # Defaults to GITHUB_REPOSITORY env var (automatically set in GitHub Actions)
    repository = getEnv "GITHUB_REPOSITORY" "local/nanna-coder";

    # Parsed repository components
    repo = parseRepository (getEnv "GITHUB_REPOSITORY" "local/nanna-coder");
  };

  /** Image naming conventions

  Standard names for all container images (without registry/repository prefix).

  # Example

  ```nix
  images.harness
  => "nanna-coder-harness"

  images.ollama
  => "nanna-coder-ollama"

  images.models.qwen3
  => "nanna-coder-ollama-qwen3"
  ```

  # Full Image Reference

  To build complete image reference, use helpers.imageRef:

  ```nix
  helpers.imageRef images.ollama "latest"
  => "ghcr.io/dominicburkart/nanna-coder/nanna-coder-ollama:latest"
  ```
  */
  images = {
    # Base image names (without registry/repository prefix)
    harness = "nanna-coder-harness";
    ollama = "nanna-coder-ollama";

    # Model-specific image names
    models = {
      qwen3 = "nanna-coder-ollama-qwen3";
      llama3 = "nanna-coder-ollama-llama3";
      mistral = "nanna-coder-ollama-mistral";
      gemma = "nanna-coder-ollama-gemma";
    };
  };

  /** Tag strategies

  Various tagging strategies for different environments.

  # Example

  ```nix
  tags.default
  => "latest"

  tags.fromCommit  # if GITHUB_SHA="abc123..."
  => "abc123..."

  tags.fromBranch  # if GITHUB_REF_NAME="main"
  => "main"
  ```

  # CI Usage

  ```yaml
  docker tag ollama:latest ollama:${{ github.sha }}
  # Uses tags.fromCommit automatically
  ```
  */
  tags = {
    # Default tag for development builds
    default = "latest";

    # Generate tag from git commit (set via env var in CI)
    fromCommit = getEnv "GITHUB_SHA" "dev";

    # Generate tag from git branch (set via env var in CI)
    fromBranch = getEnv "GITHUB_REF_NAME" "main";

    # Semantic version tag (set via env var or input)
    semver = getEnv "VERSION" null;
  };

  /** Helper functions for constructing full image references

  Utility functions to build fully-qualified image references.

  # Examples

  ```nix
  # Build full image reference
  helpers.imageRef "nanna-coder-ollama" "v1.0.0"
  => "ghcr.io/dominicburkart/nanna-coder/nanna-coder-ollama:v1.0.0"

  # Quick references for common images
  helpers.ollamaRef "latest"
  => "ghcr.io/dominicburkart/nanna-coder/nanna-coder-ollama:latest"

  helpers.harnessRef "dev"
  => "ghcr.io/dominicburkart/nanna-coder/nanna-coder-harness:dev"

  helpers.modelRef "qwen3" "v1.0.0"
  => "ghcr.io/dominicburkart/nanna-coder/nanna-coder-ollama-qwen3:v1.0.0"
  ```

  # Usage in CI

  ```yaml
  # Tag for registry (works in forks)
  REPO_LOWERCASE=$(echo "${{ github.repository }}" | tr '[:upper:]' '[:lower:]')
  docker tag ollama:latest ghcr.io/${REPO_LOWERCASE}/ollama:latest
  ```

  # Usage in Nix

  ```nix
  # Use in container definitions
  buildImage {
    name = containerConfig.images.ollama;
    tag = containerConfig.tags.default;
  }
  ```
  */
  helpers = {
    # Build fully-qualified image reference: registry/owner/repo/image:tag
    imageRef = imageName: tag:
      "${registry.url}/${registry.repository}/${imageName}:${tag}";

    # Build image reference for harness
    harnessRef = tag: helpers.imageRef images.harness tag;

    # Build image reference for ollama
    ollamaRef = tag: helpers.imageRef images.ollama tag;

    # Build image reference for model container
    modelRef = modelName: tag:
      helpers.imageRef images.models.${modelName} tag;
  };

  /** Container runtime configuration

  Settings for container runtime behavior and reproducibility.

  # Example

  ```nix
  runtime.maxLayers
  => 100

  runtime.buildTimestamp
  => "2025-09-20T00:00:00Z"

  runtime.preferPodman
  => true
  ```

  # Usage

  ```nix
  buildImage {
    maxLayers = containerConfig.runtime.maxLayers;
    created = containerConfig.runtime.buildTimestamp;
  }
  ```
  */
  runtime = {
    # Prefer podman over docker for local development
    preferPodman = true;

    # Maximum layer count for nix2container images
    maxLayers = 100;

    # Reproducible build timestamp (ISO 8601 format)
    # Using a fixed timestamp for reproducibility
    buildTimestamp = "2025-09-20T00:00:00Z";
  };

  /** Loading configuration

  Settings for container loading operations.

  # Example

  ```nix
  loading.method
  => "copyToDockerDaemon"

  loading.retries.maxAttempts
  => 3

  loading.timeoutSeconds
  => 300
  ```

  # Note

  Retry logic and timeout are defined but not yet implemented in CI.
  Reserved for future use.
  */
  loading = {
    # Method to use for loading images
    # Options: "copyToDockerDaemon", "copyToPodman", "legacy"
    method = "copyToDockerDaemon";

    # Retry configuration for transient failures (reserved for future use)
    retries = {
      maxAttempts = 3;
      delaySeconds = 5;
    };

    # Timeout for loading operations in seconds (reserved for future use)
    timeoutSeconds = 300;

    # Enable verbose logging during load
    verbose = getEnv "CONTAINER_LOAD_VERBOSE" "false" == "true";
  };

  /** Validation helpers

  Runtime validation utilities for detecting environment.

  # Example

  ```nix
  # In original repo
  validation.isFork
  => false

  # In fork
  validation.isFork
  => true

  # In GitHub Actions
  validation.isGitHubActions
  => true
  ```

  # Usage

  ```nix
  if containerConfig.validation.isFork
  then "Fork detected - using ${registry.repository}"
  else "Original repo"
  ```
  */
  validation = {
    # Check if we're running in a fork (different from original repo)
    isFork = registry.repo.full != "dominicburkart/nanna-coder";

    # Check if we're in CI environment
    isCI = (getEnv "CI" "") == "true";

    # Check if we're in GitHub Actions
    isGitHubActions = (getEnv "GITHUB_ACTIONS" "") == "true";
  };
}
