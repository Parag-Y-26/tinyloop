//! Deterministic context budget and compaction.
//!
//! At 85% of the window we write `state.md` first, then replace the middle of
//! the transcript with a compacted state message while keeping the exact tail.

use crate::model::Message;
use std::fs;
use std::path::Path;

pub const DEFAULT_CONTEXT_TOKENS: usize = 8192;
pub const COMPACT_AT: f64 = 0.85;
pub const KEEP_RECENT: usize = 4;
/// The frozen system prompt plus the seven tool schemas cost roughly this
/// much of the window even though they are not part of `messages`.
pub const TOOL_SCHEMA_TOKENS: usize = 1200;

#[derive(Debug, Clone)]
pub struct ContextState {
    pub goal: String,
    pub done: String,
    pub boundaries: Vec<String>,
    pub facts: Vec<String>,
    pub next_step: String,
    pub compactions: usize,
}

impl ContextState {
    pub fn new(goal: impl Into<String>, done: impl Into<String>, boundaries: &[String]) -> Self {
        Self {
            goal: goal.into(),
            done: done.into(),
            boundaries: boundaries.to_vec(),
            facts: Vec::new(),
            next_step: String::new(),
            compactions: 0,
        }
    }

    pub fn observe(&mut self, tool: &str, summary: &str) {
        let fact = format!("{tool}: {summary}");
        if !self.facts.iter().any(|f| f == &fact) {
            self.facts.push(fact);
        }
        // Keep the state bounded; the newest facts are the useful ones.
        if self.facts.len() > 16 {
            let excess = self.facts.len() - 16;
            self.facts.drain(0..excess);
        }
    }

    pub fn render(&self) -> String {
        let boundaries = if self.boundaries.is_empty() { "none".to_owned() } else { self.boundaries.join("; ") };
        let facts = if self.facts.is_empty() { "none yet".to_owned() } else { self.facts.join("\n- ") };
        format!(
            "# tinyloop state\n\n- Goal: {}\n- Done when: {}\n- Must not: {}\n- Next step: {}\n- Compactions: {}\n\n## Facts\n- {}\n",
            self.goal, self.done, boundaries,
            if self.next_step.is_empty() { "unknown" } else { &self.next_step },
            self.compactions, facts
        )
    }
}

pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() / 4).max(1)
}

pub fn estimate_message(message: &Message) -> usize {
    let mut total = estimate_tokens(&message.role);
    if let Some(content) = &message.content { total += estimate_tokens(content); }
    if let Some(calls) = &message.tool_calls {
        if let Ok(json) = serde_json::to_string(calls) { total += estimate_tokens(&json); }
    }
    if let Some(id) = &message.tool_call_id { total += estimate_tokens(id); }
    total
}

pub fn estimate_messages(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message).sum()
}

pub fn should_compact(messages: &[Message], context_tokens: usize) -> bool {
    should_compact_with_usage(messages, context_tokens, None)
}

/// Prefer the exact `usage.prompt_tokens` the server already reported; it
/// includes the tool schemas. Fall back to the char estimate plus the schema
/// overhead only until the first response arrives.
pub fn should_compact_with_usage(
    messages: &[Message],
    context_tokens: usize,
    reported_prompt_tokens: Option<u32>,
) -> bool {
    let threshold = (context_tokens as f64 * COMPACT_AT) as usize;
    let estimated = estimate_messages(messages) + TOOL_SCHEMA_TOKENS;
    let reported = reported_prompt_tokens.unwrap_or(0) as usize;
    estimated > threshold || reported > threshold
}

pub fn write_state(root: &Path, state: &ContextState) {
    let dir = root.join(".tinyloop");
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join("state.md"), state.render());
}

pub fn compact(messages: &mut Vec<Message>, state: &mut ContextState, root: &Path) -> bool {
    if messages.len() <= KEEP_RECENT + 1 { return false; }
    write_state(root, state);
    let system = messages.first().cloned();
    // Never start the kept tail with a tool result: an orphaned `role: "tool"`
    // message is rejected by chat templates. Walk back until the assistant
    // message that owns the tool call is included too.
    let mut cut = messages.len().saturating_sub(KEEP_RECENT);
    while cut > 1 && messages.get(cut).map(|m| m.role == "tool").unwrap_or(false) {
        cut -= 1;
    }
    let tail: Vec<Message> = messages[cut..].to_vec();
    // A single leading system message is required by many templates. The
    // compacted state goes in a user message, which also keeps the cacheable
    // system prefix byte-identical.
    let summary = Message::user(format!("Compacted state (deterministic):\n{}", state.render()));
    messages.clear();
    if let Some(system) = system { messages.push(system); }
    messages.push(summary);
    messages.extend(tail);
    state.compactions += 1;
    true
}

