use std::env;
use tinyloop_core::loop_engine::{run_agent, AnyVerifier, CommandVerifier, NeverVerifier, RunConfig, RunEvent, RunStatus};
use tinyloop_core::model::LlamaCppClient;
use tinyloop_core::tools::{RunMode, ToolContext};

fn main() {
    let task = env::var("TINYLOOP_TASK").unwrap_or_else(|_| "Read Cargo.toml and call finish.".to_owned());
    let cwd = env::var("TINYLOOP_CWD").unwrap_or_else(|_| ".".to_owned());
    let verify = env::var("TINYLOOP_VERIFY").ok();
    let model = env::var("TINYLOOP_MODEL").unwrap_or_else(|_| "spark-x26".to_owned());
    let max_steps = env::var("TINYLOOP_MAX_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(8usize);
    let done = env::var("TINYLOOP_DONE").unwrap_or_else(|_| "the verifier command exits 0".to_owned());

    let transport = LlamaCppClient::local(&model).expect("client");
    let artifacts = std::path::Path::new(&cwd).join(".tinyloop").join("artifacts");
    let ctx = ToolContext::new(&cwd, artifacts, RunMode::Auto);
    let verifier = match verify.clone() {
        Some(cmd) => AnyVerifier::Command(CommandVerifier::new(cmd, &cwd)),
        None => AnyVerifier::Never(NeverVerifier),
    };
    let mut config = RunConfig::new(&model, task, done).with_max_steps(max_steps);
    if let Some(cmd) = verify { config = config.with_verifier(cmd); }

    println!("model={model} cwd={cwd} max_steps={max_steps}");
    let report = run_agent(&transport, &config, &ctx, &verifier, Some(&tinyloop_core::loop_engine::AutoApprover), |event| match event {
        RunEvent::Plan(steps) => println!("PLAN: {}", steps.join(" | ")),
        RunEvent::ToolCall { name, args } => println!("CALL {name} {}", args),
        RunEvent::ToolResult { status, summary } => println!("  -> {status}: {summary}"),
        RunEvent::Verify { passed, output } => println!("VERIFY passed={passed} {}", output.lines().next().unwrap_or("")),
        RunEvent::Finished { summary } => println!("FINISHED: {summary}"),
        RunEvent::Escalated { reason } => println!("ESCALATED: {reason}"),
        RunEvent::TextOnlyRepair { text } => println!("REPAIR (text only): {}", text.chars().take(120).collect::<String>()),
        RunEvent::ApprovalRequired(request) => println!("APPROVAL: {} {}", request.tool, request.hash),
        RunEvent::Masked { masked_turns, before_tokens, after_tokens } => println!("MASK: {masked_turns} turns ({before_tokens} -> {after_tokens} tokens)"),
        RunEvent::Compacted { before_tokens, after_tokens } => println!("COMPACT: {before_tokens} -> {after_tokens} tokens"),
    }).expect("run");

    println!("STATUS: {:?} steps={} prompt_tokens={} completion_tokens={} cache_hits={}", report.status, report.steps, report.prompt_tokens, report.completion_tokens, report.cache_hits);
    std::process::exit(if report.status == Some(RunStatus::Done) { 0 } else { 1 });
}
