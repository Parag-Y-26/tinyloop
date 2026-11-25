//! Blocking HTTP client for llama-server's OpenAI-compatible endpoint.

use super::{ChatRequest, ChatResponse, ChatTransport, ModelError};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct LlamaCppClient {
    base_url: String,
    model: String,
    http: reqwest::blocking::Client,
}

impl LlamaCppClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Result<Self, ModelError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| ModelError::Transport(e.to_string()))?;
        Ok(Self { base_url: base_url.into().trim_end_matches('/').to_owned(), model: model.into(), http })
    }

    pub fn local(model: impl Into<String>) -> Result<Self, ModelError> {
        Self::new("http://127.0.0.1:9941", model)
    }

    pub fn model(&self) -> &str { &self.model }
    pub fn base_url(&self) -> &str { &self.base_url }

    fn url(&self, path: &str) -> String { format!("{}{}", self.base_url, path) }
}

impl ChatTransport for LlamaCppClient {
    fn health(&self) -> Result<(), ModelError> {
        let response = self.http.get(self.url("/health")).send().map_err(|e| ModelError::Transport(e.to_string()))?;
        if !response.status().is_success() {
            return Err(ModelError::Health(format!("HTTP {}", response.status())));
        }
        let body = response.text().map_err(|e| ModelError::Transport(e.to_string()))?;
        if body.trim().is_empty() || body.contains("\"ok\"") || body.contains("\"status\"") {
            Ok(())
        } else {
            Err(ModelError::Health(body.chars().take(200).collect()))
        }
    }

    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ModelError> {
        let response = self
            .http
            .post(self.url("/v1/chat/completions"))
            .json(request)
            .send()
            .map_err(|e| ModelError::Transport(e.to_string()))?;
        let status = response.status();
        let body = response.text().map_err(|e| ModelError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(ModelError::Http { status: status.as_u16(), body: body.chars().take(600).collect() });
        }
        serde_json::from_str(&body).map_err(|e| ModelError::Parse(format!("{e}; body={}", body.chars().take(400).collect::<String>())))
    }
}
