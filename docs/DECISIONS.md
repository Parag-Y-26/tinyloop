# DECISIONS.md — tinyloop Architecture Decision Log

Significant architectural decisions recorded chronologically (What, Why, Date).

---

## Baseline Decisions (Phase 0 & Phase 1)

### DEC-001: Rust-Owned Bounded Loop + Native OpenAI `tools` Array (2026-11-25)
- **What**: Run a bounded `Clarify → Plan → Act → Observe → Verify` state machine in `tinyloop_core` (`src/loop_engine/engine.rs`), calling `llama-server --jinja` with native `tools`, `enable_thinking: false`, `parallel_tool_calls: false`, vendor sampling (`temperature: 1.0`, `top_p: 0.95`, `top_k: 0`), and a deterministic external `Verifier`. Startup probe runs at `PROBE_TEMPERATURE = 0.0` (`src/model/mod.rs`).
- **Why**: Small local models (`Spark-X2.6-4B`) fail with prompt-encoded action text, can spend all `max_tokens` inside `<think>`, and cannot reliably grade their own work. Separating action proposals from Rust-enforced verification and approval ensures deterministic termination.

### DEC-002: Tauri 2 + React 19 Desktop Shell Over `tinyloop_core` (2026-11-25)
- **What**: Ship a native desktop UI in `desktop/` backed directly by `tinyloop_core` (`src/lib.rs`), keeping `src/bin/{run,probe,eval}.rs` as thin headless wrappers. Bind Tauri default capabilities to `["main"]`.
- **Why**: Direct Rust linkage avoids duplicating loop, budget, or approval logic in TypeScript while giving operators live step timelines and SHA-256 argument-bound `Allow`/`Deny` cards (`desktop/src-tauri/src/main.rs`).

### DEC-003: Bounded Pipe Draining, Symlink Rejection & Command Policy (2026-11-27)
- **What**: Drain child `stdout`/`stderr` concurrently on reader threads with a `2 MiB` per-stream capture cap (`src/tools/shell.rs`), reject writes through any symlink (`src/tools/fs_tools.rs`), reject unmatched command quotes and shell metacharacters, block destructive `git` subcommands and default inline `python -c` / `node -e`, and emit `"command produced no output"` for silent runs (`commits f0c4969`, `e957c12`).
- **Why**: Awaiting exit before draining deadlocked on full OS pipe buffers (>4 KiB Windows / >64 KiB Linux); symlinks can escape the workspace; explicit no-output markers stop models from endlessly re-running silent commands.

---

## Phase 2 Decisions — Context Management & Token Efficiency (2026-11-27)

### DEC-004: Two-Tier Context Management — 75% Tier 1 Block Observation Masking Before 85% Tier 2 Compaction
- **What**: Introduce `MASK_AT = 0.75` and `KEEP_RECENT_TURNS = 2` in `src/context.rs` alongside `COMPACT_AT = 0.85` (`KEEP_RECENT = 4`). When effective prompt tokens exceed 75% (up to 85%), `manage_context` runs a single-batch Tier 1 pass (`mask_old_observations`) that preserves the last 2 atomic turns verbatim and replaces `stdout_excerpt` in older `role: "tool"` messages with `"[omitted — see summary and artifact path]"` while keeping `status`, `summary`, `artifacts`, `next_actions`, `exit_code`, and `truncated` intact. Tier 2 full compaction runs only when tokens already exceed 85%, when `masked_turns == 0`, or when Tier 1 fails to bring estimated tokens below 75%. Track `masked_turns` and `compactions` in `RunReport`.
- **Why**: Full compaction breaks a larger prompt-cache prefix and discards older turn structure. Masking stale raw `stdout_excerpt` payloads in one batch (rather than one turn at a time, which would invalidate KV cache on every turn) reclaims token headroom while keeping error status, one-line summaries, and `.tinyloop/artifacts/` pointers available to the model.

### DEC-005: Drive Thresholds from Real `usage.prompt_tokens + new_turn_tokens` Delta (Fallback Only When `== 0`)
- **What**: Implement `effective_prompt_tokens(messages, reported_prompt_tokens, new_turn_tokens)` in `src/context.rs`. When `reported_prompt_tokens` is `Some(pt)` with `pt > 0`, compute `(pt as usize).saturating_add(new_turn_tokens)` and ignore the heuristic `chars / 4` count. Fall back to `estimate_messages(messages).saturating_add(TOOL_SCHEMA_TOKENS)` (`1200`) **only** when `reported_prompt_tokens` is `None` or `Some(0)`.
- **Why**: The prior `estimated > threshold || reported > threshold` check allowed naive `chars / 4` estimates plus hardcoded schema overhead to trigger compaction prematurely even when `llama-server` had already reported real token usage below the threshold. Using server `prompt_tokens` plus only the new turn's delta gives accurate budget tracking without double-counting schemas.

### DEC-006: User-Role State Recap & Atomic Tool-Pair Tail Slicing
- **What**: Never insert a second `role: "system"` message during compaction; keep `messages[0]` untouched and insert the deterministic state recap as a `role: "user"` message (`[Compacted state — prior turns persisted to .tinyloop/state.md]\n...`). When slicing `KEEP_RECENT`, walk backward in atomic `(assistant(tool_calls) + tool ...)` turn units so the kept tail always starts on a valid turn boundary and never orphans a `role: "tool"` message.
- **Why**: Spark2.5 and OpenAI-compatible Jinja chat templates reject transcripts with multiple `system` messages or orphaned `tool` messages missing their owning assistant `tool_calls`. Keeping `messages[0]` byte-identical also preserves the frozen leading prompt-cache prefix.

### DEC-007: Increment `state.compactions` Before `write_state` and Log `prefix_reset` in `audit.jsonl`
- **What**: Increment `state.compactions += 1` **before** calling `write_state(root, state)` and `state.render()` in `compact()`, and log a `prefix_reset` audit entry (with reason such as `"compaction"`) in `.tinyloop/audit.jsonl` from `src/loop_engine/engine.rs`.
- **Why**: Incrementing after `write_state` left `.tinyloop/state.md` and the in-transcript recap showing a stale compaction count (off by 1). Logging `prefix_reset` fulfills the prompt-cache observability rule (`ARCHITECTURE.md` §4.4 & §5) so every cache invalidation has an explicit audit record.

### DEC-008: Sparse `ToolResult` JSON Serialization & `ToolContext::context_tokens` Wiring
- **What**: Derive `serde::Deserialize` (with `#[serde(default)]`) on `ToolResult` and annotate empty/none optional fields (`next_actions`, `artifacts`, `exit_code`, `stdout_excerpt`) with `#[serde(skip_serializing_if = ...)]` in `src/tools/mod.rs`. Ensure `finish_with_artifact` derives limits from `observation::resolve_tool_result_budget(ctx.context_tokens)` (with `RunConfig::context_tokens` passed via `ToolContext::with_context_tokens`) rather than hardcoded `8192`, while preserving `shell::run_command`'s `"command produced no output"` marker as a non-empty `stdout_excerpt`.
- **Why**: Serializing `"next_actions":[]`, `"artifacts":[]`, `"exit_code":null`, and `"stdout_excerpt":""` wastes precious 8K prompt tokens on every tool turn. Adding `Deserialize` allows Tier 1 masking to parse, update `stdout_excerpt`, and re-serialize `ToolResult` payloads safely without brittle string replacement.
