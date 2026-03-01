use crate::config::VLLMConfig;
use crate::judge::{
    JudgeConfig, ModelJudge, ValidationCriteria, ValidationMetrics, ValidationResult,
};
use crate::provider::{ModelError, ModelProvider, ModelResult};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, MessageRole, ModelInfo,
    ToolDefinition, Usage,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use std::time::Instant;
use tracing::{debug, error, info, warn};

/// OpenAI-compatible chat completion request for vLLM
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VLLMChatRequest {
    model: String,
    messages: Vec<VLLMMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

/// OpenAI-compatible message format for vLLM
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VLLMMessage {
    role: String,
    content: String,
}

/// OpenAI-compatible chat completion response from vLLM
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VLLMChatResponse {
    choices: Vec<VLLMChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<VLLMUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VLLMChoice {
    message: VLLMMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VLLMUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

/// OpenAI-compatible models list response from vLLM
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VLLMModelsResponse {
    data: Vec<VLLMModelData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VLLMModelData {
    id: String,
}

pub struct VLLMProvider {
    client: reqwest::Client,
    config: VLLMConfig,
    judge_config: JudgeConfig,
}

impl VLLMProvider {
    pub fn new(config: VLLMConfig) -> ModelResult<Self> {
        config
            .validate()
            .map_err(|msg| ModelError::InvalidConfig { message: msg })?;

        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| ModelError::Unknown {
                message: format!("Failed to create HTTP client: {}", e),
            })?;

        Ok(Self {
            client,
            config,
            judge_config: JudgeConfig::default(),
        })
    }

    pub fn with_default_config() -> ModelResult<Self> {
        Self::new(VLLMConfig::default())
    }

    pub fn with_judge_config(mut self, judge_config: JudgeConfig) -> Self {
        self.judge_config = judge_config;
        self
    }

    fn convert_message_role(role: &MessageRole) -> String {
        match role {
            MessageRole::System => "system".to_string(),
            MessageRole::User => "user".to_string(),
            MessageRole::Assistant => "assistant".to_string(),
            MessageRole::Tool => "tool".to_string(),
        }
    }

    fn convert_message_to_vllm(msg: &ChatMessage) -> VLLMMessage {
        let role = Self::convert_message_role(&msg.role);

        VLLMMessage {
            role,
            content: msg.content.clone().unwrap_or_default(),
        }
    }

    fn convert_finish_reason(reason: Option<String>) -> Option<FinishReason> {
        reason.map(|r| match r.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "tool_calls" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        })
    }

    fn handle_http_error(err: reqwest::Error) -> ModelError {
        if err.is_timeout() {
            ModelError::ServiceUnavailable {
                message: "Request timeout".to_string(),
            }
        } else if err.is_connect() {
            ModelError::ServiceUnavailable {
                message: "Cannot connect to vLLM service".to_string(),
            }
        } else if let Some(status) = err.status() {
            match status.as_u16() {
                404 => ModelError::ModelNotFound {
                    model: "unknown".to_string(),
                },
                503 => ModelError::ServiceUnavailable {
                    message: "vLLM service unavailable".to_string(),
                },
                401 | 403 => ModelError::Authentication,
                429 => ModelError::RateLimit,
                _ => ModelError::Network(err),
            }
        } else {
            ModelError::Network(err)
        }
    }
}

#[async_trait]
impl ModelProvider for VLLMProvider {
    async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
        debug!("Starting chat request with model: {}", request.model);

        let vllm_messages: Vec<VLLMMessage> = request
            .messages
            .iter()
            .map(Self::convert_message_to_vllm)
            .collect();

        let vllm_request = VLLMChatRequest {
            model: request.model.clone(),
            messages: vllm_messages,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
        };

        let url = format!("{}/v1/chat/completions", self.config.base_url);

        let response = self
            .client
            .post(&url)
            .json(&vllm_request)
            .send()
            .await
            .map_err(Self::handle_http_error)?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(ModelError::Unknown {
                message: format!("vLLM API error ({}): {}", status, error_text),
            });
        }

        let vllm_response: VLLMChatResponse =
            response.json().await.map_err(Self::handle_http_error)?;

        let choices: Vec<Choice> = vllm_response
            .choices
            .into_iter()
            .map(|choice| {
                let message = ChatMessage {
                    role: MessageRole::Assistant,
                    content: Some(choice.message.content),
                    tool_calls: None,
                    tool_call_id: None,
                };

                Choice {
                    message,
                    finish_reason: Self::convert_finish_reason(choice.finish_reason),
                }
            })
            .collect();

        let usage = vllm_response.usage.map(|u| Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        info!("Chat request completed successfully");

        Ok(ChatResponse { choices, usage })
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        debug!("Listing available models");

        let url = format!("{}/v1/models", self.config.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(Self::handle_http_error)?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(ModelError::Unknown {
                message: format!("vLLM API error ({}): {}", status, error_text),
            });
        }

        let models_response: VLLMModelsResponse =
            response.json().await.map_err(Self::handle_http_error)?;

        let model_infos: Vec<ModelInfo> = models_response
            .data
            .into_iter()
            .map(|model| ModelInfo {
                name: model.id,
                size: None,
                digest: None,
                modified_at: None,
            })
            .collect();

        info!("Retrieved {} models", model_infos.len());
        Ok(model_infos)
    }

    async fn health_check(&self) -> ModelResult<()> {
        debug!("Performing health check");

        let url = format!("{}/health", self.config.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(Self::handle_http_error)?;

        if response.status().is_success() {
            info!("Health check passed");
            Ok(())
        } else {
            error!("Health check failed with status: {}", response.status());
            Err(ModelError::ServiceUnavailable {
                message: format!("Health check failed: {}", response.status()),
            })
        }
    }

    fn provider_name(&self) -> &'static str {
        "vllm"
    }
}

#[async_trait]
impl ModelJudge for VLLMProvider {
    fn judge_config(&self) -> &JudgeConfig {
        &self.judge_config
    }

    async fn validate_api_responsiveness(
        &self,
        latency_threshold: Duration,
    ) -> ModelResult<ValidationResult> {
        let start_time = Instant::now();
        let mut retry_count = 0;
        let config = &self.judge_config;

        loop {
            let attempt_start = Instant::now();

            match self.health_check().await {
                Ok(()) => {
                    let duration = attempt_start.elapsed();
                    let total_duration = start_time.elapsed();

                    if config.verbose_logging {
                        debug!(
                            "API responsiveness check passed in {:?} (total: {:?}, retries: {})",
                            duration, total_duration, retry_count
                        );
                    }

                    let metrics = ValidationMetrics {
                        duration: total_duration,
                        retry_count,
                        response_length: None,
                        coherence_score: None,
                        relevance_score: None,
                        success_rate: None,
                        custom_metrics: std::collections::HashMap::new(),
                    };

                    if duration <= latency_threshold {
                        return Ok(ValidationResult::Success {
                            message: format!(
                                "API responded within {}ms (threshold: {}ms)",
                                duration.as_millis(),
                                latency_threshold.as_millis()
                            ),
                            metrics,
                        });
                    } else {
                        return Ok(ValidationResult::Warning {
                            message: format!(
                                "API responded in {}ms (exceeds threshold: {}ms)",
                                duration.as_millis(),
                                latency_threshold.as_millis()
                            ),
                            suggestions: vec![
                                "Consider increasing latency threshold".to_string(),
                                "Check network connectivity".to_string(),
                                "Verify server performance".to_string(),
                            ],
                            metrics,
                        });
                    }
                }
                Err(e) => {
                    if retry_count >= config.max_retries {
                        let total_duration = start_time.elapsed();
                        warn!(
                            "API responsiveness validation failed after {} retries: {}",
                            retry_count, e
                        );

                        return Ok(ValidationResult::Failure {
                            message: "API unresponsive after retries".to_string(),
                            error_details: e.to_string(),
                            suggestions: vec![
                                "Check if vLLM service is running".to_string(),
                                "Verify network connectivity".to_string(),
                                "Check firewall settings".to_string(),
                                "Increase retry count or timeout".to_string(),
                            ],
                            metrics: Some(ValidationMetrics {
                                duration: total_duration,
                                retry_count,
                                response_length: None,
                                coherence_score: None,
                                relevance_score: None,
                                success_rate: None,
                                custom_metrics: std::collections::HashMap::new(),
                            }),
                        });
                    }

                    retry_count += 1;
                    let delay = config.calculate_retry_delay(retry_count - 1);

                    if config.verbose_logging {
                        debug!(
                            "API health check failed (attempt {}), retrying in {:?}: {}",
                            retry_count, delay, e
                        );
                    }

                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    async fn validate_response_quality(
        &self,
        prompt: &str,
        expected_criteria: &ValidationCriteria,
    ) -> ModelResult<ValidationResult> {
        let start_time = Instant::now();
        let mut retry_count = 0;
        let config = &self.judge_config;

        loop {
            let request =
                ChatRequest::new(&self.config.default_model, vec![ChatMessage::user(prompt)]);

            match self.chat(request).await {
                Ok(response) => {
                    let duration = start_time.elapsed();

                    if let Some(choice) = response.choices.first() {
                        if let Some(content) = &choice.message.content {
                            // Calculate quality metrics
                            let response_length = content.len();
                            let coherence_score = crate::judge::calculate_coherence_score(content);
                            let relevance_score = crate::judge::calculate_relevance_score(
                                content,
                                prompt,
                                expected_criteria,
                            );

                            let mut metrics = ValidationMetrics {
                                duration,
                                retry_count,
                                response_length: Some(response_length),
                                coherence_score: Some(coherence_score),
                                relevance_score: Some(relevance_score),
                                success_rate: None,
                                custom_metrics: std::collections::HashMap::new(),
                            };

                            // Check length criteria
                            if response_length < expected_criteria.min_response_length {
                                return Ok(ValidationResult::Failure {
                                    message: format!(
                                        "Response too short: {} chars (minimum: {})",
                                        response_length, expected_criteria.min_response_length
                                    ),
                                    error_details: "Response length below minimum threshold"
                                        .to_string(),
                                    suggestions: vec![
                                        "Adjust prompt to encourage longer responses".to_string(),
                                        "Lower minimum response length criteria".to_string(),
                                        "Check if model is functioning correctly".to_string(),
                                    ],
                                    metrics: Some(metrics),
                                });
                            }

                            if response_length > expected_criteria.max_response_length {
                                return Ok(ValidationResult::Warning {
                                    message: format!(
                                        "Response too long: {} chars (maximum: {})",
                                        response_length, expected_criteria.max_response_length
                                    ),
                                    suggestions: vec![
                                        "Adjust prompt to encourage concise responses".to_string(),
                                        "Increase maximum response length criteria".to_string(),
                                        "Consider setting max_tokens parameter".to_string(),
                                    ],
                                    metrics,
                                });
                            }

                            // Check coherence score
                            if coherence_score < expected_criteria.min_coherence_score {
                                return Ok(ValidationResult::Warning {
                                    message: format!(
                                        "Low coherence score: {:.2} (minimum: {:.2})",
                                        coherence_score, expected_criteria.min_coherence_score
                                    ),
                                    suggestions: vec![
                                        "Adjust model temperature for more coherent responses"
                                            .to_string(),
                                        "Improve prompt clarity and structure".to_string(),
                                        "Consider using a different model".to_string(),
                                    ],
                                    metrics,
                                });
                            }

                            // Check relevance score
                            if relevance_score < expected_criteria.min_relevance_score {
                                return Ok(ValidationResult::Warning {
                                    message: format!(
                                        "Low relevance score: {:.2} (minimum: {:.2})",
                                        relevance_score, expected_criteria.min_relevance_score
                                    ),
                                    suggestions: vec![
                                        "Make prompt more specific and clear".to_string(),
                                        "Add context or examples to the prompt".to_string(),
                                        "Adjust validation criteria".to_string(),
                                    ],
                                    metrics,
                                });
                            }

                            // Check for forbidden keywords
                            let forbidden_found: Vec<&String> = expected_criteria
                                .forbidden_keywords
                                .iter()
                                .filter(|keyword| {
                                    content.to_lowercase().contains(&keyword.to_lowercase())
                                })
                                .collect();

                            if !forbidden_found.is_empty() {
                                return Ok(ValidationResult::Warning {
                                    message: format!(
                                        "Response contains forbidden keywords: {:?}",
                                        forbidden_found
                                    ),
                                    suggestions: vec![
                                        "Adjust prompt to avoid triggering forbidden responses"
                                            .to_string(),
                                        "Update forbidden keywords list".to_string(),
                                        "Use system prompts to guide behavior".to_string(),
                                    ],
                                    metrics,
                                });
                            }

                            // Check for required keywords
                            if !expected_criteria.required_keywords.is_empty() {
                                let missing_keywords: Vec<&String> = expected_criteria
                                    .required_keywords
                                    .iter()
                                    .filter(|keyword| {
                                        !content.to_lowercase().contains(&keyword.to_lowercase())
                                    })
                                    .collect();

                                if !missing_keywords.is_empty() {
                                    return Ok(ValidationResult::Warning {
                                        message: format!(
                                            "Response missing required keywords: {:?}",
                                            missing_keywords
                                        ),
                                        suggestions: vec![
                                            "Make required keywords more prominent in prompt"
                                                .to_string(),
                                            "Add examples that include required keywords"
                                                .to_string(),
                                            "Adjust required keywords criteria".to_string(),
                                        ],
                                        metrics,
                                    });
                                }
                            }

                            // Add token usage to custom metrics if available
                            if let Some(usage) = &response.usage {
                                metrics.add_custom_metric(
                                    "prompt_tokens".to_string(),
                                    usage.prompt_tokens as f64,
                                );
                                metrics.add_custom_metric(
                                    "completion_tokens".to_string(),
                                    usage.completion_tokens as f64,
                                );
                                metrics.add_custom_metric(
                                    "total_tokens".to_string(),
                                    usage.total_tokens as f64,
                                );
                            }

                            if config.verbose_logging {
                                info!("Response quality validation passed: length={}, coherence={:.2}, relevance={:.2}",
                                      response_length, coherence_score, relevance_score);
                            }

                            return Ok(ValidationResult::Success {
                                message: format!("Response quality meets criteria (coherence: {:.2}, relevance: {:.2})",
                                               coherence_score, relevance_score),
                                metrics,
                            });
                        } else {
                            return Ok(ValidationResult::Failure {
                                message: "Response contains no content".to_string(),
                                error_details: "Model returned empty response".to_string(),
                                suggestions: vec![
                                    "Check model configuration".to_string(),
                                    "Verify prompt is not empty".to_string(),
                                    "Try a different model".to_string(),
                                ],
                                metrics: None,
                            });
                        }
                    } else {
                        return Ok(ValidationResult::Failure {
                            message: "No choices in response".to_string(),
                            error_details: "Model returned response with no choices".to_string(),
                            suggestions: vec![
                                "Check model configuration".to_string(),
                                "Verify API compatibility".to_string(),
                            ],
                            metrics: None,
                        });
                    }
                }
                Err(e) => {
                    if retry_count >= config.max_retries {
                        warn!(
                            "Response quality validation failed after {} retries: {}",
                            retry_count, e
                        );

                        return Ok(ValidationResult::Failure {
                            message: "Failed to get response for quality validation".to_string(),
                            error_details: e.to_string(),
                            suggestions: vec![
                                "Check model availability".to_string(),
                                "Verify network connectivity".to_string(),
                                "Increase retry count".to_string(),
                                "Simplify the prompt".to_string(),
                            ],
                            metrics: Some(ValidationMetrics {
                                duration: start_time.elapsed(),
                                retry_count,
                                response_length: None,
                                coherence_score: None,
                                relevance_score: None,
                                success_rate: None,
                                custom_metrics: std::collections::HashMap::new(),
                            }),
                        });
                    }

                    retry_count += 1;
                    let delay = config.calculate_retry_delay(retry_count - 1);

                    if config.verbose_logging {
                        debug!(
                            "Response quality check failed (attempt {}), retrying in {:?}: {}",
                            retry_count, delay, e
                        );
                    }

                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    async fn validate_tool_calling(
        &self,
        tools: &[ToolDefinition],
    ) -> ModelResult<ValidationResult> {
        let start_time = Instant::now();

        if tools.is_empty() {
            return Ok(ValidationResult::Success {
                message: "No tools to validate".to_string(),
                metrics: ValidationMetrics::with_duration(start_time.elapsed()),
            });
        }

        // For now, vLLM provider doesn't support tool calling in our implementation
        // This is a placeholder that acknowledges the limitation
        warn!("Tool calling validation requested but not yet implemented for vLLM provider");

        Ok(ValidationResult::Warning {
            message: "Tool calling validation not implemented for vLLM provider".to_string(),
            suggestions: vec![
                "Implement tool calling support in VLLMProvider".to_string(),
                "Use a different provider that supports tool calling".to_string(),
                "Skip tool calling validation for now".to_string(),
            ],
            metrics: ValidationMetrics::with_duration(start_time.elapsed()),
        })
    }

    async fn validate_consistency(
        &self,
        prompts: &[&str],
        iterations: usize,
    ) -> ModelResult<ValidationResult> {
        let start_time = Instant::now();
        let config = &self.judge_config;

        if prompts.is_empty() {
            return Ok(ValidationResult::Success {
                message: "No prompts to validate for consistency".to_string(),
                metrics: ValidationMetrics::with_duration(start_time.elapsed()),
            });
        }

        if iterations == 0 {
            return Ok(ValidationResult::Success {
                message: "Zero iterations requested".to_string(),
                metrics: ValidationMetrics::with_duration(start_time.elapsed()),
            });
        }

        let mut total_attempts = 0;
        let mut successful_attempts = 0;
        let mut response_lengths = Vec::new();
        let mut coherence_scores = Vec::new();

        for prompt in prompts {
            if config.verbose_logging {
                debug!("Testing consistency for prompt: {}", prompt);
            }

            for iteration in 0..iterations {
                total_attempts += 1;

                let request =
                    ChatRequest::new(&self.config.default_model, vec![ChatMessage::user(*prompt)]);

                match self.chat(request).await {
                    Ok(response) => {
                        if let Some(choice) = response.choices.first() {
                            if let Some(content) = &choice.message.content {
                                successful_attempts += 1;
                                response_lengths.push(content.len());
                                coherence_scores
                                    .push(crate::judge::calculate_coherence_score(content));

                                if config.verbose_logging {
                                    debug!(
                                        "Prompt '{}' iteration {}: {} chars, coherence: {:.2}",
                                        prompt,
                                        iteration + 1,
                                        content.len(),
                                        coherence_scores.last().unwrap()
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if config.verbose_logging {
                            debug!(
                                "Consistency test failed for prompt '{}' iteration {}: {}",
                                prompt,
                                iteration + 1,
                                e
                            );
                        }
                    }
                }

                // Small delay between requests to avoid overwhelming the server
                if iteration < iterations - 1 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        let duration = start_time.elapsed();
        let success_rate = successful_attempts as f64 / total_attempts as f64;

        let mut metrics = ValidationMetrics {
            duration,
            retry_count: 0,
            response_length: if response_lengths.is_empty() {
                None
            } else {
                Some(response_lengths.iter().sum::<usize>() / response_lengths.len())
            },
            coherence_score: if coherence_scores.is_empty() {
                None
            } else {
                Some(coherence_scores.iter().sum::<f64>() / coherence_scores.len() as f64)
            },
            relevance_score: None,
            success_rate: Some(success_rate),
            custom_metrics: std::collections::HashMap::new(),
        };

        // Calculate consistency metrics
        if response_lengths.len() > 1 {
            let length_variance = calculate_variance(
                &response_lengths
                    .iter()
                    .map(|&x| x as f64)
                    .collect::<Vec<_>>(),
            );
            metrics.add_custom_metric("length_variance".to_string(), length_variance);
        }

        if coherence_scores.len() > 1 {
            let coherence_variance = calculate_variance(&coherence_scores);
            metrics.add_custom_metric("coherence_variance".to_string(), coherence_variance);
        }

        metrics.add_custom_metric("total_attempts".to_string(), total_attempts as f64);
        metrics.add_custom_metric(
            "successful_attempts".to_string(),
            successful_attempts as f64,
        );

        if config.verbose_logging {
            info!(
                "Consistency validation completed: {}/{} successful attempts, success rate: {:.2}%",
                successful_attempts,
                total_attempts,
                success_rate * 100.0
            );
        }

        if success_rate < 0.8 {
            Ok(ValidationResult::Warning {
                message: format!(
                    "Low success rate: {:.1}% ({}/{} attempts)",
                    success_rate * 100.0,
                    successful_attempts,
                    total_attempts
                ),
                suggestions: vec![
                    "Check model availability and stability".to_string(),
                    "Reduce request frequency".to_string(),
                    "Simplify test prompts".to_string(),
                    "Increase timeout settings".to_string(),
                ],
                metrics,
            })
        } else if response_lengths.len() > 1 {
            let length_variance = metrics
                .custom_metrics
                .get("length_variance")
                .unwrap_or(&0.0);
            let coherence_variance = metrics
                .custom_metrics
                .get("coherence_variance")
                .unwrap_or(&0.0);

            if *length_variance > 10000.0 || *coherence_variance > 0.1 {
                Ok(ValidationResult::Warning {
                    message: format!(
                        "High response variance detected (length: {:.1}, coherence: {:.3})",
                        length_variance, coherence_variance
                    ),
                    suggestions: vec![
                        "Set a lower temperature for more consistent responses".to_string(),
                        "Use more specific prompts".to_string(),
                        "Consider the model's inherent variability".to_string(),
                    ],
                    metrics,
                })
            } else {
                Ok(ValidationResult::Success {
                    message: format!(
                        "Consistency validation passed: {:.1}% success rate, low variance",
                        success_rate * 100.0
                    ),
                    metrics,
                })
            }
        } else {
            Ok(ValidationResult::Success {
                message: format!(
                    "Consistency validation passed: {:.1}% success rate",
                    success_rate * 100.0
                ),
                metrics,
            })
        }
    }
}

/// Helper function to calculate variance
fn calculate_variance(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }

    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / values.len() as f64;

    variance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = VLLMConfig::default();
        assert_eq!(config.base_url, "http://localhost:8000");
        assert_eq!(config.timeout, Duration::from_secs(120));
        assert_eq!(config.default_model, "XiaomiMiMo/MiMo-V2-Flash");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation() {
        let mut config = VLLMConfig::default();

        // Test empty base URL
        config.base_url = "".to_string();
        assert!(config.validate().is_err());

        // Test invalid base URL
        config.base_url = "not-a-url".to_string();
        assert!(config.validate().is_err());

        // Test valid base URL
        config.base_url = "http://localhost:8000".to_string();
        assert!(config.validate().is_ok());

        // Test zero timeout
        config.timeout = Duration::from_secs(0);
        assert!(config.validate().is_err());

        // Test valid timeout
        config.timeout = Duration::from_secs(120);
        assert!(config.validate().is_ok());

        // Test empty model
        config.default_model = "".to_string();
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn test_provider_creation() {
        let config = VLLMConfig::default();
        let provider = VLLMProvider::new(config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "vllm");
    }

    #[tokio::test]
    async fn test_provider_with_default_config() {
        let provider = VLLMProvider::with_default_config();
        assert!(provider.is_ok());
    }

    #[test]
    fn test_message_conversion() {
        let system_msg = ChatMessage::system("You are helpful");
        let vllm_msg = VLLMProvider::convert_message_to_vllm(&system_msg);
        assert_eq!(vllm_msg.role, "system");
        assert_eq!(vllm_msg.content, "You are helpful");

        let user_msg = ChatMessage::user("Hello");
        let vllm_msg = VLLMProvider::convert_message_to_vllm(&user_msg);
        assert_eq!(vllm_msg.role, "user");
        assert_eq!(vllm_msg.content, "Hello");

        let assistant_msg = ChatMessage::assistant("Hi there");
        let vllm_msg = VLLMProvider::convert_message_to_vllm(&assistant_msg);
        assert_eq!(vllm_msg.role, "assistant");
        assert_eq!(vllm_msg.content, "Hi there");
    }

    #[test]
    fn test_finish_reason_conversion() {
        assert_eq!(
            VLLMProvider::convert_finish_reason(Some("stop".to_string())),
            Some(FinishReason::Stop)
        );
        assert_eq!(
            VLLMProvider::convert_finish_reason(Some("length".to_string())),
            Some(FinishReason::Length)
        );
        assert_eq!(
            VLLMProvider::convert_finish_reason(Some("tool_calls".to_string())),
            Some(FinishReason::ToolCalls)
        );
        assert_eq!(
            VLLMProvider::convert_finish_reason(Some("content_filter".to_string())),
            Some(FinishReason::ContentFilter)
        );
        assert_eq!(
            VLLMProvider::convert_finish_reason(Some("unknown".to_string())),
            Some(FinishReason::Stop)
        );
        assert_eq!(VLLMProvider::convert_finish_reason(None), None);
    }

    #[test]
    fn test_variance_calculation() {
        // Test empty vector
        assert_eq!(calculate_variance(&[]), 0.0);

        // Test single value
        assert_eq!(calculate_variance(&[5.0]), 0.0);

        // Test identical values
        assert_eq!(calculate_variance(&[5.0, 5.0, 5.0]), 0.0);

        // Test known variance
        let values = vec![2.0, 4.0, 6.0, 8.0];
        let variance = calculate_variance(&values);
        assert!((variance - 5.0).abs() < 0.01); // Variance should be 5.0
    }

    #[test]
    fn test_provider_with_judge_config() {
        let provider = VLLMProvider::with_default_config().unwrap();
        let judge_config = JudgeConfig::default().with_verbose_logging();
        let provider = provider.with_judge_config(judge_config);
        assert!(provider.judge_config.verbose_logging);
    }

    #[test]
    fn test_role_conversion() {
        assert_eq!(
            VLLMProvider::convert_message_role(&MessageRole::System),
            "system"
        );
        assert_eq!(
            VLLMProvider::convert_message_role(&MessageRole::User),
            "user"
        );
        assert_eq!(
            VLLMProvider::convert_message_role(&MessageRole::Assistant),
            "assistant"
        );
        assert_eq!(
            VLLMProvider::convert_message_role(&MessageRole::Tool),
            "tool"
        );
    }
}
