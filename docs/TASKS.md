# TASKS.md — tinyloop Checklist

Markers: `[ ]` todo · `[~]` in progress · `[x]` done · `[!]` blocked

---

## Phase 0 & 1 — Foundation, Core Loop, Desktop UI & Safety Hardening

- [x] Research local harness patterns and vendor audited references (`docs/research/`, `docs/REUSE_REGISTER.md`, `vendor/`)
- [x] Implement `ModelClient` (`LlamaCppClient`) and startup capability probe (`src/model/mod.rs`, `src/model/llama_cpp.rs`, `src/bin/probe.rs`)
- [x] Implement sandboxed 7-tool registry and head+tail observation caps (`src/tools/{mod,fs_tools,shell,observation}.rs`)
- [x] Implement bounded `Plan → Act → Observe → Verify` loop, repeat guard, and `CommandVerifier` (`src/loop_engine/{engine,repeat_guard}.rs`)
- [x] Implement append-only `.tinyloop/audit.jsonl` trail (`src/audit.rs`) and Phase 1 context state persistence (`src/context.rs`)
- [x] Implement Tauri 2 + React 19 desktop shell, Clarify gate, approval binding, cancel handling, and MSI/NSIS bundles (`desktop/`)
- [x] Build 15-task TOML eval suite and record real-model run metrics (`tests/eval/tasks/*.toml`, `src/bin/eval.rs`, `docs/EVAL_RESULTS.md`)
- [x] Close P0 safety, pipe-capture deadlock, symlink write, and command policy defects (commits `f0c4969`, `e957c12`)

---

## Phase 2 — Context Management, Observation Masking & Protocol Safety (Active)

- [~] Drive compaction threshold from actual `usage.prompt_tokens` + new turn delta (fallback only when `== 0`)
- [~] Pass `context_tokens` into `ToolContext` and remove hardcoded `8192` in `src/tools/mod.rs`
- [~] Make `KEEP_RECENT` slice respect atomic `(assistant tool_calls + tool)` pairs
- [~] Replace second `role: "system"` message with a `role: "user"` state recap; increment `state.compactions` before `write_state`; log `prefix_reset` in `audit.jsonl`
- [~] Implement Tier 1 Block Observation Masking at 75% before Tier 2 full compaction at 85% (track `masked_turns` and `compactions` in `RunReport`)
- [~] Omit empty fields in `ToolResult` JSON serialization
- [~] Add unit tests for tool-pair boundary safety, single-system-message invariant, and block masking

---

## Backlog & Open Verification Gaps

- [ ] Automate real-window Allow/Deny click-through via `tauri-driver` + Edge WebDriver once live model startup probe passes reliably (`docs/AGENT_BOARD.md`)
- [ ] Capture explicit `llama_kv_cache: size = ... MiB` startup line and non-empty `content` check under `enable_thinking: false` (`docs/MODEL_PROFILE.md`)
- [ ] Optional MLX backend (`src/model/mlx.rs`) for Apple Silicon (`docs/ARCHITECTURE.md` §9)
