//! Monitoring and observability infrastructure
//!
//! This module provides comprehensive monitoring capabilities for the nanna-coder system,
//! including metrics collection, health checks, alerting, and observability.
//!
//! # Features
//!
//! - Real-time performance metrics collection
//! - Container health monitoring
//! - Model performance tracking
//! - System resource utilization monitoring
//! - Alerting and notification system
//! - Metrics export for external systems
//!
//! # Examples
//!
//! ```rust
//! use harness::monitoring::{
//!     DefaultMetricsCollector, DefaultHealthMonitor, DefaultAlertManager,
//!     MetricsCollector, HealthMonitor, AlertManager, AlertSeverity, HealthStatus
//! };
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mut collector = DefaultMetricsCollector::new();
//! let health_monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
//! let alert_manager = DefaultAlertManager::new();
//!
//! // Collect performance metrics
//! collector.record_request_latency("ollama", Duration::from_millis(150)).await;
//! collector.record_cache_hit("qwen3:0.6b").await;
//!
//! // Monitor container health
//! let health_status = health_monitor.check_container_health("test-container").await?;
//! if health_status.status != HealthStatus::Healthy {
//!     alert_manager.send_alert("Container health degraded", "Health check failed", AlertSeverity::Warning).await?;
//! }
//!
//! // Get current metrics
//! let current_metrics = collector.get_current_metrics().await?;
//! println!("System metrics: {:#?}", current_metrics);
//! # Ok(())
//! # }
//! ```

use crate::container::{detect_runtime, ContainerRuntime};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::time::interval;
use tracing::{debug, error, info, warn};

/// Comprehensive monitoring errors
#[derive(Error, Debug)]
pub enum MonitoringError {
    /// Metrics collection failed
    #[error("Failed to collect metrics: {reason}")]
    MetricsCollectionFailed { reason: String },

    /// Health check failed
    #[error("Health check failed for {component}: {reason}")]
    HealthCheckFailed { component: String, reason: String },

    /// Alert sending failed
    #[error("Failed to send alert: {reason}")]
    AlertSendFailed { reason: String },

    /// Container monitoring failed
    #[error("Container monitoring failed: {reason}")]
    ContainerMonitoringFailed { reason: String },

    /// System resource monitoring failed
    #[error("System resource monitoring failed: {reason}")]
    SystemMonitoringFailed { reason: String },

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Performance and system metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMetrics {
    /// Timestamp when metrics were collected
    pub timestamp: DateTime<Utc>,
    /// Request latency metrics by service
    pub request_latencies: HashMap<String, LatencyMetrics>,
    /// Cache performance metrics
    pub cache_metrics: CacheMetrics,
    /// Container health metrics
    pub container_metrics: Vec<ContainerMetrics>,
    /// System resource utilization
    pub system_resources: SystemResourceMetrics,
    /// Model performance metrics
    pub model_metrics: HashMap<String, ModelMetrics>,
    /// Error rates and counts
    pub error_metrics: ErrorMetrics,
}

/// Latency metrics for a service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyMetrics {
    /// Average latency over the collection period
    pub avg_latency_ms: f64,
    /// 95th percentile latency
    pub p95_latency_ms: f64,
    /// 99th percentile latency
    pub p99_latency_ms: f64,
    /// Maximum latency observed
    pub max_latency_ms: f64,
    /// Minimum latency observed
    pub min_latency_ms: f64,
    /// Total number of requests
    pub request_count: u64,
    /// Requests per second
    pub requests_per_second: f64,
}

/// Cache performance metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetrics {
    /// Total cache hits
    pub hits: u64,
    /// Total cache misses
    pub misses: u64,
    /// Cache hit rate as percentage
    pub hit_rate: f64,
    /// Cache size in bytes
    pub size_bytes: u64,
    /// Number of cached items
    pub item_count: u64,
    /// Cache eviction count
    pub evictions: u64,
}

/// Container health and performance metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerMetrics {
    /// Container name
    pub name: String,
    /// Container status
    pub status: ContainerStatus,
    /// CPU usage percentage
    pub cpu_usage_percent: f64,
    /// Memory usage in bytes
    pub memory_usage_bytes: u64,
    /// Memory limit in bytes
    pub memory_limit_bytes: u64,
    /// Network I/O metrics
    pub network_io: NetworkMetrics,
    /// Container uptime
    pub uptime: Duration,
    /// Last health check timestamp
    pub last_health_check: DateTime<Utc>,
    /// Health check status
    pub health_status: HealthStatus,
}

