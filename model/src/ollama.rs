use crate::config::OllamaConfig;
use crate::judge::{
    JudgeConfig, ModelJudge, ValidationCriteria, ValidationMetrics, ValidationResult,
};
use crate::provider::{ModelError, ModelProvider, ModelResult};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, JsonSchema,
    MessageRole, ModelInfo, PropertySchema, ToolCall, ToolDefinition, Usage,
};
use async_trait::async_trait;
use ollama_rs::Ollama;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use std::time::Instant;
use tracing::{debug, error, info, warn};

#[derive(Serialize)]
struct OllamaApiRequest {
    model: String,
    messages: Vec<OllamaApiMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OllamaApiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaApiOptions>,
}

#[derive(Serialize)]
struct OllamaApiMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OllamaApiToolCall>,
}

#[derive(Serialize)]
struct OllamaApiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OllamaApiFunctionDef,
}

#[derive(Serialize)]
struct OllamaApiFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Serialize)]
struct OllamaApiOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

#[derive(Deserialize)]
struct OllamaApiResponse {
    message: OllamaApiResponseMessage,
    #[allow(dead_code)]
    done: bool,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
}

#[derive(Deserialize)]
struct OllamaApiResponseMessage {
    #[allow(dead_code)]
    role: String,
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaApiToolCall>,
}

#[derive(Serialize, Deserialize, Clone)]
struct OllamaApiToolCall {
    function: OllamaApiToolCallFunction,
}

#[derive(Serialize, Deserialize, Clone)]
struct OllamaApiToolCallFunction {
    name: String,
    arguments: serde_json::Value,
}

pub struct OllamaProvider {
    client: Ollama,
    http_client: reqwest::Client,
    base_url: String,
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

        let base_url = if host.ends_with('/') {
            host.clone()
        } else {
            format!("{}/", host)
        };

        let client = Ollama::new(host, 11434);

