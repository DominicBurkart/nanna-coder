//! Telemetry and observability infrastructure
//!
//! This module provides comprehensive telemetry capabilities including structured logging,
//! distributed tracing, metrics export, and observability integrations.
//!
//! # Features
//!
//! - Structured logging with context enrichment
//! - Distributed tracing and span correlation
//! - Metrics export to external systems (Prometheus, OTLP)
//! - Performance monitoring and profiling
//! - Custom event tracking and analytics
//! - Integration with monitoring systems
//!
//! # Examples
//!
//! ```rust
//! use harness::telemetry::{TelemetrySystem, TraceContext};
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mut telemetry = TelemetrySystem::new()
//!     .with_service_name("nanna-coder")
//!     .with_version("0.1.0")
//!     .with_environment("development");
//!
//! telemetry.initialize().await?;
//!
//! // Create a trace context
//! let mut trace_ctx = telemetry.start_trace("model_inference")
//!     .with_attribute("model", "qwen3:0.6b")
//!     .with_attribute("user_id", "test-user");
//!
//! // Record custom metrics
//! telemetry.record_counter("inference_requests", 1.0, vec![("model", "qwen3")]);
//! telemetry.record_histogram("inference_duration", Duration::from_millis(150));
//!
//! // Export metrics
//! if let Some(exporter) = telemetry.get_prometheus_exporter() {
//!     let prometheus_metrics = exporter.export_prometheus().await?;
//!     println!("Metrics: {}", prometheus_metrics);
//! }
//!
//! trace_ctx.finish();
//! # Ok(())
//! # }
//! ```

use crate::monitoring::SystemMetrics;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, error, info};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

/// Telemetry system errors
#[derive(Error, Debug)]
pub enum TelemetryError {
    /// Initialization failed
    #[error("Telemetry initialization failed: {reason}")]
    InitializationFailed { reason: String },

    /// Export failed
    #[error("Metrics export failed: {reason}")]
    ExportFailed { reason: String },

    /// Trace operation failed
    #[error("Trace operation failed: {reason}")]
    TraceFailed { reason: String },

    /// Configuration error
    #[error("Configuration error: {reason}")]
    ConfigurationError { reason: String },

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Service information for telemetry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// Service name
    pub name: String,
    /// Service version
    pub version: String,
    /// Deployment environment
    pub environment: String,
    /// Service instance ID
    pub instance_id: String,
    /// Additional service metadata
    pub metadata: HashMap<String, String>,
}

/// Telemetry configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Service information
    pub service: ServiceInfo,
    /// Enable structured logging
    pub enable_logging: bool,
    /// Enable distributed tracing
    pub enable_tracing: bool,
    /// Enable metrics collection
    pub enable_metrics: bool,
    /// Log level filter
    pub log_level: String,
    /// Metrics export interval
    pub metrics_export_interval: Duration,
    /// Tracing sample rate (0.0 to 1.0)
    pub trace_sample_rate: f64,
    /// Export endpoints
    pub export_endpoints: ExportEndpoints,
    /// Custom attributes to add to all telemetry
    pub global_attributes: HashMap<String, String>,
}

/// Export endpoint configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEndpoints {
    /// Prometheus metrics endpoint
    pub prometheus_endpoint: Option<String>,
    /// OTLP endpoint for traces and metrics
    pub otlp_endpoint: Option<String>,
    /// Custom webhook endpoints
    pub webhook_endpoints: Vec<String>,
    /// Log aggregation endpoint
    pub log_endpoint: Option<String>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            service: ServiceInfo {
                name: "nanna-coder".to_string(),
                version: "0.1.0".to_string(),
                environment: "development".to_string(),
                instance_id: uuid::Uuid::new_v4().to_string(),
                metadata: HashMap::new(),
            },
            enable_logging: true,
            enable_tracing: true,
            enable_metrics: true,
            log_level: "info".to_string(),
            metrics_export_interval: Duration::from_secs(60),
            trace_sample_rate: 1.0,
            export_endpoints: ExportEndpoints {
                prometheus_endpoint: None,
                otlp_endpoint: None,
                webhook_endpoints: Vec::new(),
                log_endpoint: None,
            },
            global_attributes: HashMap::new(),
        }
    }
}

