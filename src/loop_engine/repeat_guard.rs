//! Repeat guard for tinyloop.
//!
//! Ported from OpenClaw `packages/agent-core/src/tool-repeat-guard.ts`
//! (MIT, vendored under `vendor/openclaw/agent-core/tool-repeat-guard.ts`).
//! Adaptation: OpenClaw notes at 3 and stops at 20; tinyloop stops at 4
//! because its step cap is 12, so a 20-call streak can never occur.

use serde_json::Value;

pub const TOOL_REPEAT_NOTE_AT: usize = 3;
pub const TOOL_REPEAT_STOP_AT: usize = 4;
pub const TOOL_REPEAT_LIMIT_CODE: &str = "tool_repeat_limit";

#[derive(Debug, Clone, Default)]
pub struct RepeatState {
    key: Option<String>,
    count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Note,
    Stop,
}

/// Recursively canonical JSON: object keys sorted, arrays keep their order.
/// This is the Rust equivalent of OpenClaw's stableStringify.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", serde_json::to_string(k).unwrap_or_default(), canonical_json(&map[*k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

pub fn observe(state: &mut RepeatState, tool: &str, args: &Value) -> Option<Verdict> {
    // Unknown tools repeat by name only; declared tools bind canonical args.
    let key = format!("{}\0{}", tool, canonical_json(args));
    state.count = if state.key.as_deref() == Some(key.as_str()) {
        state.count + 1
    } else {
        1
    };
    state.key = Some(key);
    if state.count >= TOOL_REPEAT_STOP_AT {
        Some(Verdict::Stop)
    } else if state.count == TOOL_REPEAT_NOTE_AT {
        Some(Verdict::Note)
    } else {
        None
    }
}

pub fn reset(state: &mut RepeatState) {
    state.key = None;
    state.count = 0;
}

pub fn note_text(tool: &str, count: usize) -> String {
    format!(
        "[System note: {} called {} times in a row with identical arguments. Change arguments or use another tool; after {} identical calls tinyloop escalates.]",
        tool, count, TOOL_REPEAT_STOP_AT
    )
}

pub fn stop_text(tool: &str, count: usize) -> String {
    format!(
        "Not executed: {} was called {} times in a row with identical arguments. Stopping ({}).",
        tool, count, TOOL_REPEAT_LIMIT_CODE
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn note_on_third_stop_on_fourth() {
        let mut s = RepeatState::default();
        assert_eq!(observe(&mut s, "read_file", &json!({"path":"a"})), None);
        assert_eq!(observe(&mut s, "read_file", &json!({"path":"a"})), None);
        assert_eq!(observe(&mut s, "read_file", &json!({"path":"a"})), Some(Verdict::Note));
        assert_eq!(observe(&mut s, "read_file", &json!({"path":"a"})), Some(Verdict::Stop));
    }

    #[test]
    fn different_arguments_reset_streak() {
        let mut s = RepeatState::default();
        observe(&mut s, "read_file", &json!({"path":"a"}));
        observe(&mut s, "read_file", &json!({"path":"b"}));
        assert_eq!(s.count, 1);
    }

    #[test]
    fn key_order_does_not_matter() {
        let a = canonical_json(&json!({"b":2,"a":1}));
        let b = canonical_json(&json!({"a":1,"b":2}));
        assert_eq!(a, b);
    }
}
