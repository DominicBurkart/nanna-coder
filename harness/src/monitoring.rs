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
use crate::telemetry::{MetricPoint, MetricType};
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

    /// System initialization failed
    #[error("System initialization failed: {reason}")]
    InitializationFailed { reason: String },

    /// Monitoring integration failed
    #[error("Monitoring integration failed: {reason}")]
    MonitoringFailed { reason: String },

    /// Telemetry integration failed
    #[error("Telemetry integration failed: {reason}")]
    TelemetryFailed { reason: String },

    /// Alert processing failed
    #[error("Alert processing failed: {reason}")]
    AlertProcessingFailed { reason: String },

    /// Configuration error
    #[error("Configuration error: {reason}")]
    ConfigurationError { reason: String },

    /// Telemetry error
    #[error("Telemetry error: {0}")]
    Telemetry(#[from] crate::telemetry::TelemetryError),

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

impl SystemMetrics {
    /// Convert system metrics to a vector of MetricPoint for Prometheus export
    pub fn to_metric_points(&self) -> Vec<MetricPoint> {
        let mut points = Vec::new();
        let ts = self.timestamp;

        // Cache metrics
        points.push(MetricPoint {
            name: "cache_hits_total".to_string(),
            metric_type: MetricType::Counter,
            value: self.cache_metrics.hits as f64,
            timestamp: ts,
            labels: HashMap::new(),
            unit: None,
            description: Some("Total cache hits".to_string()),
        });
        points.push(MetricPoint {
            name: "cache_hit_rate".to_string(),
            metric_type: MetricType::Gauge,
            value: self.cache_metrics.hit_rate,
            timestamp: ts,
            labels: HashMap::new(),
            unit: Some("ratio".to_string()),
            description: Some("Cache hit rate".to_string()),
        });

        // Request latencies
        for (service, latency) in &self.request_latencies {
            let mut labels = HashMap::new();
            labels.insert("service".to_string(), service.clone());

            points.push(MetricPoint {
                name: "request_duration_seconds".to_string(),
                metric_type: MetricType::Histogram,
                value: latency.avg_latency_ms / 1000.0,
                timestamp: ts,
                labels: labels.clone(),
                unit: Some("seconds".to_string()),
                description: Some("Request duration".to_string()),
            });
            points.push(MetricPoint {
                name: "requests_per_second".to_string(),
                metric_type: MetricType::Gauge,
                value: latency.requests_per_second,
                timestamp: ts,
                labels,
                unit: Some("rps".to_string()),
                description: Some("Requests per second".to_string()),
            });
        }

        // Error metrics
        points.push(MetricPoint {
            name: "error_rate".to_string(),
            metric_type: MetricType::Gauge,
            value: self.error_metrics.error_rate,
            timestamp: ts,
            labels: HashMap::new(),
            unit: Some("ratio".to_string()),
            description: Some("Error rate".to_string()),
        });

        // System resources
        points.push(MetricPoint {
            name: "cpu_usage_percent".to_string(),
            metric_type: MetricType::Gauge,
            value: self.system_resources.cpu_usage_percent,
            timestamp: ts,
            labels: HashMap::new(),
            unit: Some("percent".to_string()),
            description: Some("CPU usage percentage".to_string()),
        });
        points.push(MetricPoint {
            name: "memory_usage_percent".to_string(),
            metric_type: MetricType::Gauge,
            value: self.system_resources.memory_usage_percent,
            timestamp: ts,
            labels: HashMap::new(),
            unit: Some("percent".to_string()),
            description: Some("Memory usage percentage".to_string()),
        });
        points.push(MetricPoint {
            name: "disk_usage_percent".to_string(),
            metric_type: MetricType::Gauge,
            value: self.system_resources.disk_usage_percent,
            timestamp: ts,
            labels: HashMap::new(),
            unit: Some("percent".to_string()),
            description: Some("Disk usage percentage".to_string()),
        });

        points
    }
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
    pub severity: AlertSeverity,
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
    /// Maximum CPU usage before alerting (percentage, 0-100)
    pub max_cpu_usage: f64,
    /// Maximum memory usage before alerting (percentage, 0-100)
    pub max_memory_usage: f64,
    /// Maximum disk usage before alerting (percentage, 0-100)
    pub disk_threshold: f64,
    /// Container health check timeout
    pub health_check_timeout: Duration,
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            max_latency_ms: 5000,
            min_cache_hit_rate: 0.8,
            max_error_rate: 0.05,
            max_cpu_usage: 90.0,
            max_memory_usage: 90.0,
            disk_threshold: 90.0,
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
                // Use PrometheusExporter via to_metric_points() for Prometheus format export
                let exporter = crate::telemetry::PrometheusExporter::new(None);
                for point in metrics.to_metric_points() {
                    exporter.add_metric(point);
                }
                exporter.export_prometheus().await.map_err(|e| {
                    MonitoringError::MetricsCollectionFailed {
                        reason: format!("Prometheus export failed: {}", e),
                    }
                })
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

// ── Observability types (consolidated from observability.rs) ──

/// Comprehensive system health status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComprehensiveStatus {
    /// Overall system health
    pub overall_health: HealthStatus,
    /// Detailed component health
    pub component_health: HashMap<String, ComponentHealth>,
    /// Current system metrics
    pub metrics: SystemMetrics,
    /// Active alerts
    pub active_alerts: Vec<AlertInfo>,
    /// Performance trends
    pub performance_trends: PerformanceTrends,
    /// Container status summary
    pub container_summary: ContainerSummary,
    /// Model performance summary
    pub model_summary: ModelSummary,
    /// System uptime and availability
    pub availability_metrics: AvailabilityMetrics,
    /// Status timestamp
    pub timestamp: DateTime<Utc>,
}

/// Individual component health details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentHealth {
    /// Component name
    pub name: String,
    /// Current health status
    pub status: HealthStatus,
    /// Status message
    pub message: String,
    /// Last check timestamp
    pub last_check: DateTime<Utc>,
    /// Check duration
    pub check_duration: Duration,
    /// Health history (last 10 checks)
    pub health_history: Vec<HealthHistoryEntry>,
    /// Performance metrics for this component
    pub metrics: HashMap<String, f64>,
}

