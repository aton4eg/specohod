use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
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
    semaphore: Arc<Semaphore>,
}

fn sanitize_prompt(text: &str) -> String {
    text.trim().to_string()
}

fn compute_retry_delay_ms(attempt: u32) -> u64 {
    const INITIAL_BACKOFF_MS: u64 = 1000;
    (INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1)).min(30_000)
}

enum AttemptOutcome {
    Success(String),
    RetryableError,
    FatalError(LlmError),
}

impl LlmClient {
    /// Creates a new `LlmClient`.
    ///
    /// Builds an internal `reqwest::Client` with a 300-second timeout.
    ///
    /// # Parameters
    ///
    /// * `config` — LLM API configuration (base URL, API key, model, etc.).
    /// * `stats` — Shared statistics counter.
    /// * `semaphore` — Concurrency limiter for LLM requests.
    pub fn new(config: Config, stats: Arc<LlmStats>, semaphore: Arc<Semaphore>) -> Self {
        Self {
            config,
            client: Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("Failed to build reqwest Client"),
            stats,
            semaphore,
        }
    }

    fn update_usage_stats(&self, usage: Option<Usage>, prompt_len: u64, completion_len: u64) {
        if let Some(usage) = usage {
            self.stats
                .prompt_tokens
                .fetch_add(usage.prompt_tokens as u64, Ordering::Relaxed);
            self.stats
                .completion_tokens
                .fetch_add(usage.completion_tokens as u64, Ordering::Relaxed);
            self.stats
                .total_tokens
                .fetch_add(usage.total_tokens as u64, Ordering::Relaxed);
        }
        self.stats
            .prompt_chars
            .fetch_add(prompt_len, Ordering::Relaxed);
        self.stats
            .completion_chars
            .fetch_add(completion_len, Ordering::Relaxed);
    }

    async fn send_request(
        &self,
        url: &str,
        request: &ChatRequest,
    ) -> Result<reqwest::Response, reqwest::Error> {
        self.client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(request)
            .send()
            .await
    }

    async fn parse_success_response(
        &self,
        response_text: String,
        system_prompt_len: usize,
        user_prompt_len: usize,
    ) -> std::result::Result<String, LlmError> {
        let chat_response: ChatResponse = serde_json::from_str(&response_text).map_err(|e| {
            LlmError::Other(anyhow::anyhow!(
                "Failed to parse LLM response: {}. Body: {}",
                e,
                response_text
            ))
        })?;

        let content = chat_response
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| LlmError::Other(anyhow::anyhow!("No choices in LLM response")))?;

        let prompt_len = (system_prompt_len + user_prompt_len) as u64;
        let completion_len = content.len() as u64;
        self.update_usage_stats(chat_response.usage, prompt_len, completion_len);

