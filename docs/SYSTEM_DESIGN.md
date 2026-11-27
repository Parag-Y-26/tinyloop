# SYSTEM_DESIGN.md — tinyloop

**Scope:** Storage formats, HTTP and IPC contracts, approval/authorization model, resource scaling limits, security controls, and error recovery implemented in `tinyloop_core` and `desktop/src-tauri`.

---

## 1. Storage & Persistence Schema (File-Based)

`tinyloop` uses no external database. All runtime artifacts are written inside `<workspace>/.tinyloop/` (excluded from Git by `.gitignore`).

### 1.1 Compacted State (`.tinyloop/state.md`)

Written by `context::write_state` (`src/context.rs`) from `ContextState::render()`:

- **Fields**: `Goal`, `Done when`, `Must not` (boundaries joined by `; `), `Next step`, `Compactions` (incremented **before** `write_state` and `state.render()`), and `## Facts` (deduplicated FIFO capped at the newest `16` `<tool>: <summary>` observations).

### 1.2 Append-Only Audit Log (`.tinyloop/audit.jsonl`)

Written by `AuditLog::append` (`src/audit.rs`). One JSON object per line:

```json
{
  "ts_ms": 1795768583000,
  "run_id": "run-1234-1795768583000",
  "kind": "tool_call",
  "detail": { "...": "..." }
}
```

- **Emitted `kind` values (`src/loop_engine/engine.rs`)**: `plan`, `tool_call`, `tool_result`, `verify`, `finished`, `escalated`, `repair`, `approval_required`, `observation_mask`, `compaction`, `prefix_reset`, `report`.
- **Phase 2 audit payloads**:
  - `observation_mask` (from `RunEvent::Masked`): `{"masked_turns": usize, "before_tokens": usize, "after_tokens": usize}`.
  - `compaction` (from `RunEvent::Compacted`): `{"before_tokens": usize, "after_tokens": usize}`.
  - `prefix_reset`: logged whenever compaction mutates transcript history with `{"reason": "compaction", "before_tokens": usize, "after_tokens": usize, "compactions": usize}`.
  - `report` (from `finalize_run`): includes `status`, `steps`, `prompt_tokens`, `completion_tokens`, `cache_hits`, `prompt_ms`, `predicted_ms`, `tool_errors`, `malformed_repairs`, `masked_turns`, and `compactions`.

### 1.3 Spilled Tool Artifacts & Sparse `ToolResult` (`.tinyloop/artifacts/tool-<ts_ms>.txt`)

Written by `tools::finish_with_artifact` (`src/tools/mod.rs`) whenever `stdout.chars().count() > budget.max_chars`, where `budget = observation::resolve_tool_result_budget(ctx.context_tokens)`. The transcript retains only the head+tail excerpt plus the artifact file path.

- `ToolResult` derives `Serialize` and `Deserialize` (with `#[serde(default)]`) and omits empty/null optional fields (`next_actions`, `artifacts`, `exit_code`, `stdout_excerpt`) via `#[serde(skip_serializing_if = ...)]`.
- `shell::run_command` converts empty output to `"command produced no output"` via `output_or_marker`, keeping `stdout_excerpt` serialized for silent commands while truly empty excerpts (`apply_patch`, `ToolResult::error`) are omitted.

### 1.4 Evaluation Task & Output Schemas (`tests/eval/`, `src/bin/eval.rs`)

- **Task definition (`tests/eval/tasks/*.toml`)**: `id`, `goal`, `done_condition`, `boundaries` (`Vec<String>`), `[[fixture]]` (`path`, `content`), `[verify]` (`command`). Template provenance: `tests/eval/task.toml` (adapted from `vendor/terminal-bench/docs/task-template.toml`).
- **Eval output (`eval-results.json`)**: `{ "results": [TaskResult...], "passed": usize, "total": usize }`.

---

## 2. APIs & Communication Contracts

### 2.1 Model Server HTTP API (`src/model/llama_cpp.rs`, `src/model/mod.rs`)

`LlamaCppClient` talks to local `llama-server` (default `http://127.0.0.1:9941`, `connect_timeout = 5s`, `timeout = 180s`):