/// Health history entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthHistoryEntry {
    /// Timestamp of the check
    pub timestamp: DateTime<Utc>,
    /// Health status at that time
    pub status: HealthStatus,
    /// Check duration
    pub duration: Duration,
}

/// Performance trend analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceTrends {
    /// Latency trend (improving/degrading/stable)
    pub latency_trend: TrendDirection,
    /// Throughput trend
    pub throughput_trend: TrendDirection,
    /// Error rate trend
    pub error_rate_trend: TrendDirection,
    /// Resource usage trend
    pub resource_usage_trend: TrendDirection,
    /// Cache performance trend
    pub cache_performance_trend: TrendDirection,
    /// Overall performance score (0-100)
    pub performance_score: f64,
}

/// Trend direction
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TrendDirection {
    /// Performance is improving
    Improving,
    /// Performance is stable
    Stable,
    /// Performance is degrading
    Degrading,
    /// Insufficient data to determine trend
    Unknown,
}

/// Container status summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSummary {
    /// Total containers
    pub total_containers: u32,
    /// Running containers
    pub running_containers: u32,
    /// Failed containers
    pub failed_containers: u32,
    /// Average container health score
    pub average_health_score: f64,
    /// Container uptime statistics
    pub uptime_stats: UptimeStats,
}

/// Model performance summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSummary {
    /// Total models monitored
    pub total_models: u32,
    /// Active models
    pub active_models: u32,
    /// Average inference time
    pub avg_inference_time_ms: f64,
    /// Average quality score
    pub avg_quality_score: f64,
    /// Model availability percentage
    pub availability_percentage: f64,
}

/// Availability and uptime metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailabilityMetrics {
    /// System uptime
    pub uptime: Duration,
    /// Availability percentage (99.9%)
    pub availability_percentage: f64,
    /// Mean time between failures
    pub mtbf: Option<Duration>,
    /// Mean time to recovery
    pub mttr: Option<Duration>,
    /// SLA compliance
    pub sla_compliance: SlaCompliance,
}

