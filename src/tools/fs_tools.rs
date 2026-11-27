use super::{finish_with_artifact, RunMode, ToolContext, ToolResult};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use serde_json::Value;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Directories that are never useful navigation targets.
const LIST_SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".tinyloop"];
const MAX_DIR_ENTRIES: usize = 100;
const MAX_SEARCH_FILE_BYTES: usize = 1_000_000;
const BINARY_SNIFF_BYTES: usize = 8 * 1024;
const DEFAULT_READ_LIMIT: usize = 160;
const MAX_READ_LIMIT: usize = 400;

fn resolve_existing(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let joined = root.join(rel);
    let canon = joined.canonicalize().map_err(|e| format!("cannot resolve {rel}: {e}"))?;
    let root_canon = root.canonicalize().map_err(|e| format!("cannot resolve workspace: {e}"))?;
    if !canon.starts_with(&root_canon) { return Err(format!("path escapes workspace: {rel}")); }
    Ok(canon)
}

fn ensure_workspace_relative(rel: &str) -> Result<(), String> {
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(format!("path must be workspace-relative: {rel}"));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err(format!("path escapes workspace: {rel}")),
        }
    }
    Ok(())
}

fn resolve_for_write(root: &Path, rel: &str) -> Result<PathBuf, String> {
    ensure_workspace_relative(rel)?;
    let root_canon = root.canonicalize().map_err(|e| format!("cannot resolve workspace: {e}"))?;
    let joined = root.join(rel);
    // `symlink_metadata` does not follow links, so it also sees broken links
    // that `exists()` misses.
    match fs::symlink_metadata(&joined) {
        Ok(meta) => {
            // Never write through a symlink: the link target is outside the
            // caller's control even when it currently resolves inside.
            if meta.file_type().is_symlink() {
                return Err(format!("path escapes workspace: {rel} is a symlink"));
            }
            let canon = joined.canonicalize().map_err(|e| format!("cannot resolve {rel}: {e}"))?;
            if !canon.starts_with(&root_canon) { return Err(format!("path escapes workspace: {rel}")); }
            Ok(canon)
        }
        Err(_) => {
            let parent = joined.parent().ok_or_else(|| "path has no parent".to_string())?;
            let parent_canon = parent.canonicalize().map_err(|e| format!("cannot resolve parent of {rel}: {e}"))?;
            if !parent_canon.starts_with(&root_canon) { return Err(format!("path escapes workspace: {rel}")); }
            let file_name = joined.file_name().ok_or_else(|| format!("{rel} has no file name"))?;
            Ok(parent_canon.join(file_name))
        }
    }
}

fn resolve_for_create(root: &Path, rel: &str) -> Result<PathBuf, String> {
    ensure_workspace_relative(rel)?;
    let root_canon = root.canonicalize().map_err(|e| format!("cannot resolve workspace: {e}"))?;
    let joined = root_canon.join(rel);
    let parent = joined.parent().ok_or_else(|| "path has no parent".to_string())?;
    // A symlinked ancestor must not redirect the new file outside the root.
    let mut ancestor = parent;
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or_else(|| format!("cannot resolve parent of {rel}"))?;
    }
    let ancestor_canon = ancestor.canonicalize().map_err(|e| format!("cannot resolve parent of {rel}: {e}"))?;
    if !ancestor_canon.starts_with(&root_canon) { return Err(format!("path escapes workspace: {rel}")); }
    fs::create_dir_all(parent).map_err(|e| format!("cannot create parent of {rel}: {e}"))?;
    let parent_canon = parent.canonicalize().map_err(|e| format!("cannot resolve parent of {rel}: {e}"))?;
    if !parent_canon.starts_with(&root_canon) { return Err(format!("path escapes workspace: {rel}")); }
    let file_name = joined.file_name().ok_or_else(|| format!("{rel} has no file name"))?;
    Ok(parent_canon.join(file_name))
}

