//! Integrated observability system
//!
//! This module provides a comprehensive observability solution that integrates
//! monitoring, telemetry, alerting, and container health management into a unified system.
//!
//! # Features
//!
//! - Unified observability dashboard
//! - Integrated container health monitoring with smart alerting
//! - Automatic performance regression detection
//! - Multi-level alerting with escalation policies
//! - Real-time system health visualization
//! - Integration with external monitoring systems
//!
//! # Examples
//!
//! ```rust
//! use harness::observability::{ObservabilitySystem, AlertPolicy, HealthThreshold};
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let mut observability = ObservabilitySystem::new()
//!     .with_service_name("nanna-coder")
//!     .with_alert_policy(AlertPolicy::immediate_critical())
//!     .with_health_check_interval(Duration::from_secs(30));
//!
//! observability.initialize().await?;
//! observability.start_monitoring().await?;
//!
//! // The system will automatically monitor containers, models, and system health
//! // and send alerts when thresholds are exceeded
//!
//! let status = observability.get_comprehensive_status().await?;
//! println!("System health: {:?}", status.overall_health);
//! # Ok(())
//! # }
//! ```

use crate::container::{detect_runtime, ContainerRuntime};
use crate::monitoring::{
    AlertSeverity, HealthStatus, MonitoringError, MonitoringSystem, SystemMetrics,
};
use crate::telemetry::{TelemetryError, TelemetrySystem};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::time::interval;
use tracing::{debug, error, info};

/// Comprehensive observability errors
#[derive(Error, Debug)]
pub enum ObservabilityError {
    /// System initialization failed
    #[error("Observability system initialization failed: {reason}")]
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

    /// Health check failed
    #[error("Health check failed for {component}: {reason}")]
    HealthCheckFailed { component: String, reason: String },

    /// Configuration error
    #[error("Configuration error: {reason}")]
    ConfigurationError { reason: String },

