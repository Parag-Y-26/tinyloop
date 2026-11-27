//! The bounded Plan → Act → Observe → Verify loop.
//!
//! Rust owns termination. The model proposes one tool call per turn; only the
//! external verifier can produce `Done`.

use crate::loop_engine::repeat_guard::{self, RepeatState, Verdict};
use crate::model::{ChatRequest, ChatResponse, ChatTransport, Message, ModelError};
use crate::audit::AuditLog;
use crate::context::{self, ContextState};
use crate::tools::{self, RunMode, ToolContext, ToolResult};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PLAN_MIN_STEPS: usize = 3;
const PLAN_MAX_STEPS: usize = 6;

fn known_tool(name: &str) -> bool {
    matches!(name, "submit_plan" | "read_file" | "search_files" | "run_command" | "apply_patch" | "finish" | "escalate")
}

fn plan_steps(args: &Value) -> Vec<(String, String)> {
    args.get("steps").and_then(|v| v.as_array()).map(|steps| {
        steps.iter().map(|s| (
            s.get("tool").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            s.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        )).collect()
    }).unwrap_or_default()
}

fn plan_is_valid(steps: &[(String, String)]) -> bool {
    (PLAN_MIN_STEPS..=PLAN_MAX_STEPS).contains(&steps.len())
        && steps.iter().all(|(tool, _)| known_tool(tool))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ApprovalDecision { Allow, Deny }

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub tool: String,
    pub args: Value,
    pub hash: String,
}

pub trait Approver {
    fn request(&self, request: &ApprovalRequest) -> ApprovalDecision;
}

/// Used by headless runs and tests where writes are already in Auto mode.
pub struct AutoApprover;
impl Approver for AutoApprover {
    fn request(&self, _request: &ApprovalRequest) -> ApprovalDecision { ApprovalDecision::Allow }
}

pub struct DenyApprover;
impl Approver for DenyApprover {
    fn request(&self, _request: &ApprovalRequest) -> ApprovalDecision { ApprovalDecision::Deny }
}

pub fn approval_hash(tool: &str, args: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tool.as_bytes());
    hasher.update(b"\0");
    hasher.update(repeat_guard::canonical_json(args).as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub model: String,
    pub task: String,
    pub done_condition: String,
    pub boundaries: Vec<String>,
    pub verify_command: Option<String>,
    pub max_steps: usize,
    pub context_tokens: usize,
    pub cancel: Arc<AtomicBool>,
}

