use super::{finish_with_artifact, RunMode, ToolContext, ToolResult};
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

pub fn run_command(ctx: &ToolContext, args: &Value) -> ToolResult {
    if ctx.mode == RunMode::DryRun { return ToolResult::error("dry-run: command not executed", "Re-run in ask/auto mode to execute."); }
    let raw = match args.get("command").and_then(|v| v.as_str()) { Some(c) if !c.trim().is_empty() => c.trim(), _ => return ToolResult::error("missing command", "Pass an allowlisted command.") };
    if raw.chars().any(|c| matches!(c, '|' | '&' | ';' | '<' | '>' | '$' | '`' | '\n' | '\r')) {
        return ToolResult::error("shell metacharacters are not allowed", "Run one allowlisted program with plain arguments.");
    }
    let argv: Vec<&str> = raw.split_whitespace().collect();
    let program = argv[0];
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program).trim_end_matches(".exe").to_lowercase();
    if !ctx.allowlist.iter().any(|a| a == &base) {
        return ToolResult::error(format!("program not allowlisted: {base}"), format!("Allowed: {}", ctx.allowlist.join(", ")));
    }
    let timeout = Duration::from_secs(args.get("timeout_sec").and_then(|v| v.as_u64()).unwrap_or(ctx.timeout.as_secs()).min(120));
    let mut cmd = Command::new(program);
    cmd.args(&argv[1..]).current_dir(&ctx.root).stdout(Stdio::piped()).stderr(Stdio::piped()).env_clear();
    for key in ["PATH", "SystemRoot", "TEMP", "TMP", "USERPROFILE", "HOME", "COMSPEC", "PATHEXT"] {
        if let Ok(v) = std::env::var(key) { cmd.env(key, v); }
    }
    let mut child = match cmd.spawn() { Ok(c) => c, Err(e) => return ToolResult::error(format!("cannot spawn {base}: {e}"), "Check the program is installed.") };
    let status = match child.wait_timeout(timeout) {
        Ok(Some(s)) => s,
        Ok(None) => { let _ = child.kill(); let _ = child.wait(); return ToolResult::error(format!("{base} timed out after {}s", timeout.as_secs()), "Use a shorter or narrower command."); }
        Err(e) => return ToolResult::error(format!("wait failed: {e}"), "Retry once; if it repeats, escalate."),
    };
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() { let _ = s.read_to_string(&mut out); }
    if let Some(mut s) = child.stderr.take() { let _ = s.read_to_string(&mut out); }
    let code = status.code().unwrap_or(-1);
    let summary = format!("{base} exited {code}");
    let mut result = finish_with_artifact(ctx, if status.success() { "success" } else { "error" }, summary, out, Some(code));
    if !status.success() { result.next_actions = vec!["Read the error excerpt, adjust, and retry (max 3 attempts).".into()]; }
    result
}
