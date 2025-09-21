use crate::config::OllamaConfig;
use crate::judge::{
    JudgeConfig, ModelJudge, ValidationCriteria, ValidationMetrics, ValidationResult,
};
use crate::provider::{ModelError, ModelProvider, ModelResult};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, MessageRole, ModelInfo,
    ToolDefinition, Usage,
};
use async_trait::async_trait;
use std::time::Instant;
use ollama_rs::{
    generation::chat::{ChatMessage as OllamaChatMessage, MessageRole as OllamaRole},
    Ollama,
};
use std::time::Duration;
use tracing::{debug, error, info, warn};

pub struct OllamaProvider {
    client: Ollama,
    #[allow(dead_code)]
    config: OllamaConfig,
    judge_config: JudgeConfig,
}

impl OllamaProvider {
    pub fn new(config: OllamaConfig) -> ModelResult<Self> {
        config
            .validate()
            .map_err(|msg| ModelError::InvalidConfig { message: msg })?;

        let host = if config.base_url.ends_with("/v1") {
            config.base_url[..config.base_url.len() - 3].to_string()
        } else {
            config.base_url.clone()
        };

        let client = Ollama::new(host, 11434);

        Ok(Self {
            client,
            config,
            judge_config: JudgeConfig::default(),
        })
    }

    pub fn with_default_config() -> ModelResult<Self> {
        Self::new(OllamaConfig::default())
    }

    pub fn with_judge_config(mut self, judge_config: JudgeConfig) -> Self {
        self.judge_config = judge_config;
        self
    }

    fn convert_message_role(role: &MessageRole) -> OllamaRole {
        match role {
            MessageRole::System => OllamaRole::System,
            MessageRole::User => OllamaRole::User,
            MessageRole::Assistant => OllamaRole::Assistant,
            MessageRole::Tool => OllamaRole::Tool,
        }
    }

    #[allow(dead_code)]
    fn convert_message_from_ollama(msg: &OllamaChatMessage) -> ChatMessage {
        let role = match msg.role {
            OllamaRole::System => MessageRole::System,
            OllamaRole::User => MessageRole::User,
            OllamaRole::Assistant => MessageRole::Assistant,
            OllamaRole::Tool => MessageRole::Tool,
        };

        ChatMessage {
            role,
            content: Some(msg.content.clone()),
            tool_calls: None, // Tool calls will be handled separately for now
            tool_call_id: None,
        }
    }

    fn convert_message_to_ollama(msg: &ChatMessage) -> OllamaChatMessage {
        let role = Self::convert_message_role(&msg.role);

        OllamaChatMessage {
            role,
            content: msg.content.clone().unwrap_or_default(),
            images: None,
            tool_calls: vec![], // Empty for now
        }
    }

    fn handle_ollama_error(err: ollama_rs::error::OllamaError) -> ModelError {
        match err {
            ollama_rs::error::OllamaError::ReqwestError(e) => {
                if e.is_timeout() {
                    ModelError::ServiceUnavailable {
                        message: "Request timeout".to_string(),
                    }
                } else if e.is_connect() {
                    ModelError::ServiceUnavailable {
                        message: "Cannot connect to Ollama service".to_string(),
                    }
                } else {
                    // Convert to the right reqwest error type
                    ModelError::Unknown {
                        message: format!("Network error: {}", e),
                    }
                }
            }
            ollama_rs::error::OllamaError::JsonError(e) => ModelError::Serialization(e),
            _ => ModelError::Unknown {
                message: format!("Ollama error: {}", err),
            },
        }
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
        debug!("Starting chat request with model: {}", request.model);

        let ollama_messages: Vec<OllamaChatMessage> = request
            .messages
            .iter()
            .map(Self::convert_message_to_ollama)
            .collect();

        // For now, use the completion API since it's simpler
        let prompt = ollama_messages
            .iter()
            .map(|msg| msg.content.clone())
            .collect::<Vec<_>>()
            .join("\n");

        let generation_request = ollama_rs::generation::completion::request::GenerationRequest::new(
            request.model.clone(),
            prompt,
        );

        let response = self
            .client
            .generate(generation_request)
            .await
            .map_err(Self::handle_ollama_error)?;

        let message = ChatMessage::assistant(response.response);

        let choice = Choice {
            message,
            finish_reason: Some(FinishReason::Stop),
        };

        let usage = Some(Usage {
            prompt_tokens: response.prompt_eval_count.unwrap_or(0) as u32,
            completion_tokens: response.eval_count.unwrap_or(0) as u32,
            total_tokens: (response.prompt_eval_count.unwrap_or(0)
                + response.eval_count.unwrap_or(0)) as u32,
        });

        info!("Chat request completed successfully");

        Ok(ChatResponse {
            choices: vec![choice],
            usage,
        })
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        debug!("Listing available models");

        let models = self
            .client
            .list_local_models()
            .await
            .map_err(Self::handle_ollama_error)?;

        let model_infos: Vec<ModelInfo> = models
            .into_iter()
            .map(|model| ModelInfo {
                name: model.name,
                size: Some(model.size),
                digest: None, // Not available in this API
                modified_at: Some(model.modified_at),
            })
            .collect();

        info!("Retrieved {} models", model_infos.len());
        Ok(model_infos)
    }

    async fn health_check(&self) -> ModelResult<()> {
        debug!("Performing health check");

        match self.list_models().await {
            Ok(_) => {
                info!("Health check passed");
                Ok(())
            }
            Err(e) => {
                error!("Health check failed: {}", e);
                Err(e)
            }
        }
    }

    fn provider_name(&self) -> &'static str {
        "ollama"
    }
}

#[async_trait]
impl ModelJudge for OllamaProvider {
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
                                "Check if Ollama service is running".to_string(),
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
            let request = ChatRequest::new("qwen3:0.6b", vec![ChatMessage::user(prompt)]);

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

        // For now, Ollama provider doesn't support tool calling in our implementation
        // This is a placeholder that acknowledges the limitation
        warn!("Tool calling validation requested but not yet implemented for Ollama provider");

        Ok(ValidationResult::Warning {
            message: "Tool calling validation not implemented for Ollama provider".to_string(),
            suggestions: vec![
                "Implement tool calling support in OllamaProvider".to_string(),
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

                let request = ChatRequest::new("qwen3:0.6b", vec![ChatMessage::user(*prompt)]);

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
    fn test_message_conversion() {
        let our_message = ChatMessage::user("Hello world");
        let ollama_message = OllamaProvider::convert_message_to_ollama(&our_message);
        let converted_back = OllamaProvider::convert_message_from_ollama(&ollama_message);

        assert_eq!(our_message.role, converted_back.role);
        assert_eq!(our_message.content, converted_back.content);
    }

    #[tokio::test]
    async fn test_provider_creation() {
        let config = OllamaConfig::default();
        let provider = OllamaProvider::new(config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "ollama");
    }
}