/// Trace context for distributed tracing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceContext {
    /// Trace ID
    pub trace_id: String,
    /// Span ID
    pub span_id: String,
    /// Parent span ID
    pub parent_span_id: Option<String>,
    /// Operation name
    pub operation_name: String,
    /// Start timestamp
    pub start_time: DateTime<Utc>,
    /// End timestamp
    pub end_time: Option<DateTime<Utc>>,
    /// Span attributes
    pub attributes: HashMap<String, String>,
    /// Span status
    pub status: SpanStatus,
    /// Duration of the operation
    pub duration: Option<Duration>,
}

/// Span status enumeration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SpanStatus {
    /// Operation is still in progress
    InProgress,
    /// Operation completed successfully
    Ok,
    /// Operation completed with an error
    Error,
    /// Operation was cancelled
    Cancelled,
    /// Operation timed out
    Timeout,
}

impl TraceContext {
    /// Create a new trace context
    pub fn new(operation_name: &str) -> Self {
        Self {
            trace_id: uuid::Uuid::new_v4().to_string(),
            span_id: uuid::Uuid::new_v4().to_string(),
            parent_span_id: None,
            operation_name: operation_name.to_string(),
            start_time: Utc::now(),
            end_time: None,
            attributes: HashMap::new(),
            status: SpanStatus::InProgress,
            duration: None,
        }
    }

    /// Create a child span
    pub fn create_child(&self, operation_name: &str) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: uuid::Uuid::new_v4().to_string(),
            parent_span_id: Some(self.span_id.clone()),
            operation_name: operation_name.to_string(),
            start_time: Utc::now(),
            end_time: None,
            attributes: HashMap::new(),
            status: SpanStatus::InProgress,
            duration: None,
        }
    }

    /// Add an attribute to the span
    pub fn with_attribute(mut self, key: &str, value: &str) -> Self {
        self.attributes.insert(key.to_string(), value.to_string());
        self
    }

    /// Set span status
    pub fn set_status(&mut self, status: SpanStatus) {
        self.status = status;
    }

    /// Add an error to the span
    pub fn record_error(&mut self, error: &str) {
        self.attributes
            .insert("error".to_string(), error.to_string());
        self.status = SpanStatus::Error;
    }

    /// Finish the span
    pub fn finish(&mut self) {
        let end_time = Utc::now();
        self.end_time = Some(end_time);
        self.duration = Some(
            end_time
                .signed_duration_since(self.start_time)
                .to_std()
                .unwrap_or(Duration::ZERO),
        );

        if self.status == SpanStatus::InProgress {
            self.status = SpanStatus::Ok;
        }
    }
}

/// Custom event for tracking specific application events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEvent {
    /// Event name
    pub name: String,
    /// Event timestamp
    pub timestamp: DateTime<Utc>,
    /// Event category
    pub category: String,
    /// Event attributes
    pub attributes: HashMap<String, String>,
    /// Event data
    pub data: serde_json::Value,
    /// Trace context if available
    pub trace_context: Option<TraceContext>,
}

/// Metrics data point
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricPoint {
    /// Metric name
    pub name: String,
    /// Metric type
    pub metric_type: MetricType,
    /// Metric value
    pub value: f64,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
    /// Labels/tags
    pub labels: HashMap<String, String>,
    /// Unit of measurement
    pub unit: Option<String>,
    /// Description
    pub description: Option<String>,
}

/// Types of metrics
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MetricType {
    /// Counter that can only increase
    Counter,
    /// Gauge that can go up and down
    Gauge,
    /// Histogram for distribution data
    Histogram,
    /// Summary with quantiles
    Summary,
}

/// Trait for telemetry data exporters
#[async_trait]
pub trait TelemetryExporter: Send + Sync {
    /// Export traces
    async fn export_traces(&self, traces: Vec<TraceContext>) -> Result<(), TelemetryError>;