| Endpoint | Purpose | Payload / Validation |
|---|---|---|
| `GET /health` | Readiness check | HTTP 2xx with empty body or `"ok"` / `"status"` JSON |
| `POST /v1/chat/completions` | Inference & capability probe | `ChatRequest`: `model`, `messages`, `tools`, `tool_choice: "auto"`, `temperature: 1.0`, `top_p: 0.95`, `top_k: 0`, `max_tokens: 512`, `parallel_tool_calls: false`, `chat_template_kwargs: { "enable_thinking": false }` |

- `ChatResponse` parses `choices[0].message.{content, tool_calls, reasoning_content}`, `finish_reason`, `usage.{prompt_tokens, completion_tokens, prompt_tokens_details.cached_tokens}`, and `timings.{cache_n, prompt_ms, predicted_ms}`.
- `FunctionCall` custom deserializer (`deserialize_arguments`) accepts both JSON objects and stringified JSON objects; unparseable strings become `{"__unparsed": raw}` for bounded repair rather than crashing deserialization.
- Startup capability probe (`run_startup_probe`) forces `PROBE_TEMPERATURE = 0.0`, `max_tokens = 128`, asserts `with.usage.prompt_tokens > without.usage.prompt_tokens`, and requires a parsed native `read_file` tool call.

### 2.2 Core Rust Library API (`src/lib.rs`, `src/loop_engine/engine.rs`, `src/context.rs`)

`run_agent<T: ChatTransport, V: Verifier, F: FnMut(&RunEvent)>(transport, config, ctx, verifier, approver, emit) -> Result<RunReport, ModelError>`:
- Traits: `ChatTransport` (`health`, `chat`), `Verifier` (`verify -> VerifyOutcome`), `Approver` (`request -> ApprovalDecision`).
- `RunConfig` owns `model`, `task`, `done_condition`, `boundaries`, `verify_command`, `max_steps` (default `12`), `context_tokens` (default `8192`, min `512`), `cancel: Arc<AtomicBool>`, `verify_on_write` (default `false`).
- `run_agent` clones `ctx` with `.with_context_tokens(config.context_tokens)` so `ToolContext::context_tokens` always matches the active `RunConfig` window.
- `RunEvent` variants: `Plan`, `ToolCall`, `ToolResult`, `Verify`, `Finished`, `Escalated`, `TextOnlyRepair`, `ApprovalRequired`, `Masked { masked_turns, before_tokens, after_tokens }`, `Compacted { before_tokens, after_tokens }`.
- `RunReport` fields: `status`, `steps`, `prompt_tokens`, `completion_tokens`, `cache_hits`, `prompt_ms`, `predicted_ms`, `tool_errors`, `malformed_repairs`, `masked_turns`, `compactions`, `final_text`, `events`.
- Context engine contracts (`src/context.rs`, `src/loop_engine/engine.rs`):
  - `effective_prompt_tokens(messages, reported_prompt_tokens, new_turn_tokens)`: uses `pt + new_turn_tokens` when `reported_prompt_tokens` is `Some(pt)` with `pt > 0`; falls back to `estimate_messages(messages) + TOOL_SCHEMA_TOKENS` only when `None` or `Some(0)`.
  - `manage_context(...) -> ContextOutcome { masked_turns, mask_metrics, compact_metrics }` runs Tier 1 `mask_old_observations` above `75%` and Tier 2 `compact_with_metrics` above `85%` (or when `masked_turns == 0`, or when masking leaves estimated effective tokens `>= 75%`).
  - `active_observation_chars(&messages)` recomputes unmasked observation char load after Tier 1 masking when Tier 2 does not run; `over_observation_budget` (`observation_chars > observation_budget`) invokes `compact_with_metrics` directly and resets `observation_chars = 0`.

### 2.3 Tauri IPC Bridge (`desktop/src-tauri/src/main.rs`)

| Direction | Name | Contract |
|---|---|---|
| Command (UI → Core) | `context_budget` | Returns `resolve_tool_result_budget(DEFAULT_CONTEXT).max_chars` (`9828` at 8K) |
| Command (UI → Core) | `current_dir` | Returns process working directory string for default workspace input |
| Command (UI → Core) | `run_task` | Accepts `task`, `done_condition`, `boundaries`, `verify_command`, `cwd`, `mode`, `model`; runs `ensure_probe` + `run_agent` on `spawn_blocking` |
| Command (UI → Core) | `approve(id)` / `deny(id)` | Resolves pending `mpsc::Sender<ApprovalDecision>` in `ApprovalRegistry` |
| Command (UI → Core) | `cancel_run` | Sets `AppState::cancel` (`AtomicBool`) and calls `approvals.deny_all()` to unblock waiting gates immediately |
| Event (Core → UI) | `run_event` | Streams `UiEvent { kind, title, detail, status, meta }` (`probe`, `plan`, `tool_call`, `tool_result`, `verify`, `done`, `escalated`, `repair`, `approval`, `observation_mask`, `compaction`, `max_steps`) |
| Event (Core → UI) | `approval_required` | Sends `ApprovalRequest { id, tool, args, hash }` |

