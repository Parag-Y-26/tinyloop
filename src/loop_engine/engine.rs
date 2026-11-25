//! The bounded Plan → Act → Observe → Verify loop.
//!
//! Rust owns termination. The model proposes one tool call per turn; only the
//! external verifier can produce `Done`.

use crate::loop_engine::repeat_guard::{self, RepeatState, Verdict};
use crate::model::{ChatRequest, ChatResponse, ChatTransport, Message, ModelError};
use crate::tools::{self, ToolContext, ToolResult};
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub model: String,
    pub task: String,
    pub done_condition: String,
    pub boundaries: Vec<String>,
    pub verify_command: Option<String>,
    pub max_steps: usize,
}

impl RunConfig {
    pub fn new(model: impl Into<String>, task: impl Into<String>, done_condition: impl Into<String>) -> Self {
        Self {
            model: model.into(), task: task.into(), done_condition: done_condition.into(),
            boundaries: vec![], verify_command: None, max_steps: 12,
        }
    }
    pub fn with_boundaries(mut self, boundaries: Vec<String>) -> Self { self.boundaries = boundaries; self }
    pub fn with_verifier(mut self, command: impl Into<String>) -> Self { self.verify_command = Some(command.into()); self }
    pub fn with_max_steps(mut self, steps: usize) -> Self { self.max_steps = steps; self }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RunStatus { Done, Escalated, MaxSteps }

#[derive(Debug, Clone, Serialize)]
pub enum RunEvent {
    Plan(Vec<String>),
    ToolCall { name: String, args: Value },
    ToolResult { status: String, summary: String },
    Verify { passed: bool, output: String },
    Finished { summary: String },
    Escalated { reason: String },
    TextOnlyRepair { text: String },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunReport {
    pub status: Option<RunStatus>,
    pub steps: usize,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_hits: u32,
    pub final_text: Option<String>,
    pub events: Vec<RunEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyOutcome {
    pub passed: bool,
    pub output: String,
    pub exit_code: Option<i32>,
}

impl VerifyOutcome {
    pub fn pass(output: impl Into<String>) -> Self { Self { passed: true, output: output.into(), exit_code: Some(0) } }
    pub fn fail(output: impl Into<String>) -> Self { Self { passed: false, output: output.into(), exit_code: Some(1) } }
}

pub trait Verifier {
    fn verify(&self) -> VerifyOutcome;
}

pub struct CommandVerifier {
    command: String,
    root: PathBuf,
    timeout: Duration,
}

impl CommandVerifier {
    pub fn new(command: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self { command: command.into(), root: root.into(), timeout: Duration::from_secs(120) }
    }
}

impl Verifier for CommandVerifier {
    fn verify(&self) -> VerifyOutcome {
        let mut cmd = if cfg!(windows) {
            // cmd.exe mangles nested quotes in `/C findstr /C:"x"`. PowerShell
            // preserves the verifier string as one command.
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-NonInteractive", "-Command", &self.command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &self.command]);
            c
        };
        cmd.current_dir(&self.root).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return VerifyOutcome::fail(format!("cannot spawn verifier: {e}")),
        };
        match child.wait_timeout(self.timeout) {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut s) = child.stdout.take() { use std::io::Read; let _ = s.read_to_string(&mut out); }
                if let Some(mut s) = child.stderr.take() { use std::io::Read; let _ = s.read_to_string(&mut out); }
                VerifyOutcome { passed: status.success(), output: out.chars().take(4000).collect(), exit_code: status.code() }
            }
            Ok(None) => { let _ = child.kill(); let _ = child.wait(); VerifyOutcome::fail("verifier timed out".to_string()) }
            Err(e) => VerifyOutcome::fail(format!("verifier wait failed: {e}")),
        }
    }
}

pub struct NeverVerifier;
impl Verifier for NeverVerifier {
    fn verify(&self) -> VerifyOutcome { VerifyOutcome::fail("no deterministic verifier configured; cannot declare Done") }
}

/// The desktop shell chooses a verifier at runtime; this keeps `run_agent`
/// monomorphic without boxing.
pub enum AnyVerifier { Command(CommandVerifier), Never(NeverVerifier) }
impl Verifier for AnyVerifier {
    fn verify(&self) -> VerifyOutcome {
        match self { AnyVerifier::Command(v) => v.verify(), AnyVerifier::Never(v) => v.verify() }
    }
}

fn system_prompt(config: &RunConfig) -> String {
    let boundaries = if config.boundaries.is_empty() { "none extra".to_owned() } else { config.boundaries.join("; ") };
    format!(
        "You are tinyloop, a careful computer-use agent.\n\
         Goal: {task}\n\
         Done when: {done}\n\
         Must not: {boundaries}\n\
         Rules:\n\
         - Call exactly one tool per turn.\n\
         - Use read_file/search_files before editing. Prefer a unique exact patch.\n\
         - Never claim success. Call finish and let the Rust verifier decide.\n\
         - If blocked, call escalate with a concrete reason.\n\
         - Keep outputs short.",
        task = config.task, done = config.done_condition, boundaries = boundaries
    )
}