        Ok(content)
    }

    async fn execute_attempt(
        &self,
        url: &str,
        request: &ChatRequest,
        attempt: u32,
        max_retries: u32,
        system_prompt_len: usize,
        user_prompt_len: usize,
    ) -> AttemptOutcome {
        let response = match self.send_request(url, request).await {
            Ok(r) => r,
            Err(e) => {
                log::warn!(
                    "LLM request network error (attempt {}/{}): {}",
                    attempt,
                    max_retries,
                    e,
                );
                return AttemptOutcome::RetryableError;
            }
        };

        let status = response.status();
        Self::log_rate_limits(&response);

        if status.is_success() {
            let response_text = match response.text().await {
                Ok(text) => text,
                Err(e) => return AttemptOutcome::FatalError(LlmError::Other(e.into())),
            };
            match self
                .parse_success_response(response_text, system_prompt_len, user_prompt_len)
                .await
            {
                Ok(content) => return AttemptOutcome::Success(content),
                Err(e) => return AttemptOutcome::FatalError(e),
            }
        }

        let error_text = response.text().await.unwrap_or_default();
        log::warn!(
            "LLM request failed (attempt {}/{}): status {}, body: {}",
            attempt,
            max_retries,
            status.as_u16(),
            error_text,
        );

        if status == StatusCode::BAD_REQUEST && Self::is_context_length_error(&error_text) {
            return AttemptOutcome::FatalError(LlmError::ContextLengthExceeded);
        }

        if Self::is_retryable_status(status) && attempt < max_retries {
            AttemptOutcome::RetryableError
        } else {
            AttemptOutcome::FatalError(LlmError::Other(anyhow::anyhow!(
                "LLM API error after {} attempts: {} - {}",
                attempt,
                status,
                error_text,
            )))
        }
    }

    async fn handle_retry_loop(
        &self,
        url: String,
        request: ChatRequest,
        system_prompt_len: usize,
        user_prompt_len: usize,
    ) -> std::result::Result<String, LlmError> {
        const MAX_RETRIES: u32 = 8;

        let mut attempt = 0;
        loop {
            attempt += 1;
            match self
                .execute_attempt(
                    &url,
                    &request,
                    attempt,
                    MAX_RETRIES,
                    system_prompt_len,
                    user_prompt_len,
                )
                .await
            {
                AttemptOutcome::Success(result) => return Ok(result),
                AttemptOutcome::RetryableError => {
                    let delay = compute_retry_delay_ms(attempt);
                    log::info!("Retrying after {} ms...", delay);
                    sleep(Duration::from_millis(delay)).await;
                    continue;
                }
                AttemptOutcome::FatalError(e) => return Err(e),
            }
        }
    }

    /// Logs rate-limit headers (`x-ratelimit-remaining-requests` and
    /// `x-ratelimit-remaining-tokens`) from an LLM API response at debug level.
    fn log_rate_limits(response: &reqwest::Response) {
        if let Some(remaining) = response.headers().get("x-ratelimit-remaining-requests") {
            log::debug!("RateLimit remaining requests: {:?}", remaining);
        }
        if let Some(remaining_tokens) = response.headers().get("x-ratelimit-remaining-tokens") {
            log::debug!("RateLimit remaining tokens: {:?}", remaining_tokens);
        }
    }

    /// Checks whether an error text indicates that the context length was
    /// exceeded by matching known error message patterns.
    fn is_context_length_error(error_text: &str) -> bool {
        let lower = error_text.to_lowercase();
        lower.contains("exceeds the model's maximum context length")
            || lower.contains("is longer than the model's context length")
    }

    /// Returns `true` if an HTTP status code indicates a transient error that
    /// can be retried (429, 500, 502, 503, 504).
    fn is_retryable_status(status: StatusCode) -> bool {
        matches!(
            status,
            StatusCode::TOO_MANY_REQUESTS
                | StatusCode::INTERNAL_SERVER_ERROR
                | StatusCode::BAD_GATEWAY
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT
        )
    }

    /// Sends a chat completion request to the LLM API with system and user
    /// prompts. Acquires a semaphore permit before sending.
    ///
    /// Automatically retries on transient errors with exponential backoff.
    ///
    /// # Parameters
    ///
    /// * `system_prompt` — The system-level prompt text.
    /// * `user_prompt` — The user-level prompt text.
    ///
    /// # Returns
    ///
    /// The generated completion text, or an `LlmError` on failure.
    pub async fn generate_completion(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> std::result::Result<String, LlmError> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| LlmError::Other(anyhow::anyhow!("Semaphore closed: {}", e)))?;

        let url = format!(
            "{}/chat/completions",
            self.config.api_base.trim_end_matches('/')
        );
        let system_prompt = sanitize_prompt(system_prompt);
        let user_prompt = sanitize_prompt(user_prompt);
        let system_prompt_len = system_prompt.len();
        let user_prompt_len = user_prompt.len();

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_prompt,
                },
            ],
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
        };

        self.handle_retry_loop(url, request, system_prompt_len, user_prompt_len)
            .await
    }
}
