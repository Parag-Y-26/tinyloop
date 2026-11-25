use std::env;
use tinyloop_core::model::{run_startup_probe, LlamaCppClient};

fn main() {
    let base = env::var("TINYLOOP_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:9941".to_owned());
    let model = env::var("TINYLOOP_MODEL").unwrap_or_else(|_| "spark-x26".to_owned());
    let build = env::var("TINYLOOP_BUILD").ok();
    let client = match LlamaCppClient::new(base.clone(), model.clone()) {
        Ok(c) => c,
        Err(e) => { eprintln!("client error: {e}"); std::process::exit(2); }
    };
    println!("base={base} model={model} build={build:?}");
    match run_startup_probe(&client, &model, build) {
        Ok(report) => println!("PROBE OK: {report:#?}"),
        Err(e) => { eprintln!("PROBE FAILED: {e}"); std::process::exit(1); }
    }
}