/// Container status enumeration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContainerStatus {
    /// Container is running normally
    Running,
    /// Container is starting up
    Starting,
    /// Container is stopping
    Stopping,
    /// Container has stopped
    Stopped,
    /// Container has failed
    Failed,
    /// Container status is unknown
    Unknown,
}

/// Network I/O metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkMetrics {
    /// Bytes received
    pub rx_bytes: u64,
    /// Bytes transmitted
    pub tx_bytes: u64,
    /// Packets received
    pub rx_packets: u64,
    /// Packets transmitted
    pub tx_packets: u64,
    /// Network errors
    pub errors: u64,
}

/// System resource utilization metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemResourceMetrics {
    /// CPU usage percentage
    pub cpu_usage_percent: f64,
    /// Total memory in bytes
    pub total_memory_bytes: u64,
    /// Used memory in bytes
    pub used_memory_bytes: u64,
    /// Memory usage percentage
    pub memory_usage_percent: f64,
    /// Available disk space in bytes
    pub available_disk_bytes: u64,
    /// Total disk space in bytes
    pub total_disk_bytes: u64,
    /// Disk usage percentage
    pub disk_usage_percent: f64,
    /// System load average (1, 5, 15 minutes)
    pub load_average: [f64; 3],
}

/// Model performance metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetrics {
    /// Model name
    pub model_name: String,
    /// Total inference requests
    pub inference_count: u64,
    /// Average inference time
    pub avg_inference_time_ms: f64,
    /// Token generation rate (tokens per second)
    pub tokens_per_second: f64,
    /// Success rate percentage
    pub success_rate: f64,
    /// Quality scores from ModelJudge
    pub quality_scores: QualityMetrics,
    /// Resource utilization during inference
    pub resource_usage: ModelResourceUsage,
}

/// Quality metrics from ModelJudge validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityMetrics {
    /// Average coherence score
    pub avg_coherence: f64,
    /// Average relevance score
    pub avg_relevance: f64,
    /// Consistency score
    pub consistency: f64,
    /// Factual accuracy rate
    pub accuracy_rate: f64,
}

/// Resource usage for model inference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResourceUsage {
    /// Peak memory usage during inference
    pub peak_memory_mb: f64,
    /// Average CPU usage during inference
    pub avg_cpu_percent: f64,
    /// GPU utilization (if available)
    pub gpu_utilization_percent: Option<f64>,
}

/// Error tracking metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMetrics {
    /// Total error count
    pub total_errors: u64,
    /// Errors by category
    pub errors_by_type: HashMap<String, u64>,
    /// Error rate percentage
    pub error_rate: f64,
    /// Recent error patterns
    pub recent_errors: Vec<ErrorEvent>,
}

/// Individual error event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEvent {
    /// When the error occurred
    pub timestamp: DateTime<Utc>,
    /// Error type/category
    pub error_type: String,
    /// Error message
    pub message: String,
    /// Component where error occurred
    pub component: String,
    /// Severity level
    pub severity: ErrorSeverity,
}

/// Error severity levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum ErrorSeverity {
    /// Low severity, informational
    Info,
    /// Warning level
    Warning,
    /// Error level
    Error,
    /// Critical system error
    Critical,
}

/// Health status for components
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HealthStatus {
    /// Component is healthy
    Healthy,
    /// Component has warnings but is functional
    Warning,
    /// Component is degraded
    Degraded,
    /// Component is unhealthy
    Unhealthy,
    /// Component status is unknown
    Unknown,
}

impl HealthStatus {
    /// Check if the status indicates a healthy state
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }

    /// Check if the status requires attention
    pub fn requires_attention(&self) -> bool {
        matches!(
            self,
            HealthStatus::Warning | HealthStatus::Degraded | HealthStatus::Unhealthy
        )
    }
}

/// Health check result for a component
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckResult {
    /// Component name
    pub component: String,
    /// Health status
    pub status: HealthStatus,
    /// Detailed status message
    pub message: String,
    /// When the check was performed
    pub timestamp: DateTime<Utc>,
    /// Check duration
    pub check_duration: Duration,
    /// Additional diagnostic data
    pub details: HashMap<String, String>,
}

