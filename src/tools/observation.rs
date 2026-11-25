//! Bounded tool observations for tinyloop.
//!
//! Derived from OpenClaw `src/agents/tool-result-limits.ts`,
//! `packages/agent-core/src/harness/utils/truncate.ts`, and
//! `src/utils/utf8-truncate.ts` (MIT, vendored under `vendor/openclaw/`).
//!
//! Adaptation: OpenClaw's helpers truncate from the head; tinyloop keeps a
//! head AND a tail because command errors usually appear at the end. The
//! UTF-8 boundary rule is kept verbatim in spirit: never cut inside a code
//! point. Original license: see `vendor/openclaw/LICENSE`.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ToolResultBudget {
    /// Hard cap on characters admitted from one tool result.
    pub max_chars: usize,
    /// Hard cap on total weighted observation characters in the context.
    pub max_context_chars: usize,
}

/// Context-derived caps, ported from OpenClaw's `resolveToolResultBudget`.
/// At 8192 tokens this yields max_chars=9828 and max_context_chars=8192.
pub fn resolve_tool_result_budget(context_window_tokens: usize) -> ToolResultBudget {
    let hard_cap = if context_window_tokens >= 200_000 {
        64_000
    } else if context_window_tokens >= 100_000 {
        32_000
    } else {
        16_000
    };
    let max_tokens = (context_window_tokens as f64 * 0.3).floor() as usize;
    let max_chars = max_tokens.saturating_mul(4).min(hard_cap.max(1));
    // floor(tokens * 2 * 0.5) == tokens; keep the expression for provenance.
    let max_context_chars = (context_window_tokens.max(1) * 2 * 50) / 100;
    ToolResultBudget {
        max_chars,
        max_context_chars: max_context_chars.max(1_024),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub text: String,
    pub truncated: bool,
    pub omitted_chars: usize,
    pub max_chars: usize,
}

fn byte_at_char(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len())
}

/// Keep at most `max_chars` characters, split evenly between head and tail.
/// The marker names the artifact so the model can re-read an exact slice
/// instead of re-running the command.
pub fn truncate_observation(text: &str, budget: ToolResultBudget, artifact: &str) -> Observation {
    let count = text.chars().count();
    if count <= budget.max_chars {
        return Observation {
            text: text.to_owned(),
            truncated: false,
            omitted_chars: 0,
            max_chars: budget.max_chars,
        };
    }
    let half = budget.max_chars / 2;
    let head = &text[..byte_at_char(text, half)];
    let tail_start = byte_at_char(text, count.saturating_sub(half));
    let tail = &text[tail_start..];
    let head_chars = head.chars().count();
    let tail_chars = tail.chars().count();
    let omitted = count.saturating_sub(head_chars + tail_chars);
    let marker = format!(
        "\n[... {} chars omitted; full output at {} ...]\n",
        omitted, artifact
    );
    Observation {
        text: format!("{}{}{}", head, marker, tail),
        truncated: true,
        omitted_chars: omitted,
        max_chars: budget.max_chars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_at_8k_matches_openclaw_formula() {
        let b = resolve_tool_result_budget(8192);
        assert_eq!(b.max_chars, 9828);
        assert_eq!(b.max_context_chars, 8192);
    }

    #[test]
    fn large_context_uses_higher_hard_cap() {
        assert_eq!(resolve_tool_result_budget(100_000).max_chars, 32_000);
        assert_eq!(resolve_tool_result_budget(200_000).max_chars, 64_000);
    }

    #[test]
    fn head_and_tail_are_both_kept() {
        let b = resolve_tool_result_budget(100);
        let text = format!("HEAD{}TAIL", "x".repeat(500));
        let out = truncate_observation(&text, b, "artifact.txt");
        assert!(out.truncated);
        assert!(out.text.starts_with("HEAD"));
        assert!(out.text.ends_with("TAIL"));
        assert!(out.text.contains("artifact.txt"));
        assert!(out.omitted_chars > 0);
    }

    #[test]
    fn never_cuts_a_unicode_code_point() {
        let b = resolve_tool_result_budget(64);
        let text = "é".repeat(500);
        let out = truncate_observation(&text, b, "a.txt");
        assert!(out.text.is_char_boundary(out.text.len()));
        assert!(out.truncated);
    }
}