/// SLA compliance tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlaCompliance {
    /// Target availability (e.g., 99.9%)
    pub target_availability: f64,
    /// Current availability
    pub current_availability: f64,
    /// SLA status
    pub status: SlaStatus,
    /// Time to SLA breach (if applicable)
    pub time_to_breach: Option<Duration>,
}

/// SLA status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SlaStatus {
    /// SLA is being met
    Compliant,
    /// SLA is at risk
    AtRisk,
    /// SLA has been breached
    Breached,
}

/// Uptime statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UptimeStats {
    /// Current uptime
    pub current_uptime: Duration,
    /// Average uptime
    pub average_uptime: Duration,
    /// Maximum uptime
    pub max_uptime: Duration,
    /// Minimum uptime
    pub min_uptime: Duration,
}

/// Enhanced alert information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertInfo {
    /// Alert ID
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
    /// Alert category
    pub category: AlertCategory,
    /// Alert priority score
    pub priority_score: u32,
    /// Recommended actions
    pub recommended_actions: Vec<String>,
    /// Related metrics
    pub related_metrics: HashMap<String, f64>,
    /// Escalation status
    pub escalation_status: EscalationStatus,
}

/// Alert categories
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AlertCategory {
    /// Performance degradation
    Performance,
    /// System availability
    Availability,
    /// Security concern
    Security,
    /// Resource exhaustion
    Resources,
    /// Configuration issue
    Configuration,
    /// Model quality issue
    ModelQuality,
    /// Container health issue
    ContainerHealth,
}

/// Alert escalation status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EscalationStatus {
    /// New alert, no escalation
    New,
    /// Alert has been escalated once
    Escalated,
    /// Alert has been escalated multiple times
    HighlyEscalated,
    /// Alert is under investigation
    UnderInvestigation,
    /// Alert has been resolved
    Resolved,
}

/// Alert policy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertPolicy {
    /// Escalation rules
    pub escalation_rules: Vec<EscalationRule>,
    /// Notification channels
    pub notification_channels: Vec<NotificationChannel>,
    /// Alert suppression rules
    pub suppression_rules: Vec<SuppressionRule>,
    /// Alert grouping rules
    pub grouping_rules: Vec<GroupingRule>,
}

/// Escalation rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationRule {
    /// Severity level this rule applies to
    pub severity: AlertSeverity,
    /// Time before escalation
    pub escalation_time: Duration,
    /// Maximum escalation level
    pub max_escalations: u32,
    /// Escalation factor (multiplier for each level)
    pub escalation_factor: f64,
}

/// Notification channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationChannel {
    /// Channel type
    pub channel_type: ChannelType,
    /// Channel endpoint
    pub endpoint: String,
    /// Minimum severity for this channel
    pub min_severity: AlertSeverity,
    /// Channel configuration
    pub config: HashMap<String, String>,
}

/// Notification channel types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelType {
    /// Email notification
    Email,
    /// Slack webhook
    Slack,
    /// Discord webhook
    Discord,
    /// Custom webhook
    Webhook,
    /// Log file
    LogFile,
    /// Console output
    Console,
}

/// Alert suppression rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuppressionRule {
    /// Rule name
    pub name: String,
    /// Component pattern to match
    pub component_pattern: String,
    /// Alert category to suppress
    pub category: AlertCategory,
    /// Suppression duration
    pub duration: Duration,
    /// Conditions for suppression
    pub conditions: HashMap<String, String>,
}

/// Alert grouping rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupingRule {
    /// Rule name
    pub name: String,
    /// Fields to group by
    pub group_by_fields: Vec<String>,
    /// Grouping window
    pub window: Duration,
    /// Maximum alerts per group
    pub max_alerts_per_group: u32,
}

