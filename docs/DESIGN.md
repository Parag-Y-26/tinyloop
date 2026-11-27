# DESIGN.md — tinyloop

**Status:** Core Rust engine (`tinyloop_core`), CLI binaries, Tauri 2 + React 19 desktop app, Phase 2 two-tier context management, and ACI filesystem tools implemented (`docs/TASKS.md`).

---

## 1. Product Goal

`tinyloop` is a local computer-use agent that executes plain-English tasks inside a local workspace under a bounded, deterministic `Clarify → Plan → Act → Observe → Verify` loop using a small local model (`Spark-X2.6-4B-Q4_K_M`, `8192`-token context) served by `llama-server`.

The core split: **the model proposes one tool action per turn; Rust (`tinyloop_core`) owns the state machine, token and step budgets, workspace sandbox, approval gates, and external verifier.** Completion is decided only by a deterministic machine-checkable verifier, never by the model declaring itself done.

---

## 2. Target Users

- **Developers and technical operators** running bounded repository investigation, targeted file edits, and command verification locally on Windows or Unix without cloud API dependencies.
- **Evaluators and harness engineers** measuring small-model reliability (`src/bin/eval.rs`, `tests/eval/tasks/*.toml`) with final-state grading and separate tool-call validity metrics.

---

## 3. Core Features

1. **Pre-Loop Clarify Contract (`RunConfig` + Desktop Clarify Modal)**
   - Captures the task goal, machine-decidable `done_condition`, anti-Goodhart `boundaries` (`Must not`), and optional `verify_command` before any tool executes.
   - Without a configured verifier (`NeverVerifier`), the loop cannot declare `Done`.
2. **Startup Capability Probe (`src/model/mod.rs`, `src/bin/probe.rs`)**
   - Checks `/health`, compares `prompt_tokens` with and without `tools` (detecting silent template tool-drop), and executes one read-only tool call round-trip at `PROBE_TEMPERATURE = 0.0`.
   - Cached per model per desktop app process (`AppState::probes`).
3. **Bounded 7-Tool Agent Loop (`src/loop_engine/engine.rs`, `src/tools/mod.rs`)**
   - Seven frozen tool schemas: `submit_plan`, `read_file`, `search_files`, `run_command`, `apply_patch`, `finish`, `escalate`.
   - ACI-grade filesystem behavior (`src/tools/fs_tools.rs`, `src/tools/repo_map.rs`): `read_file` supports directory listings, line-range headers, empty markers, and offset hints; `search_files` uses `ignore` (`.gitignore`-aware, hidden/binary skipping, `glob` filtering, workspace-relative paths); `apply_patch` supports parent-creating new files (`old_string == ""`) and whitespace-normalized mismatch hints; `build_repo_map` injects a deterministic `<= 1,600`-char map into the initial `user` prompt when `> 3` source files exist.
   - One tool call per turn (`parallel_tool_calls: false`), vendor sampling (`temperature: 1.0`, `top_p: 0.95`, `top_k: 0`), `enable_thinking: false`, `max_tokens: 512`.
   - Repeat guard (`src/loop_engine/repeat_guard.rs`): injects a system note on the 3rd identical `(tool, canonical_json(args))` call and escalates on the 4th.
   - Bounded recoveries: max 2 text-only repairs (escalates on 3rd), truncated `finish_reason: "length"` calls never executed, max 1 malformed-argument repair, max 1 invalid-plan repair, max 3 verifier failures before escalating.
4. **Two-Layer Safety & Approval Gates (`src/tools/fs_tools.rs`, `src/tools/shell.rs`)**
   - Three run modes (`RunMode`): `Ask` (default in desktop), `Auto`, and `DryRun`.
   - In `Ask` mode, `apply_patch` and `run_command` pause and emit `ApprovalRequest { id, tool, args, hash }` bound via SHA-256 (`approval_hash`).
   - Strict workspace canonicalization (`resolve_existing`, `resolve_for_write`, `resolve_for_create`), unconditional symlink write rejection, command allowlisting (`cargo`, `git`, `rg`, `pnpm`, `npm`, `node`, `python`, `python3`), shell metacharacter denial, destructive `git`/inline-interpreter blocking, and environment scrubbing.
5. **Context Management & Spillover (`src/context.rs`, `src/tools/mod.rs`, `src/tools/observation.rs`)**
   - `ToolContext::context_tokens` wired from `RunConfig::context_tokens` derives per-result limits (`9,828` chars at `8192` tokens) with head+tail UTF-8-safe truncation and raw output spill to `.tinyloop/artifacts/`.
   - Sparse `ToolResult` JSON serialization (`skip_serializing_if` + `#[serde(default)]` + `Deserialize`) omits empty optional fields while preserving `"command produced no output"` on silent commands.
   - Real-token two-tier protection (`manage_context`, `ContextOutcome`): `effective_prompt_tokens` tracks `usage.prompt_tokens + new_turn_tokens` (falling back to char estimate only when `== 0`). At `75%` (`MASK_AT`), Tier 1 `mask_old_observations` replaces older `stdout_excerpt` payloads with `"[omitted — see summary and artifact path]"` while preserving the last `2` turns and `ToolResult` metadata, and `active_observation_chars` updates the live observation load. At `85%` (`COMPACT_AT`, ineffective masking, or `over_observation_budget`), Tier 2 `compact` increments `state.compactions` before `write_state`, writes `.tinyloop/state.md`, slices `KEEP_RECENT` on atomic `(assistant(tool_calls) + tool)` boundaries, inserts a `role: "user"` recap (keeping single `role: "system"` intact), and logs `prefix_reset` (`reason: "compaction"`). `RunEvent::Masked` and `RunReport::{masked_turns, compactions}` expose both tiers to callers and audit logs.