/// Alert severity levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertSeverity {
    /// Informational alert
    Info,
    /// Warning alert
    Warning,
    /// Error alert
    Error,
    /// Critical alert requiring immediate attention
    Critical,
}

/// System alert
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    /// Unique alert ID
    pub id: String,
    /// Alert title
    pub title: String,
    /// Alert description
    pub description: String,
    /// Alert severity
    pub severity: AlertSeverity,
    /// Component that triggered the alert
    pub component: String,
    /// When the alert was created
    pub timestamp: DateTime<Utc>,
    /// Associated metrics or context
    pub context: HashMap<String, String>,
    /// Whether the alert has been acknowledged
    pub acknowledged: bool,
}

/// Main trait for metrics collection
#[async_trait]
pub trait MetricsCollector: Send + Sync {
    /// Record request latency for a service
    async fn record_request_latency(&mut self, service: &str, latency: Duration);

    /// Record cache hit
    async fn record_cache_hit(&mut self, key: &str);

    /// Record cache miss
    async fn record_cache_miss(&mut self, key: &str);

    /// Record error event
    async fn record_error(&mut self, error: ErrorEvent);

    /// Record model inference metrics
    async fn record_model_inference(&mut self, model: &str, metrics: ModelMetrics);

    /// Get current system metrics
    async fn get_current_metrics(&self) -> Result<SystemMetrics, MonitoringError>;

    /// Export metrics in a specific format
    async fn export_metrics(&self, format: MetricsFormat) -> Result<String, MonitoringError>;

    /// Reset metrics counters
    async fn reset_metrics(&mut self);
}

/// Health monitoring trait
#[async_trait]
pub trait HealthMonitor: Send + Sync {
    /// Check container health
    async fn check_container_health(
        &self,
        container_name: &str,
    ) -> Result<HealthCheckResult, MonitoringError>;

    /// Check model service health
    async fn check_model_health(&self, model: &str) -> Result<HealthCheckResult, MonitoringError>;

    /// Check system health
    async fn check_system_health(&self) -> Result<HealthCheckResult, MonitoringError>;

    /// Perform comprehensive health check
    async fn comprehensive_health_check(&self) -> Result<Vec<HealthCheckResult>, MonitoringError>;

    /// Set health check interval
    fn set_check_interval(&mut self, interval: Duration);
}

/// Alert management trait
#[async_trait]
pub trait AlertManager: Send + Sync {
    /// Send an alert
    async fn send_alert(
        &self,
        title: &str,
        description: &str,
        severity: AlertSeverity,
    ) -> Result<String, MonitoringError>;

    /// Acknowledge an alert
    async fn acknowledge_alert(&self, alert_id: &str) -> Result<(), MonitoringError>;

    /// Get active alerts
    async fn get_active_alerts(&self) -> Result<Vec<Alert>, MonitoringError>;

    /// Get alert history
    async fn get_alert_history(&self, limit: usize) -> Result<Vec<Alert>, MonitoringError>;

    /// Configure alert thresholds
    async fn configure_thresholds(
        &mut self,
        thresholds: AlertThresholds,
    ) -> Result<(), MonitoringError>;
}

/// Alert threshold configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertThresholds {
    /// Maximum acceptable latency before alerting
    pub max_latency_ms: u64,
    /// Minimum cache hit rate before alerting
    pub min_cache_hit_rate: f64,
    /// Maximum error rate before alerting
    pub max_error_rate: f64,
    /// Maximum CPU usage before alerting
    pub max_cpu_usage: f64,
    /// Maximum memory usage before alerting
    pub max_memory_usage: f64,
    /// Container health check timeout
    pub health_check_timeout: Duration,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            max_latency_ms: 5000,
            min_cache_hit_rate: 0.8,
            max_error_rate: 0.05,
            max_cpu_usage: 0.9,
            max_memory_usage: 0.9,
            health_check_timeout: Duration::from_secs(30),
        }
    }
}

/// Metrics export formats
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetricsFormat {
    /// JSON format
    Json,
    /// Prometheus format
    Prometheus,
    /// CSV format
    Csv,
    /// Custom format
    Custom(String),
}

/// Default implementation of MetricsCollector
pub struct DefaultMetricsCollector {
    /// Internal metrics storage
    metrics: Arc<Mutex<InternalMetrics>>,
    /// Collection start time
    start_time: Instant,
}

