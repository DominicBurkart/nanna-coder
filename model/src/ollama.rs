use crate::config::{OllamaConfig, DEFAULT_MODEL};
use crate::judge::{
    JudgeConfig, ModelJudge, ValidationCriteria, ValidationMetrics, ValidationResult,
};
use crate::provider::{ModelError, ModelProvider, ModelResult};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, MessageRole,
    ModelInfo, ToolCall, ToolDefinition, Usage,
};
use async_trait::async_trait;
use ollama_rs::Ollama;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;
use tracing::{debug, error, info, warn};

pub struct OllamaProvider {
    client: Ollama,
    config: OllamaConfig,
    judge_config: JudgeConfig,
    http_client: reqwest::Client,
    base_url: String,
}

impl OllamaProvider {
    pub fn new(config: OllamaConfig) -> ModelResult<Self> {
        config
            .validate()
            .map_err(|msg| ModelError::InvalidConfig { message: msg })?;

        let base_url = if config.base_url.ends_with("/v1") {
            config.base_url[..config.base_url.len() - 3].to_string()
        } else {
            config.base_url.clone()
        };

        let ollama_host = if base_url.ends_with(':') {
            base_url.trim_end_matches(':').to_string()
        } else {
            base_url.clone()
        };

        let client = Ollama::new(ollama_host, 11434);

        let http_client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| ModelError::Unknown {
                message: format!("Failed to build HTTP client: {}", e),
            })?;

        Ok(Self {
            client,
            config,
            judge_config: JudgeConfig::default(),
            http_client,
            base_url,
        })
    }

    pub fn with_default_config() -> ModelResult<Self> {
        Self::new(OllamaConfig::default())
    }

    pub fn with_judge_config(mut self, judge_config: JudgeConfig) -> Self {
        self.judge_config = judge_config;
        self
    }

    fn messages_to_json(messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .map(|msg| {
                let role = match &msg.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                };

                let mut obj = serde_json::json!({
                    "role": role,
                    "content": msg.content.as_deref().unwrap_or(""),
                });

                if let Some(tool_calls) = &msg.tool_calls {
                    if !tool_calls.is_empty() {
                        let tc_json: Vec<Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "function": {
                                        "name": tc.function.name,
                                        "arguments": tc.function.arguments
                                    }
                                })
                            })
                            .collect();
                        obj["tool_calls"] = Value::Array(tc_json);
                    }
                }

                if let Some(tool_call_id) = &msg.tool_call_id {
                    obj["tool_call_id"] = Value::String(tool_call_id.clone());
                }

                obj
            })
            .collect()
    }

    fn tools_to_json(tools: &[ToolDefinition]) -> Vec<Value> {
        tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": tool.function
                })
            })
            .collect()
    }

    fn parse_raw_response(raw: OllamaChatRawResponse) -> ModelResult<ChatResponse> {
        let has_tool_calls = raw
            .message
            .tool_calls
            .as_ref()
            .map(|tc| !tc.is_empty())
            .unwrap_or(false);

        let (finish_reason, tool_calls) = if has_tool_calls {
            let calls: Vec<ToolCall> = raw
                .message
                .tool_calls
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(idx, tc)| ToolCall {
                    id: format!("call_{}", idx),
                    function: FunctionCall {
                        name: tc.function.name,
                        arguments: tc.function.arguments,
                    },
                })
                .collect();
            (FinishReason::ToolCalls, Some(calls))
        } else {
            (FinishReason::Stop, None)
        };

        let content = if raw.message.content.is_empty() {
            None
        } else {
            Some(raw.message.content)
        };

        let message = ChatMessage {
            role: MessageRole::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
        };

        let usage = match (raw.prompt_eval_count, raw.eval_count) {
            (Some(p), Some(e)) => Some(Usage {
                prompt_tokens: p as u32,
                completion_tokens: e as u32,
                total_tokens: (p + e) as u32,
            }),
            _ => None,
        };

        Ok(ChatResponse {
            choices: vec![Choice {
                message,
                finish_reason: Some(finish_reason),
            }],
            usage,
        })
    }

    /// Shared mapping logic for `reqwest::Error` values.
    ///
    /// Both `handle_reqwest_error` and `handle_ollama_error` delegate here so
    /// the timeout / connect / fallthrough pattern is defined exactly once.
    fn map_reqwest_error(e: &reqwest::Error) -> ModelError {
        if e.is_timeout() {
            ModelError::ServiceUnavailable {
                message: "Request timeout".to_string(),
            }
        } else if e.is_connect() {
            ModelError::ServiceUnavailable {
                message: "Cannot connect to Ollama service".to_string(),
            }
        } else {
            ModelError::Unknown {
                message: format!("Network error: {}", e),
            }
        }
    }

    fn handle_ollama_error(err: ollama_rs::error::OllamaError) -> ModelError {
        match err {
            ollama_rs::error::OllamaError::ReqwestError(e) => Self::map_reqwest_error(&e),
            ollama_rs::error::OllamaError::JsonError(e) => ModelError::Serialization(e),
            _ => ModelError::Unknown {
                message: format!("Ollama error: {}", err),
            },
        }
    }

    fn handle_reqwest_error(e: reqwest::Error) -> ModelError {
        Self::map_reqwest_error(&e)
    }
}

