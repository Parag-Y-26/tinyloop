#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use tauri::Emitter;
use tinyloop_core::loop_engine::{
    run_agent, AnyVerifier, CommandVerifier, NeverVerifier, RunConfig, RunEvent, RunReport, RunStatus,
};
use tinyloop_core::model::{run_startup_probe, LlamaCppClient};
use tinyloop_core::tools::{RunMode, ToolContext};

#[derive(Clone, Serialize)]
struct UiEvent {
    kind: String,
    title: String,
    detail: String,
    status: String,
    meta: String,
}

fn compact(value: &serde_json::Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 220 { text.chars().take(220).collect::<String>() + "…" } else { text }
}

fn ui_event(event: &RunEvent) -> UiEvent {
    match event {
        RunEvent::Plan(steps) => UiEvent {
            kind: "plan".into(), title: "Plan".into(),
            detail: steps.join(" · "), status: "done".into(), meta: format!("{} steps", steps.len()),
        },
        RunEvent::ToolCall { name, args } => UiEvent {
            kind: "tool_call".into(), title: "Act".into(),
            detail: format!("{name} {}", compact(args)), status: "running".into(), meta: String::new(),
        },
        RunEvent::ToolResult { status, summary } => UiEvent {
            kind: "tool_result".into(), title: "Observe".into(),
            detail: summary.clone(), status: if status == "error" { "blocked".into() } else { "done".into() }, meta: String::new(),
        },
        RunEvent::Verify { passed, output } => UiEvent {
            kind: "verify".into(), title: "Verify".into(),
            detail: output.lines().next().unwrap_or("verifier ran").to_string(),
            status: if *passed { "done".into() } else { "blocked".into() }, meta: if *passed { "passed".into() } else { "failed".into() },
        },
        RunEvent::Finished { summary } => UiEvent {
            kind: "done".into(), title: "Done".into(), detail: summary.clone(), status: "done".into(), meta: "verified".into(),
        },
        RunEvent::Escalated { reason } => UiEvent {
            kind: "escalated".into(), title: "Escalate".into(), detail: reason.clone(), status: "blocked".into(), meta: "human".into(),
        },
        RunEvent::TextOnlyRepair { text } => UiEvent {
            kind: "repair".into(), title: "Repair".into(), detail: text.chars().take(180).collect(), status: "running".into(), meta: "text only".into(),
        },
    }
}

#[tauri::command]
fn context_budget() -> usize {
    tinyloop_core::tools::observation::resolve_tool_result_budget(8192).max_chars
}

#[tauri::command]
async fn run_task(
    app: tauri::AppHandle,
    task: String,
    done_condition: String,
    boundaries: Vec<String>,
    verify_command: Option<String>,
    cwd: String,
    mode: Option<String>,
    model: Option<String>,
) -> Result<RunReport, String> {
    let mode = match mode.as_deref() {
        Some("auto") => RunMode::Auto,
        Some("dry") => RunMode::DryRun,
        _ => RunMode::Ask,
    };
    let model = model.unwrap_or_else(|| "spark-x26".to_owned());

    tauri::async_runtime::spawn_blocking(move || {
        let transport = LlamaCppClient::local(&model).map_err(|e| e.to_string())?;
        let probe = run_startup_probe(&transport, &model, None).map_err(|e| e.to_string())?;
        let _ = app.emit("run_event", UiEvent {
            kind: "probe".into(), title: "Probe".into(),
            detail: format!("tools visible ({} -> {} prompt tokens), tool call parsed", probe.prompt_tokens_without_tools, probe.prompt_tokens_with_tools),
            status: "done".into(), meta: "capability".into(),
        });

        let artifacts = std::path::Path::new(&cwd).join(".tinyloop").join("artifacts");
        let ctx = ToolContext::new(&cwd, artifacts, mode);
        let verifier = match verify_command.clone() {
            Some(cmd) => AnyVerifier::Command(CommandVerifier::new(cmd, &cwd)),
            None => AnyVerifier::Never(NeverVerifier),
        };
        let mut config = RunConfig::new(&model, task, done_condition)
            .with_boundaries(boundaries)
            .with_max_steps(12);
        if let Some(cmd) = verify_command {
            config = config.with_verifier(cmd);
        }

        let emitter = app.clone();
        let report = run_agent(&transport, &config, &ctx, &verifier, |event| {
            let _ = emitter.emit("run_event", ui_event(event));
        }).map_err(|e| e.to_string())?;

        if report.status == Some(RunStatus::MaxSteps) {
            let _ = app.emit("run_event", UiEvent {
                kind: "max_steps".into(), title: "Escalate".into(),
                detail: "step cap reached without a passing verifier".into(),
                status: "blocked".into(), meta: "cap".into(),
            });
        }
        Ok(report)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![context_budget, run_task])
        .run(tauri::generate_context!())
        .expect("error while running tinyloop");
}
