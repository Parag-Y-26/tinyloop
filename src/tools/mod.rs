//! Tool registry: seven narrow tools, risk-tiered, deterministic result shape.

pub mod fs_tools;
pub mod observation;
pub mod shell;

use crate::model::ToolDef;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode { DryRun, Ask, Auto }

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub root: PathBuf,
    pub artifacts: PathBuf,
    pub mode: RunMode,
    pub allowlist: Vec<String>,
    pub timeout: Duration,
    /// Context window used to derive live result caps. `run_agent` overrides
    /// this with `RunConfig::context_tokens` before executing any tool.
    pub context_tokens: usize,
    /// Opt-in: allow `python -c` inline code. Denied by default.
    pub allow_inline_code: bool,
}

impl ToolContext {
    pub fn new(root: impl Into<PathBuf>, artifacts: impl Into<PathBuf>, mode: RunMode) -> Self {
        Self {
            root: root.into(), artifacts: artifacts.into(), mode,
            allowlist: ["cargo", "git", "rg", "pnpm", "npm", "node", "python", "python3"]
                .iter().map(|s| s.to_string()).collect(),
            timeout: Duration::from_secs(30),
            context_tokens: crate::model::DEFAULT_CONTEXT,
            allow_inline_code: false,
        }
    }

    pub fn with_context_tokens(mut self, tokens: usize) -> Self {
        self.context_tokens = tokens.max(512);
        self
    }

    pub fn with_inline_code(mut self, allow: bool) -> Self {
        self.allow_inline_code = allow;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub status: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout_excerpt: String,
    #[serde(default)]
    pub truncated: bool,
}

impl ToolResult {
    pub fn ok(summary: impl Into<String>, excerpt: impl Into<String>) -> Self {
        Self { status: "success".into(), summary: summary.into(), next_actions: vec![], artifacts: vec![], exit_code: Some(0), stdout_excerpt: excerpt.into(), truncated: false }
    }
    pub fn error(summary: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { status: "error".into(), summary: summary.into(), next_actions: vec![hint.into()], artifacts: vec![], exit_code: None, stdout_excerpt: String::new(), truncated: false }
    }
    pub fn to_json(&self) -> String { serde_json::to_string(self).unwrap_or_else(|_| "{\"status\":\"error\",\"summary\":\"result serialization failed\"}".into()) }
}

pub fn specs() -> Vec<ToolDef> {
    vec![
        ToolDef::function("submit_plan", "Submit 3-6 steps before acting.", json!({
            "type":"object","properties":{"steps":{"type":"array","minItems":3,"maxItems":6,"items":{"type":"object","properties":{"tool":{"type":"string"},"description":{"type":"string"}},"required":["tool","description"]}}},"required":["steps"],"additionalProperties":false
        })),
        ToolDef::function("read_file", "Read a UTF-8 file inside the workspace.", json!({
            "type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":400}},"required":["path"],"additionalProperties":false
        })),
        ToolDef::function("search_files", "Search text files for a literal string.", json!({
            "type":"object","properties":{"pattern":{"type":"string","minLength":1},"path":{"type":"string"},"max_results":{"type":"integer","minimum":1,"maximum":50}},"required":["pattern"],"additionalProperties":false
        })),
        ToolDef::function("run_command", "Run an allowlisted program inside the workspace.", json!({
            "type":"object","properties":{"command":{"type":"string"},"timeout_sec":{"type":"integer","minimum":1,"maximum":120}},"required":["command"],"additionalProperties":false
        })),
        ToolDef::function("apply_patch", "Replace one exact string in a file. Fails if the match is not unique.", json!({
            "type":"object","properties":{"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["path","old_string","new_string"],"additionalProperties":false
        })),
        ToolDef::function("finish", "Ask the deterministic verifier to decide whether the task is done.", json!({
            "type":"object","properties":{"summary":{"type":"string"}},"required":["summary"],"additionalProperties":false
        })),
        ToolDef::function("escalate", "Stop and hand control back to the human.", json!({
            "type":"object","properties":{"reason":{"type":"string"}},"required":["reason"],"additionalProperties":false
        })),
    ]
}