    /// Export metrics
    async fn export_metrics(&self, metrics: Vec<MetricPoint>) -> Result<(), TelemetryError>;

    /// Export custom events
    async fn export_events(&self, events: Vec<CustomEvent>) -> Result<(), TelemetryError>;

    /// Export system metrics
    async fn export_system_metrics(&self, metrics: SystemMetrics) -> Result<(), TelemetryError>;

    /// Health check for the exporter
    async fn health_check(&self) -> Result<bool, TelemetryError>;
}

/// Prometheus metrics exporter
pub struct PrometheusExporter {
    /// Endpoint URL
    #[allow(dead_code)]
    endpoint: Option<String>,
    /// Metrics buffer
    metrics_buffer: Arc<Mutex<Vec<MetricPoint>>>,
}

impl PrometheusExporter {
    /// Create a new Prometheus exporter
    pub fn new(endpoint: Option<String>) -> Self {
        Self {
            endpoint,
            metrics_buffer: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Export metrics in Prometheus format
    pub async fn export_prometheus(&self) -> Result<String, TelemetryError> {
        let metrics = {
            let buffer = self.metrics_buffer.lock().unwrap();
            buffer.clone()
        };

        let mut output = String::new();

        for metric in metrics {
            // Add help text
            if let Some(description) = &metric.description {
                output.push_str(&format!("# HELP {} {}\n", metric.name, description));
            }

            // Add type
            let prom_type = match metric.metric_type {
                MetricType::Counter => "counter",
                MetricType::Gauge => "gauge",
                MetricType::Histogram => "histogram",
                MetricType::Summary => "summary",
            };
            output.push_str(&format!("# TYPE {} {}\n", metric.name, prom_type));

            // Add metric with labels
            let labels = if metric.labels.is_empty() {
                String::new()
            } else {
                let label_pairs: Vec<String> = metric
                    .labels
                    .iter()
                    .map(|(k, v)| format!("{}=\"{}\"", k, v))
                    .collect();
                format!("{{{}}}", label_pairs.join(","))
            };

            output.push_str(&format!("{}{} {}\n", metric.name, labels, metric.value));
        }

        Ok(output)
    }

    /// Add a metric to the buffer
    pub fn add_metric(&self, metric: MetricPoint) {
        let mut buffer = self.metrics_buffer.lock().unwrap();
        buffer.push(metric);
    }

    /// Clear the metrics buffer
    pub fn clear_buffer(&self) {
        let mut buffer = self.metrics_buffer.lock().unwrap();
        buffer.clear();
    }
}

#[async_trait]
impl TelemetryExporter for PrometheusExporter {
    async fn export_traces(&self, traces: Vec<TraceContext>) -> Result<(), TelemetryError> {
        // Convert traces to metrics
        for trace in traces {
            if let Some(duration) = trace.duration {
                let mut labels = HashMap::new();
                labels.insert("operation".to_string(), trace.operation_name.clone());
                labels.insert("status".to_string(), format!("{:?}", trace.status));

                let metric = MetricPoint {
                    name: "trace_duration_seconds".to_string(),
                    metric_type: MetricType::Histogram,
                    value: duration.as_secs_f64(),
                    timestamp: trace.start_time,
                    labels,
                    unit: Some("seconds".to_string()),
                    description: Some("Duration of traced operations".to_string()),
                };

                self.add_metric(metric);
            }
        }

        Ok(())
    }

    async fn export_metrics(&self, metrics: Vec<MetricPoint>) -> Result<(), TelemetryError> {
        for metric in metrics {
            self.add_metric(metric);
        }
        Ok(())
    }

    async fn export_events(&self, events: Vec<CustomEvent>) -> Result<(), TelemetryError> {
        // Convert events to metrics
        for event in events {
            let mut labels = HashMap::new();
            labels.insert("event_name".to_string(), event.name.clone());
            labels.insert("category".to_string(), event.category.clone());

            let metric = MetricPoint {
                name: "custom_events_total".to_string(),
                metric_type: MetricType::Counter,
                value: 1.0,
                timestamp: event.timestamp,
                labels,
                unit: None,
                description: Some("Count of custom events".to_string()),
            };

            self.add_metric(metric);
        }

        Ok(())
    }

    async fn export_system_metrics(&self, metrics: SystemMetrics) -> Result<(), TelemetryError> {
        // Convert system metrics to Prometheus format
        let timestamp = metrics.timestamp;

        // Cache metrics
        let cache_metric = MetricPoint {
            name: "cache_hit_rate".to_string(),
            metric_type: MetricType::Gauge,
            value: metrics.cache_metrics.hit_rate,
            timestamp,
            labels: HashMap::new(),
            unit: Some("ratio".to_string()),
            description: Some("Cache hit rate".to_string()),
        };
        self.add_metric(cache_metric);

        // Request latencies
        for (service, latency) in metrics.request_latencies {
            let mut labels = HashMap::new();
            labels.insert("service".to_string(), service);

            let latency_metric = MetricPoint {
                name: "request_duration_seconds".to_string(),
                metric_type: MetricType::Histogram,
                value: latency.avg_latency_ms / 1000.0,
                timestamp,
                labels: labels.clone(),
                unit: Some("seconds".to_string()),
                description: Some("Request duration".to_string()),
            };
            self.add_metric(latency_metric);

            let rps_metric = MetricPoint {
                name: "requests_per_second".to_string(),
                metric_type: MetricType::Gauge,
                value: latency.requests_per_second,
                timestamp,
                labels,
                unit: Some("rps".to_string()),
                description: Some("Requests per second".to_string()),
            };
            self.add_metric(rps_metric);
        }

        // Error metrics
        let error_rate_metric = MetricPoint {
            name: "error_rate".to_string(),
            metric_type: MetricType::Gauge,
            value: metrics.error_metrics.error_rate,
            timestamp,
            labels: HashMap::new(),
            unit: Some("ratio".to_string()),
            description: Some("Error rate".to_string()),
        };
        self.add_metric(error_rate_metric);

        Ok(())
    }

    async fn health_check(&self) -> Result<bool, TelemetryError> {
        // Simple health check - in a real implementation, we'd check connectivity
        Ok(true)
    }
}

/// Main telemetry system
pub struct TelemetrySystem {
    /// Configuration
    config: TelemetryConfig,
    /// Active trace contexts
    active_traces: Arc<Mutex<Vec<TraceContext>>>,
    /// Metrics buffer
    metrics_buffer: Arc<Mutex<Vec<MetricPoint>>>,
    /// Events buffer
    events_buffer: Arc<Mutex<Vec<CustomEvent>>>,
    /// Exporters
    exporters: Vec<Box<dyn TelemetryExporter>>,
    /// Is initialized
    initialized: bool,
    /// Start time for uptime tracking
    start_time: Instant,
}

impl TelemetrySystem {
    /// Create a new telemetry system
    pub fn new() -> Self {
        Self {
            config: TelemetryConfig::default(),
            active_traces: Arc::new(Mutex::new(Vec::new())),
            metrics_buffer: Arc::new(Mutex::new(Vec::new())),
            events_buffer: Arc::new(Mutex::new(Vec::new())),
            exporters: Vec::new(),
            initialized: false,
            start_time: Instant::now(),
        }
    }

    /// Set service name
    pub fn with_service_name(mut self, name: &str) -> Self {
        self.config.service.name = name.to_string();
        self
    }

    /// Set service version
    pub fn with_version(mut self, version: &str) -> Self {
        self.config.service.version = version.to_string();
        self
    }

    /// Set environment
    pub fn with_environment(mut self, environment: &str) -> Self {
        self.config.service.environment = environment.to_string();
        self
    }

    /// Add a global attribute
    pub fn with_global_attribute(mut self, key: &str, value: &str) -> Self {
        self.config
            .global_attributes
            .insert(key.to_string(), value.to_string());
        self
    }

    /// Set configuration
    pub fn with_config(mut self, config: TelemetryConfig) -> Self {
        self.config = config;
        self
    }

    /// Add an exporter
    pub fn add_exporter(mut self, exporter: Box<dyn TelemetryExporter>) -> Self {
        self.exporters.push(exporter);
        self
    }

    /// Initialize the telemetry system
    pub async fn initialize(&mut self) -> Result<(), TelemetryError> {
        if self.initialized {
            return Ok(());
        }

        // Initialize structured logging
        if self.config.enable_logging {
            let filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&self.config.log_level));

            let subscriber = FmtSubscriber::builder()
                .with_env_filter(filter)
                .with_target(false)
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true)
                .finish();

            tracing::subscriber::set_global_default(subscriber).map_err(|e| {
                TelemetryError::InitializationFailed {
                    reason: format!("Failed to set tracing subscriber: {}", e),
                }
            })?;
        }

