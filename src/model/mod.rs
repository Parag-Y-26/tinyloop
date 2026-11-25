//! Model protocol types and the startup capability probe.
//!
//! The request shape is frozen deliberately: native tools, thinking off,
//! vendor sampling, one call per turn. See docs/MODEL_PROFILE.md.

pub mod llama_cpp;
pub use llama_cpp::LlamaCppClient;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

pub const DEFAULT_CONTEXT: usize = 8192;
pub const DEFAULT_MAX_TOKENS: u32 = 512;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatTemplateKwargs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDef,
}

impl ToolDef {
    pub fn function(name: &str, description: &str, parameters: Value) -> Self {
        Self {
            kind: "function".to_owned(),
            function: FunctionDef { name: name.to_owned(), description: description.to_owned(), parameters },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_arguments")]
    pub arguments: Value,
}

/// llama.cpp may return `arguments` as a JSON string even when the template
/// wants a dict. Normalize both forms before the tool layer sees them.
fn deserialize_arguments<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(text) => Ok(serde_json::from_str(&text)
            .unwrap_or_else(|_| serde_json::json!({ "__unparsed": text }))),
        other => Ok(other),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "default_function_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn default_function_type() -> String { "function".to_owned() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: Some(content.into()), tool_calls: None, tool_call_id: None, name: None }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: Some(content.into()), tool_calls: None, tool_call_id: None, name: None }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: Some(content.into()), tool_calls: None, tool_call_id: None, name: None }
    }
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self { role: "tool".into(), content: Some(content.into()), tool_calls: None, tool_call_id: Some(tool_call_id.into()), name: None }
    }
    pub fn assistant_with_tool_calls(content: Option<String>, calls: Vec<ToolCall>) -> Self {
        Self { role: "assistant".into(), content, tool_calls: Some(calls), tool_call_id: None, name: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: i32,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<ChatTemplateKwargs>,
}

impl ChatRequest {
    pub fn for_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            tools: None,
            tool_choice: Some("auto".to_owned()),
            temperature: 1.0,
            top_p: 0.95,
            top_k: 0,
            max_tokens: DEFAULT_MAX_TOKENS,
            parallel_tool_calls: Some(false),
            chat_template_kwargs: Some(ChatTemplateKwargs { enable_thinking: Some(false), reasoning_effort: None }),
        }
    }
    pub fn with_messages(mut self, messages: Vec<Message>) -> Self { self.messages = messages; self }
    pub fn with_tools(mut self, tools: Vec<ToolDef>) -> Self { self.tools = Some(tools); self }
    pub fn with_max_tokens(mut self, max: u32) -> Self { self.max_tokens = max; self }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Timings {
    #[serde(default)]
    pub cache_n: u32,
    #[serde(default)]
    pub prompt_ms: f64,
    #[serde(default)]
    pub predicted_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub message: AssistantMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub timings: Option<Timings>,
}

impl ChatResponse {
    pub fn first(&self) -> Option<&Choice> { self.choices.first() }
    pub fn tool_calls(&self) -> &[ToolCall] { self.first().map(|c| c.message.tool_calls.as_slice()).unwrap_or(&[]) }
    pub fn text(&self) -> Option<&str> { self.first().and_then(|c| c.message.content.as_deref()) }
    pub fn finish_reason(&self) -> Option<&str> { self.first().and_then(|c| c.finish_reason.as_deref()) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    Health(String),
    Transport(String),
    Http { status: u16, body: String },
    Parse(String),
    ToolsNotVisible { without: u32, with: u32 },
    ToolCallFailed(String),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::Health(m) => write!(f, "model health check failed: {m}"),
            ModelError::Transport(m) => write!(f, "model transport failed: {m}"),
            ModelError::Http { status, body } => write!(f, "model returned HTTP {status}: {body}"),
            ModelError::Parse(m) => write!(f, "could not parse model response: {m}"),
            ModelError::ToolsNotVisible { without, with } => write!(
                f,
                "tools were silently dropped: prompt_tokens without tools = {without}, with tools = {with}. Check --jinja and the GGUF chat template."
            ),
            ModelError::ToolCallFailed(m) => write!(f, "model did not produce a valid tool call: {m}"),
        }
    }
}
impl std::error::Error for ModelError {}

pub trait ChatTransport {
    fn health(&self) -> Result<(), ModelError>;
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ModelError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    pub build: Option<String>,
    pub tools_visible: bool,
    pub tool_call_ok: bool,
    pub prompt_tokens_without_tools: u32,
    pub prompt_tokens_with_tools: u32,
}