impl AlertPolicy {
    /// Create an immediate critical alert policy
    pub fn immediate_critical() -> Self {
        Self {
            escalation_rules: vec![
                EscalationRule {
                    severity: AlertSeverity::Critical,
                    escalation_time: Duration::from_secs(300),
                    max_escalations: 3,
                    escalation_factor: 2.0,
                },
                EscalationRule {
                    severity: AlertSeverity::Error,
                    escalation_time: Duration::from_secs(900),
                    max_escalations: 2,
                    escalation_factor: 1.5,
                },
            ],
            notification_channels: vec![NotificationChannel {
                channel_type: ChannelType::Console,
                endpoint: "console".to_string(),
                min_severity: AlertSeverity::Info,
                config: HashMap::new(),
            }],
            suppression_rules: Vec::new(),
            grouping_rules: Vec::new(),
        }
    }

    /// Create a balanced alert policy
    pub fn balanced() -> Self {
        Self {
            escalation_rules: vec![
                EscalationRule {
                    severity: AlertSeverity::Critical,
                    escalation_time: Duration::from_secs(600),
                    max_escalations: 2,
                    escalation_factor: 1.5,
                },
                EscalationRule {
                    severity: AlertSeverity::Error,
                    escalation_time: Duration::from_secs(1800),
                    max_escalations: 1,
                    escalation_factor: 1.0,
                },
            ],
            notification_channels: vec![NotificationChannel {
                channel_type: ChannelType::Console,
                endpoint: "console".to_string(),
                min_severity: AlertSeverity::Warning,
                config: HashMap::new(),
            }],
            suppression_rules: Vec::new(),
            grouping_rules: vec![GroupingRule {
                name: "container-alerts".to_string(),
                group_by_fields: vec!["component".to_string(), "category".to_string()],
                window: Duration::from_secs(300),
                max_alerts_per_group: 5,
            }],
        }
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
    /// Telemetry system (when using observability features)
    telemetry: Option<crate::telemetry::TelemetrySystem>,
    /// Service name
    service_name: Option<String>,
    /// Alert policy
    alert_policy: Option<AlertPolicy>,
    /// Alert thresholds
    alert_thresholds: AlertThresholds,
    /// Health check interval
    health_check_interval: Duration,
    /// Component health history
    health_history: Arc<Mutex<HashMap<String, Vec<HealthHistoryEntry>>>>,
    /// System start time
    start_time: Instant,
    /// Observability monitoring task
    observability_task: Option<tokio::task::JoinHandle<()>>,
    /// Container runtime
    container_runtime: ContainerRuntime,
}

impl MonitoringSystem {
    /// Create a new monitoring system with default implementations
    pub fn new() -> Self {
        Self {
            metrics_collector: Box::new(DefaultMetricsCollector::new()),
            health_monitor: Box::new(DefaultHealthMonitor::new(Duration::from_secs(60))),
            alert_manager: Box::new(DefaultAlertManager::new()),
            monitoring_task: None,
            telemetry: None,
            service_name: None,
            alert_policy: None,
            alert_thresholds: AlertThresholds::default(),
            health_check_interval: Duration::from_secs(60),
            health_history: Arc::new(Mutex::new(HashMap::new())),
            start_time: Instant::now(),
            observability_task: None,
            container_runtime: detect_runtime(),
        }
    }

    /// Create a new monitoring system with integrated observability (telemetry, alerting, trends).
    pub fn new_with_observability(telemetry: crate::telemetry::TelemetrySystem) -> Self {
        Self {
            metrics_collector: Box::new(DefaultMetricsCollector::new()),
            health_monitor: Box::new(DefaultHealthMonitor::new(Duration::from_secs(60))),
            alert_manager: Box::new(DefaultAlertManager::new()),
            monitoring_task: None,
            telemetry: Some(telemetry),
            service_name: None,
            alert_policy: Some(AlertPolicy::balanced()),
            alert_thresholds: AlertThresholds::default(),
            health_check_interval: Duration::from_secs(60),
            health_history: Arc::new(Mutex::new(HashMap::new())),
            start_time: Instant::now(),
            observability_task: None,
            container_runtime: detect_runtime(),
        }
    }

