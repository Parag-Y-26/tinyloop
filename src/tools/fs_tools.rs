use super::{finish_with_artifact, RunMode, ToolContext, ToolResult};
use std::fs;
use std::path::{Path, PathBuf};
use serde_json::Value;

fn resolve_existing(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let joined = root.join(rel);
    let canon = joined.canonicalize().map_err(|e| format!("cannot resolve {rel}: {e}"))?;
    let root_canon = root.canonicalize().map_err(|e| format!("cannot resolve workspace: {e}"))?;
    if !canon.starts_with(&root_canon) { return Err(format!("path escapes workspace: {rel}")); }
    Ok(canon)
}

fn resolve_for_write(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let root_canon = root.canonicalize().map_err(|e| format!("cannot resolve workspace: {e}"))?;
    let joined = root.join(rel);
    // `symlink_metadata` also catches broken links that `exists()` misses.
    if std::fs::symlink_metadata(&joined).is_ok() {
        // An existing path may be a symlink; follow it and re-check so a
        // workspace-local link cannot redirect the write outside the root.
        let canon = joined.canonicalize().map_err(|e| format!("cannot resolve {rel}: {e}"))?;
        if !canon.starts_with(&root_canon) { return Err(format!("path escapes workspace: {rel}")); }
        return Ok(canon);
    }
    let parent = joined.parent().ok_or_else(|| "path has no parent".to_string())?;
    let parent_canon = parent.canonicalize().map_err(|e| format!("cannot resolve parent of {rel}: {e}"))?;
    if !parent_canon.starts_with(&root_canon) { return Err(format!("path escapes workspace: {rel}")); }
    let file_name = joined.file_name().ok_or_else(|| format!("{rel} has no file name"))?;
    Ok(parent_canon.join(file_name))
}

pub fn read_file(ctx: &ToolContext, args: &Value) -> ToolResult {
    let rel = match args.get("path").and_then(|v| v.as_str()) { Some(p) => p, None => return ToolResult::error("missing path", "Pass a workspace-relative path.") };
    let path = match resolve_existing(&ctx.root, rel) { Ok(p) => p, Err(e) => return ToolResult::error(e, "Use a path inside the workspace.") };
    let text = match fs::read_to_string(&path) { Ok(t) => t, Err(e) => return ToolResult::error(format!("cannot read {rel}: {e}"), "Check the path and that the file is UTF-8.") };
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(160) as usize;
    let selected: String = text.lines().skip(offset).take(limit).enumerate()
        .map(|(i, l)| format!("{}: {}\n", offset + i + 1, l)).collect();
    finish_with_artifact(ctx, "success", format!("read {rel} ({} lines)", text.lines().count()), selected, Some(0))
}

pub fn search_files(ctx: &ToolContext, args: &Value) -> ToolResult {
    let pattern = match args.get("pattern").and_then(|v| v.as_str()) { Some(p) if !p.is_empty() => p, _ => return ToolResult::error("missing pattern", "Pass a non-empty literal string.") };
    let start_rel = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let start = match resolve_existing(&ctx.root, start_rel) { Ok(p) => p, Err(e) => return ToolResult::error(e, "Use a path inside the workspace.") };
    let max = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
    let needle = pattern.to_lowercase();
    let mut hits = Vec::new();
    walk(&start, &needle, max, &mut hits);
    let body = if hits.is_empty() { format!("No matches for {pattern:?}.") } else { hits.join("\n") };
    finish_with_artifact(ctx, "success", format!("{} matches for {pattern:?}", hits.len()), body, Some(0))
}

fn walk(dir: &Path, needle: &str, max: usize, hits: &mut Vec<String>) {
    if hits.len() >= max { return; }
    let entries = match fs::read_dir(dir) { Ok(e) => e, Err(_) => return };
    for entry in entries.flatten() {
        if hits.len() >= max { return; }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if matches!(name.as_str(), ".git" | "target" | "node_modules" | "dist" | ".tinyloop") { continue; }
            walk(&path, needle, max, hits);
        } else if let Ok(text) = fs::read_to_string(&path) {
            if text.len() > 1_000_000 { continue; }
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(needle) {
                    hits.push(format!("{}:{}: {}", path.display(), i + 1, line.trim_end()));
                    if hits.len() >= max { return; }
                }
            }
        }
    }
}

pub fn apply_patch(ctx: &ToolContext, args: &Value) -> ToolResult {
    if ctx.mode == RunMode::DryRun { return ToolResult::error("dry-run: patch not applied", "Re-run in ask/auto mode to apply."); }
    let rel = match args.get("path").and_then(|v| v.as_str()) { Some(p) => p, None => return ToolResult::error("missing path", "Pass a workspace-relative path.") };
    let old = match args.get("old_string").and_then(|v| v.as_str()) { Some(s) if !s.is_empty() => s, _ => return ToolResult::error("missing old_string", "Provide the exact text to replace.") };
    let new = args.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
    let path = match resolve_for_write(&ctx.root, rel) { Ok(p) => p, Err(e) => return ToolResult::error(e, "Use a path inside the workspace.") };
    let text = match fs::read_to_string(&path) { Ok(t) => t, Err(e) => return ToolResult::error(format!("cannot read {rel}: {e}"), "Read the file first.") };
    let count = text.matches(old).count();
    if count != 1 { return ToolResult::error(format!("old_string matched {count} times in {rel}"), "Provide a unique exact string."); }
    let updated = text.replacen(old, new, 1);
    if let Err(e) = fs::write(&path, &updated) { return ToolResult::error(format!("cannot write {rel}: {e}"), "Check permissions."); }
    ToolResult::ok(format!("patched {rel} ({} -> {} bytes)", text.len(), updated.len()), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::RunMode;
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn symlink_write_escape_is_rejected() {
        let root = std::env::temp_dir().join(format!("tinyloop-fs-{}", crate::audit::now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let outside = root
            .parent()
            .unwrap()
            .join(format!("tinyloop-fs-outside-{}.txt", crate::audit::now_ms()));
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link.txt")).unwrap();

        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = apply_patch(&ctx, &json!({"path": "link.txt", "old_string": "secret", "new_string": "pwned"}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("escapes"));
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
    }

    #[test]
    fn existing_file_inside_the_workspace_is_writable() {
        let root = std::env::temp_dir().join(format!("tinyloop-fs-ok-{}", crate::audit::now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "old").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = apply_patch(&ctx, &json!({"path": "a.txt", "old_string": "old", "new_string": "new"}));
        assert_eq!(result.status, "success");
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "new");
    }
}