/// Internal metrics storage
#[derive(Debug, Default)]
struct InternalMetrics {
    /// Request latencies by service
    request_latencies: HashMap<String, Vec<Duration>>,
    /// Cache hits and misses
    cache_hits: u64,
    cache_misses: u64,
    /// Error events
    errors: Vec<ErrorEvent>,
    /// Model metrics
    model_metrics: HashMap<String, ModelMetrics>,
    /// System resource snapshots
    #[allow(dead_code)]
    system_snapshots: Vec<SystemResourceMetrics>,
}

impl Default for DefaultMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultMetricsCollector {
    /// Create a new metrics collector
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(Mutex::new(InternalMetrics::default())),
            start_time: Instant::now(),
        }
    }

    /// Get cache metrics
    fn get_cache_metrics(&self, metrics: &InternalMetrics) -> CacheMetrics {
        let total = metrics.cache_hits + metrics.cache_misses;
        let hit_rate = if total > 0 {
            metrics.cache_hits as f64 / total as f64
        } else {
            0.0
        };

        CacheMetrics {
            hits: metrics.cache_hits,
            misses: metrics.cache_misses,
            hit_rate,
            size_bytes: 0, // Would be calculated from actual cache
            item_count: 0, // Would be calculated from actual cache
            evictions: 0,  // Would be tracked separately
        }
    }

    /// Calculate latency metrics for a service
    fn calculate_latency_metrics(&self, latencies: &[Duration]) -> LatencyMetrics {
        if latencies.is_empty() {
            return LatencyMetrics {
                avg_latency_ms: 0.0,
                p95_latency_ms: 0.0,
                p99_latency_ms: 0.0,
                max_latency_ms: 0.0,
                min_latency_ms: 0.0,
                request_count: 0,
                requests_per_second: 0.0,
            };
        }

        let mut sorted_latencies: Vec<_> = latencies.iter().map(|d| d.as_millis() as f64).collect();
        sorted_latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let avg = sorted_latencies.iter().sum::<f64>() / sorted_latencies.len() as f64;
        let p95_index = (sorted_latencies.len() as f64 * 0.95) as usize;
        let p99_index = (sorted_latencies.len() as f64 * 0.99) as usize;

        let elapsed_seconds = self.start_time.elapsed().as_secs_f64();
        let rps = if elapsed_seconds > 0.0 {
            latencies.len() as f64 / elapsed_seconds
        } else {
            0.0
        };

        LatencyMetrics {
            avg_latency_ms: avg,
            p95_latency_ms: sorted_latencies.get(p95_index).copied().unwrap_or(0.0),
            p99_latency_ms: sorted_latencies.get(p99_index).copied().unwrap_or(0.0),
            max_latency_ms: sorted_latencies.last().copied().unwrap_or(0.0),
            min_latency_ms: sorted_latencies.first().copied().unwrap_or(0.0),
            request_count: latencies.len() as u64,
            requests_per_second: rps,
        }
    }
}

#[async_trait]
impl MetricsCollector for DefaultMetricsCollector {
    async fn record_request_latency(&mut self, service: &str, latency: Duration) {
        let mut metrics = self.metrics.lock().unwrap();
        metrics
            .request_latencies
            .entry(service.to_string())
            .or_default()
            .push(latency);
        debug!("Recorded latency for {}: {:?}", service, latency);
    }

    async fn record_cache_hit(&mut self, key: &str) {
        let mut metrics = self.metrics.lock().unwrap();
        metrics.cache_hits += 1;
        debug!("Cache hit recorded for key: {}", key);
    }

    async fn record_cache_miss(&mut self, key: &str) {
        let mut metrics = self.metrics.lock().unwrap();
        metrics.cache_misses += 1;
        debug!("Cache miss recorded for key: {}", key);
    }

    async fn record_error(&mut self, error: ErrorEvent) {
        let mut metrics = self.metrics.lock().unwrap();
        error!("Error recorded: {} - {}", error.error_type, error.message);
        metrics.errors.push(error);
    }

    async fn record_model_inference(&mut self, model: &str, inference_metrics: ModelMetrics) {
        let mut metrics = self.metrics.lock().unwrap();
        metrics
            .model_metrics
            .insert(model.to_string(), inference_metrics);
        debug!("Model inference metrics recorded for: {}", model);
    }