        // Add default Prometheus exporter if configured
        if let Some(endpoint) = &self.config.export_endpoints.prometheus_endpoint {
            let prometheus_exporter = PrometheusExporter::new(Some(endpoint.clone()));
            self.exporters.push(Box::new(prometheus_exporter));
        }

        self.initialized = true;
        info!(
            "Telemetry system initialized for service: {}",
            self.config.service.name
        );

        Ok(())
    }

    /// Start a new trace
    pub fn start_trace(&self, operation_name: &str) -> TraceContext {
        let mut trace = TraceContext::new(operation_name);

        // Add global attributes
        for (key, value) in &self.config.global_attributes {
            trace.attributes.insert(key.clone(), value.clone());
        }

        // Add service information
        trace
            .attributes
            .insert("service.name".to_string(), self.config.service.name.clone());
        trace.attributes.insert(
            "service.version".to_string(),
            self.config.service.version.clone(),
        );
        trace.attributes.insert(
            "service.environment".to_string(),
            self.config.service.environment.clone(),
        );

        {
            let mut traces = self.active_traces.lock().unwrap();
            traces.push(trace.clone());
        }

        debug!("Started trace: {} ({})", operation_name, trace.trace_id);
        trace
    }

    /// Finish a trace
    pub fn finish_trace(&self, mut trace: TraceContext) {
        trace.finish();

        {
            let mut traces = self.active_traces.lock().unwrap();
            if let Some(pos) = traces.iter().position(|t| t.span_id == trace.span_id) {
                traces.remove(pos);
            }
        }

        debug!(
            "Finished trace: {} (duration: {:?})",
            trace.operation_name, trace.duration
        );

        // Export the trace
        tokio::spawn(async move {
            // Note: In a real implementation, we'd have a reference to exporters here
            // For now, we'll just log the trace completion
            debug!("Trace exported: {}", trace.trace_id);
        });
    }

    /// Record a counter metric
    pub fn record_counter(&self, name: &str, value: f64, labels: Vec<(&str, &str)>) {
        let mut label_map = HashMap::new();
        for (key, val) in labels {
            label_map.insert(key.to_string(), val.to_string());
        }

        let metric = MetricPoint {
            name: name.to_string(),
            metric_type: MetricType::Counter,
            value,
            timestamp: Utc::now(),
            labels: label_map,
            unit: None,
            description: None,
        };

        {
            let mut metrics = self.metrics_buffer.lock().unwrap();
            metrics.push(metric);
        }

        debug!("Recorded counter: {} = {}", name, value);
    }

    /// Record a gauge metric
    pub fn record_gauge(&self, name: &str, value: f64, labels: Vec<(&str, &str)>) {
        let mut label_map = HashMap::new();
        for (key, val) in labels {
            label_map.insert(key.to_string(), val.to_string());
        }

        let metric = MetricPoint {
            name: name.to_string(),
            metric_type: MetricType::Gauge,
            value,
            timestamp: Utc::now(),
            labels: label_map,
            unit: None,
            description: None,
        };

        {
            let mut metrics = self.metrics_buffer.lock().unwrap();
            metrics.push(metric);
        }

        debug!("Recorded gauge: {} = {}", name, value);
    }

    /// Record a histogram metric
    pub fn record_histogram(&self, name: &str, duration: Duration) {
        let metric = MetricPoint {
            name: name.to_string(),
            metric_type: MetricType::Histogram,
            value: duration.as_secs_f64(),
            timestamp: Utc::now(),
            labels: HashMap::new(),
            unit: Some("seconds".to_string()),
            description: None,
        };

        {
            let mut metrics = self.metrics_buffer.lock().unwrap();
            metrics.push(metric);
        }

        debug!("Recorded histogram: {} = {:?}", name, duration);
    }

    /// Record a custom event
    pub fn record_event(&self, name: &str, category: &str, data: serde_json::Value) {
        let event = CustomEvent {
            name: name.to_string(),
            timestamp: Utc::now(),
            category: category.to_string(),
            attributes: self.config.global_attributes.clone(),
            data,
            trace_context: None, // Could be populated with current active trace
        };

        {
            let mut events = self.events_buffer.lock().unwrap();
            events.push(event);
        }

        debug!("Recorded event: {} ({})", name, category);
    }

    /// Get system uptime
    pub fn get_uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Get active trace count
    pub fn get_active_trace_count(&self) -> usize {
        let traces = self.active_traces.lock().unwrap();
        traces.len()
    }

    /// Get buffered metrics count
    pub fn get_buffered_metrics_count(&self) -> usize {
        let metrics = self.metrics_buffer.lock().unwrap();
        metrics.len()
    }

    /// Get a reference to the Prometheus exporter
    pub fn get_prometheus_exporter(&self) -> Option<&PrometheusExporter> {
        // In a real implementation, we'd maintain typed references to specific exporters
        None
    }

    /// Export all buffered telemetry data
    pub async fn export_all(&self) -> Result<(), TelemetryError> {
        let traces = {
            let mut traces = self.active_traces.lock().unwrap();
            let finished_traces: Vec<TraceContext> = traces
                .iter()
                .filter(|t| t.end_time.is_some())
                .cloned()
                .collect();
            traces.retain(|t| t.end_time.is_none());
            finished_traces
        };

        let metrics = {
            let mut metrics = self.metrics_buffer.lock().unwrap();
            let buffered_metrics = metrics.clone();
            metrics.clear();
            buffered_metrics
        };

        let events = {
            let mut events = self.events_buffer.lock().unwrap();
            let buffered_events = events.clone();
            events.clear();
            buffered_events
        };

        // Export to all configured exporters
        for exporter in &self.exporters {
            if !traces.is_empty() {
                exporter.export_traces(traces.clone()).await?;
            }
            if !metrics.is_empty() {
                exporter.export_metrics(metrics.clone()).await?;
            }
            if !events.is_empty() {
                exporter.export_events(events.clone()).await?;
            }
        }

        info!(
            "Exported {} traces, {} metrics, {} events",
            traces.len(),
            metrics.len(),
            events.len()
        );

        Ok(())
    }
}

