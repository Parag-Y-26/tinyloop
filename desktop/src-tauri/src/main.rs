#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, State};
use tinyloop_core::loop_engine::{
    run_agent, AnyVerifier, ApprovalDecision, ApprovalRequest, Approver, CommandVerifier,
    NeverVerifier, RunConfig, RunEvent, RunReport, RunStatus,
};
use tinyloop_core::model::{run_startup_probe, LlamaCppClient, ProbeReport, DEFAULT_CONTEXT};
use tinyloop_core::tools::{RunMode, ToolContext};

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
const APPROVAL_POLL: Duration = Duration::from_millis(250);

#[derive(Default)]
struct ApprovalRegistry {
    pending: Mutex<HashMap<String, mpsc::Sender<ApprovalDecision>>>,
}

impl ApprovalRegistry {
    fn resolve(&self, id: &str, decision: ApprovalDecision) -> bool {
        let sender = self.pending.lock().ok().and_then(|mut map| map.remove(id));
        match sender {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// Cancel must resolve every waiting approval, otherwise a run stays
    /// blocked in `recv_timeout` long after the user pressed Cancel.
    fn deny_all(&self) -> usize {
        let Ok(mut map) = self.pending.lock() else { return 0 };
        let count = map.len();
        for (_, tx) in map.drain() {
            let _ = tx.send(ApprovalDecision::Deny);
        }
        count
    }
}

#[derive(Default)]
struct AppState {
    approvals: ApprovalRegistry,
    cancel: Arc<AtomicBool>,
    /// The startup probe is a per-process, per-model check; cache the result
    /// instead of paying three requests on every task.
    probes: Mutex<HashMap<String, Result<ProbeReport, String>>>,
}

impl AppState {
    fn ensure_probe(&self, model: &str, transport: &LlamaCppClient) -> Result<(ProbeReport, bool), String> {
        if let Ok(map) = self.probes.lock() {
            if let Some(cached) = map.get(model) {
                return cached.clone().map(|report| (report, false));
            }
        }
        let outcome = run_startup_probe(transport, model, None).map_err(|e| e.to_string());
        // Cache only success: a transient outage must not poison every later
        // task for this process.
        if let Ok(report) = &outcome {
            if let Ok(mut map) = self.probes.lock() {
                map.insert(model.to_owned(), Ok(report.clone()));
            }
        }
        outcome.map(|report| (report, true))
    }
}

struct TauriApprover {
    app: tauri::AppHandle,
    state: Arc<AppState>,
}

impl Approver for TauriApprover {
    fn request(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut map) = self.state.approvals.pending.lock() {
            map.insert(request.id.clone(), tx);
        }
        let _ = self.app.emit("approval_required", request.clone());
        let deadline = Instant::now() + APPROVAL_TIMEOUT;
        loop {
            if self.state.cancel.load(Ordering::SeqCst) {
                if let Ok(mut map) = self.state.approvals.pending.lock() {
                    map.remove(&request.id);
                }
                return ApprovalDecision::Deny;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return ApprovalDecision::Deny;
            }
            match rx.recv_timeout(remaining.min(APPROVAL_POLL)) {
                Ok(decision) => return decision,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return ApprovalDecision::Deny,
            }
        }
    }
}

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
        RunEvent::ApprovalRequired(request) => UiEvent {
            kind: "approval".into(), title: "Approval".into(),
            detail: format!("{} {}", request.tool, compact(&request.args)),
            status: "running".into(), meta: request.hash.chars().take(8).collect(),
        },
        RunEvent::Compacted { before_tokens, after_tokens } => UiEvent {
            kind: "compaction".into(), title: "Compacted".into(),
            detail: format!("{before_tokens} -> {after_tokens} estimated tokens"),
            status: "done".into(), meta: "state.md".into(),
        },
    }
}

#[tauri::command]
fn context_budget() -> usize {
    tinyloop_core::tools::observation::resolve_tool_result_budget(DEFAULT_CONTEXT).max_chars
}

#[tauri::command]
fn current_dir() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn run_task(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
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
    let state = state.inner().clone();
    state.cancel.store(false, Ordering::SeqCst);

    tauri::async_runtime::spawn_blocking(move || {
        let transport = LlamaCppClient::local(&model).map_err(|e| e.to_string())?;
        let (probe, fresh) = state.ensure_probe(&model, &transport)?;
        if fresh {
            let _ = app.emit("run_event", UiEvent {
                kind: "probe".into(), title: "Probe".into(),
                detail: format!("tools visible ({} -> {} prompt tokens), tool call parsed", probe.prompt_tokens_without_tools, probe.prompt_tokens_with_tools),
                status: "done".into(), meta: "capability".into(),
            });
        }

        let artifacts = std::path::Path::new(&cwd).join(".tinyloop").join("artifacts");
        let ctx = ToolContext::new(&cwd, artifacts, mode);
        let verifier = match verify_command.clone() {
            Some(cmd) => AnyVerifier::Command(CommandVerifier::new(cmd, &cwd)),
            None => AnyVerifier::Never(NeverVerifier),
        };
        let mut config = RunConfig::new(&model, task, done_condition)
            .with_boundaries(boundaries)
            .with_max_steps(12)
            .with_cancel(state.cancel.clone());
        if let Some(cmd) = verify_command {
            config = config.with_verifier(cmd);
        }

        let emitter = app.clone();
        let approver = TauriApprover { app: app.clone(), state: state.clone() };
        let report = run_agent(&transport, &config, &ctx, &verifier, Some(&approver), |event| {
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

#[tauri::command]
fn approve(state: State<'_, Arc<AppState>>, id: String) -> bool {
    state.approvals.resolve(&id, ApprovalDecision::Allow)
}

#[tauri::command]
fn deny(state: State<'_, Arc<AppState>>, id: String) -> bool {
    state.approvals.resolve(&id, ApprovalDecision::Deny)
}

#[tauri::command]
fn cancel_run(state: State<'_, Arc<AppState>>) -> bool {
    state.cancel.store(true, Ordering::SeqCst);
    state.approvals.deny_all();
    true
}

fn main() {
    tauri::Builder::default()
        .manage(Arc::new(AppState::default()))
        .invoke_handler(tauri::generate_handler![
            context_budget,
            current_dir,
            run_task,
            approve,
            deny,
            cancel_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running tinyloop");
}