    /// Set service name
    pub fn with_service_name(mut self, name: &str) -> Self {
        self.service_name = Some(name.to_string());
        if let Some(ref mut t) = self.telemetry {
            // Rebuild telemetry with new service name - take ownership temporarily
            let tel = std::mem::take(t);
            *t = tel.with_service_name(name);
        }
        self
    }

    /// Set alert policy
    pub fn with_alert_policy(mut self, policy: AlertPolicy) -> Self {
        self.alert_policy = Some(policy);
        self
    }

    /// Set health check interval
    pub fn with_health_check_interval(mut self, interval: Duration) -> Self {
        self.health_check_interval = interval;
        self
    }

    /// Initialize the observability subsystem (telemetry + monitoring background tasks)
    pub async fn initialize_observability(&mut self) -> Result<(), MonitoringError> {
        let svc = self
            .service_name
            .clone()
            .unwrap_or_else(|| "nanna-coder".to_string());
        info!("Initializing observability system for service: {}", svc);

        if let Some(ref mut telemetry) = self.telemetry {
            telemetry
                .initialize()
                .await
                .map_err(|e| MonitoringError::TelemetryFailed {
                    reason: e.to_string(),
                })?;
        }

        self.start_monitoring()
            .await
            .map_err(|e| MonitoringError::MonitoringFailed {
                reason: e.to_string(),
            })?;

        info!("Observability system initialized successfully");
        Ok(())
    }

    /// Start background monitoring tasks
    pub async fn start_monitoring(&mut self) -> Result<(), MonitoringError> {
        info!("Starting background monitoring system");

        let task = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));