---

## 3. Auth, Permissions & Approval Gates

- **Network / Server Auth**: None required; `llama-server` binds strictly to loopback (`127.0.0.1:9941`).
- **Tauri Capability ACL (`desktop/src-tauri/capabilities/default.json`)**: Window-bound (`"windows": ["main"]`) with `["core:default", "core:window:allow-start-dragging"]` so event emission/listening works cleanly on the main window without broad wildcard ACLs.
- **Execution Approval Gate**:
  - `RunMode::DryRun`: rejects `apply_patch` and `run_command` inside the tools layer.
  - `RunMode::Ask`: `apply_patch` and `run_command` emit `ApprovalRequired` with `hash = sha256(tool + "\0" + canonical_json(args))` (`approval_hash`). `TauriApprover` blocks on an `mpsc` channel up to `APPROVAL_TIMEOUT = 300s` (polling `cancel` every `250ms`). Missing approver, channel disconnect, timeout, or cancel returns `ApprovalDecision::Deny`.
  - `RunMode::Auto`: executes allowlisted tools without interactive prompt (used by CLI `run`/`eval` and explicit Auto mode).

---

## 4. Scaling & Resource Bounds

| Bound | Value / Formula | Enforced In |
|---|---|---|
| Context window | `8192` tokens default (`-c 8192 -np 1`, min `512`) | `src/model/mod.rs`, `src/context.rs`, `src/loop_engine/engine.rs` |
| Generation cap | `DEFAULT_MAX_TOKENS = 512` (`128` for probe) | `src/model/mod.rs` |
| Step cap | `12` (`RunConfig::default`), `8` (`src/bin/run.rs`), `6` (`src/bin/eval.rs`) | `src/loop_engine/engine.rs`, `src/bin/` |
| Observation per-result cap | `min(floor(ctx * 0.3) * 4, hard_cap)` → `9,828` chars at 8K (`16k`/`32k`/`64k` tiers) | `src/tools/observation.rs`, `src/tools/mod.rs` |
| Total observation budget | `max(1024, floor(ctx * 2 * 0.5))` → `8,192` chars at 8K; tracked via `observation_chars` & `active_observation_chars` | `src/tools/observation.rs`, `src/loop_engine/engine.rs` |
| Context thresholds | Tier 1 mask `75%` (`MASK_AT`), Tier 2 compact `85%` (`COMPACT_AT`), `KEEP_RECENT_TURNS = 2`, `KEEP_RECENT = 4`, `TOOL_SCHEMA_TOKENS = 1200`, `MASKED_EXCERPT_MARKER` | `src/context.rs` |
| Read & directory bounds | `read_file`: `DEFAULT_READ_LIMIT = 160`, `MAX_READ_LIMIT = 400`; dir listing skips `LIST_SKIP_DIRS` (`.git`, `target`, `node_modules`, `.tinyloop`), caps at `MAX_DIR_ENTRIES = 100` | `src/tools/fs_tools.rs` |
| Search bounds | `ignore::WalkBuilder` (`.gitignore`, global/exclude, `hidden(true)`, `follow_links(false)`), optional `glob` (`OverrideBuilder`), `max_results` `1..=50` (default `50`), skips files `> 1,000,000` bytes (`MAX_SEARCH_FILE_BYTES`) and binary files (`BINARY_SNIFF_BYTES = 8 KiB`) | `src/tools/fs_tools.rs` |
| Repo map bounds | `REPO_MAP_BUDGET_CHARS = 1600`, `MAX_FILE_BYTES = 64 KiB`, `MAX_DEFS_PER_FILE = 6`, skips hidden/symlinks/`SKIP_DIRS` (`.git`, `target`, `node_modules`, `dist`, `.tinyloop`, `vendor`); injected once when `files > 3` | `src/tools/repo_map.rs`, `src/loop_engine/engine.rs` |
| Subprocess capture & timeout | `MAX_CAPTURE_BYTES = 2 MiB` per stream (pipe drained past cap to prevent deadlock); `30s` default shell timeout, `MAX_TIMEOUT_SECS = 120s`; verifier timeout `120s`, output cap `4000` chars | `src/tools/shell.rs`, `src/loop_engine/engine.rs` |