#[derive(Deserialize)]
struct OllamaChatRawResponse {
    message: OllamaRawMessage,
    #[allow(dead_code)]
    done: bool,
    prompt_eval_count: Option<i64>,
    eval_count: Option<i64>,
}

#[derive(Deserialize)]
struct OllamaRawMessage {
    #[allow(dead_code)]
    role: String,
    content: String,
    tool_calls: Option<Vec<OllamaRawToolCall>>,
}

#[derive(Deserialize)]
struct OllamaRawToolCall {
    function: OllamaRawFunction,
}

#[derive(Deserialize)]
struct OllamaRawFunction {
    name: String,
    arguments: serde_json::Value,
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
        debug!("Starting chat request with model: {}", request.model);

        let messages = Self::messages_to_json(&request.messages);

        let temperature = request
            .temperature
            .unwrap_or(self.config.default_temperature);

        let mut payload = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": false,
            "options": {
                "temperature": temperature
            }
        });

        if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                payload["tools"] = Value::Array(Self::tools_to_json(tools));
            }
        }

        let url = format!("{}/api/chat", self.base_url);

        let response = self
            .http_client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(Self::handle_reqwest_error)?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ModelError::Unknown {
                message: format!("Ollama API error {}: {}", status, body),
            });
        }

        let raw: OllamaChatRawResponse =
            response.json().await.map_err(|e| ModelError::Unknown {
                message: format!("Failed to parse response: {}", e),
            })?;

        info!("Chat request completed successfully");

        Self::parse_raw_response(raw)
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
                digest: None,
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
            let request = ChatRequest::new(DEFAULT_MODEL, vec![ChatMessage::user(prompt)]);

            match self.chat(request).await {
                Ok(response) => {
                    let duration = start_time.elapsed();

                    if let Some(choice) = response.choices.first() {
                        if let Some(content) = &choice.message.content {
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

        let request = ChatRequest::new(
            DEFAULT_MODEL,
            vec![ChatMessage::user(
                "What is the weather in Paris? Use the available tool.",
            )],
        )
        .with_tools(tools.to_vec());

        match self.chat(request).await {
            Ok(response) => {
                let duration = start_time.elapsed();
                if let Some(choice) = response.choices.first() {
                    if choice
                        .message
                        .tool_calls
                        .as_ref()
                        .map(|tc| !tc.is_empty())
                        .unwrap_or(false)
                    {
                        return Ok(ValidationResult::Success {
                            message: format!("Tool calling validated in {:?}", duration),
                            metrics: ValidationMetrics::with_duration(duration),
                        });
                    }
                }
                Ok(ValidationResult::Warning {
                    message: "Model did not use tools in test request".to_string(),
                    suggestions: vec![
                        "Model may not support tool calling".to_string(),
                        "Try a tool-calling capable model".to_string(),
                    ],
                    metrics: ValidationMetrics::with_duration(start_time.elapsed()),
                })
            }
            Err(e) => Ok(ValidationResult::Failure {
                message: "Tool calling test request failed".to_string(),
                error_details: e.to_string(),
                suggestions: vec![
                    "Check model availability".to_string(),
                    "Verify tool definitions are correct".to_string(),
                ],
                metrics: Some(ValidationMetrics::with_duration(start_time.elapsed())),
            }),
        }
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

                let request = ChatRequest::new(DEFAULT_MODEL, vec![ChatMessage::user(*prompt)]);

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
    use crate::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType};
    use std::collections::HashMap;

    #[test]
    fn test_tool_definition_to_json() {
        let mut props = HashMap::new();
        props.insert(
            "location".to_string(),
            PropertySchema {
                schema_type: SchemaType::String,
                description: Some("The city name".to_string()),
                items: None,
            },
        );
        let tool = ToolDefinition {
            function: FunctionDefinition {
                name: "get_weather".to_string(),
                description: "Get the weather for a location".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some(props),
                    required: Some(vec!["location".to_string()]),
                },
            },
        };

        let json = OllamaProvider::tools_to_json(&[tool]);

        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["type"], "function");
        assert_eq!(json[0]["function"]["name"], "get_weather");
        assert_eq!(
            json[0]["function"]["description"],
            "Get the weather for a location"
        );
        assert_eq!(json[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn test_messages_to_json_preserves_roles() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there"),
        ];

        let json = OllamaProvider::messages_to_json(&messages);

        assert_eq!(json.len(), 3);
        assert_eq!(json[0]["role"], "system");
        assert_eq!(json[0]["content"], "You are helpful");
        assert_eq!(json[1]["role"], "user");
        assert_eq!(json[1]["content"], "Hello");
        assert_eq!(json[2]["role"], "assistant");
        assert_eq!(json[2]["content"], "Hi there");
    }

    #[test]
    fn test_tool_response_message_to_json() {
        let msg = ChatMessage::tool_response("call_123", "The weather is sunny");
        let json = OllamaProvider::messages_to_json(&[msg]);

        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["role"], "tool");
        assert_eq!(json[0]["content"], "The weather is sunny");
        assert_eq!(json[0]["tool_call_id"], "call_123");
    }

    #[test]
    fn test_parse_tool_call_response() {
        let raw = OllamaChatRawResponse {
            message: OllamaRawMessage {
                role: "assistant".to_string(),
                content: String::new(),
                tool_calls: Some(vec![OllamaRawToolCall {
                    function: OllamaRawFunction {
                        name: "get_weather".to_string(),
                        arguments: serde_json::json!({"location": "Paris"}),
                    },
                }]),
            },
            done: true,
            prompt_eval_count: Some(10),
            eval_count: Some(20),
        };

        let response = OllamaProvider::parse_raw_response(raw).unwrap();

        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].finish_reason,
            Some(FinishReason::ToolCalls)
        );

        let tool_calls = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_0");
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(tool_calls[0].function.arguments["location"], "Paris");

        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.total_tokens, 30);
    }

    #[test]
    fn test_parse_plain_response() {
        let raw = OllamaChatRawResponse {
            message: OllamaRawMessage {
                role: "assistant".to_string(),
                content: "Hello, how can I help you?".to_string(),
                tool_calls: None,
            },
            done: true,
            prompt_eval_count: Some(5),
            eval_count: Some(15),
        };

        let response = OllamaProvider::parse_raw_response(raw).unwrap();

        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].finish_reason, Some(FinishReason::Stop));
        assert_eq!(
            response.choices[0].message.content,
            Some("Hello, how can I help you?".to_string())
        );
        assert!(response.choices[0].message.tool_calls.is_none());
    }

    #[test]
    fn test_parse_response_with_empty_tool_calls() {
        let raw = OllamaChatRawResponse {
            message: OllamaRawMessage {
                role: "assistant".to_string(),
                content: "No tools needed".to_string(),
                tool_calls: Some(vec![]),
            },
            done: true,
            prompt_eval_count: None,
            eval_count: None,
        };

        let response = OllamaProvider::parse_raw_response(raw).unwrap();

        assert_eq!(response.choices[0].finish_reason, Some(FinishReason::Stop));
        assert!(response.usage.is_none());
    }

    #[test]
    fn test_assistant_message_with_tool_calls_to_json() {
        let msg = ChatMessage::assistant_with_tools(
            Some("I will check the weather".to_string()),
            vec![ToolCall {
                id: "call_0".to_string(),
                function: FunctionCall {
                    name: "get_weather".to_string(),
                    arguments: serde_json::json!({"location": "London"}),
                },
            }],
        );

        let json = OllamaProvider::messages_to_json(&[msg]);

        assert_eq!(json[0]["role"], "assistant");
        assert_eq!(json[0]["content"], "I will check the weather");
        assert_eq!(json[0]["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(
            json[0]["tool_calls"][0]["function"]["arguments"]["location"],
            "London"
        );
    }

    #[tokio::test]
    async fn test_provider_creation() {
        let config = OllamaConfig::default();
        let provider = OllamaProvider::new(config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "ollama");
    }

    #[tokio::test]
    async fn test_provider_creation_strips_v1() {
        let config = OllamaConfig::default().with_base_url("http://localhost:11434/v1");
        let provider = OllamaProvider::new(config).unwrap();
        assert_eq!(provider.base_url, "http://localhost:11434");
    }
}