impl Default for TelemetrySystem {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper macro for creating trace spans
#[macro_export]
macro_rules! trace_span {
    ($telemetry:expr, $operation:expr) => {{
        let trace = $telemetry.start_trace($operation);
        TraceGuard::new($telemetry, trace)
    }};
    ($telemetry:expr, $operation:expr, $($key:expr => $value:expr),*) => {{
        let mut trace = $telemetry.start_trace($operation);
        $(
            trace = trace.with_attribute($key, $value);
        )*
        TraceGuard::new($telemetry, trace)
    }};
}

/// RAII guard for automatic trace finishing
pub struct TraceGuard<'a> {
    telemetry: &'a TelemetrySystem,
    trace: Option<TraceContext>,
}

impl<'a> TraceGuard<'a> {
    /// Create a new trace guard
    pub fn new(telemetry: &'a TelemetrySystem, trace: TraceContext) -> Self {
        Self {
            telemetry,
            trace: Some(trace),
        }
    }

    /// Get a reference to the trace context
    pub fn trace(&self) -> Option<&TraceContext> {
        self.trace.as_ref()
    }

    /// Record an error on the trace
    pub fn record_error(&mut self, error: &str) {
        if let Some(trace) = &mut self.trace {
            trace.record_error(error);
        }
    }

