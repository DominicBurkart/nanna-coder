use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use thiserror::Error;
use tokio::time::{sleep, timeout};

/// Container runtime types supported
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerRuntime {
    /// Podman container runtime
    Podman,
    /// Docker container runtime
    Docker,
    /// No container runtime available
    None,
}

impl ContainerRuntime {
    /// Get the command name for this runtime
    pub fn command(&self) -> &'static str {
        match self {
            ContainerRuntime::Podman => "podman",
            ContainerRuntime::Docker => "docker",
            ContainerRuntime::None => "",
        }
    }

    /// Check if this runtime is available
    pub fn is_available(&self) -> bool {
        matches!(self, ContainerRuntime::Podman | ContainerRuntime::Docker)
    }
}

/// Comprehensive container operation errors
#[derive(Error, Debug)]
pub enum ContainerError {
    /// No container runtime is available
    #[error("No container runtime available. Please install Docker or Podman to run containerized tests.")]
    NoRuntimeAvailable,

    /// Container image not found
    #[error("Container image '{image}' not found. {suggestion}")]
    ImageNotFound { image: String, suggestion: String },

    /// Container failed to start
    #[error("Failed to start container '{name}': {reason}")]
    ContainerStartFailed { name: String, reason: String },

    /// Container operation timed out
    #[error("Container operation timed out after {timeout}s: {operation}")]
    OperationTimeout { operation: String, timeout: u64 },

    /// Health check failed
    #[error(
        "Container health check failed: {reason}. Check if the service is properly configured."
    )]
    HealthCheckFailed { reason: String },

    /// Model pull failed
    #[error("Failed to pull model '{model}': {reason}. This might be due to network issues or insufficient disk space.")]
    ModelPullFailed { model: String, reason: String },

    /// Container cleanup failed
    #[error("Failed to cleanup container '{name}': {reason}")]
    CleanupFailed { name: String, reason: String },

    /// Command execution failed
    #[error("Command execution failed: {command}")]
    CommandFailed { command: String },

    /// Image loading failed
    #[error("Failed to load image from path '{path}': {reason}")]
    ImageLoadFailed { path: String, reason: String },

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Configuration for container operations
#[derive(Debug, Clone)]
pub struct ContainerConfig {
    /// Base container image to use
    pub base_image: String,
    /// Pre-built test image (if available)
    pub test_image: Option<String>,
    /// Container name for the instance
    pub container_name: String,
    /// Port mapping (host_port, container_port)
    pub port_mapping: Option<(u16, u16)>,
    /// Model to pull if using base image
    pub model_to_pull: Option<String>,
    /// Startup timeout in seconds
    pub startup_timeout: Duration,
    /// Health check timeout in seconds
    pub health_check_timeout: Duration,
    /// Environment variables
    pub env_vars: Vec<(String, String)>,
    /// Additional container arguments
    pub additional_args: Vec<String>,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        Self {
            base_image: "ollama/ollama:latest".to_string(),
            test_image: None,
            container_name: "nanna-coder-test".to_string(),
            port_mapping: Some((11435, 11434)),
            model_to_pull: None,
            startup_timeout: Duration::from_secs(30),
            health_check_timeout: Duration::from_secs(10),
            env_vars: Vec::new(),
            additional_args: Vec::new(),
        }
    }
}

/// Handle for a running container
#[derive(Debug)]
pub struct ContainerHandle {
    /// Container name
    pub name: String,
    /// Runtime used
    pub runtime: ContainerRuntime,
    /// Port the container is accessible on
    pub port: Option<u16>,
    /// Whether the container needs cleanup
    pub needs_cleanup: bool,
}

impl Drop for ContainerHandle {
    fn drop(&mut self) {
        if self.needs_cleanup && self.runtime.is_available() {
            let _ = Command::new(self.runtime.command())
                .args(["rm", "-f", &self.name])
                .output();
        }
    }
}

/// Detect available container runtime in order of preference
pub fn detect_runtime() -> ContainerRuntime {
    // Try Podman first (often better for rootless containers)
    if Command::new("podman")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        return ContainerRuntime::Podman;
    }

    // Fall back to Docker
    if Command::new("docker")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        return ContainerRuntime::Docker;
    }

    ContainerRuntime::None
}

/// Verify that a container image exists locally
pub fn verify_image_exists(
    runtime: &ContainerRuntime,
    image_name: &str,
) -> Result<bool, ContainerError> {
    if !runtime.is_available() {
        return Err(ContainerError::NoRuntimeAvailable);
    }

    let output = Command::new(runtime.command())
        .args(["image", "exists", image_name])
        .output()
        .map_err(|_e| ContainerError::CommandFailed {
            command: format!("{} image exists {}", runtime.command(), image_name),
        })?;

    Ok(output.status.success())
}