        let http_client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| ModelError::Unknown {
                message: format!("Failed to build HTTP client: {}", e),
            })?;

        Ok(Self {
            client,
            http_client,
            base_url,
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

    fn convert_tool_def(tool: &ToolDefinition) -> OllamaApiTool {
        OllamaApiTool {
            tool_type: "function".to_string(),
            function: OllamaApiFunctionDef {
                name: tool.function.name.clone(),
                description: tool.function.description.clone(),
                parameters: Self::convert_schema_to_json(&tool.function.parameters),
            },
        }
    }

    fn convert_schema_to_json(schema: &JsonSchema) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "type".to_string(),
            serde_json::to_value(&schema.schema_type)
                .unwrap_or(serde_json::Value::String("object".to_string())),
        );

        if let Some(properties) = &schema.properties {
            let mut props = serde_json::Map::new();
            for (name, prop) in properties {
                props.insert(name.clone(), Self::convert_property_to_json(prop));
            }
            obj.insert("properties".to_string(), serde_json::Value::Object(props));
        }

        if let Some(required) = &schema.required {
            obj.insert(
                "required".to_string(),
                serde_json::to_value(required).unwrap_or_default(),
            );
        }

        serde_json::Value::Object(obj)
    }

    fn convert_property_to_json(prop: &PropertySchema) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "type".to_string(),
            serde_json::to_value(&prop.schema_type)
                .unwrap_or(serde_json::Value::String("string".to_string())),
        );

        if let Some(description) = &prop.description {
            obj.insert(
                "description".to_string(),
                serde_json::Value::String(description.clone()),
            );
        }

        if let Some(items) = &prop.items {
            obj.insert("items".to_string(), Self::convert_property_to_json(items));
        }

        serde_json::Value::Object(obj)
    }

    fn convert_message_to_api(msg: &ChatMessage) -> OllamaApiMessage {
        let role = match &msg.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };

        let tool_calls = msg.tool_calls.as_ref().map_or_else(Vec::new, |calls| {
            calls
                .iter()
                .map(|tc| OllamaApiToolCall {
                    function: OllamaApiToolCallFunction {
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    },
                })
                .collect()
        });

        OllamaApiMessage {
            role: role.to_string(),
            content: msg.content.clone().unwrap_or_default(),
            tool_calls,
        }
    }

    fn build_request_body(request: &ChatRequest) -> OllamaApiRequest {
        let messages = request
            .messages
            .iter()
            .map(Self::convert_message_to_api)
            .collect();

        let tools = request.tools.as_ref().map_or_else(Vec::new, |tools| {
            tools.iter().map(Self::convert_tool_def).collect()
        });

        let options = if request.temperature.is_some() || request.max_tokens.is_some() {
            Some(OllamaApiOptions {
                temperature: request.temperature,
                num_predict: request.max_tokens,
            })
        } else {
            None
        };

        OllamaApiRequest {
            model: request.model.clone(),
            messages,
            stream: false,
            tools,
            options,
        }
    }

    fn parse_response(response: OllamaApiResponse) -> ChatResponse {
        let has_tool_calls = !response.message.tool_calls.is_empty();

        let tool_calls: Option<Vec<ToolCall>> = if has_tool_calls {
            Some(
                response
                    .message
                    .tool_calls
                    .iter()
                    .enumerate()
                    .map(|(i, tc)| ToolCall {
                        id: format!("call_{}", i),
                        function: FunctionCall {
                            name: tc.function.name.clone(),
                            arguments: tc.function.arguments.clone(),
                        },
                    })
                    .collect(),
            )
        } else {
            None
        };

        let content = if response.message.content.is_empty() {
            None
        } else {
            Some(response.message.content)
        };

        let finish_reason = if has_tool_calls {
            Some(FinishReason::ToolCalls)
        } else {
            Some(FinishReason::Stop)
        };

        let message = ChatMessage {
            role: MessageRole::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
        };

        let choice = Choice {
            message,
            finish_reason,
        };

        let usage = Some(Usage {
            prompt_tokens: response.prompt_eval_count.unwrap_or(0) as u32,
            completion_tokens: response.eval_count.unwrap_or(0) as u32,
            total_tokens: (response.prompt_eval_count.unwrap_or(0)
                + response.eval_count.unwrap_or(0)) as u32,
        });

        ChatResponse {
            choices: vec![choice],
            usage,
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

        let body = Self::build_request_body(&request);
        let url = format!("{}api/chat", self.base_url);

        let http_response = self
            .http_client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ModelError::ServiceUnavailable {
                        message: "Request timeout".to_string(),
                    }
                } else if e.is_connect() {
                    ModelError::ServiceUnavailable {
                        message: "Cannot connect to Ollama service".to_string(),
                    }
                } else {
                    ModelError::Network(e)
                }
            })?;

        let status = http_response.status();
        if !status.is_success() {
            let error_text = http_response.text().await.unwrap_or_default();
            return Err(ModelError::Unknown {
                message: format!("Ollama API returned {}: {}", status, error_text),
            });
        }

        let api_response: OllamaApiResponse =
            http_response.json().await.map_err(ModelError::Network)?;

        let chat_response = Self::parse_response(api_response);

        info!("Chat request completed successfully");

        Ok(chat_response)
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
            let request = ChatRequest::new("qwen3:0.6b", vec![ChatMessage::user(prompt)]);

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
    use crate::types::{FunctionDefinition, SchemaType};
    use std::collections::HashMap;

    #[test]
    fn test_tool_definition_to_ollama_json() {
        let mut properties = HashMap::new();
        properties.insert(
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
                    properties: Some(properties),
                    required: Some(vec!["location".to_string()]),
                },
            },
        };

        let ollama_tool = OllamaProvider::convert_tool_def(&tool);
        assert_eq!(ollama_tool.tool_type, "function");
        assert_eq!(ollama_tool.function.name, "get_weather");
        assert_eq!(
            ollama_tool.function.description,
            "Get the weather for a location"
        );

        let params = &ollama_tool.function.parameters;
        assert_eq!(params["type"], "object");
        assert_eq!(params["properties"]["location"]["type"], "string");
        assert_eq!(
            params["properties"]["location"]["description"],
            "The city name"
        );
        assert_eq!(params["required"][0], "location");
    }

    #[test]
    fn test_parse_chat_response_no_tools() {
        let api_response = OllamaApiResponse {
            message: OllamaApiResponseMessage {
                role: "assistant".to_string(),
                content: "Hello! How can I help?".to_string(),
                tool_calls: vec![],
            },
            done: true,
            prompt_eval_count: Some(10),
            eval_count: Some(5),
        };

        let response = OllamaProvider::parse_response(api_response);
        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].message.content,
            Some("Hello! How can I help?".to_string())
        );
        assert!(matches!(
            response.choices[0].finish_reason,
            Some(FinishReason::Stop)
        ));
        assert!(response.choices[0].message.tool_calls.is_none());

        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn test_parse_chat_response_with_tool_calls() {
        let api_response = OllamaApiResponse {
            message: OllamaApiResponseMessage {
                role: "assistant".to_string(),
                content: "".to_string(),
                tool_calls: vec![
                    OllamaApiToolCall {
                        function: OllamaApiToolCallFunction {
                            name: "get_weather".to_string(),
                            arguments: serde_json::json!({"city": "Paris"}),
                        },
                    },
                    OllamaApiToolCall {
                        function: OllamaApiToolCallFunction {
                            name: "get_time".to_string(),
                            arguments: serde_json::json!({"timezone": "CET"}),
                        },
                    },
                ],
            },
            done: true,
            prompt_eval_count: Some(20),
            eval_count: Some(0),
        };

        let response = OllamaProvider::parse_response(api_response);
        assert!(matches!(
            response.choices[0].finish_reason,
            Some(FinishReason::ToolCalls)
        ));
        assert!(response.choices[0].message.content.is_none());

        let tool_calls = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].id, "call_0");
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(
            tool_calls[0].function.arguments,
            serde_json::json!({"city": "Paris"})
        );
        assert_eq!(tool_calls[1].id, "call_1");
        assert_eq!(tool_calls[1].function.name, "get_time");
        assert_eq!(
            tool_calls[1].function.arguments,
            serde_json::json!({"timezone": "CET"})
        );
    }

    #[test]
    fn test_build_chat_request_body() {
        let tool = ToolDefinition {
            function: FunctionDefinition {
                name: "test_fn".to_string(),
                description: "A test function".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: None,
                    required: None,
                },
            },
        };

        let request = ChatRequest::new(
            "llama3.1:8b",
            vec![
                ChatMessage::system("You are helpful"),
                ChatMessage::user("Hello"),
            ],
        )
        .with_tools(vec![tool])
        .with_temperature(0.5)
        .with_max_tokens(100);

        let body = OllamaProvider::build_request_body(&request);

        assert_eq!(body.model, "llama3.1:8b");
        assert!(!body.stream);
        assert_eq!(body.messages.len(), 2);
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[0].content, "You are helpful");
        assert_eq!(body.messages[1].role, "user");
        assert_eq!(body.messages[1].content, "Hello");
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].function.name, "test_fn");

        let options = body.options.unwrap();
        assert_eq!(options.temperature, Some(0.5));
        assert_eq!(options.num_predict, Some(100));
    }

    #[test]
    fn test_message_conversion() {
        let msg = ChatMessage::user("Hello world");
        let api_msg = OllamaProvider::convert_message_to_api(&msg);
        assert_eq!(api_msg.role, "user");
        assert_eq!(api_msg.content, "Hello world");
        assert!(api_msg.tool_calls.is_empty());

        let tool_calls = vec![ToolCall {
            id: "call_0".to_string(),
            function: FunctionCall {
                name: "test".to_string(),
                arguments: serde_json::json!({"key": "value"}),
            },
        }];
        let assistant_msg =
            ChatMessage::assistant_with_tools(Some("thinking...".to_string()), tool_calls);
        let api_msg = OllamaProvider::convert_message_to_api(&assistant_msg);
        assert_eq!(api_msg.role, "assistant");
        assert_eq!(api_msg.content, "thinking...");
        assert_eq!(api_msg.tool_calls.len(), 1);
        assert_eq!(api_msg.tool_calls[0].function.name, "test");
        assert_eq!(
            api_msg.tool_calls[0].function.arguments,
            serde_json::json!({"key": "value"})
        );

        let tool_msg = ChatMessage::tool_response("call_0", "result");
        let api_msg = OllamaProvider::convert_message_to_api(&tool_msg);
        assert_eq!(api_msg.role, "tool");
        assert_eq!(api_msg.content, "result");
    }

    #[test]
    fn test_finish_reason_mapping() {
        let no_tools = OllamaApiResponse {
            message: OllamaApiResponseMessage {
                role: "assistant".to_string(),
                content: "hi".to_string(),
                tool_calls: vec![],
            },
            done: true,
            prompt_eval_count: None,
            eval_count: None,
        };
        assert!(matches!(
            OllamaProvider::parse_response(no_tools).choices[0].finish_reason,
            Some(FinishReason::Stop)
        ));

        let with_tools = OllamaApiResponse {
            message: OllamaApiResponseMessage {
                role: "assistant".to_string(),
                content: "".to_string(),
                tool_calls: vec![OllamaApiToolCall {
                    function: OllamaApiToolCallFunction {
                        name: "f".to_string(),
                        arguments: serde_json::json!({}),
                    },
                }],
            },
            done: true,
            prompt_eval_count: None,
            eval_count: None,
        };
        assert!(matches!(
            OllamaProvider::parse_response(with_tools).choices[0].finish_reason,
            Some(FinishReason::ToolCalls)
        ));
    }

    #[tokio::test]
    async fn test_provider_creation() {
        let config = OllamaConfig::default();
        let provider = OllamaProvider::new(config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "ollama");
    }

    #[test]
    fn test_build_request_body_no_options() {
        let request = ChatRequest::new("llama3.1:8b", vec![ChatMessage::user("Hello")]);
        let body = OllamaProvider::build_request_body(&request);
        assert!(body.options.is_none());
    }

    #[test]
    fn test_message_conversion_system_role() {
        let msg = ChatMessage::system("Be helpful");
        let api_msg = OllamaProvider::convert_message_to_api(&msg);
        assert_eq!(api_msg.role, "system");
        assert_eq!(api_msg.content, "Be helpful");
    }

    #[test]
    fn test_provider_creation_url_normalization() {
        let config = OllamaConfig::default().with_base_url("http://localhost:11434/v1");
        let provider = OllamaProvider::new(config).unwrap();
        assert_eq!(provider.base_url, "http://localhost:11434/");

        let config = OllamaConfig::default().with_base_url("http://localhost:11434");
        let provider = OllamaProvider::new(config).unwrap();
        assert_eq!(provider.base_url, "http://localhost:11434/");
    }

    #[test]
    fn test_handle_ollama_error_json_error() {
        let json_err = serde_json::from_str::<i32>("invalid").unwrap_err();
        let ollama_err = ollama_rs::error::OllamaError::JsonError(json_err);
        let result = OllamaProvider::handle_ollama_error(ollama_err);
        assert!(matches!(result, ModelError::Serialization(_)));
    }

    #[tokio::test]
    async fn test_chat_returns_error_on_non_success_status() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/api/chat")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let config = OllamaConfig::default().with_base_url(server.url());
        let provider = OllamaProvider::new(config).unwrap();
        let request = ChatRequest::new("test-model", vec![ChatMessage::user("hi")]);
        let result = provider.chat(request).await;
        assert!(matches!(result, Err(ModelError::Unknown { .. })));
    }

    #[tokio::test]
    async fn test_chat_returns_error_on_invalid_json() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/api/chat")
            .with_status(200)
            .with_body("not valid json")
            .create_async()
            .await;

        let config = OllamaConfig::default().with_base_url(server.url());
        let provider = OllamaProvider::new(config).unwrap();
        let request = ChatRequest::new("test-model", vec![ChatMessage::user("hi")]);
        let result = provider.chat(request).await;
        assert!(matches!(result, Err(ModelError::Network(_))));
    }
}
