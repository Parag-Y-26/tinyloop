use super::{finish_with_artifact, RunMode, ToolContext, ToolResult};
use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use wait_timeout::ChildExt;

const MAX_TIMEOUT_SECS: u64 = 120;
/// Hard per-stream capture cap. The reader keeps draining past the cap so a
/// runaway child never blocks on a full pipe, but only this much is kept in
/// memory.
const MAX_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
const NO_OUTPUT: &str = "command produced no output";

const GIT_BLOCKED_SUBCOMMANDS: &[&str] = &[
    "push", "reset", "clean", "rebase", "filter-branch", "restore", "commit", "config", "remote",
];
const GIT_BLOCKED_FLAGS: &[&str] = &["-C", "--git-dir", "--work-tree", "-c"];
const NODE_INLINE_FLAGS: &[&str] = &["-e", "--eval", "-p", "--print", "-"];

/// Read a piped stream on its own thread so the child never blocks on a full
/// OS pipe buffer while the parent waits for exit (4 KiB on Windows, 64 KiB
/// on Linux). Capture is bounded, but draining continues past the cap.
fn spawn_reader<R: Read + Send + 'static>(mut pipe: R) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let room = MAX_CAPTURE_BYTES.saturating_sub(kept.len());
                    if room > 0 {
                        kept.extend_from_slice(&buf[..n.min(room)]);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    })
}

fn join_reader(handle: Option<JoinHandle<String>>) -> String {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
}

/// Wait for a child while draining both pipes, killing it on timeout.
/// Returns `None` status when the timeout fired; partial output is kept either
/// way.
pub(crate) fn wait_with_output_timeout(
    mut child: Child,
    timeout: Duration,
) -> Result<(Option<ExitStatus>, String), std::io::Error> {
    let out = child.stdout.take().map(spawn_reader);
    let err = child.stderr.take().map(spawn_reader);
    let status = child.wait_timeout(timeout)?;
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let mut text = join_reader(out);
    text.push_str(&join_reader(err));
    Ok((status, text))
}

/// Empty output gets an explicit marker so the model does not re-run the same
/// silent command (ARCHITECTURE.md §4.3).
fn output_or_marker(text: String) -> String {
    if text.trim().is_empty() { NO_OUTPUT.to_string() } else { text }
}

/// Split a command line into argv, honoring single and double quotes so quoted
/// arguments and paths with spaces survive. Backslashes are literal except
/// before `"` or `\` inside double quotes, which keeps Windows paths intact.
/// Unmatched quotes are rejected.
pub fn split_command(raw: &str) -> Result<Vec<String>, String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                    has_token = true;
                } else if ch == '\\' && q == '"' {
                    match chars.peek() {
                        Some('"') | Some('\\') => {
                            current.push(chars.next().expect("peeked character"));
                        }
                        _ => current.push('\\'),
                    }
                    has_token = true;
                } else {
                    current.push(ch);
                    has_token = true;
                }
            }
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    has_token = true;
                }
                c if c.is_whitespace() => {
                    if has_token {
                        argv.push(std::mem::take(&mut current));
                        has_token = false;
                    }
                }
                c => {
                    current.push(c);
                    has_token = true;
                }
            },
        }
    }
    if let Some(q) = quote {
        return Err(format!("unmatched {q} quote in command"));
    }
    if has_token {
        argv.push(current);
    }
    Ok(argv)
}

fn blocked_reason(program: &str, args: &[String], allow_inline_code: bool) -> Option<String> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match program {
        "git" => {
            if GIT_BLOCKED_SUBCOMMANDS.contains(&sub) {
                return Some(format!(
                    "git {sub} is blocked: no auto-commit, push, reset, clean, rebase, restore, config, or remote changes"
                ));
            }
            if let Some(flag) = args.iter().find(|a| GIT_BLOCKED_FLAGS.contains(&a.as_str())) {
                return Some(format!(
                    "git flag {flag} is blocked: it can point git outside the workspace"
                ));
            }
            if sub == "checkout" && args.iter().any(|a| a == "--") {
                return Some("git checkout -- is blocked: it discards local changes".to_string());
            }
            None
        }
        "python" | "python3" => {
            if !allow_inline_code && args.iter().any(|a| a == "-c") {
                return Some(
                    "inline Python (-c) is blocked by default; enable it explicitly in the tool context"
                        .to_string(),
                );
            }
            if args.windows(2).any(|w| w[0] == "-m" && w[1] == "http.server") {
                return Some("python -m http.server is blocked: no network servers".to_string());
            }
            if args.iter().any(|a| a == "-") {
                return Some(
                    "reading Python from stdin is blocked: run a script file inside the workspace"
                        .to_string(),
                );
            }
            None
        }
        "node" => args
            .iter()
            .find(|a| NODE_INLINE_FLAGS.contains(&a.as_str()))
            .map(|a| format!("inline JavaScript ({a}) is blocked: run a script file inside the workspace")),
        _ => None,
    }
}