fn read_directory(ctx: &ToolContext, rel: &str, path: &Path) -> ToolResult {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) => return ToolResult::error(format!("cannot list {rel}: {e}"), "Check the path."),
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if LIST_SKIP_DIRS.contains(&name.as_str()) { continue; }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        names.push(if is_dir { format!("{name}/") } else { name });
    }
    names.sort();
    let total = names.len();
    let mut body = format!("[Dir: {rel} ({total} entries)]\n");
    for name in names.iter().take(MAX_DIR_ENTRIES) {
        body.push_str(name);
        body.push('\n');
    }
    if total > MAX_DIR_ENTRIES {
        body.push_str(&format!("… ({} more)\n", total - MAX_DIR_ENTRIES));
    }
    finish_with_artifact(ctx, "success", format!("listed {rel} ({total} entries)"), body, Some(0))
}

pub fn read_file(ctx: &ToolContext, args: &Value) -> ToolResult {
    let rel = match args.get("path").and_then(|v| v.as_str()) { Some(p) => p, None => return ToolResult::error("missing path", "Pass a workspace-relative path.") };
    let path = match resolve_existing(&ctx.root, rel) { Ok(p) => p, Err(e) => return ToolResult::error(e, "Use a path inside the workspace.") };
    if path.is_dir() {
        return read_directory(ctx, rel, &path);
    }
    let text = match fs::read_to_string(&path) { Ok(t) => t, Err(e) => return ToolResult::error(format!("cannot read {rel}: {e}"), "Check the path and that the file is UTF-8.") };
    let total = text.lines().count();
    if total == 0 {
        let body = format!("[File: {rel} (empty)]\n");
        return finish_with_artifact(ctx, "success", format!("read {rel} (empty)"), body, Some(0));
    }
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_READ_LIMIT as u64)
        .clamp(1, MAX_READ_LIMIT as u64) as usize;
    if offset >= total {
        return ToolResult::error(
            format!("offset {offset} is past the end of {rel}"),
            format!("{rel} has {total} lines; valid lines are 1-{total} (offset 0..{})", total.saturating_sub(1)),
        );
    }
    let selected: Vec<&str> = text.lines().skip(offset).take(limit).collect();
    let start = offset + 1;
    let end = offset + selected.len();
    let mut body = format!("[File: {rel} ({total} lines), showing lines {start}-{end}]\n");
    for (index, line) in selected.iter().enumerate() {
        body.push_str(&format!("{}: {line}\n", offset + index + 1));
    }
    finish_with_artifact(ctx, "success", format!("read {rel} ({total} lines)"), body, Some(0))
}

pub fn search_files(ctx: &ToolContext, args: &Value) -> ToolResult {
    let pattern = match args.get("pattern").and_then(|v| v.as_str()) { Some(p) if !p.is_empty() => p, _ => return ToolResult::error("missing pattern", "Pass a non-empty literal string.") };
    let start_rel = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let start = match resolve_existing(&ctx.root, start_rel) { Ok(p) => p, Err(e) => return ToolResult::error(e, "Use a path inside the workspace.") };
    let max = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50).clamp(1, 50) as usize;
    let glob = args.get("glob").and_then(|v| v.as_str()).map(str::trim).filter(|g| !g.is_empty());
    let needle = pattern.to_lowercase();

    let mut builder = WalkBuilder::new(&start);
    builder
        .hidden(true)
        .parents(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false);
    if let Some(glob) = glob {
        let mut overrides = OverrideBuilder::new(&ctx.root);
        if let Err(e) = overrides.add(glob) {
            return ToolResult::error(format!("invalid glob {glob:?}: {e}"), "Use a gitignore-style glob, e.g. \"*.rs\".");
        }
        match overrides.build() {
            Ok(overrides) => { builder.overrides(overrides); }
            Err(e) => return ToolResult::error(format!("invalid glob {glob:?}: {e}"), "Use a gitignore-style glob, e.g. \"*.rs\"."),
        }
    }

    let mut files: Vec<PathBuf> = builder
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|file_type| file_type.is_file()))
        .map(|entry| entry.into_path())
        .collect();
    files.sort();

    let root_canon = ctx.root.canonicalize().unwrap_or_else(|_| ctx.root.clone());
    let mut hits: Vec<String> = Vec::new();
    for path in files {
        if hits.len() >= max { break; }
        let Ok(bytes) = fs::read(&path) else { continue };
        if bytes.len() > MAX_SEARCH_FILE_BYTES { continue; }
        if bytes.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0) { continue; }
        let Ok(text) = String::from_utf8(bytes) else { continue };
        let rel_path = path
            .strip_prefix(&root_canon)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        for (index, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                hits.push(format!("{rel_path}:{}: {}", index + 1, line.trim_end()));
                if hits.len() >= max { break; }
            }
        }
    }
    let body = if hits.is_empty() { format!("No matches for {pattern:?}.") } else { hits.join("\n") };
    finish_with_artifact(ctx, "success", format!("{} matches for {pattern:?}", hits.len()), body, Some(0))
}