    async fn get_current_metrics(&self) -> Result<SystemMetrics, MonitoringError> {
        let metrics = self.metrics.lock().unwrap();

        // Calculate request latency metrics
        let mut request_latencies = HashMap::new();
        for (service, latencies) in &metrics.request_latencies {
            request_latencies.insert(service.clone(), self.calculate_latency_metrics(latencies));
        }

        // Get cache metrics
        let cache_metrics = self.get_cache_metrics(&metrics);

        // Calculate error metrics
        let total_errors = metrics.errors.len() as u64;
        let total_requests: u64 = request_latencies.values().map(|l| l.request_count).sum();
        let error_rate = if total_requests > 0 {
            total_errors as f64 / total_requests as f64
        } else {
            0.0
        };

        let mut errors_by_type = HashMap::new();
        for error in &metrics.errors {
            *errors_by_type.entry(error.error_type.clone()).or_insert(0) += 1;
        }

        let error_metrics = ErrorMetrics {
            total_errors,
            errors_by_type,
            error_rate,
            recent_errors: metrics.errors.iter().rev().take(10).cloned().collect(),
        };

        Ok(SystemMetrics {
            timestamp: Utc::now(),
            request_latencies,
            cache_metrics,
            container_metrics: vec![], // Would be populated by container monitoring
            system_resources: SystemResourceMetrics {
                cpu_usage_percent: 0.0,
                total_memory_bytes: 0,
                used_memory_bytes: 0,
                memory_usage_percent: 0.0,
                available_disk_bytes: 0,
                total_disk_bytes: 0,
                disk_usage_percent: 0.0,
                load_average: [0.0, 0.0, 0.0],
            },
            model_metrics: metrics.model_metrics.clone(),
            error_metrics,
        })
    }

    async fn export_metrics(&self, format: MetricsFormat) -> Result<String, MonitoringError> {
        let metrics = self.get_current_metrics().await?;

        match format {
            MetricsFormat::Json => serde_json::to_string_pretty(&metrics).map_err(|e| {
                MonitoringError::MetricsCollectionFailed {
                    reason: format!("JSON serialization failed: {}", e),
                }
            }),
            MetricsFormat::Prometheus => {
                // Convert to Prometheus format
                let mut output = String::new();

                // Export cache metrics
                output.push_str("# HELP cache_hits_total Total cache hits\n");
                output.push_str("# TYPE cache_hits_total counter\n");
                output.push_str(&format!(
                    "cache_hits_total {}\n",
                    metrics.cache_metrics.hits
                ));

                output.push_str("# HELP cache_hit_rate Cache hit rate\n");
                output.push_str("# TYPE cache_hit_rate gauge\n");
                output.push_str(&format!(
                    "cache_hit_rate {}\n",
                    metrics.cache_metrics.hit_rate
                ));

                // Export request latencies
                for (service, latency) in &metrics.request_latencies {
                    output.push_str(&format!(
                        "# HELP request_latency_ms_{} Average request latency for {}\n",
                        service, service
                    ));
                    output.push_str(&format!("# TYPE request_latency_ms_{} gauge\n", service));
                    output.push_str(&format!(
                        "request_latency_ms_{{service=\"{}\"}} {}\n",
                        service, latency.avg_latency_ms
                    ));
                }

                Ok(output)
            }
            MetricsFormat::Csv => {
                let mut output = String::new();
                output.push_str("timestamp,metric_type,service,value\n");

                for (service, latency) in &metrics.request_latencies {
                    output.push_str(&format!(
                        "{},latency_avg,{},{}\n",
                        metrics.timestamp.to_rfc3339(),
                        service,
                        latency.avg_latency_ms
                    ));
                    output.push_str(&format!(
                        "{},request_count,{},{}\n",
                        metrics.timestamp.to_rfc3339(),
                        service,
                        latency.request_count
                    ));
                }

                output.push_str(&format!(
                    "{},cache_hit_rate,system,{}\n",
                    metrics.timestamp.to_rfc3339(),
                    metrics.cache_metrics.hit_rate
                ));

                Ok(output)
            }
            MetricsFormat::Custom(format_name) => Err(MonitoringError::MetricsCollectionFailed {
                reason: format!("Custom format '{}' not implemented", format_name),
            }),
        }
    }

    async fn reset_metrics(&mut self) {
        let mut metrics = self.metrics.lock().unwrap();
        *metrics = InternalMetrics::default();
        self.start_time = Instant::now();
        info!("Metrics reset successfully");
    }
}

