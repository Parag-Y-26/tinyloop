//! Deterministic context budget and compaction.
//!
//! At 85% of the window we write `state.md` first, then replace the middle of
//! the transcript with a compacted state message while keeping the exact tail.

use crate::model::Message;
use std::fs;
use std::path::Path;

pub const DEFAULT_CONTEXT_TOKENS: usize = 8192;
pub const COMPACT_AT: f64 = 0.85;
pub const MASK_AT: f64 = 0.75;
pub const KEEP_RECENT: usize = 4;
pub const KEEP_RECENT_TURNS: usize = 2;
/// The frozen system prompt plus the seven tool schemas cost roughly this
/// much of the window even though they are not part of `messages`.
pub const TOOL_SCHEMA_TOKENS: usize = 1200;
pub const MASKED_EXCERPT_MARKER: &str = "[omitted — see summary and artifact path]";

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

/// Return the effective prompt token count for budget decisions.
///
/// Uses `reported_prompt_tokens + new_turn_tokens` whenever the model server
/// already reported a non-zero `usage.prompt_tokens`. Falls back to the
/// character estimate plus `TOOL_SCHEMA_TOKENS` only when `reported_prompt_tokens`
/// is `None` or `Some(0)`.
pub fn effective_prompt_tokens(
    messages: &[Message],
    reported_prompt_tokens: Option<u32>,
    new_turn_tokens: usize,
) -> usize {
    match reported_prompt_tokens {
        Some(pt) if pt > 0 => (pt as usize).saturating_add(new_turn_tokens),
        _ => estimate_messages(messages).saturating_add(TOOL_SCHEMA_TOKENS),
    }
}

pub fn should_compact(messages: &[Message], context_tokens: usize) -> bool {
    should_compact_with_usage(messages, context_tokens, None)
}

/// Prefer the exact `usage.prompt_tokens` the server already reported; it
/// includes the tool schemas. Fall back to the char estimate plus the schema
/// overhead only when no non-zero usage has been reported yet.
pub fn should_compact_with_usage(
    messages: &[Message],
    context_tokens: usize,
    reported_prompt_tokens: Option<u32>,
) -> bool {
    let threshold = (context_tokens as f64 * COMPACT_AT) as usize;
    effective_prompt_tokens(messages, reported_prompt_tokens, 0) > threshold
}

pub fn write_state(root: &Path, state: &ContextState) {
    let dir = root.join(".tinyloop");
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join("state.md"), state.render());
}

fn is_assistant_tool_call(message: &Message) -> bool {
    message.role == "assistant"
        && message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
}

/// Return `Some(start)` for the atomic turn unit ending at `end` (`end > min_start`),
/// pairing consecutive `role == "tool"` results with their preceding assistant
/// tool-call message. Returns `None` if `messages[end - 1]` is an unmatched tool
/// message.
fn atomic_turn_start(messages: &[Message], end: usize, min_start: usize) -> Option<usize> {
    if end <= min_start {
        return None;
    }
    let last = end - 1;
    if messages[last].role != "tool" {
        return Some(last);
    }
    let mut first_tool = last;
    while first_tool > min_start && messages[first_tool - 1].role == "tool" {
        first_tool -= 1;
    }
    if first_tool > min_start && is_assistant_tool_call(&messages[first_tool - 1]) {
        Some(first_tool - 1)
    } else {
        None
    }
}

fn compaction_cut(messages: &[Message], keep_recent: usize) -> Option<usize> {
    if messages.len() <= keep_recent + 1 {
        return None;
    }
    let mut cut = messages.len();
    while cut > 1 && messages.len() - cut < keep_recent {
        match atomic_turn_start(messages, cut, 1) {
            Some(start) => cut = start,
            None => break,
        }
    }
    if cut <= 1 || cut >= messages.len() {
        return None;
    }
    Some(cut)
}