pub fn apply_patch(ctx: &ToolContext, args: &Value) -> ToolResult {
    if ctx.mode == RunMode::DryRun { return ToolResult::error("dry-run: patch not applied", "Re-run in ask/auto mode to apply."); }
    let rel = match args.get("path").and_then(|v| v.as_str()) { Some(p) => p, None => return ToolResult::error("missing path", "Pass a workspace-relative path.") };
    let old = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolResult::error("missing old_string", "Pass \"\" to create a new file."),
    };
    let new = args.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
    if old.is_empty() {
        return create_file(ctx, rel, new);
    }
    let path = match resolve_for_write(&ctx.root, rel) { Ok(p) => p, Err(e) => return ToolResult::error(e, "Use a path inside the workspace.") };
    let text = match fs::read_to_string(&path) { Ok(t) => t, Err(e) => return ToolResult::error(format!("cannot read {rel}: {e}"), "Read the file first.") };
    let count = text.matches(old).count();
    if count == 0 {
        let hint = match normalized_match_line(&text, old) {
            Some(line) => format!("a whitespace-normalized match exists at line {line}; copy the file's exact indentation and spacing"),
            None => "read the file again and copy an exact substring for old_string".to_string(),
        };
        return ToolResult::error(format!("old_string matched 0 times in {rel}"), hint);
    }
    if count > 1 {
        return ToolResult::error(format!("old_string matched {count} times in {rel}"), "Include more surrounding lines so the match is unique.");
    }
    let updated = text.replacen(old, new, 1);
    if let Err(e) = fs::write(&path, &updated) { return ToolResult::error(format!("cannot write {rel}: {e}"), "Check permissions."); }
    ToolResult::ok(format!("patched {rel} ({} -> {} bytes)", text.len(), updated.len()), String::new())
}

fn create_file(ctx: &ToolContext, rel: &str, contents: &str) -> ToolResult {
    let target = ctx.root.join(rel);
    if fs::symlink_metadata(&target).is_ok() {
        return ToolResult::error(format!("file already exists: {rel}"), "Pass a unique old_string to patch an existing file.");
    }
    let path = match resolve_for_create(&ctx.root, rel) { Ok(p) => p, Err(e) => return ToolResult::error(e, "Use a path inside the workspace.") };
    if let Err(e) = fs::write(&path, contents) { return ToolResult::error(format!("cannot create {rel}: {e}"), "Check permissions."); }
    ToolResult::ok(format!("created {rel} ({} bytes)", contents.len()), String::new())
}

