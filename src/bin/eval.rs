use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use tinyloop_core::loop_engine::{run_agent, AnyVerifier, CommandVerifier, RunConfig, RunStatus, Verifier};
use tinyloop_core::model::LlamaCppClient;
use tinyloop_core::tools::{RunMode, ToolContext};

#[derive(Debug, Deserialize)]
struct Task {
    id: String,
    goal: String,
    done_condition: String,
    #[serde(default)]
    boundaries: Vec<String>,
    #[serde(default)]
    fixture: Vec<Fixture>,
    verify: Verify,
}

#[derive(Debug, Deserialize)]
struct Fixture { path: String, content: String }

#[derive(Debug, Deserialize)]
struct Verify { command: String }

#[derive(Debug, Serialize)]
struct TaskResult {
    id: String,
    passed: bool,
    status: String,
    steps: usize,
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_hits: u32,
    prompt_ms: f64,
    predicted_ms: f64,
    tool_errors: u32,
    wall_ms: u128,
}

fn main() {
    let filter = std::env::var("EVAL_FILTER").ok();
    let limit = std::env::var("EVAL_LIMIT").ok().and_then(|v| v.parse::<usize>().ok());
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eval/tasks");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir).expect("tasks dir").flatten().map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml")).collect();
    paths.sort();
    let mut results = Vec::new();
    for path in paths {
        let task: Task = toml::from_str(&fs::read_to_string(&path).unwrap()).expect("task toml");
        if let Some(filter) = &filter { if !task.id.contains(filter.as_str()) { continue; } }
        if let Some(limit) = limit { if results.len() >= limit { break; } }
        print!("{:<14} ", task.id);
        std::io::Write::flush(&mut std::io::stdout()).ok();
        let workspace = std::env::temp_dir().join(format!("tinyloop-eval-{}", task.id));
        let _ = fs::remove_dir_all(&workspace);
        fs::create_dir_all(&workspace).unwrap();
        for fixture in &task.fixture {
            let p = workspace.join(&fixture.path);
            if let Some(parent) = p.parent() { fs::create_dir_all(parent).unwrap(); }
            fs::write(p, &fixture.content).unwrap();
        }

        let transport = LlamaCppClient::local("spark-x26").expect("client");
        let ctx = ToolContext::new(&workspace, workspace.join(".tinyloop"), RunMode::Auto);
        let verifier = AnyVerifier::Command(CommandVerifier::new(&task.verify.command, &workspace));
        let config = RunConfig::new("spark-x26", &task.goal, &task.done_condition)
            .with_boundaries(task.boundaries.clone())
            .with_verifier(&task.verify.command)
            .with_max_steps(6);
        let started = Instant::now();
        let report = run_agent(&transport, &config, &ctx, &verifier, None, |_| {}).expect("run");
        let wall_ms = started.elapsed().as_millis();
        let independent = CommandVerifier::new(&task.verify.command, &workspace).verify();
        let passed = report.status == Some(RunStatus::Done) && independent.passed;
        let result = TaskResult {
            id: task.id.clone(),
            passed,
            status: format!("{:?}", report.status),
            steps: report.steps,
            prompt_tokens: report.prompt_tokens,
            completion_tokens: report.completion_tokens,
            cache_hits: report.cache_hits,
            prompt_ms: report.prompt_ms,
            predicted_ms: report.predicted_ms,
            tool_errors: report.tool_errors,
            wall_ms,
        };
        println!("{} steps={} tokens={}/{} wall={}ms", if passed { "PASS" } else { "FAIL" }, result.steps, result.prompt_tokens, result.completion_tokens, wall_ms);
        results.push(result);
    }
    let passed = results.iter().filter(|r| r.passed).count();
    println!("\nSUMMARY {}/{} passed", passed, results.len());
    let out = serde_json::json!({ "results": results, "passed": passed, "total": results.len() });
    fs::write(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("eval-results.json"), serde_json::to_string_pretty(&out).unwrap()).unwrap();
}
