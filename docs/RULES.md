# RULES.md — tinyloop Engineering Standards

All contributors and agents working on `tinyloop` must follow these rules.

---

## 1. Coding Standards

1. **Core / UI Separation**:
   - All agent loop logic, context management, sandbox checks, tool schemas, and verification belong in `tinyloop_core` (`src/`).
   - `desktop/src-tauri/` and `src/bin/` are thin shells over `tinyloop_core`. Never duplicate loop, budget, or validation logic in TypeScript (`desktop/src/`).
2. **Rust Standards (Edition 2021)**:
   - `cargo clippy --all-targets -- -D warnings` must pass cleanly for both the root crate and `desktop/src-tauri/`.
   - Prefer small, single-purpose functions and explicit `Result` / `ToolResult::error` returns; never `.unwrap()` on untrusted model output, filesystem reads, or subprocess I/O outside `#[cfg(test)]`.
   - Subprocess pipes (`stdout` and `stderr`) must always be drained concurrently on reader threads with bounded memory capture (`MAX_CAPTURE_BYTES`) so child processes never deadlock on full OS pipe buffers.
   - Preserve frozen model request settings (`tools` order/bytes, `enable_thinking: false`, `parallel_tool_calls: false`, vendor sampling in the loop, `PROBE_TEMPERATURE = 0.0` in startup probe, and `reasoning_content` in assistant history).
3. **Reuse & Provenance (`docs/REUSE_REGISTER.md`)**:
   - Reuse `src/tools/observation.rs` and `src/loop_engine/repeat_guard.rs` instead of reimplementing truncation or repeat-detection logic.
   - Never pull TypeScript files from `vendor/` into builds; keep upstream licenses intact and cite adaptations in module doc comments.
4. **Desktop UI Standards (`desktop/`)**:
   - `pnpm build` (`tsc --noEmit && vite build`) and `pnpm test` (`vitest run`) must pass cleanly.
   - Keep CSS tokens in `desktop/src/styles.css`; respect `prefers-reduced-motion` and read-only Clarify contract rendering in the inspector.

---

## 2. Naming Conventions

- **Rust modules, files, functions, fields**: `snake_case` (`fs_tools.rs`, `finish_with_artifact`, `masked_turns`).
- **Rust structs, enums, traits**: `PascalCase` (`ContextState`, `ContextOutcome`, `RunReport`, `ChatTransport`).
- **Rust constants**: `SCREAMING_SNAKE_CASE` (`DEFAULT_CONTEXT_TOKENS`, `MASK_AT`, `COMPACT_AT`, `KEEP_RECENT`, `KEEP_RECENT_TURNS`).
- **Tool names & serialized JSON keys**: `snake_case` (`submit_plan`, `read_file`, `search_files`, `run_command`, `apply_patch`, `finish`, `escalate`; `stdout_excerpt`, `next_actions`, `exit_code`).
- **Tauri IPC commands & events**: `snake_case` (`run_task`, `context_budget`, `cancel_run`, `run_event`, `approval_required`).
- **Eval task files**: `<tier>-<NN>-<codename>.toml` (`smoke-01-alpha.toml`, `full-06-foxtrot.toml`).
- **Git commits**: Conventional Commits (`feat:`, `fix:`, `test:`, `docs:`, `merge:`), optionally scoped (`feat(context): ...`, `feat(tools): ...`).

---

## 3. Testing Rules

1. **Core Unit & Engine Tests**:
   - Live inside `#[cfg(test)] mod tests` in each module (`src/context.rs`, `src/tools/*.rs`, `src/model/mod.rs`, `src/loop_engine/*.rs`).
   - Use deterministic fakes (`FakeTransport`, `FakeVerifier`, `FixedApprover`, `CountingVerifier`) and isolated temp workspaces (`std::env::temp_dir().join(...)`).
   - Every safety, protocol, or context invariant fix must include targeted regression tests (empty/malformed input, boundary values, tool-pair integrity, symlink/path escapes, timeout/pipe saturation).
2. **Desktop Tests**:
   - `desktop/src/App.test.tsx` tests preview/UI behavior; `desktop/src/App.tauri.test.tsx` mocks `@tauri-apps/api/core` and `@tauri-apps/api/event` to verify approval binding, cancel, error surfacing, and `run_task` payload wiring.
3. **Verification Gate**:
   - Never report tests or builds as passing unless executed after the final edit with all assertions passing and zero skipped/relaxed checks:
     - Rust core: `cargo test --lib` and `cargo clippy --all-targets -- -D warnings`
     - Desktop: `cd desktop && pnpm test && pnpm build`
4. **Model Evaluation (`src/bin/eval.rs`)**:
   - Grade final sandbox state with an external `CommandVerifier`, never the model's prose. Track tool-call validity separately from task pass rate.

---

## 4. "Never Do" List

- **Never** let the model declare `Done` or alter `done_condition` / `boundaries` / `verify_command`.
- **Never** execute a tool call when `finish_reason == Some("length")` or when arguments contain `__unparsed`.
- **Never** allow `apply_patch` or `run_command` in `Ask` mode without an explicit `ApprovalDecision::Allow` bound to the exact SHA-256 `approval_hash`.
- **Never** follow symlinks when writing (`apply_patch`) or accept paths that canonicalize outside `ToolContext::root`.
- **Never** allow destructive `git` subcommands (`push`, `reset`, `clean`, `rebase`, `filter-branch`, `restore`, `commit`, `config`, `remote`, `checkout --`), shell metacharacters, or unscoped interpreter inline code by default.
- **Never** insert a second `role: "system"` message into `messages` or split an `(assistant(tool_calls) + tool)` pair across a `KEEP_RECENT` compaction boundary.
- **Never** trigger compaction from naive `chars / 4` estimates when `reported_prompt_tokens > 0`.
- **Never** send `response_format` JSON schemas alongside `tools` to Spark-X2.6-4B on `llama-server`, or pass `--chat-template` / `--swa-full`.
- **Never** fabricate file paths, symbol behavior, test counts, or benchmark numbers in code, comments, or `/docs`.