fn workspace_script(root: &Path, program: &str, args: &[String]) -> Result<(), String> {
    let script = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| format!("{program} needs a script file argument inside the workspace"))?;
    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve workspace: {e}"))?;
    let canon = root
        .join(script)
        .canonicalize()
        .map_err(|_| format!("script not found inside the workspace: {script}"))?;
    if !canon.starts_with(&root_canon) {
        return Err(format!("script escapes workspace: {script}"));
    }
    Ok(())
}

pub fn run_command(ctx: &ToolContext, args: &Value) -> ToolResult {
    if ctx.mode == RunMode::DryRun {
        return ToolResult::error("dry-run: command not executed", "Re-run in ask/auto mode to execute.");
    }
    let raw = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c.trim(),
        _ => return ToolResult::error("missing command", "Pass an allowlisted command."),
    };
    if raw.chars().any(|c| matches!(c, '|' | '&' | ';' | '<' | '>' | '$' | '`' | '\n' | '\r')) {
        return ToolResult::error("shell metacharacters are not allowed", "Run one allowlisted program with plain arguments.");
    }
    let argv = match split_command(raw) {
        Ok(argv) => argv,
        Err(e) => return ToolResult::error(e, "Close the quote or remove the stray character and retry."),
    };
    if argv.is_empty() {
        return ToolResult::error("missing command", "Pass an allowlisted command.");
    }
    let program = argv[0].as_str();
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program).trim_end_matches(".exe").to_lowercase();
    if !ctx.allowlist.iter().any(|a| a == &base) {
        return ToolResult::error(format!("program not allowlisted: {base}"), format!("Allowed: {}", ctx.allowlist.join(", ")));
    }
    let rest = &argv[1..];
    if let Some(reason) = blocked_reason(&base, rest, ctx.allow_inline_code) {
        return ToolResult::error(reason, "Rewrite the command as a non-destructive, workspace-local action.");
    }
    if matches!(base.as_str(), "python" | "python3" | "node") {
        let python = matches!(base.as_str(), "python" | "python3");
        let inline = python && rest.iter().any(|a| a == "-c");
        let module = python && rest.iter().any(|a| a == "-m");
        if !inline && !module {
            if let Err(e) = workspace_script(&ctx.root, &base, rest) {
                return ToolResult::error(e, "Run a script file that lives inside the workspace.");
            }
        }
    }
    let timeout = Duration::from_secs(
        args.get("timeout_sec").and_then(|v| v.as_u64()).unwrap_or(ctx.timeout.as_secs()).min(MAX_TIMEOUT_SECS),
    );
    let mut cmd = Command::new(program);
    cmd.args(rest).current_dir(&ctx.root).stdout(Stdio::piped()).stderr(Stdio::piped()).env_clear();
    for key in ["PATH", "SystemRoot", "TEMP", "TMP", "USERPROFILE", "HOME", "COMSPEC", "PATHEXT"] {
        if let Ok(v) = std::env::var(key) { cmd.env(key, v); }
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("cannot spawn {base}: {e}"), "Check the program is installed."),
    };
    let (status, out) = match wait_with_output_timeout(child, timeout) {
        Ok((status, out)) => (status, output_or_marker(out)),
        Err(e) => return ToolResult::error(format!("wait failed: {e}"), "Retry once; if it repeats, escalate."),
    };
    let status = match status {
        Some(status) => status,
        None => {
            let mut result = finish_with_artifact(
                ctx,
                "error",
                format!("{base} timed out after {}s", timeout.as_secs()),
                out,
                None,
            );
            result.next_actions = vec!["Use a shorter or narrower command.".into()];
            return result;
        }
    };
    let code = status.code().unwrap_or(-1);
    let summary = format!("{base} exited {code}");
    let mut result = finish_with_artifact(ctx, if status.success() { "success" } else { "error" }, summary, out, Some(code));
    if !status.success() { result.next_actions = vec!["Read the error excerpt, adjust, and retry (max 3 attempts).".into()]; }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::RunMode;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    fn workspace(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tinyloop-shell-{name}-{}", crate::audit::now_ms()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn split_command_keeps_quoted_arguments_together() {
        let argv = split_command("rg \"fn main\" 'src dir'").unwrap();
        assert_eq!(argv, vec!["rg", "fn main", "src dir"]);
    }

    #[test]
    fn split_command_handles_empty_quotes() {
        assert_eq!(split_command("git commit -m \"\"").unwrap(), vec!["git", "commit", "-m", ""]);
    }

    #[test]
    fn split_command_preserves_windows_paths() {
        assert_eq!(
            split_command(r"rg pattern C:\Users\parag\repo\src").unwrap(),
            vec!["rg", "pattern", r"C:\Users\parag\repo\src"]
        );
        assert_eq!(
            split_command(r#"node "C:\Users\parag\repo\script.js""#).unwrap(),
            vec!["node", r"C:\Users\parag\repo\script.js"]
        );
    }

    #[test]
    fn split_command_rejects_unmatched_quotes() {
        assert!(split_command("rg \"unterminated").is_err());
        assert!(split_command("rg 'unterminated").is_err());
    }

    #[test]
    fn inline_interpreter_code_is_rejected_by_default() {
        let root = workspace("inline");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = run_command(&ctx, &json!({"command": "python -c \"print(1)\""}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("inline Python"));
        let result = run_command(&ctx, &json!({"command": "node -e \"console.log(1)\""}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("inline JavaScript"));
    }

    #[test]
    fn inline_code_can_be_enabled_explicitly() {
        let args = vec!["-c".to_string(), "print(1)".to_string()];
        assert!(blocked_reason("python", &args, false).is_some());
        assert!(blocked_reason("python", &args, true).is_none());
    }

    #[test]
    fn destructive_git_is_rejected() {
        let root = workspace("git");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        for command in [
            "git push --force",
            "git reset --hard",
            "git clean -fd",
            "git commit -m x",
            "git config user.name x",
            "git remote add origin https://example.com/x.git",
        ] {
            let result = run_command(&ctx, &json!({ "command": command }));
            assert_eq!(result.status, "error", "{command} should be rejected");
            assert!(result.summary.contains("blocked"), "{command}: {}", result.summary);
        }
        let result = run_command(&ctx, &json!({"command": "git -C ../outside status"}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("blocked"));
    }

    #[test]
    fn python_http_server_is_rejected() {
        let root = workspace("http");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = run_command(&ctx, &json!({"command": "python -m http.server 8000"}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("http.server"));
    }

    #[test]
    fn interpreter_script_must_live_inside_the_workspace() {
        let root = workspace("script");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = run_command(&ctx, &json!({"command": "python ../outside.py"}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("script"));
    }

    #[test]
    fn empty_output_gets_an_explicit_marker() {
        assert_eq!(output_or_marker(String::new()), NO_OUTPUT);
        assert_eq!(output_or_marker(" \n\t".into()), NO_OUTPUT);
        assert_eq!(output_or_marker("ok".into()), "ok");
    }

    #[test]
    fn reader_caps_capture_but_drains_the_stream() {
        let data = vec![b'x'; MAX_CAPTURE_BYTES + 4096];
        let text = spawn_reader(std::io::Cursor::new(data)).join().expect("reader joins");
        assert_eq!(text.len(), MAX_CAPTURE_BYTES);
    }

    #[test]
    fn large_output_does_not_deadlock_the_pipe() {
        // Old code awaited exit before reading, so >64 KiB (Linux) or
        // >4 KiB (Windows) of output filled the pipe and forced a timeout.
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "for /L %i in (1,1,4000) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "yes xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx | head -n 4000"]);
            c
        };
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = cmd.spawn().expect("spawn output producer");
        let (status, out) = wait_with_output_timeout(child, Duration::from_secs(30)).expect("wait");
        assert!(status.expect("child must exit before the timeout").success());
        assert!(out.len() > 64 * 1024, "captured only {} bytes", out.len());
    }

    #[test]
    fn timeout_keeps_partial_output() {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "echo partial && ping -n 6 127.0.0.1 > NUL"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "echo partial; sleep 5"]);
            c
        };
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = cmd.spawn().expect("spawn sleeper");
        let (status, out) = wait_with_output_timeout(child, Duration::from_millis(400)).expect("wait");
        assert!(status.is_none(), "child should have been killed at the timeout");
        assert!(out.contains("partial"), "partial output was dropped: {out:?}");
    }
}