pub fn compact_with_metrics(
    messages: &mut Vec<Message>,
    state: &mut ContextState,
    root: &Path,
) -> Option<(usize, usize)> {
    let before = estimate_messages(messages);
    if compact(messages, state, root) {
        Some((before, estimate_messages(messages)))
    } else {
        None
    }
}

pub fn maybe_compact(
    messages: &mut Vec<Message>,
    state: &mut ContextState,
    root: &Path,
    context_tokens: usize,
    reported_prompt_tokens: Option<u32>,
) -> Option<(usize, usize)> {
    if !should_compact_with_usage(messages, context_tokens, reported_prompt_tokens) { return None; }
    compact_with_metrics(messages, state, root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn call(id: &str) -> crate::model::ToolCall {
        crate::model::ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: crate::model::FunctionCall {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "a.txt"}),
            },
        }
    }

    #[test]
    fn compaction_keeps_system_and_exact_tail() {
        let root = std::env::temp_dir().join(format!("tinyloop-ctx-{}", crate::audit::now_ms()));
        fs::create_dir_all(&root).unwrap();
        let mut messages = vec![msg("system", "rules")];
        for i in 0..40 { messages.push(msg("user", &format!("old message {i} with some text"))); }
        let tail = [msg("user", "tail-1"), msg("assistant", "tail-2"), msg("user", "tail-3"), msg("assistant", "tail-4")];
        messages.extend(tail.iter().cloned());
        let mut state = ContextState::new("goal", "done", &[]);
        assert!(compact(&mut messages, &mut state, &root));
        assert_eq!(messages[0].content.as_deref(), Some("rules"));
        assert!(messages[1].content.as_deref().unwrap().contains("Compacted state"));
        let kept: Vec<String> = messages.iter().rev().take(4).rev().filter_map(|m| m.content.clone()).collect();
        assert_eq!(kept, vec!["tail-1", "tail-2", "tail-3", "tail-4"]);
        assert!(root.join(".tinyloop").join("state.md").exists());
        assert_eq!(state.compactions, 1);
    }

    #[test]
    fn threshold_is_85_percent() {
        let small = vec![msg("user", "short")];
        assert!(!should_compact(&small, 8192));
        let huge = vec![msg("user", &"x".repeat(40_000))];
        assert!(should_compact(&huge, 8192));
    }

    #[test]
    fn tool_schema_overhead_is_part_of_the_estimate() {
        let messages = vec![msg("user", &"x".repeat(24_000))];
        let threshold = (8192f64 * COMPACT_AT) as usize;
        assert!(estimate_messages(&messages) < threshold);
        assert!(should_compact(&messages, 8192));
    }

    #[test]
    fn reported_prompt_tokens_trigger_compaction() {
        let small = vec![msg("user", "short")];
        assert!(!should_compact_with_usage(&small, 8192, Some(100)));
        assert!(should_compact_with_usage(&small, 8192, Some(7000)));
    }

    #[test]
    fn compaction_never_orphans_tool_results() {
        let root = std::env::temp_dir().join(format!("tinyloop-ctx-tool-{}", crate::audit::now_ms()));
        fs::create_dir_all(&root).unwrap();
        let mut messages = vec![
            msg("system", "rules"),
            msg("user", "start"),
            Message::assistant_with_tool_calls(None, vec![call("c1")]),
            msg("tool", "result-1"),
            msg("user", "again"),
            Message::assistant_with_tool_calls(None, vec![call("c2")]),
            msg("tool", "result-2"),
        ];
        let mut state = ContextState::new("goal", "done", &[]);
        assert!(compact(&mut messages, &mut state, &root));
        assert_eq!(messages.iter().filter(|m| m.role == "system").count(), 1);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_ne!(messages[2].role, "tool", "tail must not start with a tool result");
        let tail = &messages[2..];
        assert!(tail.windows(2).all(|pair| !(pair[0].role == "assistant" && pair[1].role == "tool")
            || pair[0].tool_calls.is_some()));
    }

    #[test]
    fn state_is_bounded() {
        let mut state = ContextState::new("g", "d", &["no network".into()]);
        for i in 0..40 { state.observe("read_file", &format!("fact {i}")); }
        assert_eq!(state.facts.len(), 16);
        assert!(state.render().contains("no network"));
    }
}
