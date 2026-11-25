//! Tool registry: seven narrow tools, risk-tiered, deterministic result shape.

pub mod fs_tools;
pub mod observation;
pub mod shell;

use crate::model::ToolDef;
use serde::Serialize;
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
}

impl ToolContext {
    pub fn new(root: impl Into<PathBuf>, artifacts: impl Into<PathBuf>, mode: RunMode) -> Self {
        Self {
            root: root.into(), artifacts: artifacts.into(), mode,
            allowlist: ["cargo", "git", "rg", "pnpm", "npm", "node", "python", "python3"]
                .iter().map(|s| s.to_string()).collect(),
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub status: String,
    pub summary: String,
    pub next_actions: Vec<String>,
    pub artifacts: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout_excerpt: String,
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

pub fn finish_with_artifact(ctx: &ToolContext, status: &str, summary: String, stdout: String, exit_code: Option<i32>) -> ToolResult {
    let budget = observation::resolve_tool_result_budget(8192);
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