/// Load container image from a file path (e.g., from Nix build)
pub fn load_image_from_path(
    runtime: &ContainerRuntime,
    image_path: &Path,
) -> Result<(), ContainerError> {
    if !runtime.is_available() {
        return Err(ContainerError::NoRuntimeAvailable);
    }

    if !image_path.exists() {
        return Err(ContainerError::ImageLoadFailed {
            path: image_path.display().to_string(),
            reason: "Path does not exist".to_string(),
        });
    }

    let output = Command::new(runtime.command())
        .args(["load", "-i"])
        .arg(image_path)
        .output()
        .map_err(|e| ContainerError::ImageLoadFailed {
            path: image_path.display().to_string(),
            reason: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(ContainerError::ImageLoadFailed {
            path: image_path.display().to_string(),
            reason: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    Ok(())
}

/// Start container with intelligent fallback logic
pub async fn start_container_with_fallback(
    config: &ContainerConfig,
) -> Result<ContainerHandle, ContainerError> {
    let runtime = detect_runtime();

    if !runtime.is_available() {
        return Err(ContainerError::NoRuntimeAvailable);
    }

    // Clean up any existing container with the same name
    let _ = Command::new(runtime.command())
        .args(["rm", "-f", &config.container_name])
        .output();

    // Try pre-built test image first if specified
    let (image_to_use, needs_model_pull) = if let Some(test_image) = &config.test_image {
        match verify_image_exists(&runtime, test_image) {
            Ok(true) => {
                println!("✅ Using pre-built test container: {}", test_image);
                (test_image.clone(), false)
            }
            Ok(false) => {
                println!(
                    "📦 Pre-built container not found, falling back to base: {}",
                    config.base_image
                );
                println!("   To build cached container: nix build .#ollama-qwen3");
                (config.base_image.clone(), true)
            }
            Err(_) => {
                println!(
                    "⚠️  Could not check test image, using base: {}",
                    config.base_image
                );
                (config.base_image.clone(), true)
            }
        }
    } else {
        (config.base_image.clone(), config.model_to_pull.is_some())
    };

    // Verify base image exists, pull if needed
    if !verify_image_exists(&runtime, &image_to_use)? {
        println!("📥 Pulling container image: {}", image_to_use);
        let pull_output = Command::new(runtime.command())
            .args(["pull", &image_to_use])
            .output()
            .map_err(|e| ContainerError::ImageNotFound {
                image: image_to_use.clone(),
                suggestion: format!("Failed to pull image: {}", e),
            })?;

        if !pull_output.status.success() {
            return Err(ContainerError::ImageNotFound {
                image: image_to_use,
                suggestion: format!(
                    "Pull failed: {}. Check network connectivity and image name.",
                    String::from_utf8_lossy(&pull_output.stderr)
                ),
            });
        }
    }

    // Build container run command
    let mut cmd = Command::new(runtime.command());
    cmd.args(["run", "-d", "--name", &config.container_name]);

    // Add port mapping if specified
    if let Some((host_port, container_port)) = config.port_mapping {
        cmd.args(["-p", &format!("{}:{}", host_port, container_port)]);
    }

    // Add environment variables
    for (key, value) in &config.env_vars {
        cmd.args(["-e", &format!("{}={}", key, value)]);
    }

    // Add additional arguments
    for arg in &config.additional_args {
        cmd.arg(arg);
    }

    // Add remove flag for automatic cleanup
    cmd.arg("--rm");

    // Finally add the image
    cmd.arg(&image_to_use);

    // Start the container
    println!("🚀 Starting container: {}", config.container_name);
    let start_output = cmd
        .output()
        .map_err(|e| ContainerError::ContainerStartFailed {
            name: config.container_name.clone(),
            reason: e.to_string(),
        })?;

    if !start_output.status.success() {
        return Err(ContainerError::ContainerStartFailed {
            name: config.container_name.clone(),
            reason: String::from_utf8_lossy(&start_output.stderr).to_string(),
        });
    }

    // Wait for container to be ready
    println!("⏳ Waiting for container to be ready...");
    sleep(config.startup_timeout).await;

    // Pull model if needed
    if needs_model_pull {
        if let Some(model) = &config.model_to_pull {
            println!(
                "📥 Pulling model: {} (this may take a while without cache)...",
                model
            );

            let pull_result = timeout(
                Duration::from_secs(300), // 5 minute timeout for model pull
                async {
                    Command::new(runtime.command())
                        .args(["exec", &config.container_name, "ollama", "pull", model])
                        .output()
                },
            )
            .await;

            match pull_result {
                Ok(Ok(output)) => {
                    if !output.status.success() {
                        // Clean up failed container
                        let _ = Command::new(runtime.command())
                            .args(["rm", "-f", &config.container_name])
                            .output();

                        return Err(ContainerError::ModelPullFailed {
                            model: model.clone(),
                            reason: String::from_utf8_lossy(&output.stderr).to_string(),
                        });
                    }
                    println!("✅ Model pulled successfully");
                }
                Ok(Err(e)) => {
                    let _ = Command::new(runtime.command())
                        .args(["rm", "-f", &config.container_name])
                        .output();

                    return Err(ContainerError::ModelPullFailed {
                        model: model.clone(),
                        reason: e.to_string(),
                    });
                }
                Err(_) => {
                    let _ = Command::new(runtime.command())
                        .args(["rm", "-f", &config.container_name])
                        .output();

                    return Err(ContainerError::OperationTimeout {
                        operation: format!("pull model {}", model),
                        timeout: 300,
                    });
                }
            }
        }
    }

    let host_port = config.port_mapping.map(|(host, _)| host);

    Ok(ContainerHandle {
        name: config.container_name.clone(),
        runtime,
        port: host_port,
        needs_cleanup: true,
    })
}

/// Perform health check on a running container service
pub async fn health_check_container(
    handle: &ContainerHandle,
    health_url: &str,
    timeout_duration: Duration,
) -> Result<(), ContainerError> {
    let start_time = std::time::Instant::now();

    loop {
        // Simple HTTP-like check using curl in container
        let check_result = Command::new(handle.runtime.command())
            .args(["exec", &handle.name, "curl", "-f", "-s", health_url])
            .output();

        match check_result {
            Ok(output) if output.status.success() => {
                println!("✅ Health check passed for container: {}", handle.name);
                return Ok(());
            }
            Ok(_) => {
                // Health check failed, but container might still be starting
                if start_time.elapsed() < timeout_duration {
                    sleep(Duration::from_secs(2)).await;
                    continue;
                } else {
                    return Err(ContainerError::HealthCheckFailed {
                        reason: format!(
                            "Health check at {} failed after {}s",
                            health_url,
                            timeout_duration.as_secs()
                        ),
                    });
                }
            }
            Err(e) => {
                return Err(ContainerError::HealthCheckFailed {
                    reason: e.to_string(),
                });
            }
        }
    }
}

/// Clean up container manually (called automatically by Drop trait)
pub fn cleanup_container(handle: &ContainerHandle) -> Result<(), ContainerError> {
    if !handle.runtime.is_available() {
        return Ok(()); // Nothing to clean up
    }

    let output = Command::new(handle.runtime.command())
        .args(["rm", "-f", &handle.name])
        .output()
        .map_err(|e| ContainerError::CleanupFailed {
            name: handle.name.clone(),
            reason: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(ContainerError::CleanupFailed {
            name: handle.name.clone(),
            reason: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    println!("✅ Container cleaned up: {}", handle.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_runtime_command() {
        assert_eq!(ContainerRuntime::Podman.command(), "podman");
        assert_eq!(ContainerRuntime::Docker.command(), "docker");
        assert_eq!(ContainerRuntime::None.command(), "");
    }

    #[test]
    fn test_container_runtime_availability() {
        assert!(ContainerRuntime::Podman.is_available());
        assert!(ContainerRuntime::Docker.is_available());
        assert!(!ContainerRuntime::None.is_available());
    }

    #[test]
    fn test_detect_runtime() {
        let runtime = detect_runtime();
        // We can't predict what will be available in test environment
        // Just ensure it returns a valid enum variant
        match runtime {
            ContainerRuntime::Podman | ContainerRuntime::Docker | ContainerRuntime::None => {}
        }
    }

    #[test]
    fn test_container_config_default() {
        let config = ContainerConfig::default();
        assert_eq!(config.base_image, "ollama/ollama:latest");
        assert_eq!(config.container_name, "nanna-coder-test");
        assert_eq!(config.port_mapping, Some((11435, 11434)));
    }

    #[test]
    fn test_container_error_display() {
        let error = ContainerError::NoRuntimeAvailable;
        assert!(error.to_string().contains("No container runtime available"));

        let error = ContainerError::ImageNotFound {
            image: "test:latest".to_string(),
            suggestion: "Run docker pull test:latest".to_string(),
        };
        assert!(error.to_string().contains("test:latest"));
        assert!(error.to_string().contains("Run docker pull"));
    }

    #[tokio::test]
    async fn test_verify_image_exists_no_runtime() {
        let runtime = ContainerRuntime::None;
        let result = verify_image_exists(&runtime, "test:latest");
        assert!(matches!(result, Err(ContainerError::NoRuntimeAvailable)));
    }

    #[test]
    fn test_load_image_from_nonexistent_path() {
        let runtime = ContainerRuntime::Podman;
        let path = Path::new("/nonexistent/path");
        let result = load_image_from_path(&runtime, path);
        assert!(matches!(
            result,
            Err(ContainerError::ImageLoadFailed { .. })
        ));
    }
}