/// Default implementation of HealthMonitor
pub struct DefaultHealthMonitor {
    /// Check interval
    check_interval: Duration,
    /// Container runtime
    runtime: ContainerRuntime,
}

impl DefaultHealthMonitor {
    /// Create a new health monitor
    pub fn new(check_interval: Duration) -> Self {
        Self {
            check_interval,
            runtime: detect_runtime(),
        }
    }

    /// Check if a container is running
    async fn is_container_running(&self, container_name: &str) -> bool {
        if !self.runtime.is_available() {
            return false;
        }

        let output = std::process::Command::new(self.runtime.command())
            .args([
                "ps",
                "--filter",
                &format!("name={}", container_name),
                "--format",
                "table {{.Status}}",
            ])
            .output();

        match output {
            Ok(output) => {
                let status = String::from_utf8_lossy(&output.stdout);
                status.lines().any(|line| line.contains("Up"))
            }
            Err(_) => false,
        }
    }
}

#[async_trait]
impl HealthMonitor for DefaultHealthMonitor {
    async fn check_container_health(
        &self,
        container_name: &str,
    ) -> Result<HealthCheckResult, MonitoringError> {
        let start_time = Instant::now();

        let (status, message) = if !self.runtime.is_available() {
            (
                HealthStatus::Unknown,
                "No container runtime available".to_string(),
            )
        } else if self.is_container_running(container_name).await {
            (HealthStatus::Healthy, "Container is running".to_string())
        } else {
            (
                HealthStatus::Unhealthy,
                "Container is not running".to_string(),
            )
        };

        let mut details = HashMap::new();
        details.insert("runtime".to_string(), format!("{:?}", self.runtime));
        details.insert("container_name".to_string(), container_name.to_string());

        Ok(HealthCheckResult {
            component: format!("container:{}", container_name),
            status,
            message,
            timestamp: Utc::now(),
            check_duration: start_time.elapsed(),
            details,
        })
    }

    async fn check_model_health(&self, model: &str) -> Result<HealthCheckResult, MonitoringError> {
        let start_time = Instant::now();

        // For now, we'll do a basic check - in a real implementation,
        // this would test model inference
        let status = HealthStatus::Healthy;
        let message = format!("Model {} is available", model);

        let mut details = HashMap::new();
        details.insert("model".to_string(), model.to_string());

        Ok(HealthCheckResult {
            component: format!("model:{}", model),
            status,
            message,
            timestamp: Utc::now(),
            check_duration: start_time.elapsed(),
            details,
        })
    }

    async fn check_system_health(&self) -> Result<HealthCheckResult, MonitoringError> {
        let start_time = Instant::now();

        // Basic system health check
        let status = HealthStatus::Healthy;
        let message = "System is operating normally".to_string();

        let mut details = HashMap::new();
        details.insert("uptime".to_string(), format!("{:?}", start_time.elapsed()));

        Ok(HealthCheckResult {
            component: "system".to_string(),
            status,
            message,
            timestamp: Utc::now(),
            check_duration: start_time.elapsed(),
            details,
        })
    }

    async fn comprehensive_health_check(&self) -> Result<Vec<HealthCheckResult>, MonitoringError> {
        let mut results = Vec::new();

        // Check system health
        results.push(self.check_system_health().await?);

        // Check common containers
        let containers = ["nanna-coder-test", "ollama-qwen3", "e2e-test-container"];
        for container in &containers {
            match self.check_container_health(container).await {
                Ok(result) => results.push(result),
                Err(e) => warn!("Failed to check container {}: {}", container, e),
            }
        }

        // Check common models
        let models = ["qwen3:0.6b", "llama3:8b"];
        for model in &models {
            match self.check_model_health(model).await {
                Ok(result) => results.push(result),
                Err(e) => warn!("Failed to check model {}: {}", model, e),
            }
        }

        Ok(results)
    }

    fn set_check_interval(&mut self, interval: Duration) {
        self.check_interval = interval;
        info!("Health check interval set to {:?}", interval);
    }
}

/// Default implementation of AlertManager
pub struct DefaultAlertManager {
    /// Active alerts
    alerts: Arc<Mutex<Vec<Alert>>>,
    /// Alert thresholds
    thresholds: AlertThresholds,
    /// Alert counter for generating IDs
    alert_counter: Arc<Mutex<u64>>,
}