pub fn probe_tool() -> ToolDef {
    ToolDef::function(
        "read_file",
        "Read a UTF-8 file inside the task workspace.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Workspace-relative path" },
                "offset": { "type": "integer", "minimum": 0 },
                "limit": { "type": "integer", "minimum": 1 }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

fn probe_base(model: &str, prompt: &str) -> ChatRequest {
    ChatRequest::for_model(model).with_messages(vec![Message::user(prompt)]).with_max_tokens(128)
}

/// Verify before any real work that native tool calling is actually alive.
pub fn run_startup_probe<T: ChatTransport>(
    transport: &T,
    model: &str,
    build: Option<String>,
) -> Result<ProbeReport, ModelError> {
    transport.health()?;

    let without = transport.chat(&probe_base(model, "Reply with the single word OK."))?;
    let with = transport.chat(
        &probe_base(model, "Reply with the single word OK.").with_tools(vec![probe_tool()]),
    )?;

    let tools_visible = with.usage.prompt_tokens > without.usage.prompt_tokens;
    if !tools_visible {
        return Err(ModelError::ToolsNotVisible {
            without: without.usage.prompt_tokens,
            with: with.usage.prompt_tokens,
        });
    }

    let call_request = probe_base(
        model,
        "Call the read_file tool for path Cargo.toml. Return only the tool call, no prose.",
    )
    .with_tools(vec![probe_tool()]);
    let call_response = transport.chat(&call_request)?;
    let tool_call_ok = !call_response.tool_calls().is_empty();
    if !tool_call_ok {
        return Err(ModelError::ToolCallFailed(format!(
            "text={:?} finish_reason={:?}",
            call_response.text(), call_response.finish_reason()
        )));
    }

    Ok(ProbeReport {
        build,
        tools_visible,
        tool_call_ok,
        prompt_tokens_without_tools: without.usage.prompt_tokens,
        prompt_tokens_with_tools: with.usage.prompt_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct Fake {
        health: Result<(), ModelError>,
        queue: RefCell<VecDeque<Result<ChatResponse, ModelError>>>,
    }
    impl Fake {
        fn new(health: Result<(), ModelError>, responses: Vec<Result<ChatResponse, ModelError>>) -> Self {
            Self { health, queue: RefCell::new(responses.into()) }
        }
    }
    impl ChatTransport for Fake {
        fn health(&self) -> Result<(), ModelError> { self.health.clone() }
        fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, ModelError> {
            self.queue.borrow_mut().pop_front().expect("fake ran out of responses")
        }
    }

    fn response(prompt_tokens: u32, tool_calls: Vec<ToolCall>) -> ChatResponse {
        ChatResponse {
            choices: vec![Choice {
                message: AssistantMessage { content: Some("OK".into()), tool_calls, reasoning_content: None },
                finish_reason: Some("stop".into()),
            }],
            usage: Usage { prompt_tokens, completion_tokens: 4, prompt_tokens_details: None },
            timings: Some(Timings { cache_n: 0, prompt_ms: 1.0, predicted_ms: 2.0 }),
        }
    }

    fn sample_call() -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall { name: "read_file".into(), arguments: serde_json::json!({"path":"Cargo.toml"}) },
        }
    }

    #[test]
    fn probe_accepts_visible_tools_and_a_real_tool_call() {
        let fake = Fake::new(Ok(()), vec![Ok(response(100, vec![])), Ok(response(160, vec![])), Ok(response(170, vec![sample_call()]))]);
        let report = run_startup_probe(&fake, "spark-x26", Some("b12015".into())).unwrap();
        assert!(report.tools_visible);
        assert!(report.tool_call_ok);
        assert_eq!(report.prompt_tokens_with_tools, 160);
    }

    #[test]
    fn probe_rejects_silent_tool_drop() {
        let fake = Fake::new(Ok(()), vec![Ok(response(100, vec![])), Ok(response(100, vec![]))]);
        let err = run_startup_probe(&fake, "spark-x26", None).unwrap_err();
        assert!(matches!(err, ModelError::ToolsNotVisible { without: 100, with: 100 }));
    }

    #[test]
    fn probe_rejects_text_only_action() {
        let fake = Fake::new(Ok(()), vec![Ok(response(100, vec![])), Ok(response(160, vec![])), Ok(response(170, vec![]))]);
        let err = run_startup_probe(&fake, "spark-x26", None).unwrap_err();
        assert!(matches!(err, ModelError::ToolCallFailed(_)));
    }

    #[test]
    fn probe_surfaces_health_failure_first() {
        let fake = Fake::new(Err(ModelError::Health("down".into())), vec![]);
        let err = run_startup_probe(&fake, "spark-x26", None).unwrap_err();
        assert!(matches!(err, ModelError::Health(_)));
    }

    #[test]
    fn request_serializes_the_frozen_contract() {
        let request = ChatRequest::for_model("spark-x26")
            .with_messages(vec![Message::user("hi")])
            .with_tools(vec![probe_tool()]);
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(json["temperature"], 1.0);
        assert_eq!(json["top_p"], 0.95);
        assert_eq!(json["top_k"], 0);
        assert_eq!(json["parallel_tool_calls"], false);
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn malformed_tool_arguments_become_a_tool_error_not_a_parse_crash() {
        let raw = r#"{"choices":[{"message":{"content":null,"tool_calls":[
            {"id":"c1","type":"function","function":{"name":"read_file","arguments":"{not json"}}]},
            "finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#;
        let parsed: ChatResponse = serde_json::from_str(raw).unwrap();
        assert!(parsed.tool_calls()[0].function.arguments.get("__unparsed").is_some());
    }

    #[test]
    fn stringified_tool_arguments_are_normalized() {
        let raw = r#"{"choices":[{"message":{"content":null,"tool_calls":[
            {"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"Cargo.toml\"}"}}]},
            "finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#;
        let parsed: ChatResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.tool_calls()[0].function.arguments["path"], "Cargo.toml");
    }

    #[test]
    fn response_parses_openai_tool_calls() {
        let raw = r#"{
          "choices":[{"message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"read_file","arguments":{"path":"src/tax.rs"}}}]},
            "finish_reason":"tool_calls"}],
          "usage":{"prompt_tokens":900,"completion_tokens":24,"prompt_tokens_details":{"cached_tokens":512}},
          "timings":{"cache_n":512,"prompt_ms":10.0,"predicted_ms":20.0}
        }"#;
        let parsed: ChatResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.tool_calls().len(), 1);
        assert_eq!(parsed.tool_calls()[0].function.name, "read_file");
        assert_eq!(parsed.tool_calls()[0].function.arguments["path"], "src/tax.rs");
        assert_eq!(parsed.usage.prompt_tokens_details.as_ref().unwrap().cached_tokens, 512);
        assert_eq!(parsed.timings.as_ref().unwrap().cache_n, 512);
    }
}
