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

## Phase 2 — Context Management, Observation Masking & Protocol Safety (Complete)

- [x] Drive compaction threshold from actual `usage.prompt_tokens` + new turn delta (fallback only when `== 0`) — `context::effective_prompt_tokens` (`src/context.rs`, `0b2e4dc`) + per-step `turn_prompt_tokens` and `new_turn_tokens` in `engine::run_agent` (`src/loop_engine/engine.rs`, `b8786af`); verified by `effective_prompt_tokens_uses_real_usage_plus_delta_and_falls_back_only_at_zero`, `zero_prompt_tokens_falls_back_to_estimate_messages`, `compaction_fires_and_writes_state`
- [x] Pass `context_tokens` into `ToolContext` and remove hardcoded `8192` in `src/tools/mod.rs` — `ToolContext::context_tokens` + `with_context_tokens` and `finish_with_artifact` using `observation::resolve_tool_result_budget(ctx.context_tokens)` (`src/tools/mod.rs`), overridden from `RunConfig::context_tokens` in `engine::run_agent` (`src/loop_engine/engine.rs`); verified by `tools::tests::result_budget_tracks_the_configured_context`
- [x] Make `KEEP_RECENT` slice respect atomic `(assistant tool_calls + tool)` pairs — `context::atomic_turn_start` + `context::compaction_cut` (`src/context.rs`, `0b2e4dc`) pair consecutive `role: "tool"` messages with their preceding `assistant(tool_calls)` turn; verified by `compaction_never_orphans_tool_results` and `compaction_pairs_consecutive_tool_results_and_handles_unmatched_or_single_turn`
- [x] Replace second `role: "system"` message with a `role: "user"` state recap; increment `state.compactions` before `write_state`; log `prefix_reset` in `audit.jsonl` — `context::compact` (`src/context.rs`, `0b2e4dc`) keeps `messages[0]` untouched, increments `state.compactions += 1` before `write_state` + `state.render()`, and inserts `[Compacted state — prior turns persisted to .tinyloop/state.md]\n...` as `Message::user`; `engine::run_agent` (`src/loop_engine/engine.rs`, `b8786af`) logs `prefix_reset` (`{"reason": "compaction", "before_tokens", "after_tokens", "compactions"}`); verified by `compaction_keeps_single_system_inserts_user_recap_and_writes_incremented_count` and `compaction_fires_and_writes_state`
- [x] Implement Tier 1 Block Observation Masking at 75% before Tier 2 full compaction at 85% (track `masked_turns` and `compactions` in `RunReport`) — `MASK_AT = 0.75`, `KEEP_RECENT_TURNS = 2`, `mask_old_observations`, `manage_context`, `ContextOutcome` (`src/context.rs`, `0b2e4dc`), wired in `src/loop_engine/engine.rs` (`active_observation_chars`, `RunEvent::Masked`, `RunReport::{masked_turns, compactions}`, `"observation_mask"` + `"report"` audit entries) and surfaced in `src/bin/run.rs` + `desktop/src-tauri/src/main.rs` (`b8786af`); verified by `block_masking_triggers_only_above_75_percent_and_preserves_metadata_and_last_two_turns`, `tier2_compaction_runs_above_85_percent_or_when_masking_fails_to_drop_below_75_percent`, `tier1_block_masking_uses_prompt_tokens_plus_delta_and_adjusts_observation_budget`, `tier2_compaction_triggers_at_85_percent_and_when_masking_stays_above_75_percent`
- [x] Omit empty fields in `ToolResult` JSON serialization — `ToolResult` derives `Deserialize` with `#[serde(default)]` and uses `#[serde(skip_serializing_if = ...)]` for `next_actions`, `artifacts`, `exit_code`, and `stdout_excerpt` (`src/tools/mod.rs`, `1add346`), preserving `shell::output_or_marker`'s `"command produced no output"`; verified by `to_json_omits_empty_optional_fields`, `to_json_preserves_no_output_marker_and_non_empty_fields`, `tool_result_round_trips_via_serde_json`
- [x] Add unit tests for tool-pair boundary safety, single-system-message invariant, and block masking — verified across `src/context.rs`, `src/tools/mod.rs`, and `src/loop_engine/engine.rs` (`cargo test --lib`: **83 passed, 0 failed, 0 ignored**; `cargo clippy --all-targets -- -D warnings`: **PASS**; `cargo check --all-targets`: **PASS**; `desktop` `pnpm test` **2 files / 10 tests** + `pnpm build`: **PASS**; `desktop/src-tauri` `cargo check/clippy --locked --all-targets -- -D warnings`: **PASS**)

> Also landed on Phase 2 HEAD (`e2ed854`): ACI-grade `read_file` (directory listing, line-range headers, empty marker, offset hints), `ignore`-based `.gitignore`-aware `search_files` (hidden/binary skipping, `glob` filter, workspace-relative paths), parent-creating + whitespace-hinting `apply_patch`, and deterministic `repo_map` (`<= 1,600` chars) injected into the initial `user` message when workspace has `> 3` source files.

---

## Backlog & Open Verification Gaps

- [ ] Automate real-window Allow/Deny click-through via `tauri-driver` + Edge WebDriver once live model startup probe passes reliably (`docs/AGENT_BOARD.md`)
- [ ] Capture explicit `llama_kv_cache: size = ... MiB` startup line and non-empty `content` check under `enable_thinking: false` (`docs/MODEL_PROFILE.md`)
- [ ] Optional MLX backend (`src/model/mlx.rs`) for Apple Silicon (`docs/ARCHITECTURE.md` §9)