    /// Monitoring error
    #[error("Monitoring error: {0}")]
    Monitoring(#[from] MonitoringError),

    /// Telemetry error
    #[error("Telemetry error: {0}")]
    Telemetry(#[from] TelemetryError),
}

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

/// Derive an [`SlaStatus`] from an observed availability and a target.
///
/// Pure function so it can be tested independently of any system state:
/// - `Compliant` when `availability >= target`
/// - `AtRisk` when availability is within ~1 percentage point below target
///   (i.e. `>= target - 0.9`), reflecting "below target but not yet breached"
/// - `Breached` otherwise
pub fn sla_status_for(availability: f64, target: f64) -> SlaStatus {
    if availability >= target {
        SlaStatus::Compliant
    } else if availability >= target - 0.9 {
        SlaStatus::AtRisk
    } else {
        SlaStatus::Breached
    }
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
                    escalation_time: Duration::from_secs(300), // 5 minutes
                    max_escalations: 3,
                    escalation_factor: 2.0,
                },
                EscalationRule {
                    severity: AlertSeverity::Error,
                    escalation_time: Duration::from_secs(900), // 15 minutes
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
                    escalation_time: Duration::from_secs(600), // 10 minutes
                    max_escalations: 2,
                    escalation_factor: 1.5,
                },
                EscalationRule {
                    severity: AlertSeverity::Error,
                    escalation_time: Duration::from_secs(1800), // 30 minutes
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

/// Health check thresholds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthThreshold {
    /// CPU usage threshold (percentage)
    pub cpu_threshold: f64,
    /// Memory usage threshold (percentage)
    pub memory_threshold: f64,
    /// Disk usage threshold (percentage)
    pub disk_threshold: f64,
    /// Maximum latency (milliseconds)
    pub max_latency_ms: u64,
    /// Minimum cache hit rate
    pub min_cache_hit_rate: f64,
    /// Maximum error rate
    pub max_error_rate: f64,
    /// Container health check timeout
    pub container_timeout: Duration,
}

impl Default for HealthThreshold {
    fn default() -> Self {
        Self {
            cpu_threshold: 80.0,
            memory_threshold: 85.0,
            disk_threshold: 90.0,
            max_latency_ms: 2000,
            min_cache_hit_rate: 0.8,
            max_error_rate: 0.05,
            container_timeout: Duration::from_secs(30),
        }
    }
}

/// Comprehensive observability system
pub struct ObservabilitySystem {
    /// Service name
    service_name: String,
    /// Monitoring system
    monitoring: MonitoringSystem,
    /// Telemetry system
    telemetry: TelemetrySystem,
    /// Alert policy
    alert_policy: AlertPolicy,
    /// Health thresholds
    health_thresholds: HealthThreshold,
    /// Health check interval
    health_check_interval: Duration,
    /// Component health history
    health_history: Arc<std::sync::Mutex<HashMap<String, Vec<HealthHistoryEntry>>>>,
    /// System start time
    start_time: Instant,
    /// Background monitoring task
    monitoring_task: Option<tokio::task::JoinHandle<()>>,
    /// Container runtime
    container_runtime: ContainerRuntime,
}

impl ObservabilitySystem {
    /// Create a new observability system
    pub fn new() -> Self {
        Self {
            service_name: "nanna-coder".to_string(),
            monitoring: MonitoringSystem::new(),
            telemetry: TelemetrySystem::new(),
            alert_policy: AlertPolicy::balanced(),
            health_thresholds: HealthThreshold::default(),
            health_check_interval: Duration::from_secs(60),
            health_history: Arc::new(std::sync::Mutex::new(HashMap::new())),
            start_time: Instant::now(),
            monitoring_task: None,
            container_runtime: detect_runtime(),
        }
    }

    /// Set service name
    pub fn with_service_name(mut self, name: &str) -> Self {
        self.service_name = name.to_string();
        self.telemetry = self.telemetry.with_service_name(name);
        self
    }

    /// Set alert policy
    pub fn with_alert_policy(mut self, policy: AlertPolicy) -> Self {
        self.alert_policy = policy;
        self
    }

    /// Set health thresholds
    pub fn with_health_thresholds(mut self, thresholds: HealthThreshold) -> Self {
        self.health_thresholds = thresholds;
        self
    }

    /// Set health check interval
    pub fn with_health_check_interval(mut self, interval: Duration) -> Self {
        self.health_check_interval = interval;
        self
    }

    /// Initialize the observability system
    pub async fn initialize(&mut self) -> Result<(), ObservabilityError> {
        info!(
            "Initializing observability system for service: {}",
            self.service_name
        );

        // Initialize telemetry
        self.telemetry
            .initialize()
            .await
            .map_err(|e| ObservabilityError::TelemetryFailed {
                reason: e.to_string(),
            })?;

        // Initialize monitoring
        self.monitoring.start_monitoring().await.map_err(|e| {
            ObservabilityError::MonitoringFailed {
                reason: e.to_string(),
            }
        })?;

        info!("Observability system initialized successfully");
        Ok(())
    }

    /// Start comprehensive monitoring
    pub async fn start_monitoring(&mut self) -> Result<(), ObservabilityError> {
        info!(
            "Starting comprehensive monitoring with {}s interval",
            self.health_check_interval.as_secs()
        );

        let health_history = Arc::clone(&self.health_history);
        let health_interval = self.health_check_interval;
        let thresholds = self.health_thresholds.clone();
        let runtime = self.container_runtime.clone();

        let task = tokio::spawn(async move {
            let mut interval = interval(health_interval);

            loop {
                interval.tick().await;
                debug!("Performing comprehensive health check");

                // Perform health checks
                if let Err(e) =
                    Self::perform_health_checks(&health_history, &thresholds, &runtime).await
                {
                    error!("Health check failed: {}", e);
                }

                // Analyze performance trends
                if let Err(e) = Self::analyze_performance_trends().await {
                    error!("Performance trend analysis failed: {}", e);
                }

                // Check SLA compliance
                if let Err(e) = Self::check_sla_compliance().await {
                    error!("SLA compliance check failed: {}", e);
                }
            }
        });

        self.monitoring_task = Some(task);
        Ok(())
    }

    /// Stop monitoring
    pub async fn stop_monitoring(&mut self) {
        if let Some(task) = self.monitoring_task.take() {
            task.abort();
            info!("Comprehensive monitoring stopped");
        }

        self.monitoring.stop_monitoring().await;
    }

    /// Get comprehensive system status
    pub async fn get_comprehensive_status(
        &self,
    ) -> Result<ComprehensiveStatus, ObservabilityError> {
        let start_time = Instant::now();
        let trace = self.telemetry.start_trace("get_comprehensive_status");

        // Get base system status
        let system_status = self.monitoring.get_system_status().await?;

        // Get detailed component health
        let component_health = self.get_component_health().await?;

        // Analyze performance trends
        let performance_trends = self.analyze_current_trends(&system_status.metrics)?;

        // Get container summary
        let container_summary = self.get_container_summary().await?;

        // Get model summary
        let model_summary = self.get_model_summary(&system_status.metrics)?;

        // Calculate availability metrics
        let availability_metrics = self.calculate_availability_metrics()?;

        // Convert alerts to enhanced format
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

        self.telemetry.finish_trace(trace);

        // Record status retrieval metrics
        self.telemetry
            .record_histogram("status_retrieval_duration", start_time.elapsed());

        Ok(comprehensive_status)
    }

    /// Get detailed component health
    async fn get_component_health(
        &self,
    ) -> Result<HashMap<String, ComponentHealth>, ObservabilityError> {
        let health_checks = self
            .monitoring
            .health_monitor
            .comprehensive_health_check()
            .await?;
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
                metrics: HashMap::new(), // Would be populated with component-specific metrics
            };

            component_health.insert(check.component, component);
        }

        Ok(component_health)
    }