/// Find where `old` would match if indentation and blank lines were ignored,
/// so a small model can correct a whitespace-only mismatch in one step.
fn normalized_match_line(text: &str, old: &str) -> Option<usize> {
    let file_lines: Vec<&str> = text.lines().map(str::trim).collect();
    let needle: Vec<&str> = old.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if needle.is_empty() {
        return None;
    }
    if needle.len() == 1 {
        return file_lines.iter().position(|line| *line == needle[0]).map(|index| index + 1);
    }
    for start in 0..file_lines.len() {
        if file_lines[start] != needle[0] { continue; }
        let mut matched = 1;
        let mut cursor = start + 1;
        while matched < needle.len() && cursor < file_lines.len() {
            if file_lines[cursor].is_empty() {
                cursor += 1;
                continue;
            }
            if file_lines[cursor] != needle[matched] { break; }
            matched += 1;
            cursor += 1;
        }
        if matched == needle.len() {
            return Some(start + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::RunMode;
    use serde_json::json;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tinyloop-fs-{name}-{}", crate::audit::now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Symlink creation needs Developer Mode on Windows; tests that depend on
    /// it skip when the OS refuses.
    fn try_symlink_file(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

    #[test]
    fn read_file_lists_directories_sorted_and_skips_build_dirs() {
        let root = temp_root("dir");
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::write(root.join("src/b.rs"), "").unwrap();
        fs::write(root.join("src/a.rs"), "").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = read_file(&ctx, &json!({"path": "src"}));
        assert_eq!(result.status, "success");
        assert!(result.stdout_excerpt.starts_with("[Dir: src (3 entries)]"), "{}", result.stdout_excerpt);
        let body = &result.stdout_excerpt;
        assert!(body.contains("a.rs\nb.rs\nsub/\n"), "{}", body);
        assert!(!body.contains("target"));
    }

    #[test]
    fn read_file_adds_aci_header_and_reports_the_shown_range() {
        let root = temp_root("header");
        fs::write(root.join("f.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = read_file(&ctx, &json!({"path": "f.txt", "offset": 1, "limit": 2}));
        assert_eq!(result.status, "success");
        assert!(result.stdout_excerpt.starts_with("[File: f.txt (5 lines), showing lines 2-3]"), "{}", result.stdout_excerpt);
        assert!(result.stdout_excerpt.contains("2: two"));
        assert!(result.stdout_excerpt.contains("3: three"));
        assert!(!result.stdout_excerpt.contains("4: four"));
    }

    #[test]
    fn read_file_handles_empty_and_out_of_range_offsets() {
        let root = temp_root("offsets");
        fs::write(root.join("empty.txt"), "").unwrap();
        fs::write(root.join("f.txt"), "a\nb\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let empty = read_file(&ctx, &json!({"path": "empty.txt"}));
        assert_eq!(empty.status, "success");
        assert!(empty.stdout_excerpt.contains("[File: empty.txt (empty)]"));
        let past = read_file(&ctx, &json!({"path": "f.txt", "offset": 9}));
        assert_eq!(past.status, "error");
        assert!(past.next_actions[0].contains("valid lines are 1-2"), "{:?}", past.next_actions);
    }

    #[test]
    fn search_respects_gitignore_skips_binaries_and_returns_relative_paths() {
        let root = temp_root("search");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(root.join("src/a.rs"), "let needle = 1;\n").unwrap();
        fs::write(root.join("src/b.txt"), "needle here\n").unwrap();
        fs::write(root.join("ignored.txt"), "needle ignored\n").unwrap();
        fs::write(root.join("src/blob.dat"), b"needle\0binary\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = search_files(&ctx, &json!({"pattern": "needle"}));
        assert_eq!(result.status, "success");
        assert!(result.stdout_excerpt.contains("src/a.rs:1: let needle = 1;"), "{}", result.stdout_excerpt);
        assert!(result.stdout_excerpt.contains("src/b.txt:1: needle here"), "{}", result.stdout_excerpt);
        assert!(!result.stdout_excerpt.contains("ignored.txt"));
        assert!(!result.stdout_excerpt.contains("blob.dat"));
        assert!(!result.stdout_excerpt.contains(&root.display().to_string()));
    }

    #[test]
    fn search_glob_filters_results() {
        let root = temp_root("glob");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "needle\n").unwrap();
        fs::write(root.join("src/b.txt"), "needle\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = search_files(&ctx, &json!({"pattern": "needle", "glob": "*.rs"}));
        assert_eq!(result.status, "success");
        assert!(result.stdout_excerpt.contains("src/a.rs"), "{}", result.stdout_excerpt);
        assert!(!result.stdout_excerpt.contains("src/b.txt"), "{}", result.stdout_excerpt);
    }

    #[test]
    fn patch_creates_missing_files_and_parents() {
        let root = temp_root("create");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = apply_patch(&ctx, &json!({"path": "src/new/deep.txt", "old_string": "", "new_string": "hello"}));
        assert_eq!(result.status, "success", "{}", result.summary);
        assert!(result.summary.contains("created"));
        assert_eq!(fs::read_to_string(root.join("src/new/deep.txt")).unwrap(), "hello");
        let again = apply_patch(&ctx, &json!({"path": "src/new/deep.txt", "old_string": "", "new_string": "x"}));
        assert_eq!(again.status, "error");
        assert!(again.summary.contains("already exists"));
    }

    #[test]
    fn patch_create_cannot_escape_the_workspace() {
        let root = temp_root("create-escape");
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        for rel in ["../outside.txt", "/absolute.txt", "src/../../outside.txt"] {
            let result = apply_patch(&ctx, &json!({"path": rel, "old_string": "", "new_string": "x"}));
            assert_eq!(result.status, "error", "{rel} should be rejected");
        }
    }

    #[test]
    fn patch_mismatch_hints_at_a_whitespace_normalized_line() {
        let root = temp_root("mismatch");
        fs::write(root.join("f.rs"), "fn main() {\n    let x = 1;\n}\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        // Eight spaces of indentation vs the file's four: no exact match.
        let result = apply_patch(&ctx, &json!({"path": "f.rs", "old_string": "        let x = 1;", "new_string": "        let x = 2;"}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("matched 0 times"));
        assert!(result.next_actions[0].contains("line 2"), "{:?}", result.next_actions);
    }

    #[test]
    fn patch_rejects_ambiguous_and_missing_matches() {
        let root = temp_root("ambiguous");
        fs::write(root.join("f.txt"), "same\nsame\n").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let ambiguous = apply_patch(&ctx, &json!({"path": "f.txt", "old_string": "same", "new_string": "x"}));
        assert_eq!(ambiguous.status, "error");
        assert!(ambiguous.summary.contains("2 times"));
        let missing = apply_patch(&ctx, &json!({"path": "f.txt", "old_string": "absent", "new_string": "x"}));
        assert_eq!(missing.status, "error");
        assert!(missing.next_actions[0].contains("exact substring"));
    }

    #[test]
    fn symlink_escape_is_rejected_by_read_and_write() {
        let root = temp_root("link-outside");
        let outside = root
            .parent()
            .unwrap()
            .join(format!("tinyloop-fs-outside-{}.txt", crate::audit::now_ms()));
        std::fs::write(&outside, "secret").unwrap();
        let link = root.join("link.txt");
        if !try_symlink_file(&outside, &link) {
            eprintln!("skipping symlink escape test: link creation not permitted");
            return;
        }

        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let read = read_file(&ctx, &json!({"path": "link.txt"}));
        assert_eq!(read.status, "error");
        assert!(read.summary.contains("escapes"), "{}", read.summary);

        let write = apply_patch(&ctx, &json!({"path": "link.txt", "old_string": "secret", "new_string": "pwned"}));
        assert_eq!(write.status, "error");
        assert!(write.summary.contains("escapes"), "{}", write.summary);
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "secret");
    }

    #[test]
    fn writes_through_any_symlink_are_rejected() {
        let root = temp_root("link-inside");
        std::fs::write(root.join("a.txt"), "old").unwrap();
        let link = root.join("link.txt");
        if !try_symlink_file(&root.join("a.txt"), &link) {
            eprintln!("skipping symlink write test: link creation not permitted");
            return;
        }
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = apply_patch(&ctx, &json!({"path": "link.txt", "old_string": "old", "new_string": "new"}));
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("symlink"), "{}", result.summary);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "old");
    }

    #[test]
    fn existing_file_inside_the_workspace_is_writable() {
        let root = temp_root("ok");
        std::fs::write(root.join("a.txt"), "old").unwrap();
        let ctx = ToolContext::new(&root, root.join(".tinyloop"), RunMode::Auto);
        let result = apply_patch(&ctx, &json!({"path": "a.txt", "old_string": "old", "new_string": "new"}));
        assert_eq!(result.status, "success");
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "new");
    }
}
