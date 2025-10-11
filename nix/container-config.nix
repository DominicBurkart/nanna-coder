# Centralized container configuration
# This module provides configurable settings for container builds,
# eliminating hardcoded values and enabling fork compatibility.
#
# Configuration can be overridden via:
# - Environment variables (CI/runtime)
# - Nix inputs (build-time)
# - Function arguments (programmatic)

{ lib }:

let
  # Get environment variable with fallback
  getEnv = var: default:
    let val = builtins.getEnv var;
    in if val != "" then val else default;

  # Parse repository name from GitHub format (owner/repo) or use default
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
  # Registry configuration
  registry = {
    # Container registry URL (e.g., ghcr.io, docker.io, quay.io)
    url = getEnv "CONTAINER_REGISTRY" "ghcr.io";

    # Repository in format "owner/name" (e.g., "dominicburkart/nanna-coder")
    # Defaults to GITHUB_REPOSITORY env var (automatically set in GitHub Actions)
    repository = getEnv "GITHUB_REPOSITORY" "local/nanna-coder";

    # Parsed repository components
    repo = parseRepository (getEnv "GITHUB_REPOSITORY" "local/nanna-coder");
  };

  # Image naming conventions
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

  # Tag strategies
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

  # Helper functions for constructing full image references
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

  # Container runtime configuration
  runtime = {
    # Prefer podman over docker for local development
    preferPodman = true;

    # Maximum layer count for nix2container images
    maxLayers = 100;

    # Reproducible build timestamp (ISO 8601 format)
    # Using a fixed timestamp for reproducibility
    buildTimestamp = "2025-09-20T00:00:00Z";
  };

  # Loading configuration
  loading = {
    # Method to use for loading images
    # Options: "copyToDockerDaemon", "copyToPodman", "legacy"
    method = "copyToDockerDaemon";

    # Retry configuration for transient failures
    retries = {
      maxAttempts = 3;
      delaySeconds = 5;
    };

    # Timeout for loading operations (seconds)
    timeoutSeconds = 300;

    # Enable verbose logging during load
    verbose = getEnv "CONTAINER_LOAD_VERBOSE" "false" == "true";
  };

  # Validation helpers
  validation = {
    # Check if we're running in a fork (different from original repo)
    isFork = registry.repo.full != "dominicburkart/nanna-coder";

    # Check if we're in CI environment
    isCI = (getEnv "CI" "") == "true";

    # Check if we're in GitHub Actions
    isGitHubActions = (getEnv "GITHUB_ACTIONS" "") == "true";
  };
}