6. **Audit Trail & Headless / Eval Tooling (`src/audit.rs`, `src/bin/`)**
   - Append-only `.tinyloop/audit.jsonl` event logging per run (`plan`, `tool_call`, `tool_result`, `verify`, `finished`, `escalated`, `repair`, `approval_required`, `observation_mask`, `compaction`, `prefix_reset`, `report`).
   - Headless binaries: `probe`, `run`, and `eval` (15 TOML tasks under `tests/eval/tasks/`).

---

## 4. UI / UX Style (`desktop/`)

Full desktop UX specification is recorded in [`DESKTOP_UI.md`](DESKTOP_UI.md). Implemented in `desktop/src/App.tsx`, `desktop/src/styles.css`, and `desktop/src-tauri/tauri.conf.json`:

- **Archetype**: **Operate + Monitor** desktop workspace (`1440×920` default window, `900×600` minimum, title `tinyloop`). No marketing chrome or hero panels.
- **Three-Column Layout**:
  - **Titlebar (`54px`)**: Window traffic dots, `tinyloop` brand glyph (`t`) + `Tauri shell` badge, segmented mode selector (`Ask | Auto | Dry run`), model pill (`Spark X2.6 · 4B · 8K`), theme toggle (`◐`), active run cancel button (`■`), and primary `▶ Run` button.
  - **Left Sidebar (`222px`)**: Session list, `＋ New task` action (opens Clarify gate), and `Model` metrics footer (`Cache hits`, `Context: 8192`, `Result cap` queried from `tinyloop_core` via `context_budget`).
  - **Center Main Pane**: Breadcrumb (`Sessions / current run`), task title, scrollable step timeline (`Plan`, `Act`, `Observe`, `Verify`, `Done`, `Escalate`, `Repair`, `Approval`, `Masked`, `Compacted`, `Probe`), and bottom-pinned task composer (`↵` send, `⌘K` command palette).
  - **Right Inspector (`292px`)**: `Run setup` (`Workspace` and `Verifier` inputs when running inside Tauri), read-only Clarify contract cards (`Goal`, `Boundaries`, `Verifier` with `Judge: Rust, not the model`), and live `Approval` panel (`Allow` / `Deny` buttons displaying bound `sha256` prefix/suffix).

### Colors (from `desktop/src/styles.css`)

| Token | Light Theme (`:root`) | Dark Theme (`html[data-theme="dark"]`) | Usage |
|---|---|---|---|
| `--bg` | `#f2f2f7` | `#1c1c1e` | Window background |
| `--surface` | `rgba(255,255,255,.74)` | `rgba(44,44,48,.76)` | Titlebar and composer bar (with `backdrop-filter`) |
| `--sidebar` | `rgba(248,248,251,.82)` | `rgba(32,32,35,.84)` | Sidebar and inspector panes |
| `--card` | `rgba(255,255,255,.66)` | `rgba(58,58,62,.58)` | Step cards, contract cards, inputs |
| `--ink` | `#1d1d1f` | `#f5f5f7` | Primary text |
| `--muted` | `#6e6e73` | `#a1a1a6` | Secondary details and code |
| `--faint` | `#8e8e93` | `#8e8e93` | Labels, timestamps, metadata |
| `--accent` | `#0a84ff` | `#0a84ff` | Primary actions, active rings, running state |
| `--green` | `#2fae4b` | `#30d158` | Done / verified / live model dot |
| `--amber` | `#d98316` | `#ff9f0a` | Approval attention |
| `--red` | `#d9433c` | `#ff453a` | Blocked / error / denied state |

### Typography, Radii & Motion

- **UI Font (`--font`)**: `-apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", sans-serif`
- **Monospace Font (`--mono`)**: `ui-monospace, "SF Mono", "Cascadia Code", Consolas, monospace` (paths, args, hashes, metadata; tabular numerals for counters)
- **Radii**: `--r-window: 14px`, `--r-card: 12px`, `--r-control: 9px` (`html[data-density="compact"]` uses `9px` / `7px`)
- **Motion**: `--spring: cubic-bezier(.32,.72,0,1)` plus `motion/react` (`AnimatePresence` / `motion.div` entering with `opacity: 0 → 1`, `y: 8 → 0`, `filter: blur(3px) → none`).

---

## 5. UI & Core Components

| Component / Artifact | Path | Responsibility |
|---|---|---|
| React Desktop UI | `desktop/src/App.tsx` | Three-column Operate surface, Clarify modal (`Confirm the goal`), Command Palette (`⌘K`), Tauri event listeners (`run_event`, `approval_required`), browser fallback preview |
| Desktop Styles | `desktop/src/styles.css` | CSS variables, light/dark themes, concentric radii, glass surfaces |
| Standalone Prototype | `desktop/prototype.html` | Self-contained clickable browser prototype |
| Tauri Shell Bridge | `desktop/src-tauri/src/main.rs` | Commands (`context_budget`, `current_dir`, `run_task`, `approve`, `deny`, `cancel_run`), `TauriApprover`, probe caching, `RunEvent` → `UiEvent` mapping (including `Masked` / `observation_mask` and `Compacted` / `compaction`) |
| Core Library | `src/lib.rs` | `audit`, `context`, `loop_engine`, `model`, `tools` (including `tools::repo_map`) |
| Headless Binaries | `src/bin/{run,probe,eval}.rs` | Env-configured headless task runner, model probe CLI, 15-task TOML eval runner |