pub fn execute(ctx: &ToolContext, name: &str, args: &Value) -> ToolResult {
    match name {
        "read_file" => fs_tools::read_file(ctx, args),
        "search_files" => fs_tools::search_files(ctx, args),
        "apply_patch" => fs_tools::apply_patch(ctx, args),
        "run_command" => shell::run_command(ctx, args),
        "submit_plan" => ToolResult::ok("plan accepted", plan_text(args)),
        "finish" => ToolResult::ok("finish requested; verifier decides", args.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string()),
        "escalate" => ToolResult::ok("escalation requested", args.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string()),
        other => ToolResult::error(format!("unknown tool: {other}"), "Use one of the seven declared tools."),
    }
}

fn plan_text(args: &Value) -> String {
    args.get("steps").and_then(|v| v.as_array()).map(|steps| {
        steps.iter().enumerate().map(|(i, s)| {
            format!("{}. {} — {}", i + 1, s.get("tool").and_then(|v| v.as_str()).unwrap_or("?"), s.get("description").and_then(|v| v.as_str()).unwrap_or(""))
        }).collect::<Vec<_>>().join("\n")
    }).unwrap_or_else(|| "plan received".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workspace(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tinyloop-tools-{name}-{}", crate::audit::now_ms()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn path_escape_is_rejected() {
        let root = workspace("escape");
        let outside = root.parent().unwrap().join("tinyloop-outside.txt");
        fs::write(&outside, "secret").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = execute(&ctx, "read_file", &json!({"path": "../tinyloop-outside.txt"}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("escapes"));
    }

    #[test]
    fn destructive_command_is_rejected() {
        let root = workspace("command");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = execute(&ctx, "run_command", &json!({"command": "rm -rf /"}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("not allowlisted") || result.summary.contains("metacharacters"));
    }

    #[test]
    fn unknown_tool_is_rejected() {
        let root = workspace("unknown");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = execute(&ctx, "shell_everything", &json!({}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("unknown tool"));
    }

    #[test]
    fn patch_escape_is_rejected() {
        let root = workspace("patch-escape");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = execute(&ctx, "apply_patch", &json!({"path": "../outside.txt", "old_string": "a", "new_string": "b"}));
        assert_eq!(result.status, "error");
    }

    #[test]
    fn result_budget_tracks_the_configured_context() {
        let root = workspace("budget");
        let body = "x".repeat(20_000);
        let wide = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto).with_context_tokens(100_000);
        let result = finish_with_artifact(&wide, "success", "big".into(), body.clone(), Some(0));
        assert!(!result.truncated, "a 100k window admits 32k chars");
        let narrow = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = finish_with_artifact(&narrow, "success", "big".into(), body, Some(0));
        assert!(result.truncated, "the 8k default caps at 9828 chars");
    }

    #[test]
    fn to_json_omits_empty_optional_fields() {
        let sparse = ToolResult {
            status: "success".into(),
            summary: "done".into(),
            next_actions: vec![],
            artifacts: vec![],
            exit_code: None,
            stdout_excerpt: String::new(),
            truncated: false,
        };
        let parsed: Value = serde_json::from_str(&sparse.to_json()).unwrap();
        assert_eq!(parsed.get("status").and_then(|v| v.as_str()), Some("success"));
        assert_eq!(parsed.get("summary").and_then(|v| v.as_str()), Some("done"));
        assert_eq!(parsed.get("truncated").and_then(|v| v.as_bool()), Some(false));
        assert!(parsed.get("next_actions").is_none());
        assert!(parsed.get("artifacts").is_none());
        assert!(parsed.get("exit_code").is_none());
        assert!(parsed.get("stdout_excerpt").is_none());

        let ok_empty = ToolResult::ok("patched a.txt", "");
        let ok_parsed: Value = serde_json::from_str(&ok_empty.to_json()).unwrap();
        assert_eq!(ok_parsed.get("exit_code"), Some(&json!(0)));
        assert!(ok_parsed.get("next_actions").is_none());
        assert!(ok_parsed.get("artifacts").is_none());
        assert!(ok_parsed.get("stdout_excerpt").is_none());

        let err = ToolResult::error("failed", "retry");
        let err_parsed: Value = serde_json::from_str(&err.to_json()).unwrap();
        assert_eq!(err_parsed.get("next_actions"), Some(&json!(["retry"])));
        assert!(err_parsed.get("artifacts").is_none());
        assert!(err_parsed.get("exit_code").is_none());
        assert!(err_parsed.get("stdout_excerpt").is_none());
    }

    #[test]
    fn to_json_preserves_no_output_marker_and_non_empty_fields() {
        let result = ToolResult {
            status: "error".into(),
            summary: "cargo exited 1".into(),
            next_actions: vec!["Read the error excerpt".into()],
            artifacts: vec![".tinyloop/tool-1.txt".into()],
            exit_code: Some(1),
            stdout_excerpt: "command produced no output".into(),
            truncated: true,
        };
        let parsed: Value = serde_json::from_str(&result.to_json()).unwrap();
        assert_eq!(parsed.get("status").and_then(|v| v.as_str()), Some("error"));
        assert_eq!(parsed.get("summary").and_then(|v| v.as_str()), Some("cargo exited 1"));
        assert_eq!(parsed.get("next_actions"), Some(&json!(["Read the error excerpt"])));
        assert_eq!(parsed.get("artifacts"), Some(&json!([".tinyloop/tool-1.txt"])));
        assert_eq!(parsed.get("exit_code"), Some(&json!(1)));
        assert_eq!(
            parsed.get("stdout_excerpt").and_then(|v| v.as_str()),
            Some("command produced no output")
        );
        assert_eq!(parsed.get("truncated"), Some(&json!(true)));
    }

    #[test]
    fn tool_result_round_trips_via_serde_json() {
        let sparse = ToolResult {
            status: "success".into(),
            summary: "ok".into(),
            next_actions: vec![],
            artifacts: vec![],
            exit_code: None,
            stdout_excerpt: String::new(),
            truncated: false,
        };
        let sparse_back: ToolResult = serde_json::from_str(&sparse.to_json()).unwrap();
        assert_eq!(sparse_back, sparse);

        let full = ToolResult {
            status: "error".into(),
            summary: "cargo exited 2".into(),
            next_actions: vec!["Fix syntax".into()],
            artifacts: vec![".tinyloop/tool-2.txt".into()],
            exit_code: Some(2),
            stdout_excerpt: "command produced no output".into(),
            truncated: true,
        };
        let full_back: ToolResult = serde_json::from_str(&full.to_json()).unwrap();
        assert_eq!(full_back, full);

        let minimal: ToolResult =
            serde_json::from_str(r#"{"status":"error","summary":"result serialization failed"}"#)
                .unwrap();
        assert_eq!(
            minimal,
            ToolResult {
                status: "error".into(),
                summary: "result serialization failed".into(),
                next_actions: vec![],
                artifacts: vec![],
                exit_code: None,
                stdout_excerpt: String::new(),
                truncated: false,
            }
        );
    }
}

#[allow(clippy::items_after_test_module)]
pub fn finish_with_artifact(ctx: &ToolContext, status: &str, summary: String, stdout: String, exit_code: Option<i32>) -> ToolResult {
    let budget = observation::resolve_tool_result_budget(ctx.context_tokens);
    let _ = fs::create_dir_all(&ctx.artifacts);
    let artifact = ctx.artifacts.join(format!("tool-{}.txt", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)));
    let mut artifacts = vec![];
    let mut excerpt = stdout.clone();
    let mut truncated = false;
    if stdout.chars().count() > budget.max_chars {
        let _ = fs::write(&artifact, &stdout);
        let obs = observation::truncate_observation(&stdout, budget, &artifact.display().to_string());
        excerpt = obs.text; truncated = true; artifacts.push(artifact.display().to_string());
    }
    ToolResult { status: status.into(), summary, next_actions: vec![], artifacts, exit_code, stdout_excerpt: excerpt, truncated }
}