fn observation_keep_start(messages: &[Message], keep_turns: usize) -> usize {
    let mut keep_from = messages.len();
    for _ in 0..keep_turns {
        if keep_from <= 1 {
            break;
        }
        match atomic_turn_start(messages, keep_from, 1) {
            Some(start) => keep_from = start,
            None => {
                let mut start = keep_from - 1;
                while start > 1 && messages[start - 1].role == "tool" {
                    start -= 1;
                }
                keep_from = start;
            }
        }
    }
    keep_from
}

fn mask_tool_message(message: &mut Message) -> bool {
    if message.role != "tool" {
        return false;
    }
    let Some(content) = message.content.as_deref() else {
        return false;
    };
    if content.is_empty() || content == MASKED_EXCERPT_MARKER {
        return false;
    }
    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(content) {
        let Some(obj) = value.as_object_mut() else {
            return false;
        };
        let needs_mask = obj
            .get("stdout_excerpt")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty() && s != MASKED_EXCERPT_MARKER);
        if !needs_mask {
            return false;
        }
        obj.insert(
            "stdout_excerpt".to_owned(),
            serde_json::Value::String(MASKED_EXCERPT_MARKER.to_owned()),
        );
        if let Ok(serialized) = serde_json::to_string(&value) {
            message.content = Some(serialized);
            return true;
        }
        return false;
    }
    message.content = Some(MASKED_EXCERPT_MARKER.to_owned());
    true
}

/// Replace `stdout_excerpt` in old `role == "tool"` messages (strictly older
/// than the last `KEEP_RECENT_TURNS` atomic turns, starting at index 1) with
/// `MASKED_EXCERPT_MARKER`, preserving structured metadata intact.
pub fn mask_old_observations(messages: &mut [Message]) -> usize {
    if messages.len() <= 1 {
        return 0;
    }
    let keep_from = observation_keep_start(messages, KEEP_RECENT_TURNS);
    let mut masked = 0usize;
    for msg in &mut messages[1..keep_from] {
        if mask_tool_message(msg) {
            masked += 1;
        }
    }
    masked
}

pub fn compact(messages: &mut Vec<Message>, state: &mut ContextState, root: &Path) -> bool {
    let Some(cut) = compaction_cut(messages, KEEP_RECENT) else {
        return false;
    };

    // Increment before rendering/writing so state.md records the new count.
    state.compactions += 1;
    write_state(root, state);

    let first = messages[0].clone();
    let tail = messages[cut..].to_vec();
    // A single leading system message is required by many templates. The
    // compacted state goes in a user message, which also keeps the cacheable
    // system prefix byte-identical.
    let summary = Message::user(format!(
        "[Compacted state — prior turns persisted to .tinyloop/state.md]\n{}",
        state.render()
    ));
    messages.clear();
    messages.push(first);
    messages.push(summary);
    messages.extend(tail);
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

/// Metrics produced by two-tier context management (`mask_old_observations`
/// followed by `compact_with_metrics` when needed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextOutcome {
    pub masked_turns: usize,
    pub mask_metrics: Option<(usize, usize)>,
    pub compact_metrics: Option<(usize, usize)>,
}

/// Two-tier budget manager:
/// - Above 85% (`COMPACT_AT`): run Tier 2 full compaction directly.
/// - Above 75% (`MASK_AT`): run Tier 1 block observation masking once; if no
///   observations were masked or tokens remain `>= 75%`, escalate to Tier 2.
pub fn manage_context(
    messages: &mut Vec<Message>,
    state: &mut ContextState,
    root: &Path,
    context_tokens: usize,
    reported_prompt_tokens: Option<u32>,
    new_turn_tokens: usize,
) -> ContextOutcome {
    let current = effective_prompt_tokens(messages, reported_prompt_tokens, new_turn_tokens);
    let mask_threshold = (context_tokens as f64 * MASK_AT) as usize;
    let compact_threshold = (context_tokens as f64 * COMPACT_AT) as usize;
    let mut outcome = ContextOutcome::default();

    if current > compact_threshold {
        outcome.compact_metrics = compact_with_metrics(messages, state, root);
        return outcome;
    }

    if current > mask_threshold {
        let before = estimate_messages(messages);
        let masked_turns = mask_old_observations(messages);
        outcome.masked_turns = masked_turns;
        if masked_turns > 0 {
            let after = estimate_messages(messages);
            outcome.mask_metrics = Some((before, after));
            let saved = before.saturating_sub(after);
            let after_effective = current.saturating_sub(saved);
            if after_effective >= mask_threshold {
                outcome.compact_metrics = compact_with_metrics(messages, state, root);
            }
        } else {
            outcome.compact_metrics = compact_with_metrics(messages, state, root);
        }
    }

    outcome
}