---

## 5. Security Controls

1. **Filesystem Confinement (`src/tools/fs_tools.rs`, `src/tools/repo_map.rs`)**:
   - `resolve_existing` canonicalizes both `ctx.root` and target and rejects any path whose canonical prefix is outside `root_canon`.
   - `resolve_for_write` checks `std::fs::symlink_metadata` before canonicalization and rejects **any** symlink target (even symlinks pointing inside the workspace) so writes cannot race or follow untrusted links.
   - `resolve_for_create` (used by `apply_patch` when `old_string == ""`) enforces `ensure_workspace_relative`, walks up to the nearest existing ancestor to ensure no symlinked ancestor redirects creation outside `root_canon`, creates parent dirs, and re-verifies `parent_canon.starts_with(&root_canon)`.
   - `repo_map::collect_source_files` skips symlinks (`file_type.is_symlink()`) so map generation never follows links outside the workspace.
2. **Subprocess Policy (`src/tools/shell.rs`)**:
   - Rejects shell metacharacters (`|`, `&`, `;`, `<`, `>`, `$`, `` ` ``, `\n`, `\r`) — no shell interpreter (`sh`/`cmd`) is spawned by `run_command`.
   - Parses arguments with `split_command` (preserving single/double quotes and Windows backslashes; rejecting unmatched quotes).
   - Allowlists binary basename: `cargo`, `git`, `rg`, `pnpm`, `npm`, `node`, `python`, `python3`.
   - Blocks destructive `git` subcommands (`push`, `reset`, `clean`, `rebase`, `filter-branch`, `restore`, `commit`, `config`, `remote`, `checkout --`) and repo-escaping flags (`-C`, `--git-dir`, `--work-tree`, `-c`).
   - Blocks `python -m http.server`, `python -` (stdin), `python -c` (unless `ToolContext::allow_inline_code == true`), and inline `node` flags (`-e`, `--eval`, `-p`, `--print`, `-`); script arguments for `python`/`node` must canonicalize inside `ctx.root`.
   - Scrubs environment via `cmd.env_clear()`, passing only `PATH`, `SystemRoot`, `TEMP`, `TMP`, `USERPROFILE`, `HOME`, `COMSPEC`, `PATHEXT`.
3. **Untrusted Data Separation**: Tool output is wrapped inside structured `ToolResult` JSON, never interpreted as agent instructions, and the model cannot modify `done_condition`, `boundaries`, or `verify_command`.

---

## 6. Error Handling & Bounded Recovery

- **Model / Transport Errors (`ModelError`)**: Surfaced cleanly as `Err(ModelError)` (`Health`, `Transport`, `Http`, `Parse`, `ToolsNotVisible`, `ToolCallFailed`) without panicking.
- **In-Loop Recovery (`src/loop_engine/engine.rs`)**:
  - **No tool call (text-only)**: emits `RunEvent::TextOnlyRepair`, nudges once per turn, escalates on 3rd occurrence (`text_only_repairs >= 3`).
  - **Output-limit truncated tool call (`finish_reason == Some("length")`)**: never executed; emits `TextOnlyRepair` and prompts a smaller call.
  - **Malformed tool JSON (`__unparsed`)**: emits `TextOnlyRepair`, grants 1 retry, escalates on 2nd (`malformed_repairs > 1`).
  - **Invalid plan (`submit_plan`)**: must contain `3..=6` steps using known tools; grants 1 repair, escalates on 2nd (`plan_repairs > 1`).
  - **Repeat streak**: `repeat_guard::observe` appends a system note to `ToolResult.next_actions` on the 3rd identical call and escalates (`TOOL_REPEAT_LIMIT_CODE`) on the 4th.
  - **Verifier failure**: returns `ToolResult::error("verifier failed", <truncated output>)`; escalates after 3 failures (`verify_failures >= 3`).
  - **Tool errors**: returned as structured `ToolResult { status: "error", summary, next_actions, ... }` (including whitespace-normalized line hints on `apply_patch` 0-match misses and valid line-range hints on out-of-range `read_file` offsets) so the model receives actionable recovery hints instead of crashes.
