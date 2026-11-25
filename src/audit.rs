//! Append-only audit trail. Every run gets one JSONL file.

use serde::Serialize;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
struct Entry<'a> {
    ts_ms: u128,
    run_id: &'a str,
    kind: &'a str,
    detail: Value,
}

#[derive(Debug, Clone)]
pub struct AuditLog {
    path: PathBuf,
    run_id: String,
}

impl AuditLog {
    pub fn new(root: &Path, run_id: impl Into<String>) -> Self {
        let dir = root.join(".tinyloop");
        let _ = fs::create_dir_all(&dir);
        Self { path: dir.join("audit.jsonl"), run_id: run_id.into() }
    }

    pub fn path(&self) -> &Path { &self.path }
    pub fn run_id(&self) -> &str { &self.run_id }

    pub fn append(&self, kind: &str, detail: Value) {
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.path) else { return };
        let entry = Entry { ts_ms: now_ms(), run_id: &self.run_id, kind, detail };
        if let Ok(line) = serde_json::to_string(&entry) {
            let _ = writeln!(file, "{line}");
        }
    }

    pub fn read_all(root: &Path) -> Vec<Value> {
        let path = root.join(".tinyloop").join("audit.jsonl");
        let Ok(text) = fs::read_to_string(path) else { return Vec::new() };
        text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
    }
}

pub fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}