    /// Analyze current performance trends
    fn analyze_current_trends(
        &self,
        metrics: &SystemMetrics,
    ) -> Result<PerformanceTrends, ObservabilityError> {
        // This is a simplified implementation - in reality, we'd analyze historical data
        let latency_trend = if metrics
            .request_latencies
            .values()
            .any(|l| l.avg_latency_ms > self.health_thresholds.max_latency_ms as f64)
        {
            TrendDirection::Degrading
        } else {
            TrendDirection::Stable
        };

        let error_rate_trend =
            if metrics.error_metrics.error_rate > self.health_thresholds.max_error_rate {
                TrendDirection::Degrading
            } else {
                TrendDirection::Stable
            };

        let cache_performance_trend =
            if metrics.cache_metrics.hit_rate < self.health_thresholds.min_cache_hit_rate {
                TrendDirection::Degrading
            } else {
                TrendDirection::Stable
            };

        // Calculate overall performance score
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
    async fn get_container_summary(&self) -> Result<ContainerSummary, ObservabilityError> {
        // In a real implementation, we'd query actual container status
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
    fn get_model_summary(
        &self,
        metrics: &SystemMetrics,
    ) -> Result<ModelSummary, ObservabilityError> {
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
    fn calculate_availability_metrics(&self) -> Result<AvailabilityMetrics, ObservabilityError> {
        let uptime = self.start_time.elapsed();
        let availability = 99.5; // Would be calculated from actual downtime
        let target = 99.9;

        Ok(AvailabilityMetrics {
            uptime,
            availability_percentage: availability,
            mtbf: Some(Duration::from_secs(86400)), // 24 hours
            mttr: Some(Duration::from_secs(300)),   // 5 minutes
            sla_compliance: SlaCompliance {
                target_availability: target,
                current_availability: availability,
                status: sla_status_for(availability, target),
                time_to_breach: None,
            },
        })
    }

    /// Enhance alerts with additional context
    async fn enhance_alerts(
        &self,
        alerts: Vec<crate::monitoring::Alert>,
    ) -> Result<Vec<AlertInfo>, ObservabilityError> {
        let mut enhanced_alerts = Vec::new();

        for alert in alerts {
            let category = self.determine_alert_category(&alert);
            let priority_score = self.calculate_priority_score(&alert, &category);
            let recommended_actions = self.generate_recommended_actions(&alert, &category);

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
                related_metrics: HashMap::new(), // Would be populated with relevant metrics
                escalation_status: EscalationStatus::New,
            };

            enhanced_alerts.push(enhanced);
        }

        Ok(enhanced_alerts)
    }

    /// Determine alert category
    fn determine_alert_category(&self, alert: &crate::monitoring::Alert) -> AlertCategory {
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
    fn calculate_priority_score(
        &self,
        alert: &crate::monitoring::Alert,
        category: &AlertCategory,
    ) -> u32 {
        let mut score = match alert.severity {
            AlertSeverity::Critical => 100,
            AlertSeverity::Error => 75,
            AlertSeverity::Warning => 50,
            AlertSeverity::Info => 25,
        };

        // Adjust based on category
        match category {
            AlertCategory::Availability | AlertCategory::ContainerHealth => score += 20,
            AlertCategory::Security => score += 30,
            AlertCategory::Performance => score += 10,
            _ => {}
        }

        score.min(100)
    }

    /// Generate recommended actions
    fn generate_recommended_actions(
        &self,
        _alert: &crate::monitoring::Alert,
        category: &AlertCategory,
    ) -> Vec<String> {
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

    /// Perform comprehensive health checks
    async fn perform_health_checks(
        health_history: &Arc<std::sync::Mutex<HashMap<String, Vec<HealthHistoryEntry>>>>,
        _thresholds: &HealthThreshold,
        _runtime: &ContainerRuntime,
    ) -> Result<(), ObservabilityError> {
        // This would perform actual health checks and update history
        let mut history = health_history.lock().unwrap();

        // Add a mock health check entry
        let entry = HealthHistoryEntry {
            timestamp: Utc::now(),
            status: HealthStatus::Healthy,
            duration: Duration::from_millis(50),
        };

        history.entry("system".to_string()).or_default().push(entry);

        // Keep only last 50 entries per component
        for entries in history.values_mut() {
            if entries.len() > 50 {
                entries.drain(0..entries.len() - 50);
            }
        }

        Ok(())
    }

    /// Analyze performance trends
    async fn analyze_performance_trends() -> Result<(), ObservabilityError> {
        // This would analyze historical performance data
        debug!("Analyzing performance trends");
        Ok(())
    }

    /// Check SLA compliance
    async fn check_sla_compliance() -> Result<(), ObservabilityError> {
        // This would check SLA compliance and trigger alerts if needed
        debug!("Checking SLA compliance");
        Ok(())
    }

    /// Get system uptime
    pub fn get_uptime(&self) -> Duration {
        self.start_time.elapsed()
    }
}

impl Default for ObservabilitySystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_observability_system_initialization() {
        let mut system = ObservabilitySystem::new()
            .with_service_name("test-service")
            .with_health_check_interval(Duration::from_secs(10));

        // Initialization might fail due to global tracing subscriber already being set
        // This is expected in test environments
        let result = system.initialize().await;
        if result.is_ok() {
            assert!(system.get_uptime() < Duration::from_secs(1));
        } else {
            // Expected failure in test environment - tracing subscriber already set
            println!("Expected failure: {:?}", result);
        }
    }

    #[tokio::test]
    async fn test_comprehensive_status() {
        let mut system = ObservabilitySystem::new();

        // Skip initialization if tracing subscriber is already set
        let _ = system.initialize().await;

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
    async fn test_health_thresholds() {
        let thresholds = HealthThreshold::default();
        assert!(thresholds.cpu_threshold > 0.0);
        assert!(thresholds.memory_threshold > 0.0);
        assert!(thresholds.max_latency_ms > 0);
    }

    #[tokio::test]
    async fn test_performance_trends() {
        let system = ObservabilitySystem::new();
        let metrics = SystemMetrics {
            timestamp: Utc::now(),
            request_latencies: HashMap::new(),
            cache_metrics: crate::monitoring::CacheMetrics {
                hits: 80,
                misses: 20,
                hit_rate: 0.8,
                size_bytes: 1024,
                item_count: 100,
                evictions: 5,
            },
            container_metrics: Vec::new(),
            system_resources: crate::monitoring::SystemResourceMetrics {
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
            error_metrics: crate::monitoring::ErrorMetrics {
                total_errors: 0,
                errors_by_type: HashMap::new(),
                error_rate: 0.0,
                recent_errors: Vec::new(),
            },
        };

        let trends = system.analyze_current_trends(&metrics).unwrap();
        assert!(trends.performance_score >= 0.0 && trends.performance_score <= 100.0);
    }

    #[test]
    fn test_trend_direction_degrading_on_high_error_rate() {
        let mut system = ObservabilitySystem::new();
        // Set max_error_rate threshold to 0.05 (default)
        system.health_thresholds.max_error_rate = 0.05;

        let metrics = SystemMetrics {
            timestamp: Utc::now(),
            request_latencies: HashMap::new(),
            cache_metrics: crate::monitoring::CacheMetrics {
                hits: 80,
                misses: 20,
                hit_rate: 0.8,
                size_bytes: 1024,
                item_count: 100,
                evictions: 0,
            },
            container_metrics: Vec::new(),
            system_resources: crate::monitoring::SystemResourceMetrics {
                cpu_usage_percent: 10.0,
                total_memory_bytes: 8000000000,
                used_memory_bytes: 2000000000,
                memory_usage_percent: 25.0,
                available_disk_bytes: 100000000000,
                total_disk_bytes: 200000000000,
                disk_usage_percent: 50.0,
                load_average: [0.5, 0.5, 0.5],
            },
            model_metrics: HashMap::new(),
            error_metrics: crate::monitoring::ErrorMetrics {
                total_errors: 100,
                errors_by_type: HashMap::new(),
                error_rate: 0.20, // 20% > threshold
                recent_errors: Vec::new(),
            },
        };

        let trends = system.analyze_current_trends(&metrics).unwrap();
        assert_eq!(trends.error_rate_trend, TrendDirection::Degrading);
        // Score should be reduced by 25 for error rate degradation
        assert!(trends.performance_score <= 75.0);
    }

    #[test]
    fn test_trend_direction_degrading_on_low_cache_rate() {
        let mut system = ObservabilitySystem::new();
        system.health_thresholds.min_cache_hit_rate = 0.8;

        let metrics = SystemMetrics {
            timestamp: Utc::now(),
            request_latencies: HashMap::new(),
            cache_metrics: crate::monitoring::CacheMetrics {
                hits: 40,
                misses: 60,
                hit_rate: 0.4, // 40% < threshold of 80%
                size_bytes: 1024,
                item_count: 100,
                evictions: 0,
            },
            container_metrics: Vec::new(),
            system_resources: crate::monitoring::SystemResourceMetrics {
                cpu_usage_percent: 10.0,
                total_memory_bytes: 8000000000,
                used_memory_bytes: 2000000000,
                memory_usage_percent: 25.0,
                available_disk_bytes: 100000000000,
                total_disk_bytes: 200000000000,
                disk_usage_percent: 50.0,
                load_average: [0.5, 0.5, 0.5],
            },
            model_metrics: HashMap::new(),
            error_metrics: crate::monitoring::ErrorMetrics {
                total_errors: 0,
                errors_by_type: HashMap::new(),
                error_rate: 0.0,
                recent_errors: Vec::new(),
            },
        };

        let trends = system.analyze_current_trends(&metrics).unwrap();
        assert_eq!(trends.cache_performance_trend, TrendDirection::Degrading);
        // Score should be reduced by 15 for cache degradation
        assert!(trends.performance_score <= 85.0);
    }

    #[test]
    fn test_trend_direction_all_stable() {
        let system = ObservabilitySystem::new();
        let thresholds = &system.health_thresholds;

        let metrics = SystemMetrics {
            timestamp: Utc::now(),
            request_latencies: HashMap::new(),
            cache_metrics: crate::monitoring::CacheMetrics {
                hits: 90,
                misses: 10,
                hit_rate: 0.9, // above min_cache_hit_rate (0.8)
                size_bytes: 1024,
                item_count: 100,
                evictions: 0,
            },
            container_metrics: Vec::new(),
            system_resources: crate::monitoring::SystemResourceMetrics {
                cpu_usage_percent: 10.0,
                total_memory_bytes: 8000000000,
                used_memory_bytes: 2000000000,
                memory_usage_percent: 25.0,
                available_disk_bytes: 100000000000,
                total_disk_bytes: 200000000000,
                disk_usage_percent: 50.0,
                load_average: [0.5, 0.5, 0.5],
            },
            model_metrics: HashMap::new(),
            error_metrics: crate::monitoring::ErrorMetrics {
                total_errors: 0,
                errors_by_type: HashMap::new(),
                error_rate: 0.0, // below max_error_rate (0.05)
                recent_errors: Vec::new(),
            },
        };

        assert!(thresholds.min_cache_hit_rate <= 0.9);
        assert!(thresholds.max_error_rate >= 0.0);

        let trends = system.analyze_current_trends(&metrics).unwrap();
        assert_eq!(trends.error_rate_trend, TrendDirection::Stable);
        assert_eq!(trends.cache_performance_trend, TrendDirection::Stable);
        assert_eq!(trends.performance_score, 100.0);
    }

    #[test]
    fn test_alert_category_container_keyword() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "a1".to_string(),
            title: "Test Alert".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Warning,
            component: "container_service".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let category = system.determine_alert_category(&alert);
        assert_eq!(category, AlertCategory::ContainerHealth);
    }

    #[test]
    fn test_alert_category_model_keyword() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "a2".to_string(),
            title: "Model Issue".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Warning,
            component: "model_inference".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let category = system.determine_alert_category(&alert);
        assert_eq!(category, AlertCategory::ModelQuality);
    }

    #[test]
    fn test_alert_category_performance_keyword_in_title() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "a3".to_string(),
            title: "Performance degradation detected".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Warning,
            component: "api_gateway".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let category = system.determine_alert_category(&alert);
        assert_eq!(category, AlertCategory::Performance);
    }

    #[test]
    fn test_alert_category_resource_keyword_in_title() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "a4".to_string(),
            title: "High resource usage".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Warning,
            component: "scheduler".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let category = system.determine_alert_category(&alert);
        assert_eq!(category, AlertCategory::Resources);
    }

    #[test]
    fn test_alert_category_fallback_availability() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "a5".to_string(),
            title: "Some other issue".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Warning,
            component: "generic_service".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let category = system.determine_alert_category(&alert);
        assert_eq!(category, AlertCategory::Availability);
    }