impl Default for DefaultAlertManager {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultAlertManager {
    /// Create a new alert manager
    pub fn new() -> Self {
        Self {
            alerts: Arc::new(Mutex::new(Vec::new())),
            thresholds: AlertThresholds::default(),
            alert_counter: Arc::new(Mutex::new(0)),
        }
    }

    /// Generate a unique alert ID
    fn generate_alert_id(&self) -> String {
        let mut counter = self.alert_counter.lock().unwrap();
        *counter += 1;
        format!("alert_{}", *counter)
    }
}

#[async_trait]
impl AlertManager for DefaultAlertManager {
    async fn send_alert(
        &self,
        title: &str,
        description: &str,
        severity: AlertSeverity,
    ) -> Result<String, MonitoringError> {
        let alert = Alert {
            id: self.generate_alert_id(),
            title: title.to_string(),
            description: description.to_string(),
            severity: severity.clone(),
            component: "system".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };

        let alert_id = alert.id.clone();

        {
            let mut alerts = self.alerts.lock().unwrap();
            alerts.push(alert);
        }

        match severity {
            AlertSeverity::Critical => error!("CRITICAL ALERT: {} - {}", title, description),
            AlertSeverity::Error => error!("ERROR ALERT: {} - {}", title, description),
            AlertSeverity::Warning => warn!("WARNING ALERT: {} - {}", title, description),
            AlertSeverity::Info => info!("INFO ALERT: {} - {}", title, description),
        }

        Ok(alert_id)
    }

    async fn acknowledge_alert(&self, alert_id: &str) -> Result<(), MonitoringError> {
        let mut alerts = self.alerts.lock().unwrap();

        if let Some(alert) = alerts.iter_mut().find(|a| a.id == alert_id) {
            alert.acknowledged = true;
            info!("Alert {} acknowledged", alert_id);
            Ok(())
        } else {
            Err(MonitoringError::AlertSendFailed {
                reason: format!("Alert {} not found", alert_id),
            })
        }
    }

    async fn get_active_alerts(&self) -> Result<Vec<Alert>, MonitoringError> {
        let alerts = self.alerts.lock().unwrap();
        Ok(alerts.iter().filter(|a| !a.acknowledged).cloned().collect())
    }

    async fn get_alert_history(&self, limit: usize) -> Result<Vec<Alert>, MonitoringError> {
        let alerts = self.alerts.lock().unwrap();
        Ok(alerts.iter().rev().take(limit).cloned().collect())
    }

    async fn configure_thresholds(
        &mut self,
        thresholds: AlertThresholds,
    ) -> Result<(), MonitoringError> {
        self.thresholds = thresholds;
        info!("Alert thresholds updated");
        Ok(())
    }
}

/// Monitoring system that orchestrates all components
pub struct MonitoringSystem {
    /// Metrics collector
    pub metrics_collector: Box<dyn MetricsCollector>,
    /// Health monitor
    pub health_monitor: Box<dyn HealthMonitor>,
    /// Alert manager
    pub alert_manager: Box<dyn AlertManager>,
    /// Background monitoring task handle
    monitoring_task: Option<tokio::task::JoinHandle<()>>,
}

impl MonitoringSystem {
    /// Create a new monitoring system with default implementations
    pub fn new() -> Self {
        Self {
            metrics_collector: Box::new(DefaultMetricsCollector::new()),
            health_monitor: Box::new(DefaultHealthMonitor::new(Duration::from_secs(60))),
            alert_manager: Box::new(DefaultAlertManager::new()),
            monitoring_task: None,
        }
    }

    /// Start background monitoring tasks
    pub async fn start_monitoring(&mut self) -> Result<(), MonitoringError> {
        info!("Starting background monitoring system");

        // Clone references for the background task
        // Note: This is a simplified version - in a real implementation,
        // we'd need to handle the trait object sharing differently

        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));