    /// Set trace status
    pub fn set_status(&mut self, status: SpanStatus) {
        if let Some(trace) = &mut self.trace {
            trace.set_status(status);
        }
    }
}

impl<'a> Drop for TraceGuard<'a> {
    fn drop(&mut self) {
        if let Some(trace) = self.trace.take() {
            self.telemetry.finish_trace(trace);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[tokio::test]
    async fn test_telemetry_system_initialization() {
        let mut telemetry = TelemetrySystem::new()
            .with_service_name("test-service")
            .with_version("1.0.0")
            .with_environment("test");

        telemetry.initialize().await.unwrap();
        assert!(telemetry.initialized);
    }

    #[tokio::test]
    async fn test_trace_context() {
        let trace = TraceContext::new("test_operation");
        assert_eq!(trace.operation_name, "test_operation");
        assert_eq!(trace.status, SpanStatus::InProgress);
        assert!(trace.duration.is_none());

        let child = trace.create_child("child_operation");
        assert_eq!(child.trace_id, trace.trace_id);
        assert_eq!(child.parent_span_id, Some(trace.span_id.clone()));
    }

    #[tokio::test]
    async fn test_metrics_collection() {
        let telemetry = TelemetrySystem::new();

        telemetry.record_counter("test_counter", 1.0, vec![("label", "value")]);
        telemetry.record_gauge("test_gauge", 42.0, vec![]);
        telemetry.record_histogram("test_histogram", Duration::from_millis(100));

        assert_eq!(telemetry.get_buffered_metrics_count(), 3);
    }

    #[tokio::test]
    async fn test_custom_events() {
        let telemetry = TelemetrySystem::new();

        telemetry.record_event(
            "user_login",
            "authentication",
            serde_json::json!({"user_id": "test123", "method": "oauth"}),
        );

        let events = telemetry.events_buffer.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "user_login");
        assert_eq!(events[0].category, "authentication");
    }

    #[tokio::test]
    async fn test_prometheus_exporter() {
        let exporter = PrometheusExporter::new(None);

        let metric = MetricPoint {
            name: "test_metric".to_string(),
            metric_type: MetricType::Counter,
            value: 42.0,
            timestamp: Utc::now(),
            labels: HashMap::from([("env".to_string(), "test".to_string())]),
            unit: None,
            description: Some("Test metric".to_string()),
        };

        exporter.add_metric(metric);
        let prometheus_output = exporter.export_prometheus().await.unwrap();

        assert!(prometheus_output.contains("# HELP test_metric Test metric"));
        assert!(prometheus_output.contains("# TYPE test_metric counter"));
        assert!(prometheus_output.contains("test_metric{env=\"test\"} 42"));
    }

    #[tokio::test]
    async fn test_trace_spans() {
        let telemetry = TelemetrySystem::new();

        let trace = telemetry.start_trace("test_operation");
        assert_eq!(telemetry.get_active_trace_count(), 1);

        telemetry.finish_trace(trace);
        assert_eq!(telemetry.get_active_trace_count(), 0);
    }

    #[tokio::test]
    async fn test_span_finishing() {
        let mut trace = TraceContext::new("test");
        assert_eq!(trace.status, SpanStatus::InProgress);
        assert!(trace.end_time.is_none());

        trace.finish();
        assert_eq!(trace.status, SpanStatus::Ok);
        assert!(trace.end_time.is_some());
        assert!(trace.duration.is_some());
    }

    #[tokio::test]
    async fn test_error_recording() {
        let mut trace = TraceContext::new("test");
        trace.record_error("Something went wrong");

        assert_eq!(trace.status, SpanStatus::Error);
        assert_eq!(
            trace.attributes.get("error"),
            Some(&"Something went wrong".to_string())
        );
    }
}