/// Compact when `effective_prompt_tokens(..., new_turn_tokens)` exceeds 85%.
pub fn maybe_compact_with_delta(
    messages: &mut Vec<Message>,
    state: &mut ContextState,
    root: &Path,
    context_tokens: usize,
    reported_prompt_tokens: Option<u32>,
    new_turn_tokens: usize,
) -> Option<(usize, usize)> {
    let threshold = (context_tokens as f64 * COMPACT_AT) as usize;
    if effective_prompt_tokens(messages, reported_prompt_tokens, new_turn_tokens) <= threshold {
        return None;
    }
    compact_with_metrics(messages, state, root)
}

pub fn maybe_compact(
    messages: &mut Vec<Message>,
    state: &mut ContextState,
    root: &Path,
    context_tokens: usize,
    reported_prompt_tokens: Option<u32>,
) -> Option<(usize, usize)> {
    maybe_compact_with_delta(messages, state, root, context_tokens, reported_prompt_tokens, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "tinyloop-ctx-{label}-{}-{}-{seq}",
            std::process::id(),
            crate::audit::now_ms()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

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

    fn tool_json(status: &str, summary: &str, artifact: &str, excerpt: &str) -> String {
        serde_json::json!({
            "status": status,
            "summary": summary,
            "next_actions": ["inspect artifact"],
            "artifacts": [artifact],
            "exit_code": 0,
            "stdout_excerpt": excerpt,
            "truncated": true
        })
        .to_string()
    }

    #[test]
    fn compaction_keeps_system_and_exact_tail() {
        let root = temp_root("exact-tail");
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
        let root = temp_root("tool-pair");
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
    fn compaction_pairs_consecutive_tool_results_and_handles_unmatched_or_single_turn() {
        let root = temp_root("multi-tool-and-orphan");

        // Consecutive tool results paired with one assistant(tool_calls) message.
        let mut multi = vec![
            msg("system", "rules"),
            msg("user", "old"),
            Message::assistant_with_tool_calls(None, vec![call("c1"), call("c2")]),
            Message::tool_result("c1", "r1"),
            Message::tool_result("c2", "r2"),
            msg("user", "follow-up"),
            msg("user", "last"),
        ];
        let mut state = ContextState::new("goal", "done", &[]);
        assert!(compact(&mut multi, &mut state, &root));
        assert_eq!(multi[2].role, "assistant");
        assert_eq!(multi[3].role, "tool");
        assert_eq!(multi[4].role, "tool");
        assert_eq!(multi[5].content.as_deref(), Some("follow-up"));

        // Unmatched tool message right where naive KEEP_RECENT would cut.
        let mut with_orphan = vec![
            msg("system", "rules"),
            msg("user", "old"),
            msg("tool", "orphan-result"),
            msg("user", "u1"),
            msg("user", "u2"),
            msg("user", "u3"),
        ];
        assert!(compact(&mut with_orphan, &mut state, &root));
        assert_ne!(with_orphan[2].role, "tool");
        assert!(with_orphan[2..].iter().all(|m| m.role != "tool"));

        // Single atomic turn spanning all messages after system -> cut <= 1 -> no-op.
        let mut single_turn = vec![
            msg("system", "rules"),
            Message::assistant_with_tool_calls(None, vec![call("c1"), call("c2"), call("c3"), call("c4")]),
            Message::tool_result("c1", "r1"),
            Message::tool_result("c2", "r2"),
            Message::tool_result("c3", "r3"),
            Message::tool_result("c4", "r4"),
        ];
        let before_len = single_turn.len();
        let before_compactions = state.compactions;
        assert!(!compact(&mut single_turn, &mut state, &root));
        assert_eq!(single_turn.len(), before_len);
        assert_eq!(state.compactions, before_compactions);
    }

    #[test]
    fn compaction_keeps_single_system_inserts_user_recap_and_writes_incremented_count() {
        let root = temp_root("state-count");
        let mut messages = vec![msg("system", "frozen system prompt")];
        for i in 0..8 {
            messages.push(msg("user", &format!("turn {i}")));
        }
        let mut state = ContextState::new("fix tax", "tests pass", &["no push".into()]);
        state.observe("read_file", "tax.rs uses 8%");

        assert!(compact(&mut messages, &mut state, &root));
        assert_eq!(messages.iter().filter(|m| m.role == "system").count(), 1);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[0].content.as_deref(), Some("frozen system prompt"));
        assert_eq!(messages[1].role, "user");
        let recap = messages[1].content.as_deref().unwrap();
        assert!(recap.starts_with("[Compacted state — prior turns persisted to .tinyloop/state.md]\n"));
        assert!(recap.contains("- Compactions: 1"));
        assert_eq!(state.compactions, 1);

        let on_disk = fs::read_to_string(root.join(".tinyloop").join("state.md")).unwrap();
        assert!(on_disk.contains("- Compactions: 1"));

        // Second compaction increments to 2 on disk and in the recap message.
        messages.extend((0..6).map(|i| msg("user", &format!("later {i}"))));
        assert!(compact(&mut messages, &mut state, &root));
        assert_eq!(messages.iter().filter(|m| m.role == "system").count(), 1);
        assert_eq!(state.compactions, 2);
        let on_disk_2 = fs::read_to_string(root.join(".tinyloop").join("state.md")).unwrap();
        assert!(on_disk_2.contains("- Compactions: 2"));
        assert!(messages[1].content.as_deref().unwrap().contains("- Compactions: 2"));
    }

    #[test]
    fn block_masking_triggers_only_above_75_percent_and_preserves_metadata_and_last_two_turns() {
        let root = temp_root("masking");
        let large_excerpt = "L".repeat(4000);
        let t1_json = tool_json("success", "read big file 1", ".tinyloop/artifacts/1.txt", &large_excerpt);
        let t3_json = tool_json("success", "read file 3", ".tinyloop/artifacts/3.txt", "recent-3");
        let t4_json = tool_json("warning", "read file 4", ".tinyloop/artifacts/4.txt", "recent-4");

        let base_messages = vec![
            msg("system", "rules"),
            msg("user", "start"),
            Message::assistant_with_tool_calls(None, vec![call("c1")]),
            Message::tool_result("c1", t1_json.clone()),
            Message::assistant_with_tool_calls(None, vec![call("c2")]),
            Message::tool_result("c2", "plain non-json output"),
            Message::assistant_with_tool_calls(None, vec![call("c3")]),
            Message::tool_result("c3", t3_json.clone()),
            Message::assistant_with_tool_calls(None, vec![call("c4")]),
            Message::tool_result("c4", t4_json.clone()),
        ];

        // Under or at 75% (6144 tokens for 8192): no masking occurs.
        let mut under = base_messages.clone();
        let mut state = ContextState::new("goal", "done", &[]);
        let outcome_under = manage_context(&mut under, &mut state, &root, 8192, Some(6000), 144);
        assert_eq!(outcome_under, ContextOutcome::default());
        assert_eq!(under[3].content.as_deref(), Some(t1_json.as_str()));
        assert_eq!(under[5].content.as_deref(), Some("plain non-json output"));

        // Over 75% (6500 > 6144) and <= 85% (6963): Tier 1 masking fires and shrinks tokens below 75%.
        let mut over = base_messages;
        let before_tokens = estimate_messages(&over);
        let outcome = manage_context(&mut over, &mut state, &root, 8192, Some(6400), 100);
        assert_eq!(outcome.masked_turns, 2);
        let (b, a) = outcome.mask_metrics.expect("mask metrics expected");
        assert_eq!(b, before_tokens);
        assert!(a < b, "masking must shrink token count: {b} -> {a}");
        assert_eq!(outcome.compact_metrics, None, "masking saved enough so Tier 2 must not run");
        assert_eq!(state.compactions, 0);

        // Old JSON tool message keeps metadata intact while masking stdout_excerpt.
        let parsed: serde_json::Value = serde_json::from_str(over[3].content.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["summary"], "read big file 1");
        assert_eq!(parsed["artifacts"], serde_json::json!([".tinyloop/artifacts/1.txt"]));
        assert_eq!(parsed["next_actions"], serde_json::json!(["inspect artifact"]));
        assert_eq!(parsed["exit_code"], 0);
        assert_eq!(parsed["truncated"], true);
        assert_eq!(parsed["stdout_excerpt"], MASKED_EXCERPT_MARKER);

        // Old non-JSON tool message is replaced with marker.
        assert_eq!(over[5].content.as_deref(), Some(MASKED_EXCERPT_MARKER));

        // Last 2 turns (c3+t3, c4+t4) remain verbatim.
        assert_eq!(over[7].content.as_deref(), Some(t3_json.as_str()));
        assert_eq!(over[9].content.as_deref(), Some(t4_json.as_str()));

        // Masking is idempotent.
        assert_eq!(mask_old_observations(&mut over), 0);

        // Old JSON tool message with empty or omitted stdout_excerpt is untouched and not counted.
        let mut sparse_check = vec![
            msg("system", "rules"),
            msg("user", "start"),
            Message::assistant_with_tool_calls(None, vec![call("c0")]),
            Message::tool_result("c0", serde_json::json!({"status": "success", "summary": "ok"}).to_string()),
            Message::assistant_with_tool_calls(None, vec![call("c0b")]),
            Message::tool_result("c0b", tool_json("success", "empty", "a0", "")),
            Message::assistant_with_tool_calls(None, vec![call("c1")]),
            Message::tool_result("c1", t3_json.clone()),
            Message::assistant_with_tool_calls(None, vec![call("c2a"), call("c2b")]),
            Message::tool_result("c2a", t4_json.clone()),
            Message::tool_result("c2b", t4_json.clone()),
        ];
        assert_eq!(mask_old_observations(&mut sparse_check), 0);
        // Both tool results in the multi-tool last turn remain verbatim.
        assert_eq!(sparse_check[9].content.as_deref(), Some(t4_json.as_str()));
        assert_eq!(sparse_check[10].content.as_deref(), Some(t4_json.as_str()));
    }

    #[test]
    fn tier2_compaction_runs_above_85_percent_or_when_masking_fails_to_drop_below_75_percent() {
        let root = temp_root("tier2-escalation");
        let tiny_excerpt = "short excerpt";
        let make_messages = || {
            vec![
                msg("system", "rules"),
                msg("user", "start"),
                Message::assistant_with_tool_calls(None, vec![call("c1")]),
                Message::tool_result("c1", tool_json("success", "s1", "a1", tiny_excerpt)),
                Message::assistant_with_tool_calls(None, vec![call("c2")]),
                Message::tool_result("c2", tool_json("success", "s2", "a2", "keep-2")),
                Message::assistant_with_tool_calls(None, vec![call("c3")]),
                Message::tool_result("c3", tool_json("success", "s3", "a3", "keep-3")),
            ]
        };

        // Case A: >75% (6500 > 6144), masking saves almost nothing so after_effective >= 6144 -> Tier 2 runs.
        let mut msgs_insufficient = make_messages();
        let mut state_a = ContextState::new("goal", "done", &[]);
        let outcome_a = manage_context(&mut msgs_insufficient, &mut state_a, &root, 8192, Some(6400), 100);
        assert_eq!(outcome_a.masked_turns, 1);
        assert!(outcome_a.mask_metrics.is_some());
        assert!(outcome_a.compact_metrics.is_some(), "Tier 2 must run when masking keeps tokens >= 75%");
        assert_eq!(state_a.compactions, 1);

        // Case B: >85% (7000 > 6963) -> Tier 2 runs directly without masking first.
        let mut msgs_over_85 = make_messages();
        let mut state_b = ContextState::new("goal", "done", &[]);
        let outcome_b = manage_context(&mut msgs_over_85, &mut state_b, &root, 8192, Some(6800), 200);
        assert_eq!(outcome_b.masked_turns, 0);
        assert_eq!(outcome_b.mask_metrics, None);
        assert!(outcome_b.compact_metrics.is_some(), "Tier 2 must run directly above 85%");
        assert_eq!(state_b.compactions, 1);

        // Case C: >75% and <=85% with no maskable old tool turns (masked_turns == 0) -> Tier 2 runs.
        let mut msgs_no_old_tools = vec![
            msg("system", "rules"),
            msg("user", "old-1"),
            msg("user", "old-2"),
            Message::assistant_with_tool_calls(None, vec![call("c2")]),
            Message::tool_result("c2", tool_json("success", "s2", "a2", "keep-2")),
            Message::assistant_with_tool_calls(None, vec![call("c3")]),
            Message::tool_result("c3", tool_json("success", "s3", "a3", "keep-3")),
        ];
        let mut state_c = ContextState::new("goal", "done", &[]);
        let outcome_c = manage_context(&mut msgs_no_old_tools, &mut state_c, &root, 8192, Some(6400), 100);
        assert_eq!(outcome_c.masked_turns, 0);
        assert_eq!(outcome_c.mask_metrics, None);
        assert!(outcome_c.compact_metrics.is_some(), "Tier 2 must run when masked_turns == 0");
        assert_eq!(state_c.compactions, 1);
    }

    #[test]
    fn effective_prompt_tokens_uses_real_usage_plus_delta_and_falls_back_only_at_zero() {
        let root = temp_root("real-budget");
        // Huge chars/4 estimate (> 10,000 tokens), which must NOT trigger when prompt_tokens > 0.
        let mut huge_msgs = vec![msg("system", "rules")];
        for i in 0..8 {
            huge_msgs.push(msg("user", &format!("turn {i} {}", "x".repeat(5000))));
        }
        assert!(estimate_messages(&huge_msgs) > 8192);

        // Real usage > 0 uses pt + new_turn_tokens and ignores chars/4 estimate.
        assert_eq!(effective_prompt_tokens(&huge_msgs, Some(500), 250), 750);
        assert!(!should_compact_with_usage(&huge_msgs, 8192, Some(500)));
        assert_eq!(maybe_compact(&mut huge_msgs, &mut ContextState::new("g", "d", &[]), &root, 8192, Some(500)), None);

        let mut state = ContextState::new("goal", "done", &[]);
        assert_eq!(
            maybe_compact_with_delta(&mut huge_msgs, &mut state, &root, 8192, Some(6800), 163),
            None
        );
        assert_eq!(state.compactions, 0);

        // Real prompt_tokens + new_turn_tokens crossing 85% triggers compaction.
        assert!(
            maybe_compact_with_delta(&mut huge_msgs, &mut state, &root, 8192, Some(6800), 200).is_some()
        );
        assert_eq!(state.compactions, 1);

        // Fallback to estimate_messages + TOOL_SCHEMA_TOKENS only when None or Some(0).
        let expected_fallback = estimate_messages(&huge_msgs) + TOOL_SCHEMA_TOKENS;
        assert_eq!(effective_prompt_tokens(&huge_msgs, None, 999), expected_fallback);
        assert_eq!(effective_prompt_tokens(&huge_msgs, Some(0), 999), expected_fallback);
    }

    #[test]
    fn state_is_bounded() {
        let mut state = ContextState::new("g", "d", &["no network".into()]);
        for i in 0..40 { state.observe("read_file", &format!("fact {i}")); }
        assert_eq!(state.facts.len(), 16);
        assert!(state.render().contains("no network"));
    }
}
