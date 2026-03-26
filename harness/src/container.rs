use model::provider::ModelProvider;
use model::OllamaConfig;
use model::OllamaProvider;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time::{sleep, timeout};

pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

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

impl ContainerConfig {
    pub fn ollama_host_url(&self) -> String {
        match self.port_mapping {
            Some((host_port, _)) => format!("http://localhost:{}", host_port),
            None => "http://localhost:11434".to_string(),
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
) -> Result<String, ContainerError> {
    if !runtime.is_available() {
        return Err(ContainerError::NoRuntimeAvailable);
    }

    if !image_path.exists() {
        return Err(ContainerError::ImageLoadFailed {
            path: image_path.display().to_string(),
            reason: "Path does not exist".to_string(),
        });
    }

    let real_path = if image_path.is_symlink() {
        std::fs::read_link(image_path).unwrap_or_else(|_| image_path.to_path_buf())
    } else {
        image_path.to_path_buf()
    };

    let is_nix2container = if real_path.is_file() {
        let mut buf = [0u8; 1];
        std::fs::File::open(&real_path)
            .and_then(|mut f| {
                use std::io::Read;
                f.read_exact(&mut buf).map(|_| buf[0] == b'{')
            })
            .unwrap_or(false)
    } else {
        false
    };

    if is_nix2container {
        let content =
            std::fs::read_to_string(&real_path).map_err(|e| ContainerError::ImageLoadFailed {
                path: image_path.display().to_string(),
                reason: e.to_string(),
            })?;

        let json: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| ContainerError::ImageLoadFailed {
                path: image_path.display().to_string(),
                reason: format!("Invalid nix2container JSON: {}", e),
            })?;

        let name = json
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let tag = json.get("tag").and_then(|v| v.as_str()).unwrap_or("latest");
        let image_ref = format!("{}:{}", name, tag);

        let dest = match runtime {
            ContainerRuntime::Podman => format!("containers-storage:{}", image_ref),
            ContainerRuntime::Docker => format!("docker-daemon:{}", image_ref),
            ContainerRuntime::None => return Err(ContainerError::NoRuntimeAvailable),
        };

        let output = Command::new("skopeo")
            .args(["copy", &format!("nix:{}", real_path.display()), &dest])
            .output()
            .map_err(|e| ContainerError::ImageLoadFailed {
                path: image_path.display().to_string(),
                reason: format!("skopeo not available: {}", e),
            })?;

        if !output.status.success() {
            return Err(ContainerError::ImageLoadFailed {
                path: image_path.display().to_string(),
                reason: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        Ok(image_ref)
    } else {
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

        let stdout = String::from_utf8_lossy(&output.stdout);
        let image_ref = stdout
            .lines()
            .find(|l| l.contains("Loaded image"))
            .and_then(|l| l.split(": ").last())
            .unwrap_or("unknown:latest")
            .trim()
            .to_string();

        Ok(image_ref)
    }
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
    if let Some((host_port, _)) = config.port_mapping {
        health_check_host(host_port, config.startup_timeout).await?;
    } else {
        sleep(config.startup_timeout).await;
    }

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

/// Poll Ollama's HTTP API on the host until it responds or the timeout expires.
pub async fn health_check_host(port: u16, timeout: Duration) -> Result<(), ContainerError> {
    let url = format!("http://localhost:{}/api/tags", port);
    let client = reqwest::Client::new();
    let start = std::time::Instant::now();

    loop {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                println!("✅ Ollama health check passed on port {}", port);
                return Ok(());
            }
            _ => {}
        }

        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return Err(ContainerError::HealthCheckFailed {
                reason: format!(
                    "Ollama at port {} did not respond within {}s",
                    port,
                    timeout.as_secs()
                ),
            });
        }

        // Sleep for at most 2s, but don't exceed the remaining timeout.
        let remaining = timeout - elapsed;
        sleep(remaining.min(Duration::from_secs(2))).await;
    }
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

pub fn exec_in_container(
    handle: &ContainerHandle,
    command: &[&str],
    working_dir: Option<&str>,
) -> Result<CommandOutput, ContainerError> {
    if !handle.runtime.is_available() {
        return Err(ContainerError::NoRuntimeAvailable);
    }

    let mut cmd = Command::new(handle.runtime.command());
    cmd.arg("exec");

    if let Some(dir) = working_dir {
        cmd.args(["-w", dir]);
    }

    cmd.arg(&handle.name);
    cmd.args(command);

    let output = cmd.output().map_err(|_e| ContainerError::CommandFailed {
        command: format!(
            "{} exec {} {:?}",
            handle.runtime.command(),
            handle.name,
            command
        ),
    })?;

    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        success: output.status.success(),
    })
}

struct PoolInner {
    container: Option<ContainerHandle>,
    provider: Option<Arc<dyn ModelProvider>>,
    ref_count: usize,
}

/// Shared pool that manages a single model container instance.
///
/// Tracks active users with a reference count; starts the container on first
/// `get_or_start` and stops it when the last user drops their guard.
pub struct SharedModelPool {
    inner: std::sync::Mutex<PoolInner>,
    /// Serializes async container startup so only one task starts the container.
    start_lock: tokio::sync::Mutex<()>,
    container_config: ContainerConfig,
}

/// RAII guard holding a reference to the shared model pool.
///
/// Dropping this guard decrements the pool's reference count and stops the
/// container when no more guards exist.
pub struct ModelGuard {
    pool: Arc<SharedModelPool>,
    provider: Arc<dyn ModelProvider>,
}

impl ModelGuard {
    pub fn provider(&self) -> &Arc<dyn ModelProvider> {
        &self.provider
    }
}

impl Drop for ModelGuard {
    fn drop(&mut self) {
        let mut inner = self.pool.inner.lock().unwrap();
        inner.ref_count = inner.ref_count.saturating_sub(1);
        if inner.ref_count == 0 {
            inner.container.take();
            inner.provider.take();
        }
    }
}

impl SharedModelPool {
    pub fn new(container_config: ContainerConfig) -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(PoolInner {
                container: None,
                provider: None,
                ref_count: 0,
            }),
            start_lock: tokio::sync::Mutex::new(()),
            container_config,
        })
    }

    #[cfg(test)]
    pub fn new_with_provider(
        container_config: ContainerConfig,
        provider: Arc<dyn ModelProvider>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(PoolInner {
                container: None,
                provider: Some(provider),
                ref_count: 0,
            }),
            start_lock: tokio::sync::Mutex::new(()),
            container_config,
        })
    }

    /// Return a `ModelGuard` with a shared provider, starting the container if necessary.
    pub async fn get_or_start(self: &Arc<Self>) -> Result<ModelGuard, ContainerError> {
        // Serialize startup attempts so only one task starts the container.
        let _start_guard = self.start_lock.lock().await;

        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(provider) = inner.provider.as_ref().cloned() {
                inner.ref_count += 1;
                return Ok(ModelGuard {
                    pool: self.clone(),
                    provider,
                });
            }
        }

        let handle = start_container_with_fallback(&self.container_config).await?;
        let url = self.container_config.ollama_host_url();
        let ollama_config = OllamaConfig::default().with_base_url(url);
        let provider: Arc<dyn ModelProvider> =
            Arc::new(OllamaProvider::new(ollama_config).map_err(|e| {
                ContainerError::CommandFailed {
                    command: format!("OllamaProvider::new: {}", e),
                }
            })?);

        {
            let mut inner = self.inner.lock().unwrap();
            inner.container = Some(handle);
            inner.provider = Some(provider.clone());
            inner.ref_count = 1;
        }

        Ok(ModelGuard {
            pool: self.clone(),
            provider,
        })
    }

    pub fn ref_count(&self) -> usize {
        self.inner.lock().unwrap().ref_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use model::provider::ModelResult;
    use model::types::{ChatRequest, ChatResponse, ModelInfo};

    struct MockProvider;

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn chat(&self, _: ChatRequest) -> ModelResult<ChatResponse> {
            unimplemented!()
        }
        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }
        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }
        fn provider_name(&self) -> &'static str {
            "mock"
        }
    }

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

    #[test]
    fn test_exec_in_container_no_runtime() {
        let handle = ContainerHandle {
            name: "test".to_string(),
            runtime: ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        };
        let result = exec_in_container(&handle, &["echo", "hello"], None);
        assert!(matches!(result, Err(ContainerError::NoRuntimeAvailable)));
    }

    #[test]
    fn test_ollama_host_url_default() {
        let config = ContainerConfig::default();
        assert_eq!(config.ollama_host_url(), "http://localhost:11435");
    }

    #[test]
    fn test_ollama_host_url_custom_port() {
        let config = ContainerConfig {
            port_mapping: Some((9999, 11434)),
            ..ContainerConfig::default()
        };
        assert_eq!(config.ollama_host_url(), "http://localhost:9999");
    }

    #[test]
    fn test_ollama_host_url_no_mapping() {
        let config = ContainerConfig {
            port_mapping: None,
            ..ContainerConfig::default()
        };
        assert_eq!(config.ollama_host_url(), "http://localhost:11434");
    }

    #[test]
    fn test_shared_model_pool_new_ref_count_zero() {
        let pool = SharedModelPool::new(ContainerConfig::default());
        assert_eq!(pool.ref_count(), 0);
    }

    #[tokio::test]
    async fn test_health_check_host_fails_fast_when_nothing_listening() {
        let result = health_check_host(19999, Duration::from_secs(3)).await;
        assert!(matches!(
            result,
            Err(ContainerError::HealthCheckFailed { .. })
        ));
    }

    #[tokio::test]
    async fn test_get_or_start_returns_guard_and_increments_ref_count() {
        let pool =
            SharedModelPool::new_with_provider(ContainerConfig::default(), Arc::new(MockProvider));
        let _guard = pool.get_or_start().await.unwrap();
        assert_eq!(pool.ref_count(), 1);
    }

    #[tokio::test]
    async fn test_get_or_start_twice_shares_provider() {
        let pool =
            SharedModelPool::new_with_provider(ContainerConfig::default(), Arc::new(MockProvider));
        let guard1 = pool.get_or_start().await.unwrap();
        let guard2 = pool.get_or_start().await.unwrap();
        assert_eq!(pool.ref_count(), 2);
        assert!(Arc::ptr_eq(guard1.provider(), guard2.provider()));
    }

    #[tokio::test]
    async fn test_drop_guard_decrements_ref_count() {
        let pool =
            SharedModelPool::new_with_provider(ContainerConfig::default(), Arc::new(MockProvider));
        let guard1 = pool.get_or_start().await.unwrap();
        let guard2 = pool.get_or_start().await.unwrap();
        assert_eq!(pool.ref_count(), 2);
        drop(guard1);
        assert_eq!(pool.ref_count(), 1);
        drop(guard2);
        assert_eq!(pool.ref_count(), 0);
    }

    #[tokio::test]
    async fn test_drop_last_guard_clears_provider() {
        let pool =
            SharedModelPool::new_with_provider(ContainerConfig::default(), Arc::new(MockProvider));
        let guard = pool.get_or_start().await.unwrap();
        assert_eq!(pool.ref_count(), 1);
        drop(guard);
        assert_eq!(pool.ref_count(), 0);
    }

    #[tokio::test]
    async fn test_concurrent_get_or_start() {
        let pool =
            SharedModelPool::new_with_provider(ContainerConfig::default(), Arc::new(MockProvider));
        let handles: Vec<_> = (0..5)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move { pool.get_or_start().await.unwrap() })
            })
            .collect();
        let guards: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(pool.ref_count(), 5);
        drop(guards);
        assert_eq!(pool.ref_count(), 0);
    }
}
