use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use log::info;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct Config {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug)]
pub enum LlmError {
    ContextLengthExceeded,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for LlmError {
    fn from(e: anyhow::Error) -> Self {
        LlmError::Other(e)
    }
}

impl From<LlmError> for anyhow::Error {
    fn from(e: LlmError) -> Self {
        match e {
            LlmError::ContextLengthExceeded => {
                anyhow::anyhow!("LLM context length exceeded")
            }
            LlmError::Other(err) => err,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Default)]
pub struct LlmStats {
    pub prompt_tokens: AtomicU64,
    pub completion_tokens: AtomicU64,
    pub total_tokens: AtomicU64,
    pub prompt_chars: AtomicU64,
    pub completion_chars: AtomicU64,
}

pub struct LlmClient {
    config: Config,
    client: Client,
    stats: Arc<LlmStats>,
}

fn sanitize_prompt(text: &str) -> String {
    text.split(' ')
        .filter(|part| !part.is_empty())
        .collect::<Vec<&str>>()
        .join(" ")
}

impl LlmClient {
    pub fn new(config: Config, stats: Arc<LlmStats>) -> Self {
        Self {
            config,
            client: Client::new(),
            stats,
        }
    }

    pub async fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> std::result::Result<String, LlmError> {
        const MAX_RETRIES: u32 = 8;
        const INITIAL_BACKOFF_MS: u64 = 1000;

        let url = format!(
            "{}/chat/completions",
            self.config.api_base.trim_end_matches('/')
        );
        let system_prompt = sanitize_prompt(system_prompt);
        let user_prompt = sanitize_prompt(user_prompt);

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_prompt.to_string(),
                },
            ],
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
        };

        let mut attempt = 0;
        loop {
            attempt += 1;
            let response_result = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.config.api_key))
                .json(&request)
                .send()
                .await;

            match response_result {
                Ok(response) => {
                    let status = response.status();
                    if let Some(remaining) =
                        response.headers().get("x-ratelimit-remaining-requests")
                    {
                        log::debug!("RateLimit remaining requests: {:?}", remaining);
                    }
                    if let Some(remaining_tokens) =
                        response.headers().get("x-ratelimit-remaining-tokens")
                    {
                        log::debug!("RateLimit remaining tokens: {:?}", remaining_tokens);
                    }

                    if status.is_success() {
                        let response_text = response
                            .text()
                            .await
                            .context("Failed to read response body")
                            .map_err(LlmError::Other)?;
                        match serde_json::from_str::<ChatResponse>(&response_text) {
                            Ok(chat_response) => {
                                let content = chat_response
                                    .choices
                                    .into_iter()
                                    .next()
                                    .map(|c| c.message.content)
                                    .ok_or_else(|| anyhow::anyhow!("No choices in LLM response"))
                                    .map_err(LlmError::Other)?;
                                if let Some(usage) = chat_response.usage {
                                    self.stats
                                        .prompt_tokens
                                        .fetch_add(usage.prompt_tokens as u64, Ordering::Relaxed);
                                    self.stats.completion_tokens.fetch_add(
                                        usage.completion_tokens as u64,
                                        Ordering::Relaxed,
                                    );
                                    self.stats
                                        .total_tokens
                                        .fetch_add(usage.total_tokens as u64, Ordering::Relaxed);
                                }
                                let prompt_len = (system_prompt.len() + user_prompt.len()) as u64;
                                let completion_len = content.len() as u64;
                                self.stats
                                    .prompt_chars
                                    .fetch_add(prompt_len, Ordering::Relaxed);
                                self.stats
                                    .completion_chars
                                    .fetch_add(completion_len, Ordering::Relaxed);

                                info!("\n=== LLM Cumulative Statistics ===");
                                info!(
                                    "Prompt tokens:   {}",
                                    self.stats.prompt_tokens.load(Ordering::Relaxed)
                                );
                                info!(
                                    "Completion tokens: {}",
                                    self.stats.completion_tokens.load(Ordering::Relaxed)
                                );
                                info!(
                                    "Total tokens:    {}",
                                    self.stats.total_tokens.load(Ordering::Relaxed)
                                );
                                info!(
                                    "Prompt chars:    {}",
                                    self.stats.prompt_chars.load(Ordering::Relaxed)
                                );
                                info!(
                                    "Completion chars: {}",
                                    self.stats.completion_chars.load(Ordering::Relaxed)
                                );

                                return Ok(content);
                            }
                            Err(parse_err) => {
                                log::warn!(
                                    "Failed to parse LLM response (attempt {}/{}): {}. Raw body: {}",
                                    attempt,
                                    MAX_RETRIES,
                                    parse_err,
                                    &response_text[..response_text.len().min(500)]
                                );
                                if attempt < MAX_RETRIES {
                                    let delay = INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1);
                                    log::info!("Retrying after {} ms...", delay);
                                    sleep(Duration::from_millis(delay)).await;
                                    continue;
                                } else {
                                    return Err(LlmError::Other(anyhow::anyhow!(
                                        "Failed to parse LLM response after {} attempts: {}\nBody: {}",
                                        attempt,
                                        parse_err,
                                        response_text
                                    )));
                                }
                            }
                        }
                    } else {
                        let status_code = status.as_u16();
                        let error_text = response.text().await.unwrap_or_default();
                        log::warn!(
                            "LLM request failed (attempt {}/{}): status {}, body: {}",
                            attempt,
                            MAX_RETRIES,
                            status_code,
                            error_text
                        );

                        if status_code == 400
                            && (error_text
                                .to_lowercase()
                                .contains("exceeds the model's maximum context length")
                                || error_text
                                    .to_lowercase()
                                    .contains("is longer than the model's context length"))
                        {
                            return Err(LlmError::ContextLengthExceeded);
                        }

                        let should_retry = matches!(
                            status,
                            StatusCode::TOO_MANY_REQUESTS
                                | StatusCode::INTERNAL_SERVER_ERROR
                                | StatusCode::BAD_GATEWAY
                                | StatusCode::SERVICE_UNAVAILABLE
                                | StatusCode::GATEWAY_TIMEOUT
                        ) && attempt < MAX_RETRIES;

                        if should_retry {
                            let delay = INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1);
                            log::info!("Retrying after {} ms...", delay);
                            sleep(Duration::from_millis(delay)).await;
                            continue;
                        } else {
                            return Err(LlmError::Other(anyhow::anyhow!(
                                "LLM API error after {} attempts: {} - {}",
                                attempt,
                                status,
                                error_text
                            )));
                        }
                    }
                }
                Err(e) => {
                    log::warn!(
                        "LLM request network error (attempt {}/{}): {}",
                        attempt,
                        MAX_RETRIES,
                        e
                    );
                    if attempt < MAX_RETRIES {
                        let delay = INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1);
                        log::info!("Retrying after {} ms...", delay);
                        sleep(Duration::from_millis(delay)).await;
                        continue;
                    } else {
                        return Err(LlmError::Other(
                            anyhow::Error::new(e).context("LLM request failed after retries"),
                        ));
                    }
                }
            }
        }
    }
}