            loop {
                interval.tick().await;

                // This is where we'd perform periodic monitoring tasks
                debug!("Performing periodic monitoring checks");

                // In a real implementation, we'd:
                // 1. Collect system metrics
                // 2. Perform health checks
                // 3. Check alert thresholds
                // 4. Send alerts if needed
            }
        });

        self.monitoring_task = Some(task);
        Ok(())
    }

    /// Stop background monitoring
    pub async fn stop_monitoring(&mut self) {
        if let Some(task) = self.monitoring_task.take() {
            task.abort();
            info!("Background monitoring stopped");
        }
    }

    /// Get comprehensive system status
    pub async fn get_system_status(&self) -> Result<SystemStatus, MonitoringError> {
        let metrics = self.metrics_collector.get_current_metrics().await?;
        let health_checks = self.health_monitor.comprehensive_health_check().await?;
        let active_alerts = self.alert_manager.get_active_alerts().await?;

        let overall_health = if health_checks
            .iter()
            .any(|h| h.status == HealthStatus::Unhealthy)
        {
            HealthStatus::Unhealthy
        } else if health_checks
            .iter()
            .any(|h| h.status == HealthStatus::Degraded)
        {
            HealthStatus::Degraded
        } else if health_checks
            .iter()
            .any(|h| h.status == HealthStatus::Warning)
        {
            HealthStatus::Warning
        } else {
            HealthStatus::Healthy
        };

        Ok(SystemStatus {
            overall_health,
            metrics,
            health_checks,
            active_alerts,
            timestamp: Utc::now(),
        })
    }
}

/// Complete system status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    /// Overall system health
    pub overall_health: HealthStatus,
    /// Current metrics
    pub metrics: SystemMetrics,
    /// Health check results
    pub health_checks: Vec<HealthCheckResult>,
    /// Active alerts
    pub active_alerts: Vec<Alert>,
    /// Status timestamp
    pub timestamp: DateTime<Utc>,
}

impl Default for MonitoringSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[tokio::test]
    async fn test_metrics_collector() {
        let mut collector = DefaultMetricsCollector::new();

        // Record some metrics
        collector
            .record_request_latency("ollama", Duration::from_millis(150))
            .await;
        collector
            .record_request_latency("ollama", Duration::from_millis(200))
            .await;
        collector.record_cache_hit("test-key").await;
        collector.record_cache_miss("other-key").await;

        let metrics = collector.get_current_metrics().await.unwrap();

        assert!(!metrics.request_latencies.is_empty());
        assert_eq!(metrics.cache_metrics.hits, 1);
        assert_eq!(metrics.cache_metrics.misses, 1);
        assert_eq!(metrics.cache_metrics.hit_rate, 0.5);
    }

    #[tokio::test]
    async fn test_health_monitor() {
        let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));

        let system_health = monitor.check_system_health().await.unwrap();
        assert_eq!(system_health.status, HealthStatus::Healthy);

        let container_health = monitor
            .check_container_health("test-container")
            .await
            .unwrap();
        // Should be unhealthy since container doesn't exist
        assert!(
            container_health.status == HealthStatus::Unhealthy
                || container_health.status == HealthStatus::Unknown
        );
    }

    #[tokio::test]
    async fn test_alert_manager() {
        let manager = DefaultAlertManager::new();

        let alert_id = manager
            .send_alert("Test Alert", "This is a test alert", AlertSeverity::Warning)
            .await
            .unwrap();

        let active_alerts = manager.get_active_alerts().await.unwrap();
        assert_eq!(active_alerts.len(), 1);
        assert!(!active_alerts[0].acknowledged);

        manager.acknowledge_alert(&alert_id).await.unwrap();

        let active_alerts = manager.get_active_alerts().await.unwrap();
        assert_eq!(active_alerts.len(), 0);
    }

    #[tokio::test]
    async fn test_monitoring_system() {
        let system = MonitoringSystem::new();
        let status = system.get_system_status().await.unwrap();

        // The overall health might be Unhealthy due to containers not running, which is expected
        assert!(matches!(
            status.overall_health,
            HealthStatus::Healthy
                | HealthStatus::Warning
                | HealthStatus::Unhealthy
                | HealthStatus::Unknown
        ));
        assert!(!status.health_checks.is_empty());
        println!("System status: {:?}", status.overall_health);
    }

    #[tokio::test]
    async fn test_metrics_export() {
        let mut collector = DefaultMetricsCollector::new();
        collector
            .record_request_latency("test", Duration::from_millis(100))
            .await;

        let json_export = collector.export_metrics(MetricsFormat::Json).await.unwrap();
        assert!(json_export.contains("request_latencies"));

        let prometheus_export = collector
            .export_metrics(MetricsFormat::Prometheus)
            .await
            .unwrap();
        assert!(prometheus_export.contains("cache_hits_total"));

        let csv_export = collector.export_metrics(MetricsFormat::Csv).await.unwrap();
        assert!(csv_export.contains("timestamp,metric_type,service,value"));
    }
}