fn plan_from(args: &Value) -> Vec<String> {
    args.get("steps").and_then(|v| v.as_array()).map(|steps| {
        steps.iter().map(|s| format!("{} — {}", s.get("tool").and_then(|v| v.as_str()).unwrap_or("?"), s.get("description").and_then(|v| v.as_str()).unwrap_or(""))).collect()
    }).unwrap_or_default()
}

pub fn run_agent<T: ChatTransport, V: Verifier, F: FnMut(&RunEvent)>(
    transport: &T,
    config: &RunConfig,
    ctx: &ToolContext,
    verifier: &V,
    mut emit: F,
) -> Result<RunReport, ModelError> {
    let mut report = RunReport::default();
    let mut messages = vec![
        Message::system(system_prompt(config)),
        Message::user("Start. Call submit_plan with 3 to 6 steps. Do not write prose."),
    ];
    let mut repeat = RepeatState::default();
    let mut verify_failures = 0u32;
    let mut text_only_repairs = 0u32;
    let mut steps = 0usize;

    while steps < config.max_steps {
        let request = ChatRequest::for_model(&config.model)
            .with_messages(messages.clone())
            .with_tools(tools::specs())
            .with_max_tokens(512);
        let response: ChatResponse = transport.chat(&request)?;
        report.prompt_tokens += response.usage.prompt_tokens;
        report.completion_tokens += response.usage.completion_tokens;
        report.cache_hits += response.timings.as_ref().map(|t| t.cache_n).unwrap_or(0);

        let call = match response.tool_calls().first().cloned() {
            Some(call) => call,
            None => {
                text_only_repairs += 1;
                let text = response.text().unwrap_or("").to_string();
                emit(&RunEvent::TextOnlyRepair { text: text.clone() });
                if text_only_repairs >= 3 {
                    let reason = "model produced text only three times; no executable action".to_string();
                    emit(&RunEvent::Escalated { reason: reason.clone() });
                    report.status = Some(RunStatus::Escalated);
                    report.final_text = Some(reason);
                    return Ok(report);
                }
                messages.push(Message::assistant(text));
                messages.push(Message::user("No tool call was made. Call exactly one tool now. Do not answer in prose."));
                steps += 1;
                continue;
            }
        };

        steps += 1;
        emit(&RunEvent::ToolCall { name: call.function.name.clone(), args: call.function.arguments.clone() });

        let mut note = None;
        if call.function.name != "submit_plan" {
            match repeat_guard::observe(&mut repeat, &call.function.name, &call.function.arguments) {
                Some(Verdict::Stop) => {
                    let reason = repeat_guard::stop_text(&call.function.name, 4);
                    emit(&RunEvent::Escalated { reason: reason.clone() });
                    report.status = Some(RunStatus::Escalated);
                    report.final_text = Some(reason);
                    return Ok(report);
                }
                Some(Verdict::Note) => note = Some(repeat_guard::note_text(&call.function.name, 3)),
                None => {}
            }
        }

        let mut result = tools::execute(ctx, &call.function.name, &call.function.arguments);
        if let Some(note) = note {
            result.next_actions.push(note);
        }
        emit(&RunEvent::ToolResult { status: result.status.clone(), summary: result.summary.clone() });

        match call.function.name.as_str() {
            "submit_plan" => {
                let plan = plan_from(&call.function.arguments);
                if plan.is_empty() {
                    result = ToolResult::error("empty plan", "Submit 3-6 steps.");
                } else {
                    emit(&RunEvent::Plan(plan));
                }
            }
            "escalate" => {
                let reason = call.function.arguments.get("reason").and_then(|v| v.as_str()).unwrap_or("model escalated").to_string();
                emit(&RunEvent::Escalated { reason: reason.clone() });
                report.status = Some(RunStatus::Escalated);
                report.final_text = Some(reason);
                return Ok(report);
            }
            "finish" => {
                let outcome = verifier.verify();
                emit(&RunEvent::Verify { passed: outcome.passed, output: outcome.output.clone() });
                if outcome.passed {
                    let summary = call.function.arguments.get("summary").and_then(|v| v.as_str()).unwrap_or("done").to_string();
                    emit(&RunEvent::Finished { summary: summary.clone() });
                    report.status = Some(RunStatus::Done);
                    report.final_text = Some(summary);
                    report.steps = steps;
                    return Ok(report);
                }
                verify_failures += 1;
                result = ToolResult::error("verifier failed", outcome.output.chars().take(800).collect::<String>());
                if verify_failures >= 3 {
                    let reason = "verifier failed three times".to_string();
                    emit(&RunEvent::Escalated { reason: reason.clone() });
                    report.status = Some(RunStatus::Escalated);
                    report.final_text = Some(reason);
                    report.steps = steps;
                    return Ok(report);
                }
            }
            "apply_patch" | "run_command" => {
                let outcome = verifier.verify();
                emit(&RunEvent::Verify { passed: outcome.passed, output: outcome.output.clone() });
                if outcome.passed {
                    emit(&RunEvent::Finished { summary: "verifier passed".into() });
                    report.status = Some(RunStatus::Done);
                    report.final_text = Some("verifier passed".into());
                    report.steps = steps;
                    return Ok(report);
                }
            }
            _ => {}
        }

        messages.push(Message::assistant_with_tool_calls(response.text().map(|s| s.to_string()), vec![call.clone()]));
        messages.push(Message::tool_result(call.id.clone(), result.to_json()));
    }

    report.status = Some(RunStatus::MaxSteps);
    report.steps = steps;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssistantMessage, ChatResponse, Choice, FunctionCall, Timings, ToolCall, Usage};
    use std::time::{SystemTime, UNIX_EPOCH};
    use crate::tools::{RunMode, ToolContext};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::fs;

    struct FakeTransport { queue: RefCell<VecDeque<ChatResponse>> }
    impl FakeTransport {
        fn new(responses: Vec<ChatResponse>) -> Self { Self { queue: RefCell::new(responses.into()) } }
    }
    impl ChatTransport for FakeTransport {
        fn health(&self) -> Result<(), ModelError> { Ok(()) }
        fn chat(&self, _request: &ChatRequest) -> Result<ChatResponse, ModelError> {
            Ok(self.queue.borrow_mut().pop_front().expect("fake ran out of responses"))
        }
    }
    struct FakeVerifier { passes: Cell<bool> }
    impl Verifier for FakeVerifier {
        fn verify(&self) -> VerifyOutcome { if self.passes.get() { VerifyOutcome::pass("ok") } else { VerifyOutcome::fail("not yet") } }
    }

    fn call(name: &str, arguments: Value) -> ChatResponse {
        ChatResponse {
            choices: vec![Choice {
                message: AssistantMessage { content: None, tool_calls: vec![ToolCall { id: "c1".into(), kind: "function".into(), function: FunctionCall { name: name.into(), arguments } }], reasoning_content: None },
                finish_reason: Some("tool_calls".into()),
            }],
            usage: Usage { prompt_tokens: 100, completion_tokens: 10, prompt_tokens_details: None },
            timings: Some(Timings { cache_n: 50, prompt_ms: 1.0, predicted_ms: 1.0 }),
        }
    }
    fn text(text: &str) -> ChatResponse {
        ChatResponse {
            choices: vec![Choice { message: AssistantMessage { content: Some(text.into()), tool_calls: vec![], reasoning_content: None }, finish_reason: Some("stop".into()) }],
            usage: Usage::default(), timings: None,
        }
    }

    fn workspace(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tinyloop-test-{name}-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn plan_read_patch_then_verifier_done() {
        let root = workspace("done");
        fs::write(root.join("tax.txt"), "rate = 8\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"read_file","description":"read tax"},{"tool":"apply_patch","description":"fix rate"},{"tool":"finish","description":"verify"}]})),
            call("read_file", serde_json::json!({"path":"tax.txt"})),
            call("apply_patch", serde_json::json!({"path":"tax.txt","old_string":"rate = 8","new_string":"rate = 10"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "fix the tax rate", "rate is 10").with_verifier("test").with_max_steps(6);
        let report = run_agent(&transport, &config, &ctx, &verifier, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Done));
        assert!(fs::read_to_string(root.join("tax.txt")).unwrap().contains("rate = 10"));
        assert_eq!(report.steps, 3);
        assert!(report.prompt_tokens > 0);
    }

    #[test]
    fn three_verifier_failures_escalate() {
        let root = workspace("fail");
        fs::write(root.join("tax.txt"), "rate = 8\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"finish","description":"try"},{"tool":"finish","description":"try again"},{"tool":"finish","description":"last"}]})),
            call("finish", serde_json::json!({"summary":"one"})),
            call("finish", serde_json::json!({"summary":"two"})),
            call("finish", serde_json::json!({"summary":"three"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(false) };
        let config = RunConfig::new("spark-x26", "never passes", "impossible").with_verifier("test").with_max_steps(8);
        let report = run_agent(&transport, &config, &ctx, &verifier, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Escalated));
        assert!(report.final_text.unwrap().contains("three times"));
    }

    #[test]
    fn text_only_is_bounded_and_does_not_fabricate_success() {
        let root = workspace("text");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![text("I think it is done."), text("Really done."), text("Trust me.")]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "task", "done").with_verifier("test").with_max_steps(5);
        let report = run_agent(&transport, &config, &ctx, &verifier, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Escalated));
        assert!(report.final_text.unwrap().contains("text only"));
    }
}
