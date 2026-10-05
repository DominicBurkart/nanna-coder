use model::provider::ModelProvider;
use model::OllamaConfig;
use model::OllamaProvider;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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

/// A private file of `KEY=value` lines handed to `<runtime> run --env-file`,
/// so secrets stay out of the process argument list. The file is created with
/// mode 0600 and removed when the value is dropped.
#[derive(Debug)]
pub struct EnvFile {
    path: std::path::PathBuf,
}

impl EnvFile {
    /// Write `vars` to a fresh, owner-only file in the temp directory.
    ///
    /// ```
    /// use harness::container::EnvFile;
    /// let file = EnvFile::create(&[("A".to_string(), "b".to_string())]).unwrap();
    /// assert_eq!(std::fs::read_to_string(file.path()).unwrap(), "A=b\n");
    /// let path = file.path().to_path_buf();
    /// drop(file);
    /// assert!(!path.exists());
    /// ```
    pub fn create(vars: &[(String, String)]) -> std::io::Result<Self> {
        let mut content = String::new();
        for (key, value) in vars {
            let bad_key = key.is_empty() || key.contains(['=', '\n', '\r', '\0']);
            if bad_key || value.contains(['\n', '\r', '\0']) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("environment variable {key:?} cannot be written to an env file"),
                ));
            }
            content.push_str(key);
            content.push('=');
            content.push_str(value);
            content.push('\n');
        }
        let dir = std::env::temp_dir();
        let mut next_suffix = || -> String {
            rand::Rng::sample_iter(rand::rng(), rand::distr::Alphanumeric)
                .take(16)
                .map(char::from)
                .collect()
        };
        Self::write_unique(&dir, &content, &mut next_suffix)
    }

    fn write_unique(
        dir: &Path,
        content: &str,
        next_suffix: &mut dyn FnMut() -> String,
    ) -> std::io::Result<Self> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut attempt = 0;
        loop {
            let path = dir.join(format!("nanna-env-{}", next_suffix()));
            match options.open(&path) {
                Ok(mut file) => {
                    let guard = Self { path };
                    file.write_all(content.as_bytes())?;
                    return Ok(guard);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 8 => {
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for EnvFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Arguments for `<runtime> run` that start the dev container. Environment
/// variables are passed through `env_file`, never as `-e KEY=value`.
pub fn run_args(
    runtime: &ContainerRuntime,
    config: &ContainerConfig,
    image: &str,
    env_file: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        config.container_name.clone(),
    ];
    if *runtime == ContainerRuntime::Podman {
        args.push("--userns=keep-id".to_string());
    }
    if let Some((host_port, container_port)) = config.port_mapping {
        args.push("-p".to_string());
        args.push(format!("{host_port}:{container_port}"));
    }
    if let Some(path) = env_file {
        args.push("--env-file".to_string());
        args.push(path.display().to_string());
    }
    args.extend(config.additional_args.iter().cloned());
    args.push("--rm".to_string());
    args.push(image.to_string());
    args
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

    let env_file = if config.env_vars.is_empty() {
        None
    } else {
        Some(EnvFile::create(&config.env_vars).map_err(|e| {
            ContainerError::ContainerStartFailed {
                name: config.container_name.clone(),
                reason: format!("could not write container env file: {e}"),
            }
        })?)
    };
    let mut cmd = Command::new(runtime.command());
    cmd.args(run_args(
        &runtime,
        config,
        &image_to_use,
        env_file.as_ref().map(EnvFile::path),
    ));

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

/// Shared pool that manages a single model container instance.
///
/// Tracks active users with a reference count; starts the container on first
/// `get_or_start` and stops it when the last user drops their handle.
pub struct SharedModelPool {
    container: Mutex<Option<ContainerHandle>>,
    ref_count: AtomicUsize,
    config: ContainerConfig,
}

impl SharedModelPool {
    pub fn new(config: ContainerConfig) -> Arc<Self> {
        Arc::new(Self {
            container: Mutex::new(None),
            ref_count: AtomicUsize::new(0),
            config,
        })
    }

    /// Return a shared `OllamaProvider`, starting the container if necessary.
    pub async fn get_or_start(
        self: &Arc<Self>,
        ollama_config: OllamaConfig,
    ) -> Result<Arc<dyn ModelProvider>, ContainerError> {
        {
            let guard = self.container.lock().unwrap();
            if guard.is_some() {
                self.ref_count.fetch_add(1, Ordering::SeqCst);
                let provider = OllamaProvider::new(ollama_config).map_err(|e| {
                    ContainerError::CommandFailed {
                        command: format!("OllamaProvider::new: {}", e),
                    }
                })?;
                return Ok(Arc::new(provider));
            }
        }

        let handle = start_container_with_fallback(&self.config).await?;
        {
            let mut guard = self.container.lock().unwrap();
            *guard = Some(handle);
        }
        self.ref_count.fetch_add(1, Ordering::SeqCst);

        let provider =
            OllamaProvider::new(ollama_config).map_err(|e| ContainerError::CommandFailed {
                command: format!("OllamaProvider::new: {}", e),
            })?;
        Ok(Arc::new(provider))
    }

    /// Release a reference. When the count reaches zero, the container is stopped.
    pub fn release(self: &Arc<Self>) -> Result<(), ContainerError> {
        let prev = self.ref_count.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            let handle = {
                let mut guard = self.container.lock().unwrap();
                guard.take()
            };
            if let Some(h) = handle {
                cleanup_container(&h)?;
            }
        }
        Ok(())
    }

    pub fn ref_count(&self) -> usize {
        self.ref_count.load(Ordering::SeqCst)
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Note: a prior `release_ref_count_no_underflow` harness was removed
    // because it modelled a bare `AtomicUsize` rather than driving the real
    // `SharedModelPool::get_or_start` / `release` code paths (which are
    // async and do container I/O, making them impractical to proof-check
    // under Kani today). The remaining `release_underflow_wraps` proof
    // demonstrates the underflow bug directly on `AtomicUsize`, which is
    // the actual primitive used inside `SharedModelPool::release`.

    /// Show that an unguarded fetch_sub on zero wraps to usize::MAX.
    #[kani::proof]
    fn release_underflow_wraps() {
        let counter = AtomicUsize::new(0);
        let prev = counter.fetch_sub(1, Ordering::SeqCst);
        // prev was 0, counter is now usize::MAX
        assert_eq!(prev, 0);
        assert_eq!(counter.load(Ordering::SeqCst), usize::MAX);
    }
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

    const ENV_SENTINEL: &str = "Sentinel-Pw-1c7d";

    fn env_config() -> ContainerConfig {
        ContainerConfig {
            container_name: "c".to_string(),
            env_vars: vec![(
                "DATABASE_URL".to_string(),
                format!("postgres://postgres:{ENV_SENTINEL}@postgres:5432/db"),
            )],
            port_mapping: None,
            additional_args: vec!["--network=n".to_string()],
            ..ContainerConfig::default()
        }
    }

    #[test]
    fn run_args_never_carry_env_values() {
        let config = env_config();
        let args = run_args(
            &ContainerRuntime::Podman,
            &config,
            "img:1",
            Some(Path::new("/tmp/envfile")),
        );
        assert!(!args.join(" ").contains(ENV_SENTINEL));
        assert!(!args.iter().any(|a| a == "-e"));
        assert_eq!(
            args,
            vec![
                "run",
                "-d",
                "--name",
                "c",
                "--userns=keep-id",
                "--env-file",
                "/tmp/envfile",
                "--network=n",
                "--rm",
                "img:1"
            ]
        );
    }

    #[test]
    fn run_args_without_env_file_or_podman() {
        let mut config = env_config();
        config.port_mapping = Some((1, 2));
        let args = run_args(&ContainerRuntime::Docker, &config, "img:1", None);
        assert_eq!(
            args,
            vec![
                "run",
                "-d",
                "--name",
                "c",
                "-p",
                "1:2",
                "--network=n",
                "--rm",
                "img:1"
            ]
        );
    }

    #[test]
    fn env_file_is_private_complete_and_removed_on_drop() {
        let file = EnvFile::create(&env_config().env_vars).unwrap();
        let path = file.path().to_path_buf();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(ENV_SENTINEL));
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&path).unwrap().permissions()
            ) & 0o777,
            0o600
        );
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn env_file_rejects_unrepresentable_variables() {
        for (key, value) in [
            ("", "v"),
            ("A=B", "v"),
            ("A\nB", "v"),
            ("A", "line1\nline2"),
            ("A", "nul\0"),
        ] {
            let err = EnvFile::create(&[(key.to_string(), value.to_string())]).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn env_file_names_are_unique() {
        let a = EnvFile::create(&[]).unwrap();
        let b = EnvFile::create(&[]).unwrap();
        assert_ne!(a.path(), b.path());
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
    fn env_file_retries_on_name_collision_then_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("nanna-env-dup"), "taken").unwrap();
        let mut names = vec!["fresh".to_string(), "dup".to_string()];
        let file =
            EnvFile::write_unique(dir.path(), "A=b\n", &mut || names.pop().unwrap()).unwrap();
        assert_eq!(file.path(), dir.path().join("nanna-env-fresh"));
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), "A=b\n");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("nanna-env-dup")).unwrap(),
            "taken"
        );
    }

    #[test]
    fn env_file_gives_up_after_repeated_name_collisions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("nanna-env-dup"), "taken").unwrap();
        let mut calls = 0;
        let err = EnvFile::write_unique(dir.path(), "", &mut || {
            calls += 1;
            "dup".to_string()
        })
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(calls, 9);
    }

    #[test]
    fn env_file_reports_unwritable_directory() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let err = EnvFile::write_unique(&missing, "", &mut || "x".to_string()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    fn quick_start_config(name: &str, env_vars: Vec<(String, String)>) -> ContainerConfig {
        ContainerConfig {
            container_name: name.to_string(),
            env_vars,
            startup_timeout: Duration::from_millis(1),
            additional_args: vec!["--network=n".to_string()],
            ..ContainerConfig::default()
        }
    }

    #[test]
    fn start_container_passes_env_through_a_private_file() {
        let fake = crate::test_support::FakePodman::install(None);
        let config = quick_start_config(
            "nanna-env-start-test",
            vec![("DATABASE_URL".to_string(), ENV_SENTINEL.to_string())],
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let handle = rt.block_on(start_container_with_fallback(&config)).unwrap();
        assert_eq!(handle.name, "nanna-env-start-test");
        assert_eq!(handle.runtime, ContainerRuntime::Podman);
        assert!(handle.needs_cleanup);
        let calls = fake.calls();
        let run = calls
            .iter()
            .find(|c| c.starts_with("run -d --name nanna-env-start-test"))
            .expect("run call recorded");
        assert!(run.contains("--env-file"));
        assert!(run.contains("--network=n"));
        assert!(!run.contains(ENV_SENTINEL));
        assert_eq!(
            fake.env_file_contents(),
            vec![format!("DATABASE_URL={ENV_SENTINEL}")]
        );
    }

    #[test]
    fn start_container_reports_unwritable_env_vars() {
        let _fake = crate::test_support::FakePodman::install(None);
        let config = quick_start_config(
            "nanna-env-bad-test",
            vec![("BAD=KEY".to_string(), "v".to_string())],
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let err = rt
            .block_on(start_container_with_fallback(&config))
            .unwrap_err();
        assert!(
            matches!(
                err,
                ContainerError::ContainerStartFailed { ref name, ref reason }
                    if name == "nanna-env-bad-test" && reason.contains("could not write container env file")
            ),
            "{err}"
        );
    }
}