impl RunConfig {
    pub fn new(model: impl Into<String>, task: impl Into<String>, done_condition: impl Into<String>) -> Self {
        Self {
            model: model.into(), task: task.into(), done_condition: done_condition.into(),
            boundaries: vec![], verify_command: None, max_steps: 12,
            context_tokens: context::DEFAULT_CONTEXT_TOKENS,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn with_boundaries(mut self, boundaries: Vec<String>) -> Self { self.boundaries = boundaries; self }
    pub fn with_verifier(mut self, command: impl Into<String>) -> Self { self.verify_command = Some(command.into()); self }
    pub fn with_max_steps(mut self, steps: usize) -> Self { self.max_steps = steps; self }
    pub fn with_context_tokens(mut self, tokens: usize) -> Self { self.context_tokens = tokens.max(512); self }
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self { self.cancel = cancel; self }
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
    ApprovalRequired(ApprovalRequest),
    Compacted { before_tokens: usize, after_tokens: usize },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunReport {
    pub status: Option<RunStatus>,
    pub steps: usize,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_hits: u32,
    pub prompt_ms: f64,
    pub predicted_ms: f64,
    pub tool_errors: u32,
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
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return VerifyOutcome::fail(format!("cannot spawn verifier: {e}")),
        };
        match crate::tools::shell::wait_with_output_timeout(child, self.timeout) {
            Ok((Some(status), out)) => VerifyOutcome {
                passed: status.success(),
                output: out.chars().take(4000).collect(),
                exit_code: status.code(),
            },
            Ok((None, _)) => VerifyOutcome::fail("verifier timed out".to_string()),
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
    plan_steps(args).into_iter().map(|(tool, description)| format!("{tool} — {description}")).collect()
}

fn event_kind(event: &RunEvent) -> &'static str {
    match event {
        RunEvent::Plan(_) => "plan",
        RunEvent::ToolCall { .. } => "tool_call",
        RunEvent::ToolResult { .. } => "tool_result",
        RunEvent::Verify { .. } => "verify",
        RunEvent::Finished { .. } => "finished",
        RunEvent::Escalated { .. } => "escalated",
        RunEvent::TextOnlyRepair { .. } => "repair",
        RunEvent::ApprovalRequired(_) => "approval_required",
        RunEvent::Compacted { .. } => "compaction",
    }
}

fn push_emit<F: FnMut(&RunEvent)>(
    events: &mut Vec<RunEvent>,
    emit: &mut F,
    audit: Option<&AuditLog>,
    event: RunEvent,
) {
    if let Some(log) = audit {
        log.append(event_kind(&event), serde_json::to_value(&event).unwrap_or(serde_json::Value::Null));
    }
    events.push(event.clone());
    emit(&event);
}

fn finalize_run(mut report: RunReport, audit: &AuditLog) -> RunReport {
    audit.append("report", serde_json::json!({
        "status": report.status,
        "steps": report.steps,
        "prompt_tokens": report.prompt_tokens,
        "completion_tokens": report.completion_tokens,
        "cache_hits": report.cache_hits,
        "prompt_ms": report.prompt_ms,
        "predicted_ms": report.predicted_ms,
        "tool_errors": report.tool_errors,
    }));
    report.status = report.status.or(Some(RunStatus::MaxSteps));
    report
}

pub fn run_agent<T: ChatTransport, V: Verifier, F: FnMut(&RunEvent)>(
    transport: &T,
    config: &RunConfig,
    ctx: &ToolContext,
    verifier: &V,
    approver: Option<&dyn Approver>,
    mut emit: F,
) -> Result<RunReport, ModelError> {
    let run_id = format!("run-{}-{}", std::process::id(), crate::audit::now_ms());
    // The engine owns the budget, so tool result caps follow `RunConfig`
    // rather than whatever the caller last put on `ToolContext`.
    let tool_ctx = ctx.clone().with_context_tokens(config.context_tokens);
    let ctx = &tool_ctx;
    let audit = AuditLog::new(&ctx.root, run_id);
    let observation_budget = tools::observation::resolve_tool_result_budget(config.context_tokens).max_context_chars;
    let mut report = RunReport::default();
    let mut context_state = ContextState::new(&config.task, &config.done_condition, &config.boundaries);
    let mut messages = vec![
        Message::system(system_prompt(config)),
        Message::user("Start. Call submit_plan with 3 to 6 steps. Do not write prose."),
    ];
    let mut repeat = RepeatState::default();
    let mut verify_failures = 0u32;
    let mut text_only_repairs = 0u32;
    let mut malformed_repairs = 0u32;
    let mut plan_repairs = 0u32;
    let mut observation_chars = 0usize;
    let mut last_prompt_tokens: Option<u32> = None;
    let mut steps = 0usize;

    while steps < config.max_steps {
        if config.cancel.load(Ordering::Relaxed) {
            let reason = "cancelled by user".to_string();
            push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Escalated { reason: reason.clone() });
            report.status = Some(RunStatus::Escalated);
            report.final_text = Some(reason);
            report.steps = steps;
            return Ok(finalize_run(report, &audit));
        }
        let request = ChatRequest::for_model(&config.model)
            .with_messages(messages.clone())
            .with_tools(tools::specs())
            .with_max_tokens(512);
        let response: ChatResponse = transport.chat(&request)?;
        report.prompt_tokens += response.usage.prompt_tokens;
        report.completion_tokens += response.usage.completion_tokens;
        report.cache_hits += response.timings.as_ref().map(|t| t.cache_n).unwrap_or(0);
        report.prompt_ms += response.timings.as_ref().map(|t| t.prompt_ms).unwrap_or(0.0);
        report.predicted_ms += response.timings.as_ref().map(|t| t.predicted_ms).unwrap_or(0.0);
        if response.usage.prompt_tokens > 0 {
            last_prompt_tokens = Some(response.usage.prompt_tokens);
        }

        let call = match response.tool_calls().first().cloned() {
            Some(call) => call,
            None => {
                text_only_repairs += 1;
                let text = response.text().unwrap_or("").to_string();
                push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::TextOnlyRepair { text: text.clone() });
                if text_only_repairs >= 3 {
                    let reason = "model produced text only three times; no executable action".to_string();
                    push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Escalated { reason: reason.clone() });
                    report.status = Some(RunStatus::Escalated);
                    report.final_text = Some(reason);
                    return Ok(finalize_run(report, &audit));
                }
                messages.push(
                    Message::assistant(text)
                        .with_reasoning(response.first().and_then(|c| c.message.reasoning_content.clone())),
                );
                messages.push(Message::user("No tool call was made. Call exactly one tool now. Do not answer in prose."));
                steps += 1;
                continue;
            }
        };