    #[test]
    fn test_priority_score_critical_capped_at_100() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "p1".to_string(),
            title: "Critical failure".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Critical,
            component: "container_core".to_string(), // triggers +20
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let category = AlertCategory::ContainerHealth;
        let score = system.calculate_priority_score(&alert, &category);
        // Critical (100) + ContainerHealth (+20) = 120, capped at 100
        assert_eq!(score, 100);
    }

    #[test]
    fn test_priority_score_security_bonus() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "p2".to_string(),
            title: "Security event".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Warning,
            component: "auth_service".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let category = AlertCategory::Security;
        let score = system.calculate_priority_score(&alert, &category);
        // Warning (50) + Security (+30) = 80
        assert_eq!(score, 80);
    }

    #[test]
    fn test_priority_score_info_performance() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "p3".to_string(),
            title: "Performance notice".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Info,
            component: "metrics_collector".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let category = AlertCategory::Performance;
        let score = system.calculate_priority_score(&alert, &category);
        // Info (25) + Performance (+10) = 35
        assert_eq!(score, 35);
    }

    #[test]
    fn test_recommended_actions_container_health() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "r1".to_string(),
            title: "Container down".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Error,
            component: "container_service".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let actions = system.generate_recommended_actions(&alert, &AlertCategory::ContainerHealth);
        assert!(!actions.is_empty());
        // All container health recommendations should mention containers/logs
        let joined = actions.join(" ").to_lowercase();
        assert!(joined.contains("container"), "Should mention container");
    }

    #[test]
    fn test_recommended_actions_model_quality() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "r2".to_string(),
            title: "Model quality low".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Warning,
            component: "model_service".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let actions = system.generate_recommended_actions(&alert, &AlertCategory::ModelQuality);
        assert!(!actions.is_empty());
        let joined = actions.join(" ").to_lowercase();
        assert!(joined.contains("model"), "Should mention model");
    }

    #[test]
    fn test_recommended_actions_performance() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "r3".to_string(),
            title: "Performance degraded".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Warning,
            component: "api".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let actions = system.generate_recommended_actions(&alert, &AlertCategory::Performance);
        let joined = actions.join(" ").to_lowercase();
        // Performance arm must surface its production-specific guidance, so a
        // mistakenly-swapped match arm would change these strings and fail.
        assert!(
            joined.contains("resource"),
            "Performance actions should mention resources, got: {joined}"
        );
        assert!(
            joined.contains("scale"),
            "Performance actions should mention scaling, got: {joined}"
        );
    }

    #[test]
    fn test_recommended_actions_fallback() {
        let system = ObservabilitySystem::new();
        let alert = crate::monitoring::Alert {
            id: "r4".to_string(),
            title: "Some issue".to_string(),
            description: "desc".to_string(),
            severity: AlertSeverity::Info,
            component: "other_service".to_string(),
            timestamp: Utc::now(),
            context: HashMap::new(),
            acknowledged: false,
        };
        let actions = system.generate_recommended_actions(&alert, &AlertCategory::Availability);
        let joined = actions.join(" ").to_lowercase();
        // Fallback arm has its own distinct guidance ("investigate ...",
        // "check system logs", "contact support"). Asserting on those strings
        // catches a swap where another arm is returned by mistake.
        assert!(
            joined.contains("investigate"),
            "Fallback actions should mention investigating, got: {joined}"
        );
        assert!(
            joined.contains("logs"),
            "Fallback actions should mention checking logs, got: {joined}"
        );
        assert!(
            joined.contains("support"),
            "Fallback actions should mention support, got: {joined}"
        );
    }

    #[test]
    fn test_sla_status_for_compliant_branch() {
        // At-or-above target → Compliant.
        assert_eq!(sla_status_for(99.95, 99.9), SlaStatus::Compliant);
        assert_eq!(sla_status_for(100.0, 99.9), SlaStatus::Compliant);
        // Boundary: exactly at target is Compliant (>=).
        assert_eq!(sla_status_for(99.9, 99.9), SlaStatus::Compliant);
    }

    #[test]
    fn test_sla_status_for_at_risk_branch() {
        // Below target but within the 0.9-point at-risk window.
        assert_eq!(sla_status_for(99.5, 99.9), SlaStatus::AtRisk);
        // Just under target.
        assert_eq!(sla_status_for(99.89, 99.9), SlaStatus::AtRisk);
        // Boundary: at the at-risk floor (target - 0.9) is still AtRisk (>=).
        assert_eq!(sla_status_for(99.0, 99.9), SlaStatus::AtRisk);
    }

    #[test]
    fn test_sla_status_for_breached_branch() {
        // Below the at-risk floor → Breached.
        assert_eq!(sla_status_for(98.0, 99.9), SlaStatus::Breached);
        assert_eq!(sla_status_for(0.0, 99.9), SlaStatus::Breached);
        // Just under the at-risk floor.
        assert_eq!(sla_status_for(98.99, 99.9), SlaStatus::Breached);
    }

    #[test]
    fn test_sla_status_for_works_with_other_targets() {
        // Helper is parameterised on target, so it should classify correctly
        // for SLOs other than 99.9 (e.g. 95% availability).
        assert_eq!(sla_status_for(96.0, 95.0), SlaStatus::Compliant);
        assert_eq!(sla_status_for(94.5, 95.0), SlaStatus::AtRisk);
        assert_eq!(sla_status_for(90.0, 95.0), SlaStatus::Breached);
    }

    #[test]
    fn test_calculate_availability_metrics_produces_valid_sla() {
        let system = ObservabilitySystem::new();
        let metrics = system.calculate_availability_metrics().unwrap();

        // Availability is hardcoded at 99.5, which is below 99.9 but above 99.0 → AtRisk
        assert_eq!(metrics.sla_compliance.status, SlaStatus::AtRisk);
        assert_eq!(metrics.availability_percentage, 99.5);
        assert_eq!(metrics.sla_compliance.target_availability, 99.9);
        assert!(metrics.mtbf.is_some());
        assert!(metrics.mttr.is_some());
    }

    #[test]
    fn test_alert_policy_immediate_critical_has_console_channel() {
        let policy = AlertPolicy::immediate_critical();
        let console_channels: Vec<_> = policy
            .notification_channels
            .iter()
            .filter(|ch| ch.channel_type == ChannelType::Console)
            .collect();
        assert!(
            !console_channels.is_empty(),
            "Should have a console channel"
        );
        assert_eq!(console_channels[0].min_severity, AlertSeverity::Info);
    }

    #[test]
    fn test_alert_policy_balanced_has_grouping() {
        let policy = AlertPolicy::balanced();
        assert!(!policy.grouping_rules.is_empty());
        let rule = &policy.grouping_rules[0];
        assert!(!rule.name.is_empty());
        assert!(!rule.group_by_fields.is_empty());
        assert!(rule.max_alerts_per_group > 0);
    }

    #[test]
    fn test_alert_policy_immediate_critical_escalation_times() {
        let policy = AlertPolicy::immediate_critical();
        let critical_rule = policy
            .escalation_rules
            .iter()
            .find(|r| r.severity == AlertSeverity::Critical)
            .expect("Should have a critical escalation rule");
        // Critical alerts should escalate in 5 minutes
        assert_eq!(critical_rule.escalation_time, Duration::from_secs(300));
        assert_eq!(critical_rule.max_escalations, 3);
        assert!(critical_rule.escalation_factor > 1.0);
    }

    #[tokio::test]
    async fn test_get_container_summary_populates_uptime_stats() {
        // Exercise the actual production call site that constructs UptimeStats
        // (`get_container_summary`) and verify the invariants it must maintain:
        // current/avg/max are all derived from `start_time.elapsed()` and
        // therefore monotonically non-decreasing, and min_uptime is zero.
        let system = ObservabilitySystem::new();
        let summary = system
            .get_container_summary()
            .await
            .expect("get_container_summary should succeed");

        let stats = &summary.uptime_stats;
        assert_eq!(
            stats.min_uptime,
            Duration::ZERO,
            "min_uptime is initialised to zero in get_container_summary"
        );
        // current/average/max are all sampled from the same elapsed clock, so
        // they must be equal-or-ordered (max >= average >= current sample
        // ordering may flip by nanoseconds; just assert all are non-zero-ish
        // bounded and non-decreasing relative to one another).
        assert!(
            stats.max_uptime >= stats.current_uptime,
            "max_uptime ({:?}) must be >= current_uptime ({:?})",
            stats.max_uptime,
            stats.current_uptime
        );
        assert!(
            stats.average_uptime >= stats.min_uptime,
            "average_uptime ({:?}) must be >= min_uptime",
            stats.average_uptime
        );
    }

    #[test]
    fn test_escalation_status_variants_serialize() {
        let statuses = [
            EscalationStatus::New,
            EscalationStatus::Escalated,
            EscalationStatus::HighlyEscalated,
            EscalationStatus::UnderInvestigation,
            EscalationStatus::Resolved,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).expect("should serialize");
            let back: EscalationStatus = serde_json::from_str(&json).expect("should deserialize");
            assert_eq!(status, &back);
        }
    }

    #[test]
    fn test_alert_category_variants_serialize() {
        let categories = [
            AlertCategory::Performance,
            AlertCategory::Availability,
            AlertCategory::Security,
            AlertCategory::Resources,
            AlertCategory::Configuration,
            AlertCategory::ModelQuality,
            AlertCategory::ContainerHealth,
        ];
        for cat in &categories {
            let json = serde_json::to_string(cat).expect("should serialize");
            let back: AlertCategory = serde_json::from_str(&json).expect("should deserialize");
            assert_eq!(cat, &back);
        }
    }

    #[test]
    fn test_health_threshold_default_values() {
        let t = HealthThreshold::default();
        assert_eq!(t.cpu_threshold, 80.0);
        assert_eq!(t.memory_threshold, 85.0);
        assert_eq!(t.disk_threshold, 90.0);
        assert_eq!(t.max_latency_ms, 2000);
        assert_eq!(t.min_cache_hit_rate, 0.8);
        assert_eq!(t.max_error_rate, 0.05);
        assert_eq!(t.container_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_observability_system_builder_pattern() {
        let policy = AlertPolicy::immediate_critical();
        let thresholds = HealthThreshold::default();
        let system = ObservabilitySystem::new()
            .with_service_name("my-service")
            .with_alert_policy(policy)
            .with_health_thresholds(thresholds)
            .with_health_check_interval(Duration::from_secs(15));
        assert_eq!(system.service_name, "my-service");
        assert_eq!(system.health_check_interval, Duration::from_secs(15));
    }
}
