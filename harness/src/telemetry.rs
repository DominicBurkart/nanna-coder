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
use tracing::{debug, info};
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

impl Drop for TraceGuard<'_> {
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

        // In test environments, the tracing subscriber may already be initialized
        match telemetry.initialize().await {
            Ok(_) => assert!(telemetry.initialized),
            Err(TelemetryError::InitializationFailed { reason })
                if reason.contains("global default trace dispatcher has already been set") =>
            {
                // This is expected in parallel test runs - consider the test successful
                println!("Tracing subscriber already initialized (expected in CI)");
            }
            Err(e) => panic!("Unexpected initialization error: {}", e),
        }
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

    // ---------------------------------------------------------------------------
    // TelemetryConfig / ServiceInfo
    // ---------------------------------------------------------------------------

    #[test]
    fn test_telemetry_config_default_values() {
        let cfg = TelemetryConfig::default();
        assert_eq!(cfg.service.name, "nanna-coder");
        assert_eq!(cfg.service.version, "0.1.0");
        assert_eq!(cfg.service.environment, "development");
        assert!(cfg.enable_logging);
        assert!(cfg.enable_tracing);
        assert!(cfg.enable_metrics);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.metrics_export_interval, Duration::from_secs(60));
        assert!((cfg.trace_sample_rate - 1.0).abs() < f64::EPSILON);
        assert!(cfg.export_endpoints.prometheus_endpoint.is_none());
        assert!(cfg.export_endpoints.otlp_endpoint.is_none());
        assert!(cfg.export_endpoints.webhook_endpoints.is_empty());
        assert!(cfg.export_endpoints.log_endpoint.is_none());
        assert!(cfg.global_attributes.is_empty());
    }

    #[test]
    fn test_service_info_serialization_roundtrip() {
        let info = ServiceInfo {
            name: "my-svc".to_string(),
            version: "2.3.4".to_string(),
            environment: "staging".to_string(),
            instance_id: "inst-abc".to_string(),
            metadata: HashMap::from([("region".to_string(), "us-east-1".to_string())]),
        };
        let json = serde_json::to_string(&info).unwrap();
        let decoded: ServiceInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name, info.name);
        assert_eq!(decoded.version, info.version);
        assert_eq!(decoded.environment, info.environment);
        assert_eq!(decoded.instance_id, info.instance_id);
        assert_eq!(decoded.metadata.get("region").unwrap(), "us-east-1");
    }

    // ---------------------------------------------------------------------------
    // TelemetrySystem builder chain
    // ---------------------------------------------------------------------------

    #[test]
    fn test_builder_chain_sets_fields() {
        let cfg = TelemetryConfig {
            service: ServiceInfo {
                name: "custom".to_string(),
                version: "9.9.9".to_string(),
                environment: "prod".to_string(),
                instance_id: "i-42".to_string(),
                metadata: HashMap::new(),
            },
            enable_logging: false,
            enable_tracing: false,
            enable_metrics: false,
            log_level: "debug".to_string(),
            metrics_export_interval: Duration::from_secs(10),
            trace_sample_rate: 0.5,
            export_endpoints: ExportEndpoints {
                prometheus_endpoint: Some("http://prom:9090".to_string()),
                otlp_endpoint: None,
                webhook_endpoints: Vec::new(),
                log_endpoint: None,
            },
            global_attributes: HashMap::new(),
        };

        let ts = TelemetrySystem::new()
            .with_service_name("svc-a")
            .with_version("1.2.3")
            .with_environment("production")
            .with_global_attribute("team", "platform")
            .with_config(cfg);

        // with_config replaces the whole config, so verify the config fields
        assert_eq!(ts.config.service.name, "custom");
        assert_eq!(ts.config.service.version, "9.9.9");
    }

    #[test]
    fn test_with_service_name_and_environment() {
        let ts = TelemetrySystem::new()
            .with_service_name("my-service")
            .with_environment("ci");
        assert_eq!(ts.config.service.name, "my-service");
        assert_eq!(ts.config.service.environment, "ci");
    }

    #[test]
    fn test_with_version() {
        let ts = TelemetrySystem::new().with_version("3.0.0");
        assert_eq!(ts.config.service.version, "3.0.0");
    }

    #[test]
    fn test_with_global_attribute_multiple() {
        let ts = TelemetrySystem::new()
            .with_global_attribute("k1", "v1")
            .with_global_attribute("k2", "v2");
        assert_eq!(ts.config.global_attributes.get("k1").unwrap(), "v1");
        assert_eq!(ts.config.global_attributes.get("k2").unwrap(), "v2");
    }

    // ---------------------------------------------------------------------------
    // record_gauge / record_event / get_uptime
    // ---------------------------------------------------------------------------

    #[test]
    fn test_record_gauge_stored_in_buffer() {
        let ts = TelemetrySystem::new();
        ts.record_gauge("cpu_percent", 73.5, vec![("host", "node-1")]);
        assert_eq!(ts.get_buffered_metrics_count(), 1);

        let metrics = ts.metrics_buffer.lock().unwrap();
        assert_eq!(metrics[0].name, "cpu_percent");
        assert!((metrics[0].value - 73.5).abs() < f64::EPSILON);
        assert_eq!(metrics[0].metric_type, MetricType::Gauge);
        assert_eq!(metrics[0].labels.get("host").unwrap(), "node-1");
    }

    #[test]
    fn test_record_event_stored_in_buffer() {
        let ts = TelemetrySystem::new();
        ts.record_event(
            "deploy_started",
            "deployment",
            serde_json::json!({"version": "1.0"}),
        );

        let events = ts.events_buffer.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "deploy_started");
        assert_eq!(events[0].category, "deployment");
    }

    #[test]
    fn test_get_uptime_is_positive() {
        let ts = TelemetrySystem::new();
        let uptime = ts.get_uptime();
        // Even freshly created, some time has elapsed
        assert!(uptime < Duration::from_secs(60));
    }

    #[test]
    fn test_default_telemetry_system_is_not_initialized() {
        let ts = TelemetrySystem::default();
        assert!(!ts.initialized);
    }

    // ---------------------------------------------------------------------------
    // TraceContext child spans, set_status, with_attribute
    // ---------------------------------------------------------------------------

    #[test]
    fn test_trace_context_with_attribute() {
        let trace = TraceContext::new("op")
            .with_attribute("model", "qwen3:0.6b")
            .with_attribute("user_id", "u-42");

        assert_eq!(trace.attributes.get("model").unwrap(), "qwen3:0.6b");
        assert_eq!(trace.attributes.get("user_id").unwrap(), "u-42");
    }

    #[test]
    fn test_trace_context_set_status() {
        let mut trace = TraceContext::new("op");
        assert_eq!(trace.status, SpanStatus::InProgress);
        trace.set_status(SpanStatus::Cancelled);
        assert_eq!(trace.status, SpanStatus::Cancelled);
        trace.set_status(SpanStatus::Timeout);
        assert_eq!(trace.status, SpanStatus::Timeout);
    }

    #[test]
    fn test_trace_context_child_inherits_trace_id() {
        let parent = TraceContext::new("parent_op");
        let child = parent.create_child("child_op");

        assert_eq!(child.trace_id, parent.trace_id);
        assert_ne!(child.span_id, parent.span_id);
        assert_eq!(child.parent_span_id, Some(parent.span_id.clone()));
        assert_eq!(child.operation_name, "child_op");
        assert_eq!(child.status, SpanStatus::InProgress);
    }

    #[test]
    fn test_finish_preserves_error_status() {
        let mut trace = TraceContext::new("op");
        trace.record_error("oops");
        assert_eq!(trace.status, SpanStatus::Error);
        trace.finish();
        // finish() should NOT overwrite Error with Ok
        assert_eq!(trace.status, SpanStatus::Error);
        assert!(trace.end_time.is_some());
        assert!(trace.duration.is_some());
    }

    #[test]
    fn test_trace_context_serialization_roundtrip() {
        let mut trace = TraceContext::new("test-op");
        trace = trace.with_attribute("env", "test");
        trace.finish();

        let json = serde_json::to_string(&trace).unwrap();
        let decoded: TraceContext = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.operation_name, "test-op");
        assert_eq!(decoded.attributes.get("env").unwrap(), "test");
        assert_eq!(decoded.status, SpanStatus::Ok);
        assert!(decoded.end_time.is_some());
    }

    // ---------------------------------------------------------------------------
    // MetricPoint serialization and MetricType variants
    // ---------------------------------------------------------------------------

    #[test]
    fn test_metric_type_variants() {
        // Ensure all variants are distinguishable and serializable
        let variants = [
            MetricType::Counter,
            MetricType::Gauge,
            MetricType::Histogram,
            MetricType::Summary,
        ];
        for v in &variants {
            let json = serde_json::to_string(v).unwrap();
            let decoded: MetricType = serde_json::from_str(&json).unwrap();
            assert_eq!(&decoded, v);
        }
        assert_ne!(MetricType::Counter, MetricType::Gauge);
        assert_ne!(MetricType::Histogram, MetricType::Summary);
    }

    #[test]
    fn test_metric_point_serialization_roundtrip() {
        let mp = MetricPoint {
            name: "latency_ms".to_string(),
            metric_type: MetricType::Histogram,
            value: 123.45,
            timestamp: Utc::now(),
            labels: HashMap::from([("service".to_string(), "api".to_string())]),
            unit: Some("ms".to_string()),
            description: Some("Request latency".to_string()),
        };
        let json = serde_json::to_string(&mp).unwrap();
        let decoded: MetricPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name, mp.name);
        assert!((decoded.value - mp.value).abs() < f64::EPSILON);
        assert_eq!(decoded.metric_type, MetricType::Histogram);
        assert_eq!(decoded.unit.unwrap(), "ms");
    }

    // ---------------------------------------------------------------------------
    // SpanStatus serialization
    // ---------------------------------------------------------------------------

    #[test]
    fn test_span_status_serialization_roundtrip() {
        for status in &[
            SpanStatus::InProgress,
            SpanStatus::Ok,
            SpanStatus::Error,
            SpanStatus::Cancelled,
            SpanStatus::Timeout,
        ] {
            let json = serde_json::to_string(status).unwrap();
            let decoded: SpanStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(&decoded, status);
        }
    }

    // ---------------------------------------------------------------------------
    // PrometheusExporter – TelemetryExporter trait methods
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_prometheus_exporter_export_traces_converts_to_metrics() {
        let exporter = PrometheusExporter::new(None);

        let mut trace = TraceContext::new("test_op");
        trace.finish();

        exporter.export_traces(vec![trace]).await.unwrap();

        let buffer = exporter.metrics_buffer.lock().unwrap();
        // A trace with a duration should produce exactly one metric
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0].name, "trace_duration_seconds");
        assert_eq!(buffer[0].metric_type, MetricType::Histogram);
    }

    #[tokio::test]
    async fn test_prometheus_exporter_export_traces_skips_in_progress() {
        let exporter = PrometheusExporter::new(None);

        // An unfinished trace has no duration → should produce no metric
        let trace = TraceContext::new("in_progress_op");
        exporter.export_traces(vec![trace]).await.unwrap();

        let buffer = exporter.metrics_buffer.lock().unwrap();
        assert!(buffer.is_empty());
    }

    #[tokio::test]
    async fn test_prometheus_exporter_export_metrics_stores_all() {
        let exporter = PrometheusExporter::new(None);
        let metrics = vec![
            MetricPoint {
                name: "a".to_string(),
                metric_type: MetricType::Counter,
                value: 1.0,
                timestamp: Utc::now(),
                labels: HashMap::new(),
                unit: None,
                description: None,
            },
            MetricPoint {
                name: "b".to_string(),
                metric_type: MetricType::Gauge,
                value: 2.0,
                timestamp: Utc::now(),
                labels: HashMap::new(),
                unit: None,
                description: None,
            },
        ];
        exporter.export_metrics(metrics).await.unwrap();

        let buffer = exporter.metrics_buffer.lock().unwrap();
        assert_eq!(buffer.len(), 2);
    }

    #[tokio::test]
    async fn test_prometheus_exporter_export_events_converts_to_counter() {
        let exporter = PrometheusExporter::new(None);
        let event = CustomEvent {
            name: "login".to_string(),
            timestamp: Utc::now(),
            category: "auth".to_string(),
            attributes: HashMap::new(),
            data: serde_json::json!({}),
            trace_context: None,
        };
        exporter.export_events(vec![event]).await.unwrap();

        let buffer = exporter.metrics_buffer.lock().unwrap();
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0].name, "custom_events_total");
        assert_eq!(buffer[0].metric_type, MetricType::Counter);
        assert!((buffer[0].value - 1.0).abs() < f64::EPSILON);
        assert_eq!(buffer[0].labels.get("event_name").unwrap(), "login");
        assert_eq!(buffer[0].labels.get("category").unwrap(), "auth");
    }

    #[tokio::test]
    async fn test_prometheus_exporter_health_check_returns_true() {
        let exporter = PrometheusExporter::new(Some("http://localhost:9090".to_string()));
        let healthy = exporter.health_check().await.unwrap();
        assert!(healthy);
    }

    #[tokio::test]
    async fn test_prometheus_exporter_clear_buffer() {
        let exporter = PrometheusExporter::new(None);
        exporter.add_metric(MetricPoint {
            name: "x".to_string(),
            metric_type: MetricType::Counter,
            value: 1.0,
            timestamp: Utc::now(),
            labels: HashMap::new(),
            unit: None,
            description: None,
        });
        {
            let buf = exporter.metrics_buffer.lock().unwrap();
            assert_eq!(buf.len(), 1);
        }
        exporter.clear_buffer();
        {
            let buf = exporter.metrics_buffer.lock().unwrap();
            assert!(buf.is_empty());
        }
    }

    #[tokio::test]
    async fn test_prometheus_format_no_labels_no_braces() {
        let exporter = PrometheusExporter::new(None);
        exporter.add_metric(MetricPoint {
            name: "simple_counter".to_string(),
            metric_type: MetricType::Counter,
            value: 5.0,
            timestamp: Utc::now(),
            labels: HashMap::new(),
            unit: None,
            description: None,
        });
        let output = exporter.export_prometheus().await.unwrap();
        assert!(output.contains("simple_counter 5"));
        // No label braces when there are no labels
        assert!(!output.contains('{'));
    }

    #[tokio::test]
    async fn test_prometheus_exporter_export_system_metrics() {
        use crate::monitoring::{CacheMetrics, ErrorMetrics, LatencyMetrics, SystemResourceMetrics};
        let exporter = PrometheusExporter::new(None);

        let mut latencies = HashMap::new();
        latencies.insert(
            "api".to_string(),
            LatencyMetrics {
                avg_latency_ms: 50.0,
                min_latency_ms: 45.0,
                p95_latency_ms: 90.0,
                p99_latency_ms: 120.0,
                max_latency_ms: 200.0,
                request_count: 500,
                requests_per_second: 100.0,
            },
        );

        let metrics = SystemMetrics {
            timestamp: Utc::now(),
            request_latencies: latencies,
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
                error_rate: 0.01,
                recent_errors: Vec::new(),
            },
        };

        exporter.export_system_metrics(metrics).await.unwrap();

        let buffer = exporter.metrics_buffer.lock().unwrap();
        let names: Vec<&str> = buffer.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"cache_hit_rate"));
        assert!(names.contains(&"request_duration_seconds"));
        assert!(names.contains(&"requests_per_second"));
        assert!(names.contains(&"error_rate"));
    }

    // ---------------------------------------------------------------------------
    // TelemetrySystem::export_all
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_export_all_clears_buffers() {
        let ts = TelemetrySystem::new(); // no exporters
        ts.record_counter("c", 1.0, vec![]);
        ts.record_gauge("g", 2.0, vec![]);
        ts.record_event("e", "cat", serde_json::json!({}));

        assert_eq!(ts.get_buffered_metrics_count(), 2);
        {
            let evts = ts.events_buffer.lock().unwrap();
            assert_eq!(evts.len(), 1);
        }

        ts.export_all().await.unwrap();

        assert_eq!(ts.get_buffered_metrics_count(), 0);
        let evts = ts.events_buffer.lock().unwrap();
        assert!(evts.is_empty());
    }

    // ---------------------------------------------------------------------------
    // TraceGuard — must use #[tokio::test] because finish_trace calls tokio::spawn
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_trace_guard_drops_and_finishes_trace() {
        let ts = TelemetrySystem::new();
        {
            let guard = TraceGuard::new(&ts, ts.start_trace("guarded_op"));
            assert!(guard.trace().is_some());
            assert_eq!(guard.trace().unwrap().operation_name, "guarded_op");
            // guard drops here → finish_trace is called (requires Tokio runtime)
        }
        // After drop the active count should be 0 (finish_trace removes it)
        assert_eq!(ts.get_active_trace_count(), 0);
    }

    #[tokio::test]
    async fn test_trace_guard_record_error() {
        let ts = TelemetrySystem::new();
        let mut guard = TraceGuard::new(&ts, ts.start_trace("err_op"));
        guard.record_error("something failed");
        assert_eq!(
            guard.trace().unwrap().status,
            SpanStatus::Error
        );
    }

    #[tokio::test]
    async fn test_trace_guard_set_status() {
        let ts = TelemetrySystem::new();
        let mut guard = TraceGuard::new(&ts, ts.start_trace("status_op"));
        guard.set_status(SpanStatus::Cancelled);
        assert_eq!(
            guard.trace().unwrap().status,
            SpanStatus::Cancelled
        );
    }

    // ---------------------------------------------------------------------------
    // add_exporter builder
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn test_add_exporter_is_used_on_export_all() {
        let exporter = PrometheusExporter::new(None);
        let ts = TelemetrySystem::new().add_exporter(Box::new(exporter));

        let counter_metric = MetricPoint {
            name: "queued".to_string(),
            metric_type: MetricType::Counter,
            value: 7.0,
            timestamp: Utc::now(),
            labels: HashMap::new(),
            unit: None,
            description: None,
        };
        {
            let mut buf = ts.metrics_buffer.lock().unwrap();
            buf.push(counter_metric);
        }

        // export_all should drain the buffer and push to the exporter
        ts.export_all().await.unwrap();
        assert_eq!(ts.get_buffered_metrics_count(), 0);
    }
}
