//! Deterministic, bounded workspace map.
//!
//! A 4B model at 8K context cannot afford discovery turns. This module renders
//! a compact map of the workspace (relative layout + key definitions) that the
//! loop injects once into the first user message. Pure Rust, no model calls.

use std::fs;
use std::path::{Path, PathBuf};

/// Hard cap on the rendered map (about 400 tokens).
pub const REPO_MAP_BUDGET_CHARS: usize = 1600;

const TRUNCATION_MARKER: &str = "… (map truncated)";
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_DEFS_PER_FILE: usize = 6;

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "dist", ".tinyloop", "vendor"];
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "kts", "c", "h",
    "cc", "cpp", "hpp", "cs", "rb", "php", "swift", "scala", "lua",
];
const DEFINITION_KEYWORDS: &[&str] =
    &["fn", "struct", "enum", "trait", "def", "class", "function", "interface", "impl"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMap {
    pub text: String,
    /// Source files considered (they may not all fit inside the budget).
    pub files: usize,
}

pub fn build_repo_map(root: &Path) -> RepoMap {
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut paths: Vec<PathBuf> = Vec::new();
    collect_source_files(&root_canon, &mut paths);
    paths.sort();
    let files = paths.len();

    let mut text = String::from("[repo map]\n");
    let mut truncated = false;
    for path in &paths {
        let rel = relative_display(&root_canon, path);
        let defs = extract_definitions(path);
        let line = if defs.is_empty() {
            rel
        } else {
            format!("{rel} — {}", defs.join(", "))
        };
        let projected = text.chars().count() + line.chars().count() + 1;
        if projected > REPO_MAP_BUDGET_CHARS {
            truncated = true;
            break;
        }
        text.push_str(&line);
        text.push('\n');
    }
    if truncated {
        let marker = format!("{TRUNCATION_MARKER}\n");
        if text.chars().count() + marker.chars().count() <= REPO_MAP_BUDGET_CHARS {
            text.push_str(&marker);
        } else {
            // Keep whole lines and hard-stop inside the budget.
            let keep = REPO_MAP_BUDGET_CHARS.saturating_sub(marker.chars().count());
            let mut cut: String = text.chars().take(keep).collect();
            if let Some(pos) = cut.rfind('\n') {
                cut.truncate(pos + 1);
            }
            cut.push_str(&marker);
            text = cut;
        }
    }
    RepoMap { text, files }
}

fn collect_source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_symlink() {
            // Never wander outside the workspace through a link.
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect_source_files(&path, out);
        } else if file_type.is_file() {
            if name.starts_with('.') || !is_source_file(&path) {
                continue;
            }
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            if size > MAX_FILE_BYTES {
                continue;
            }
            out.push(path);
        }
    }
}

fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| SOURCE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn extract_definitions(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else { return Vec::new() };
    let mut defs = Vec::new();
    for line in text.lines() {
        if let Some(def) = definition_name(line) {
            if !defs.contains(&def) {
                defs.push(def);
                if defs.len() >= MAX_DEFS_PER_FILE {
                    break;
                }
            }
        }
    }
    defs
}

/// Pull a navigable definition out of one line, if it declares one.
fn definition_name(line: &str) -> Option<String> {
    let mut s = line.trim_start();
    if s.starts_with("//") || s.starts_with('#') || s.starts_with('*') || s.starts_with("/*") {
        return None;
    }
    let mut display_prefix = String::new();
    loop {
        if let Some(rest) = s.strip_prefix("pub ") {
            s = rest;
            display_prefix.push_str("pub ");
            continue;
        }
        if let Some(rest) = s.strip_prefix("export default ") {
            s = rest;
            display_prefix.push_str("export default ");
            continue;
        }
        if let Some(rest) = s.strip_prefix("export ") {
            s = rest;
            display_prefix.push_str("export ");
            continue;
        }
        if let Some(rest) = s.strip_prefix("async ") {
            s = rest;
            continue;
        }
        break;
    }
    let (keyword, rest) = DEFINITION_KEYWORDS.iter().find_map(|keyword| {
        s.strip_prefix(keyword)
            .and_then(|rest| rest.strip_prefix(' '))
            .map(|rest| (*keyword, rest))
    })?;
    let name = rest
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .next()
        .unwrap_or("");
    if name.is_empty() {
        return None;
    }
    Some(format!("{display_prefix}{keyword} {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tinyloop-map-{name}-{}", crate::audit::now_ms()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn map_lists_relative_paths_and_key_definitions() {
        let root = workspace("defs");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("web")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "pub fn alpha() {}\nstruct Beta;\npub trait Gamma {}\n",
        )
        .unwrap();
        fs::write(
            root.join("web/app.tsx"),
            "export default function App() {}\nexport function helper() {}\n",
        )
        .unwrap();
        fs::write(root.join("README.md"), "fn not_code\n").unwrap();

        let map = build_repo_map(&root);
        assert_eq!(map.files, 2, "only source files count");
        assert!(map.text.contains("src/lib.rs"), "{}", map.text);
        assert!(map.text.contains("pub fn alpha"));
        assert!(map.text.contains("struct Beta"));
        assert!(map.text.contains("pub trait Gamma"));
        assert!(map.text.contains("web/app.tsx"));
        assert!(map.text.contains("export default function App"));
        assert!(map.text.contains("export function helper"));
        assert!(!map.text.contains("README.md"));
        assert!(map.text.chars().count() <= REPO_MAP_BUDGET_CHARS);
    }

    #[test]
    fn map_is_bounded_and_states_truncation() {
        let root = workspace("budget");
        fs::create_dir_all(root.join("src")).unwrap();
        for i in 0..120 {
            let body = (0..6)
                .map(|d| format!("pub fn function_number_{i}_{d}_with_a_long_name() {{}}\n"))
                .collect::<String>();
            fs::write(root.join("src").join(format!("module_{i}.rs")), body).unwrap();
        }
        let map = build_repo_map(&root);
        assert_eq!(map.files, 120);
        assert!(map.text.chars().count() <= REPO_MAP_BUDGET_CHARS, "{}", map.text.chars().count());
        assert!(map.text.contains("map truncated"));
    }

    #[test]
    fn map_skips_build_and_hidden_directories() {
        let root = workspace("skips");
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("target/secret.rs"), "pub fn hidden_from_target() {}\n").unwrap();
        fs::write(root.join(".git/hooks.rs"), "pub fn hidden_dot_dir() {}\n").unwrap();
        fs::write(root.join("main.rs"), "fn main_fn() {}\n").unwrap();
        let map = build_repo_map(&root);
        assert_eq!(map.files, 1);
        assert!(map.text.contains("main.rs"));
        assert!(!map.text.contains("hidden_from_target"));
        assert!(!map.text.contains("hidden_dot_dir"));
    }
}