        if response.finish_reason() == Some("length") {
            push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::TextOnlyRepair {
                text: "output limit while generating a tool call; not executed".into(),
            });
            messages.push(
                Message::assistant(response.text().unwrap_or("").to_string())
                    .with_reasoning(response.first().and_then(|c| c.message.reasoning_content.clone())),
            );
            messages.push(Message::user("The previous tool call was cut off by the output limit and was not executed. Retry with a smaller tool call."));
            steps += 1;
            continue;
        }

        if let Some(raw) = call.function.malformed_text() {
            malformed_repairs += 1;
            push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::TextOnlyRepair {
                text: format!("malformed tool arguments, not executed: {}", raw.chars().take(160).collect::<String>()),
            });
            steps += 1;
            if malformed_repairs > 1 {
                let reason = "model produced malformed tool arguments twice".to_string();
                push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Escalated { reason: reason.clone() });
                report.status = Some(RunStatus::Escalated);
                report.final_text = Some(reason);
                report.steps = steps;
                return Ok(finalize_run(report, &audit));
            }
            messages.push(
                Message::assistant(response.text().unwrap_or("").to_string())
                    .with_reasoning(response.first().and_then(|c| c.message.reasoning_content.clone())),
            );
            messages.push(Message::user("Your previous tool call arguments were not valid JSON and were not executed. Retry with valid JSON arguments for the same tool."));
            continue;
        }

        steps += 1;
        push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::ToolCall { name: call.function.name.clone(), args: call.function.arguments.clone() });

        let mut note = None;
        if call.function.name != "submit_plan" {
            match repeat_guard::observe(&mut repeat, &call.function.name, &call.function.arguments) {
                Some(Verdict::Stop) => {
                    let reason = repeat_guard::stop_text(&call.function.name, 4);
                    push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Escalated { reason: reason.clone() });
                    report.status = Some(RunStatus::Escalated);
                    report.final_text = Some(reason);
                    return Ok(finalize_run(report, &audit));
                }
                Some(Verdict::Note) => note = Some(repeat_guard::note_text(&call.function.name, 3)),
                None => {}
            }
        }

        let mut allowed = true;
        if ctx.mode == RunMode::Ask && matches!(call.function.name.as_str(), "apply_patch" | "run_command") {
            let request = ApprovalRequest {
                id: format!("ap-{steps}-{}", &approval_hash(&call.function.name, &call.function.arguments)[..8]),
                tool: call.function.name.clone(),
                args: call.function.arguments.clone(),
                hash: approval_hash(&call.function.name, &call.function.arguments),
            };
            push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::ApprovalRequired(request.clone()));
            allowed = match approver {
                Some(gate) => gate.request(&request) == ApprovalDecision::Allow,
                None => false,
            };
        }

        let mut result = if allowed {
            tools::execute(ctx, &call.function.name, &call.function.arguments)
        } else {
            ToolResult::error("approval denied", "No changes were made. Ask the user before retrying this action.")
        };
        if let Some(note) = note {
            result.next_actions.push(note);
        }

        match call.function.name.as_str() {
            "submit_plan" => {
                let steps_plan = plan_steps(&call.function.arguments);
                if plan_is_valid(&steps_plan) {
                    context_state.next_step = plan_from(&call.function.arguments).join("; ");
                    push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Plan(plan_from(&call.function.arguments)));
                } else {
                    plan_repairs += 1;
                    if plan_repairs > 1 {
                        let reason = "invalid plan after one repair attempt".to_string();
                        push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Escalated { reason: reason.clone() });
                        report.status = Some(RunStatus::Escalated);
                        report.final_text = Some(reason);
                        report.steps = steps;
                        return Ok(finalize_run(report, &audit));
                    }
                    result = ToolResult::error(
                        format!("invalid plan: {} steps", steps_plan.len()),
                        "Submit 3-6 steps, each mapping to one of the seven declared tools.",
                    );
                }
            }
            "escalate" => {
                let reason = call.function.arguments.get("reason").and_then(|v| v.as_str()).unwrap_or("model escalated").to_string();
                push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Escalated { reason: reason.clone() });
                report.status = Some(RunStatus::Escalated);
                report.final_text = Some(reason);
                return Ok(finalize_run(report, &audit));
            }
            "finish" => {
                // Verification belongs on `finish`, never on every tiny
                // mutation (docs/FLOW_REASONING.md §9).
                let outcome = verifier.verify();
                push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Verify { passed: outcome.passed, output: outcome.output.clone() });
                if outcome.passed {
                    let summary = call.function.arguments.get("summary").and_then(|v| v.as_str()).unwrap_or("done").to_string();
                    push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Finished { summary: summary.clone() });
                    report.status = Some(RunStatus::Done);
                    report.final_text = Some(summary);
                    report.steps = steps;
                    return Ok(finalize_run(report, &audit));
                }
                verify_failures += 1;
                result = ToolResult::error("verifier failed", outcome.output.chars().take(800).collect::<String>());
                if verify_failures >= 3 {
                    let reason = "verifier failed three times".to_string();
                    push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Escalated { reason: reason.clone() });
                    report.status = Some(RunStatus::Escalated);
                    report.final_text = Some(reason);
                    report.steps = steps;
                    return Ok(finalize_run(report, &audit));
                }
            }
            _ => {}
        }

        // Record the final result, not an intermediate one: plan validation
        // and verifier failures rewrite `result` above.
        context_state.observe(&call.function.name, &result.summary);
        if result.status == "error" { report.tool_errors += 1; }
        push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::ToolResult { status: result.status.clone(), summary: result.summary.clone() });

        observation_chars = observation_chars.saturating_add(result.stdout_excerpt.chars().count());
        messages.push(
            Message::assistant_with_tool_calls(response.text().map(|s| s.to_string()), vec![call.clone()])
                .with_reasoning(response.first().and_then(|c| c.message.reasoning_content.clone())),
        );
        messages.push(Message::tool_result(call.id.clone(), result.to_json()));

        let over_observation_budget = observation_chars > observation_budget;
        let compacted = if over_observation_budget {
            context::compact_with_metrics(&mut messages, &mut context_state, &ctx.root).inspect(|_| {
                observation_chars = 0;
            })
        } else {
            context::maybe_compact(
                &mut messages, &mut context_state, &ctx.root, config.context_tokens, last_prompt_tokens,
            )
        };
        if let Some((before_tokens, after_tokens)) = compacted {
            push_emit(&mut report.events, &mut emit, Some(&audit), RunEvent::Compacted { before_tokens, after_tokens });
        }
    }

    report.status = Some(RunStatus::MaxSteps);
    report.steps = steps;
    Ok(finalize_run(report, &audit))
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

    struct FakeTransport {
        queue: RefCell<VecDeque<ChatResponse>>,
        requests: RefCell<Vec<ChatRequest>>,
    }
    impl FakeTransport {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self { queue: RefCell::new(responses.into()), requests: RefCell::new(Vec::new()) }
        }
    }
    impl ChatTransport for FakeTransport {
        fn health(&self) -> Result<(), ModelError> { Ok(()) }
        fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, ModelError> {
            self.requests.borrow_mut().push(request.clone());
            Ok(self.queue.borrow_mut().pop_front().expect("fake ran out of responses"))
        }
    }
    struct FakeVerifier { passes: Cell<bool> }
    impl Verifier for FakeVerifier {
        fn verify(&self) -> VerifyOutcome { if self.passes.get() { VerifyOutcome::pass("ok") } else { VerifyOutcome::fail("not yet") } }
    }
    struct FixedApprover(ApprovalDecision);
    impl Approver for FixedApprover {
        fn request(&self, _request: &ApprovalRequest) -> ApprovalDecision { self.0 }
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
    fn truncated_call(name: &str, arguments: Value) -> ChatResponse {
        let mut response = call(name, arguments);
        response.choices[0].finish_reason = Some("length".into());
        response
    }
    fn malformed_call(name: &str, raw: &str) -> ChatResponse {
        let mut response = call(name, serde_json::json!({}));
        response.choices[0].message.tool_calls[0].function.arguments = serde_json::json!({ "__unparsed": raw });
        response
    }
    fn with_reasoning(mut response: ChatResponse, reasoning: &str) -> ChatResponse {
        response.choices[0].message.reasoning_content = Some(reasoning.into());
        response
    }

    struct CountingVerifier { passes: Cell<bool>, calls: Cell<u32> }
    impl Verifier for CountingVerifier {
        fn verify(&self) -> VerifyOutcome {
            self.calls.set(self.calls.get() + 1);
            if self.passes.get() { VerifyOutcome::pass("ok") } else { VerifyOutcome::fail("not yet") }
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
            call("finish", serde_json::json!({"summary":"rate is 10"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "fix the tax rate", "rate is 10").with_verifier("test").with_max_steps(6);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Done));
        assert!(fs::read_to_string(root.join("tax.txt")).unwrap().contains("rate = 10"));
        assert_eq!(report.steps, 4);
        assert!(report.prompt_tokens > 0);
    }

    #[test]
    fn ask_mode_denies_patch_without_an_approver() {
        let root = workspace("ask-deny");
        fs::write(root.join("tax.txt"), "rate = 8\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Ask);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"apply_patch","description":"fix"},{"tool":"finish","description":"verify"},{"tool":"read_file","description":"check"}]})),
            call("apply_patch", serde_json::json!({"path":"tax.txt","old_string":"rate = 8","new_string":"rate = 10"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "fix rate", "rate 10").with_max_steps(2);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::MaxSteps));
        assert!(fs::read_to_string(root.join("tax.txt")).unwrap().contains("rate = 8"));
        assert!(report.events.iter().any(|e| matches!(e, RunEvent::ApprovalRequired(_))));
    }

    #[test]
    fn ask_mode_applies_patch_after_approval() {
        let root = workspace("ask-allow");
        fs::write(root.join("tax.txt"), "rate = 8\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Ask);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"apply_patch","description":"fix"},{"tool":"finish","description":"verify"},{"tool":"read_file","description":"check"}]})),
            call("apply_patch", serde_json::json!({"path":"tax.txt","old_string":"rate = 8","new_string":"rate = 10"})),
            call("finish", serde_json::json!({"summary":"done"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let approver = FixedApprover(ApprovalDecision::Allow);
        let config = RunConfig::new("spark-x26", "fix rate", "rate 10").with_max_steps(5);
        let report = run_agent(&transport, &config, &ctx, &verifier, Some(&approver), |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Done));
        assert!(fs::read_to_string(root.join("tax.txt")).unwrap().contains("rate = 10"));
    }

    #[test]
    fn audit_trail_has_tool_calls_and_a_report() {
        let root = workspace("audit");
        fs::write(root.join("tax.txt"), "rate = 8\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"apply_patch","description":"fix"},{"tool":"finish","description":"verify"},{"tool":"read_file","description":"check"}]})),
            call("apply_patch", serde_json::json!({"path":"tax.txt","old_string":"rate = 8","new_string":"rate = 10"})),
            call("finish", serde_json::json!({"summary":"done"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "fix rate", "rate 10").with_max_steps(5);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Done));
        let entries = crate::audit::AuditLog::read_all(&root);
        let kinds: Vec<&str> = entries.iter().filter_map(|e| e.get("kind").and_then(|k| k.as_str())).collect();
        assert!(kinds.contains(&"plan"));
        assert!(kinds.contains(&"tool_call"));
        assert!(kinds.contains(&"report"));
        assert!(entries.iter().any(|e| e.get("run_id").and_then(|v| v.as_str()).is_some()));
    }

    #[test]
    fn compaction_fires_and_writes_state() {
        let root = workspace("compact");
        let long_file = format!("{}\nrate = 8\n", "x".repeat(6000));
        fs::write(root.join("tax.txt"), long_file).unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"read_file","description":"read the tax file carefully"},{"tool":"finish","description":"verify the result"},{"tool":"search_files","description":"confirm the change"}]})),
            call("read_file", serde_json::json!({"path":"tax.txt"})),
            call("finish", serde_json::json!({"summary":"done"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "fix rate", "rate 10").with_max_steps(5).with_context_tokens(512);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Done));
        assert!(report.events.iter().any(|e| matches!(e, RunEvent::Compacted { .. })));
        assert!(root.join(".tinyloop").join("state.md").exists());
    }

    #[test]
    fn truncated_tool_call_is_never_executed() {
        let root = workspace("truncated");
        fs::write(root.join("tax.txt"), "rate = 8\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"apply_patch","description":"fix"},{"tool":"finish","description":"verify"},{"tool":"read_file","description":"check"}]})),
            truncated_call("apply_patch", serde_json::json!({"path":"tax.txt","old_string":"rate = 8","new_string":"rate = 10"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "fix", "rate 10").with_max_steps(2);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert!(fs::read_to_string(root.join("tax.txt")).unwrap().contains("rate = 8"));
        assert!(!report.events.iter().any(|e| matches!(e, RunEvent::ToolResult { summary, .. } if summary.contains("patched"))));
        assert!(report.events.iter().any(|e| matches!(e, RunEvent::TextOnlyRepair { .. })));
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
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
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
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Escalated));
        assert!(report.final_text.unwrap().contains("text only"));
    }

    #[test]
    fn mutations_do_not_run_the_verifier() {
        let root = workspace("no-verify");
        fs::write(root.join("tax.txt"), "rate = 8\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"apply_patch","description":"fix"},{"tool":"finish","description":"verify"},{"tool":"read_file","description":"check"}]})),
            call("apply_patch", serde_json::json!({"path":"tax.txt","old_string":"rate = 8","new_string":"rate = 10"})),
        ]);
        let verifier = CountingVerifier { passes: Cell::new(true), calls: Cell::new(0) };
        let config = RunConfig::new("spark-x26", "fix", "rate 10").with_max_steps(2);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::MaxSteps));
        assert_eq!(verifier.calls.get(), 0, "mutation must not trigger verification");
        assert!(fs::read_to_string(root.join("tax.txt")).unwrap().contains("rate = 10"));
    }

    #[test]
    fn invalid_plan_gets_one_repair_then_escalates() {
        let root = workspace("bad-plan");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let short_plan = serde_json::json!({"steps":[{"tool":"read_file","description":"one"},{"tool":"finish","description":"two"}]});
        let transport = FakeTransport::new(vec![
            call("submit_plan", short_plan.clone()),
            call("submit_plan", short_plan),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "x", "y").with_max_steps(4);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Escalated));
        assert!(report.final_text.unwrap().contains("invalid plan"));
    }

    #[test]
    fn plan_with_unknown_tools_is_rejected() {
        let root = workspace("unknown-tool-plan");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"shell_everything","description":"a"},{"tool":"read_file","description":"b"},{"tool":"finish","description":"c"}]})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "x", "y").with_max_steps(1);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert!(report.events.iter().any(|e| matches!(e, RunEvent::ToolResult { summary, .. } if summary.contains("invalid plan"))));
        assert!(!report.events.iter().any(|e| matches!(e, RunEvent::Plan(_))));
    }

    #[test]
    fn malformed_arguments_get_one_repair() {
        let root = workspace("malformed-repair");
        fs::write(root.join("tax.txt"), "rate = 8\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"apply_patch","description":"fix"},{"tool":"finish","description":"verify"},{"tool":"read_file","description":"check"}]})),
            malformed_call("apply_patch", "{not json"),
            call("apply_patch", serde_json::json!({"path":"tax.txt","old_string":"rate = 8","new_string":"rate = 10"})),
            call("finish", serde_json::json!({"summary":"done"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "fix", "rate 10").with_max_steps(6);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Done));
        assert!(fs::read_to_string(root.join("tax.txt")).unwrap().contains("rate = 10"));
        assert!(report.events.iter().any(|e| matches!(e, RunEvent::TextOnlyRepair { text } if text.contains("malformed"))));
    }

    #[test]
    fn malformed_arguments_twice_escalate() {
        let root = workspace("malformed-twice");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            call("submit_plan", serde_json::json!({"steps":[{"tool":"apply_patch","description":"fix"},{"tool":"finish","description":"verify"},{"tool":"read_file","description":"check"}]})),
            malformed_call("apply_patch", "{not json"),
            malformed_call("apply_patch", "still not json"),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "fix", "rate 10").with_max_steps(6);
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        assert_eq!(report.status, Some(RunStatus::Escalated));
        assert!(report.final_text.unwrap().contains("malformed"));
    }

    #[test]
    fn reasoning_content_stays_in_multi_turn_history() {
        let root = workspace("reasoning");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let transport = FakeTransport::new(vec![
            with_reasoning(
                call("submit_plan", serde_json::json!({"steps":[{"tool":"finish","description":"a"},{"tool":"read_file","description":"b"},{"tool":"finish","description":"c"}]})),
                "planning thoughts",
            ),
            call("finish", serde_json::json!({"summary":"done"})),
        ]);
        let verifier = FakeVerifier { passes: Cell::new(true) };
        let config = RunConfig::new("spark-x26", "task", "done").with_max_steps(4);
        run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).unwrap();
        let requests = transport.requests.borrow();
        assert!(requests[1].messages.iter().any(|m|
            m.role == "assistant" && m.reasoning_content.as_deref() == Some("planning thoughts")
        ));
    }
}