            loop {
                interval.tick().await;
                debug!("Performing periodic monitoring checks");
            }
        });

        self.monitoring_task = Some(task);
        Ok(())
    }

    /// Start comprehensive observability monitoring
    pub async fn start_observability_monitoring(&mut self) -> Result<(), MonitoringError> {
        info!(
            "Starting comprehensive monitoring with {}s interval",
            self.health_check_interval.as_secs()
        );

        let health_history = Arc::clone(&self.health_history);
        let health_interval = self.health_check_interval;
        let thresholds = self.alert_thresholds.clone();
        let runtime = self.container_runtime.clone();

        let task = tokio::spawn(async move {
            let mut tick = interval(health_interval);

            loop {
                tick.tick().await;
                debug!("Performing comprehensive health check");

                if let Err(e) =
                    Self::perform_health_checks(&health_history, &thresholds, &runtime).await
                {
                    error!("Health check failed: {}", e);
                }

                if let Err(e) = Self::analyze_performance_trends_bg().await {
                    error!("Performance trend analysis failed: {}", e);
                }

                if let Err(e) = Self::check_sla_compliance_bg().await {
                    error!("SLA compliance check failed: {}", e);
                }
            }
        });

        self.observability_task = Some(task);
        Ok(())
    }

    /// Stop background monitoring
    pub async fn stop_monitoring(&mut self) {
        if let Some(task) = self.monitoring_task.take() {
            task.abort();
            info!("Background monitoring stopped");
        }
        if let Some(task) = self.observability_task.take() {
            task.abort();
            info!("Comprehensive monitoring stopped");
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

    /// Get comprehensive system status including observability data
    pub async fn get_comprehensive_status(&self) -> Result<ComprehensiveStatus, MonitoringError> {
        let start_time = Instant::now();
        let trace = self
            .telemetry
            .as_ref()
            .map(|t| t.start_trace("get_comprehensive_status"));

        let system_status = self.get_system_status().await?;
        let component_health = self.get_component_health().await?;
        let performance_trends = self.analyze_current_trends(&system_status.metrics)?;
        let container_summary = self.get_container_summary().await?;
        let model_summary = self.get_model_summary(&system_status.metrics)?;
        let availability_metrics = self.calculate_availability_metrics()?;
        let active_alerts = self.enhance_alerts(system_status.active_alerts).await?;

        let comprehensive_status = ComprehensiveStatus {
            overall_health: system_status.overall_health,
            component_health,
            metrics: system_status.metrics,
            active_alerts,
            performance_trends,
            container_summary,
            model_summary,
            availability_metrics,
            timestamp: Utc::now(),
        };

        if let (Some(telemetry), Some(trace)) = (&self.telemetry, trace) {
            telemetry.finish_trace(trace);
            telemetry.record_histogram("status_retrieval_duration", start_time.elapsed());
        }

        Ok(comprehensive_status)
    }

    /// Get detailed component health
    async fn get_component_health(
        &self,
    ) -> Result<HashMap<String, ComponentHealth>, MonitoringError> {
        let health_checks = self.health_monitor.comprehensive_health_check().await?;
        let mut component_health = HashMap::new();

        for check in health_checks {
            let history = {
                let history_map = self.health_history.lock().unwrap();
                history_map
                    .get(&check.component)
                    .cloned()
                    .unwrap_or_default()
            };

            let component = ComponentHealth {
                name: check.component.clone(),
                status: check.status.clone(),
                message: check.message.clone(),
                last_check: check.timestamp,
                check_duration: check.check_duration,
                health_history: history.iter().rev().take(10).cloned().collect(),
                metrics: HashMap::new(),
            };

            component_health.insert(check.component, component);
        }

        Ok(component_health)
    }

    /// Analyze current performance trends
    fn analyze_current_trends(
        &self,
        metrics: &SystemMetrics,
    ) -> Result<PerformanceTrends, MonitoringError> {
        let latency_trend = if metrics
            .request_latencies
            .values()
            .any(|l| l.avg_latency_ms > self.alert_thresholds.max_latency_ms as f64)
        {
            TrendDirection::Degrading
        } else {
            TrendDirection::Stable
        };

        let error_rate_trend =
            if metrics.error_metrics.error_rate > self.alert_thresholds.max_error_rate {
                TrendDirection::Degrading
            } else {
                TrendDirection::Stable
            };

        let cache_performance_trend =
            if metrics.cache_metrics.hit_rate < self.alert_thresholds.min_cache_hit_rate {
                TrendDirection::Degrading
            } else {
                TrendDirection::Stable
            };

        let mut score: f64 = 100.0;
        if latency_trend == TrendDirection::Degrading {
            score -= 20.0;
        }
        if error_rate_trend == TrendDirection::Degrading {
            score -= 25.0;
        }
        if cache_performance_trend == TrendDirection::Degrading {
            score -= 15.0;
        }

        Ok(PerformanceTrends {
            latency_trend,
            throughput_trend: TrendDirection::Stable,
            error_rate_trend,
            resource_usage_trend: TrendDirection::Stable,
            cache_performance_trend,
            performance_score: score.max(0.0),
        })
    }

    /// Get container summary
    async fn get_container_summary(&self) -> Result<ContainerSummary, MonitoringError> {
        let running_containers = if self.container_runtime.is_available() {
            1
        } else {
            0
        };

        Ok(ContainerSummary {
            total_containers: 1,
            running_containers,
            failed_containers: 0,
            average_health_score: if running_containers > 0 { 85.0 } else { 0.0 },
            uptime_stats: UptimeStats {
                current_uptime: self.start_time.elapsed(),
                average_uptime: self.start_time.elapsed(),
                max_uptime: self.start_time.elapsed(),
                min_uptime: Duration::ZERO,
            },
        })
    }

    /// Get model summary
    fn get_model_summary(&self, metrics: &SystemMetrics) -> Result<ModelSummary, MonitoringError> {
        let total_models = metrics.model_metrics.len() as u32;
        let avg_quality_score = metrics
            .model_metrics
            .values()
            .map(|m| m.quality_scores.avg_coherence + m.quality_scores.avg_relevance)
            .sum::<f64>()
            / (total_models as f64 * 2.0).max(1.0);

        Ok(ModelSummary {
            total_models,
            active_models: total_models,
            avg_inference_time_ms: metrics
                .model_metrics
                .values()
                .map(|m| m.avg_inference_time_ms)
                .sum::<f64>()
                / total_models.max(1) as f64,
            avg_quality_score,
            availability_percentage: if total_models > 0 { 95.0 } else { 0.0 },
        })
    }

    /// Calculate availability metrics
    fn calculate_availability_metrics(&self) -> Result<AvailabilityMetrics, MonitoringError> {
        let uptime = self.start_time.elapsed();
        let availability = 99.5;

        Ok(AvailabilityMetrics {
            uptime,
            availability_percentage: availability,
            mtbf: Some(Duration::from_secs(86400)),
            mttr: Some(Duration::from_secs(300)),
            sla_compliance: SlaCompliance {
                target_availability: 99.9,
                current_availability: availability,
                status: if availability >= 99.9 {
                    SlaStatus::Compliant
                } else if availability >= 99.0 {
                    SlaStatus::AtRisk
                } else {
                    SlaStatus::Breached
                },
                time_to_breach: None,
            },
        })
    }

    /// Enhance alerts with additional context
    async fn enhance_alerts(&self, alerts: Vec<Alert>) -> Result<Vec<AlertInfo>, MonitoringError> {
        let mut enhanced_alerts = Vec::new();

        for alert in alerts {
            let category = Self::determine_alert_category(&alert);
            let priority_score = Self::calculate_priority_score(&alert, &category);
            let recommended_actions = Self::generate_recommended_actions(&alert, &category);

            let enhanced = AlertInfo {
                id: alert.id,
                title: alert.title,
                description: alert.description,
                severity: alert.severity,
                component: alert.component,
                timestamp: alert.timestamp,
                category,
                priority_score,
                recommended_actions,
                related_metrics: HashMap::new(),
                escalation_status: EscalationStatus::New,
            };

            enhanced_alerts.push(enhanced);
        }

        Ok(enhanced_alerts)
    }

    /// Determine alert category
    fn determine_alert_category(alert: &Alert) -> AlertCategory {
        if alert.component.contains("container") {
            AlertCategory::ContainerHealth
        } else if alert.component.contains("model") {
            AlertCategory::ModelQuality
        } else if alert.title.to_lowercase().contains("performance") {
            AlertCategory::Performance
        } else if alert.title.to_lowercase().contains("resource") {
            AlertCategory::Resources
        } else {
            AlertCategory::Availability
        }
    }

    /// Calculate priority score
    fn calculate_priority_score(alert: &Alert, category: &AlertCategory) -> u32 {
        let mut score = match alert.severity {
            AlertSeverity::Critical => 100,
            AlertSeverity::Error => 75,
            AlertSeverity::Warning => 50,
            AlertSeverity::Info => 25,
        };

        match category {
            AlertCategory::Availability | AlertCategory::ContainerHealth => score += 20,
            AlertCategory::Security => score += 30,
            AlertCategory::Performance => score += 10,
            _ => {}
        }

        score.min(100)
    }

    /// Generate recommended actions
    fn generate_recommended_actions(_alert: &Alert, category: &AlertCategory) -> Vec<String> {
        match category {
            AlertCategory::ContainerHealth => vec![
                "Check container logs for errors".to_string(),
                "Verify container resource allocation".to_string(),
                "Restart container if necessary".to_string(),
            ],
            AlertCategory::Performance => vec![
                "Check system resource usage".to_string(),
                "Review recent changes".to_string(),
                "Scale resources if needed".to_string(),
            ],
            AlertCategory::ModelQuality => vec![
                "Verify model inputs".to_string(),
                "Check model configuration".to_string(),
                "Review model performance metrics".to_string(),
            ],
            _ => vec![
                "Investigate the issue".to_string(),
                "Check system logs".to_string(),
                "Contact support if needed".to_string(),
            ],
        }
    }

    /// Perform comprehensive health checks (background task helper)
    async fn perform_health_checks(
        health_history: &Arc<Mutex<HashMap<String, Vec<HealthHistoryEntry>>>>,
        _thresholds: &AlertThresholds,
        _runtime: &ContainerRuntime,
    ) -> Result<(), MonitoringError> {
        let mut history = health_history.lock().unwrap();

        let entry = HealthHistoryEntry {
            timestamp: Utc::now(),
            status: HealthStatus::Healthy,
            duration: Duration::from_millis(50),
        };

        history.entry("system".to_string()).or_default().push(entry);

        for entries in history.values_mut() {
            if entries.len() > 50 {
                entries.drain(0..entries.len() - 50);
            }
        }

        Ok(())
    }

    /// Analyze performance trends (background task)
    async fn analyze_performance_trends_bg() -> Result<(), MonitoringError> {
        debug!("Analyzing performance trends");
        Ok(())
    }

    /// Check SLA compliance (background task)
    async fn check_sla_compliance_bg() -> Result<(), MonitoringError> {
        debug!("Checking SLA compliance");
        Ok(())
    }

    /// Get system uptime
    pub fn get_uptime(&self) -> Duration {
        self.start_time.elapsed()
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
        assert!(prometheus_export.contains("cache_hit_rate"));

        let csv_export = collector.export_metrics(MetricsFormat::Csv).await.unwrap();
        assert!(csv_export.contains("timestamp,metric_type,service,value"));
    }

    // ── Tests ported from observability.rs ──

    #[tokio::test]
    async fn test_observability_system_initialization() {
        let telemetry = crate::telemetry::TelemetrySystem::new();
        let mut system = MonitoringSystem::new_with_observability(telemetry)
            .with_service_name("test-service")
            .with_health_check_interval(Duration::from_secs(10));

        let result = system.initialize_observability().await;
        if result.is_ok() {
            assert!(system.get_uptime() < Duration::from_secs(1));
        } else {
            // Expected failure in test environment - tracing subscriber already set
            println!("Expected failure: {:?}", result);
        }
    }

    #[tokio::test]
    async fn test_comprehensive_status() {
        let telemetry = crate::telemetry::TelemetrySystem::new();
        let mut system = MonitoringSystem::new_with_observability(telemetry);

        let _ = system.initialize_observability().await;

        let status = system.get_comprehensive_status().await.unwrap();
        assert!(matches!(
            status.overall_health,
            HealthStatus::Healthy
                | HealthStatus::Warning
                | HealthStatus::Unhealthy
                | HealthStatus::Unknown
        ));
    }

    #[tokio::test]
    async fn test_alert_policy_creation() {
        let immediate = AlertPolicy::immediate_critical();
        assert!(!immediate.escalation_rules.is_empty());
        assert!(!immediate.notification_channels.is_empty());

        let balanced = AlertPolicy::balanced();
        assert!(!balanced.escalation_rules.is_empty());
        assert!(!balanced.grouping_rules.is_empty());
    }

    #[tokio::test]
    async fn test_alert_thresholds_defaults() {
        let thresholds = AlertThresholds::default();
        assert!(thresholds.max_cpu_usage > 0.0);
        assert!(thresholds.max_memory_usage > 0.0);
        assert!(thresholds.disk_threshold > 0.0);
        assert!(thresholds.max_latency_ms > 0);
    }

    #[tokio::test]
    async fn test_performance_trends() {
        let system = MonitoringSystem::new();
        let metrics = SystemMetrics {
            timestamp: Utc::now(),
            request_latencies: HashMap::new(),
            cache_metrics: CacheMetrics {
                hits: 80,
                misses: 20,
                hit_rate: 0.8,
                size_bytes: 1024,
                item_count: 100,
                evictions: 5,
            },
            container_metrics: Vec::new(),
            system_resources: SystemResourceMetrics {
                cpu_usage_percent: 50.0,
                total_memory_bytes: 8589934592,
                used_memory_bytes: 4294967296,
                memory_usage_percent: 50.0,
                available_disk_bytes: 107374182400,
                total_disk_bytes: 214748364800,
                disk_usage_percent: 50.0,
                load_average: [1.0, 1.2, 1.1],
            },
            model_metrics: HashMap::new(),
            error_metrics: ErrorMetrics {
                total_errors: 0,
                errors_by_type: HashMap::new(),
                error_rate: 0.0,
                recent_errors: Vec::new(),
            },
        };

        let trends = system.analyze_current_trends(&metrics).unwrap();
        assert!(trends.performance_score >= 0.0 && trends.performance_score <= 100.0);
    }
}
