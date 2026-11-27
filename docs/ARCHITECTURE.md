# tinyloop — Architecture & Design

**Status:** Core Rust engine (`tinyloop_core`), headless runners (`probe`, `run`, `eval`), and Tauri 2 + React 19 desktop app implemented; Phase 2 context management, observation masking, deterministic repo map, and ACI filesystem tools complete (`docs/TASKS.md`).
**Target:** a desktop agent (Tauri 2 + React 19, Rust core) that runs a small local model and performs real computer-use tasks under a bounded, verifiable loop. Headless binaries (`src/bin/{probe,run,eval}.rs`) share the same `tinyloop_core` library — see [`DESKTOP_UI.md`](DESKTOP_UI.md).
**First test model:** Spark-X2.6-4B Q4_K_M, 8K context, served by `llama-server`.
**Design basis:** folded in deep research on OpenClaw, DeepSeek Harness / ECC, small-model harnesses, and verified Spark runtime settings. Full findings: [`docs/research/`](research/INDEX.md).
**Rationale:** [`FLOW_REASONING.md`](FLOW_REASONING.md) explains why each stage exists and how to make it fast without losing accuracy.

---

## 1. What this is

`tinyloop` is a local desktop agent (with thin headless CLI binaries over `tinyloop_core`) that:

1. takes a task in plain English,
2. settles ambiguities **before** the loop starts,
3. runs a bounded `plan → act → observe → verify` loop using Spark-X2.6-4B served locally,
4. executes tools (file read/write, shell, search) inside a sandbox with approval gates,
5. stops when a **deterministic, machine-checkable** verifier says done — never when the model *says* it is done.

The model proposes actions. **Rust owns the loop, the budget, the sandbox, and the verifier.** That split is the whole design, and it is the one pattern every researched harness agrees on.

---

## 2. Constraints that drive every decision

| # | Constraint | Why | Consequence |
|---|---|---|---|
| C1 | Use OpenAI-compatible `tools` API, **never** prompt-encoded action formats | Spark-X2.6-4B scored 9/10 with API-passed tools and 0/6 when told to reply as `RUN: <cmd>`; it falls back to its own `<tool_call>` syntax | `llama-server --jinja`, real `tools` array, parse `message.tool_calls` |
| C2 | llama.cpp **≥ b10828** with `spark2_5` support | Older builds fail with `unknown model architecture: 'spark2_5'` | Version-check / capability-probe at startup; fail with a clear message |
| C3 | Thinking is **on by default** | Heavy reasoning consumes the 8K budget; a whole `max_tokens` can vanish inside `<think>` leaving empty `content` | `chat_template_kwargs: {"enable_thinking": false}`; fallback `reasoning_effort: "low"` (`medium` is rejected) |
| C4 | Native tool calling must be **probed**, not assumed | llama.cpp can silently drop `tools` when a template lacks tool support, returning HTTP 200 with no warning | Startup probe: compare `prompt_tokens` with/without tools; re-probe after any model/template change |
| C5 | 8K is small for this model | Long tool output evicts the goal | Live result caps, head+tail truncation, disk spill with path pointer |
| C6 | Small models drift on long instructions | Each tool description costs ~200 tokens; malformed calls rise with tool count | 7 tools maximum, frozen schema bytes/order, one tool call per turn |
| C7 | A model cannot judge its own work | Self-grading inflates success; tests can be deleted to "pass" | Verifier is Rust code + your `verify_cmd`; done-condition and boundaries fixed before the loop |
| C8 | Grammar and tools do not coexist here | llama.cpp rejects `Cannot specify grammar with tools` for Spark2.5 tool parsing | Tools only for action selection; separate tool-free request with `response_format` when a constrained final answer is needed |
| C9 | Irreversible actions need a human | "Use my computer" includes destructive commands | `ask` mode by default; no auto-commit, no auto-push, no auto-delete |

---

## 3. System shape

```
┌───────────────────────────────────────────────────────────────┐
│ Surfaces over tinyloop_core                                   │
│ • Desktop: Tauri 2 + React 19 (`desktop/`, `run_task` IPC)    │
│ • Headless: `src/bin/{probe,run,eval}.rs` (env-configured)    │
└───────────────────────────┬───────────────────────────────────┘
                            v
┌───────────────────────────────────────────────────────────────┐
│ CLARIFY  (once, before any tool runs; RunConfig / Modal)      │
│ • set machine-decidable `done_condition` and `verify_command` │
│ • set `boundaries` (what the agent must NOT do)               │
│ • confirm before the loop; no mid-run clarifications          │
└───────────────────────────┬───────────────────────────────────┘
                            v
┌───────────────────────────────────────────────────────────────┐
│ LOOP ENGINE  (`src/loop_engine/engine.rs`)                    │
│  Plan → Act → Observe → Verify → Done | Escalate | MaxSteps   │
│  Caps: `max_steps` (default 12), context budget, cancel flag  │
│  Repeat guard: nudge at 3 identical calls, hard stop at 4     │
└──────┬──────────────────┬──────────────────┬──────────────────┘
       │                  │                  │
       v                  v                  v
┌──────────────┐  ┌────────────────┐  ┌─────────────────────────┐
│ChatTransport │  │ ToolRegistry   │  │ ContextManager          │
│(LlamaCppHttp)│  │ (7 sandboxed   │  │ (`manage_context`,      │
│              │  │  + repo_map)   │  │  `active_observation_   │
│              │  │                │  │  chars`)                │
└──────┬───────┘  └───────┬────────┘  └──────────┬──────────────┘
       │                  │                      │
       v                  v                      v
 llama-server        filesystem / shell     .tinyloop/state.md
 --jinja :9941       (cwd-confined)         .tinyloop/artifacts/
                                            .tinyloop/audit.jsonl
```

---

## 4. Components

### 4.1 Desktop & Headless Surfaces

- **Desktop App (`desktop/`)**: Tauri 2 + React 19 interface (`desktop/src/App.tsx`, `desktop/src-tauri/src/main.rs`). Collects `task`, `done_condition`, `boundaries`, `verify_command`, `cwd`, and `mode` (`ask | auto | dry`), runs `AppState::ensure_probe`, invokes `run_agent`, streams `UiEvent` updates over `run_event`, and resolves interactive approvals via `approve` / `deny` / `cancel_run`.
- **Headless Runners (`src/bin/`)**:
  - `src/bin/probe.rs`: runs `run_startup_probe` using `TINYLOOP_BASE_URL` (default `http://127.0.0.1:9941`), `TINYLOOP_MODEL`, and optional `TINYLOOP_BUILD`.
  - `src/bin/run.rs`: runs `run_agent` in `RunMode::Auto` with `AutoApprover` using `TINYLOOP_TASK`, `TINYLOOP_CWD`, `TINYLOOP_VERIFY`, `TINYLOOP_MODEL`, `TINYLOOP_MAX_STEPS` (default `8`), and `TINYLOOP_DONE`.
  - `src/bin/eval.rs`: executes the TOML eval suite under `tests/eval/tasks/*.toml` (`EVAL_FILTER`, `EVAL_LIMIT`) and writes `eval-results.json`.

### 4.2 ChatTransport & Model Protocol (`src/model/mod.rs`, `src/model/llama_cpp.rs`)

- **Trait** `ChatTransport` (`health`, `chat`); v1 implementation `LlamaCppClient` (thin `MlxClient` deferred).
- Endpoint `POST http://127.0.0.1:9941/v1/chat/completions` (`connect_timeout = 5s`, `timeout = 180s`). Exact server flags in [`MODEL_PROFILE.md`](MODEL_PROFILE.md).
- Request essentials (`ChatRequest::for_model`, frozen for the whole session):
  - `tools`: the 7 schemas from §4.3, byte-identical names/descriptions/parameters/order
  - `tool_choice: "auto"`, `parallel_tool_calls: false`
  - vendor sampling: `temperature: 1.0`, `top_p: 0.95`, `top_k: 0` — determinism comes from the verifier, not from decoding (`--repeat-penalty 1.0` configured on `llama-server`)
  - `max_tokens: 512` (`DEFAULT_MAX_TOKENS`)
  - `chat_template_kwargs: {"enable_thinking": false}`
  - tool argument values normalized on deserialization (`deserialize_arguments` accepts JSON objects and stringified JSON objects, converting unparseable strings to `{"__unparsed": raw}`)
- Response handling:
  - `message.tool_calls` → check `finish_reason != Some("length")` and `malformed_text().is_none()` → execute first call via `tools::execute`
  - text only (no `tool_calls`) → emits `RunEvent::TextOnlyRepair`, pushes a correction message (`"No tool call was made. Call exactly one tool now. Do not answer in prose."`), and escalates on the 3rd occurrence (`text_only_repairs >= 3`) — never a fabricated done state
  - `usage.prompt_tokens`, `completion_tokens`, `timings.cache_n`, `prompt_ms`, `predicted_ms` → accumulated into `RunReport` and logged in the `"report"` audit event; per-step `response.usage.prompt_tokens` drives context management when `> 0`
  - `reasoning_content` preserved in multi-turn `Message` history via `Message::with_reasoning`
- **Startup capability probe** (`run_startup_probe`, `PROBE_TEMPERATURE = 0.0`, `max_tokens = 128`):
  1. `GET /health`
  2. fixed prompt `"Reply with the single word OK."` without and with `probe_tool()` (`read_file`); assert `with.usage.prompt_tokens > without.usage.prompt_tokens`, else fail with `ModelError::ToolsNotVisible`
  3. one `read_file` tool-call round-trip on `Cargo.toml`; if no native `tool_calls` entry is returned, fail with `ModelError::ToolCallFailed`
- **Bounded recovery for weak calls (`src/loop_engine/engine.rs`):**
  - malformed tool arguments (`malformed_text()`) → never executed; increments `report.malformed_repairs`, emits `RunEvent::TextOnlyRepair`, grants 1 retry, escalates when `malformed_repairs > 1`
  - tool-free prose → emits `RunEvent::TextOnlyRepair`, prompts native tool call, escalates at `text_only_repairs >= 3`
  - output-limit stop mid-tool-call (`finish_reason == Some("length")`) → never executed; emits `RunEvent::TextOnlyRepair`, prompts a smaller call

### 4.3 ToolRegistry (`src/tools/mod.rs`) — 7 tools, risk-tiered + `repo_map`

| Tool | Risk | Execution | Notes |
|---|---|---|---|
| `submit_plan(steps[3..6])` | low | auto | Rust validates step count (`3..=6`) and that each step maps to one of the 7 declared tools |
| `read_file(path, offset, limit)` | low | auto | UTF-8 file reader (`offset`, `limit <= 400`, default `160` lines) with `[File: <rel> (<total> lines), showing lines <start>-<end>]` ACI headers, `[File: <rel> (empty)]`, out-of-range offset hints, and directory listing (`[Dir: <rel> (<total> entries)]`, `<= 100` entries, skips `.git`/`target`/`node_modules`/`.tinyloop`); caps excerpt via `ToolContext::context_tokens` + artifact spill |
| `search_files(pattern, path, glob, max_results)` | low | auto | Literal case-insensitive search using `ignore::WalkBuilder` (respects `.gitignore`, global/exclude gitignore, skips hidden files/dirs and symlinks, optional `glob` via `OverrideBuilder`), `<= 50` hits, skips files `> 1 MB` and binary files (NUL byte in first `8 KiB`), returns workspace-relative `path:line: content` |
| `run_command(command, timeout_sec)` | **high** | approval | Allowlisted binaries (`cargo`, `git`, `rg`, `pnpm`, `npm`, `node`, `python`, `python3`), `30 s` default (`120 s` max), pipe-drained (`2 MiB` capture cap/stream) |
| `apply_patch(path, old_string, new_string)` | medium | approval | Unique exact string replacement inside canonicalized workspace (symlinks rejected); `old_string == ""` creates new files + missing parents via `resolve_for_create`; 0-match errors hint at whitespace-normalized line matches (`normalized_match_line`) |
| `finish(summary)` | low | auto | Requests deterministic external verifier run; only verifier pass yields `Done` |
| `escalate(reason)` | low | auto | Explicit stop handing control back to the human |

- **Deterministic Workspace Map (`src/tools/repo_map.rs`)**: At the start of `run_agent`, `repo_map::build_repo_map(&ctx.root)` scans source files (pure Rust, skipping hidden entries, symlinks, `.git`/`target`/`node_modules`/`dist`/`.tinyloop`/`vendor`, and files `> 64 KiB`, extracting up to `6` definitions/file) into a `<= 1,600`-char (`REPO_MAP_BUDGET_CHARS`) summary. When `repo_map.files > 3`, it is appended once to the initial `role: "user"` prompt while keeping `messages[0]` (`system`) byte-frozen.
- Clarification is handled before the loop (`RunConfig` / desktop Clarify modal), not by an `ask_user` tool — that keeps the 7-tool budget and front-loads ambiguity.

Every tool result uses `ToolResult` (`src/tools/mod.rs`), serialized sparsely so empty/null fields (`next_actions: []`, `artifacts: []`, `exit_code: None`, `stdout_excerpt: ""`) are omitted via `#[serde(skip_serializing_if = ...)]` to save prompt tokens, while deriving `Deserialize` with `#[serde(default)]` for structured Tier 1 observation masking:

```json
{
  "status": "success | warning | error",
  "summary": "one-line result",
  "next_actions": ["..."],
  "artifacts": ["C:/.../.tinyloop/artifacts/tool-1795768583000.txt"],
  "exit_code": 0,
  "stdout_excerpt": "...",
  "truncated": true
}
```

- Errors are returned as results (`status: "error"`, root-cause hint in `next_actions`), never thrown into the transcript as exceptions.
- Empty command output gets an explicit `"command produced no output"` marker via `shell::output_or_marker`, so `stdout_excerpt` is non-empty and serialized for silent commands (stopping repeat re-runs), while truly empty excerpts (e.g., `apply_patch`, `ToolResult::error`) are omitted.
- **Context-derived live caps:** `ToolContext` carries `context_tokens` (overridden by `run_agent` from `RunConfig::context_tokens` via `with_context_tokens`, no hardcoded `8192` inside `finish_with_artifact`), passed into `observation::resolve_tool_result_budget(ctx.context_tokens)`. At `8192` tokens this yields `max_chars = 9,828` chars per result and `max_context_chars = 8,192` total observation chars. Truncation is **head + tail** (UTF-8 char-boundary safe) with `[... N chars omitted; full output at <path> ...]` pointing to `.tinyloop/artifacts/tool-<ts_ms>.txt`.
- Tool output is treated as **untrusted data**, never as instructions.

**Two-layer permission gate** (per OpenClaw):
1. **Capability / Mode** — `RunMode::DryRun` blocks mutations/commands; `apply_patch` and `run_command` in `RunMode::Ask` require interactive approval.
2. **Allowlist + approval** — `deny` is the fallback; approval binds `sha256(tool + "\0" + canonical_json(args))` (`approval_hash`) shown to the user in the inspector card.

**Sandbox rules**

- `ToolContext::root` (`--cwd` / workspace) is the root; `resolve_existing` canonicalizes targets inside `root_canon`, `resolve_for_write` rejects writes through **any** symlink (via `symlink_metadata`), and `resolve_for_create` checks `ensure_workspace_relative` plus nearest existing ancestor canonicalization before creating parent directories.
- Command policy (`src/tools/shell.rs`): rejects shell metacharacters, validates quotes via `split_command`, blocks destructive `git` subcommands (`push`, `reset`, `clean`, `rebase`, `filter-branch`, `restore`, `commit`, `config`, `remote`, `checkout --`) and escaping flags (`-C`, `--git-dir`, `--work-tree`, `-c`), blocks `python -m http.server`, stdin `-`, default `python -c` (unless `allow_inline_code`), and inline `node` flags (`-e`, `--eval`, `-p`, `--print`, `-`), and requires script paths to canonicalize inside the workspace. Environment is cleared (`env_clear()`) before spawning, keeping only safe OS/PATH vars.
- Every call, approval decision, and result is appended to `.tinyloop/audit.jsonl`.

### 4.4 ContextManager — the 8K budget & Two-Tier Management

| Allocation | Tokens |
|---|---:|
| Chat-template / generation headroom | 512 |
| Frozen system prompt + 7 tool schemas (cacheable prefix) | ~900 |
| Task spec + done-condition + boundaries | ~300 |
| Compacted state (`state.md`) | ~500 |
| Recent tool observations (head/tail excerpts) | ~3000 |
| Exact recent tail (last 2 turns, verbatim) | ~900 |
| Reserved for model output | ~1024 |
| Slack | ~1056 |
| **Total** | **8192** |

Context management rules (`src/context.rs` + `src/loop_engine/engine.rs`):

- **Prefix is frozen & single-system-message invariant**: `messages[0]` (`role: "system"`) is kept untouched for the entire run — no clock, no random IDs, no tool reordering, and **never** a second `role: "system"` message. Every compaction prefix reset is logged as a `prefix_reset` event in `.tinyloop/audit.jsonl` (`{"reason": "compaction", "before_tokens", "after_tokens", "compactions"}`).
- **Real token measurement (fallback only when `== 0`)**: Each step captures `turn_prompt_tokens = (response.usage.prompt_tokens > 0).then_some(response.usage.prompt_tokens)` and `new_turn_tokens = estimate_message(&assistant_msg) + estimate_message(&tool_msg)` before pushing to `messages`. `effective_prompt_tokens(messages, reported_prompt_tokens, new_turn_tokens)` uses `(pt as usize).saturating_add(new_turn_tokens)` whenever `reported_prompt_tokens` is `Some(pt)` with `pt > 0`, falling back to `estimate_messages(messages).saturating_add(TOOL_SCHEMA_TOKENS)` (`1200`) **only** when `reported_prompt_tokens` is `None` or `Some(0)` (so naive `chars / 4` never triggers premature compaction over real server counts).
- **Observation budget guard**: `engine.rs` tracks `observation_chars` against `observation::resolve_tool_result_budget(config.context_tokens).max_context_chars` (`8,192` chars at 8K). If `observation_chars > observation_budget` (`over_observation_budget`), `engine.rs` calls `compact_with_metrics` directly and resets `observation_chars = 0`. Otherwise it invokes `manage_context`, which returns `ContextOutcome { masked_turns, mask_metrics, compact_metrics }`.
- **Two-tier context protection (`75% → 85%`)** via `manage_context`:
  1. **Tier 1 — Block Observation Masking (`MASK_AT = 0.75`, ~6,144 tokens at 8K)**: `mask_old_observations` preserves the last `KEEP_RECENT_TURNS = 2` atomic turns verbatim; in one single batch across all older `role: "tool"` messages (index `>= 1`), parses `ToolResult` JSON and replaces non-empty `stdout_excerpt` (or non-JSON text) with `"[omitted — see summary and artifact path]"` (`MASKED_EXCERPT_MARKER`), keeping `status`, `summary`, `artifacts`, `next_actions`, `exit_code`, and `truncated` intact. When Tier 1 succeeds without Tier 2, `engine.rs` recomputes `observation_chars = active_observation_chars(&messages)` so masked turns stop counting against the observation budget.
  2. **Tier 2 — Full Deterministic Compaction (`COMPACT_AT = 0.85`, ~6,963 tokens at 8K, or when Tier 1 masks 0 turns / leaves estimated effective tokens `>= 75%`, or on `over_observation_budget`)**:
     - Increment `state.compactions += 1` **before** calling `write_state(root, state)` and `state.render()`, so `.tinyloop/state.md` and the recap reflect the true count.
     - Persist `.tinyloop/state.md` first (`goal`, `done`, `boundaries`, up to 16 recent `facts`, `next_step`, `compactions`).
     - Slice `KEEP_RECENT` (`4` messages minimum) walking backward via `compaction_cut` / `atomic_turn_start` in **atomic turn units**, pairing consecutive `role: "tool"` messages with their owning `role: "assistant"` (`tool_calls`) message so the kept tail never starts with an orphaned `role: "tool"` message.
     - Insert the deterministic recap as a `role: "user"` message: `[Compacted state — prior turns persisted to .tinyloop/state.md]\n...`. Reset `observation_chars = 0`.
- **Observability & Metrics**: `RunEvent::Masked { masked_turns, before_tokens, after_tokens }` (`event_kind = "observation_mask"`) and `RunEvent::Compacted { before_tokens, after_tokens }` (`event_kind = "compaction"`) emit to both listeners and `audit.jsonl`; `RunReport` tracks `masked_turns` and `compactions` (logged in `"report"`); `timings.cache_n` / `usage.prompt_tokens_details.cached_tokens` measure prompt-cache reuse.
- Full raw tool output goes to `.tinyloop/artifacts/tool-<ts_ms>.txt`; the model re-reads slices with `read_file(offset, limit)`.

### 4.5 LoopEngine (`src/loop_engine/engine.rs`)

States: `Clarify → Plan → Act → Observe → Verify → Done | Escalate | MaxSteps`.

- **Plan:** `submit_plan(steps[3..6])`; Rust checks the count (`3..=6`) and that each step maps to one of the 7 declared tools. Invalid plan → 1 repair attempt → escalate (`plan_repairs > 1`).
- **Act:** exactly one tool call per turn; `parallel_tool_calls: false`.
- **Observe:** bounded, sanitized `ToolResult` appended; artifact path recorded; `ContextState::observe` records up to 16 recent facts.
- **Verify:** Rust runs `verify_command` via `CommandVerifier` on `finish` (or after mutations if opt-in `RunConfig::verify_on_write == true`). The model cannot edit acceptance conditions; if no verifier is configured (`NeverVerifier`), `finish` fails verification and cannot declare `Done`. Pass → `Done`; fail → failure reason returned to the model. **3 failed attempts → `Escalate`.**
- **Damping:** `max_steps` cap (`12` default in `RunConfig`), cooperative `cancel: Arc<AtomicBool>` checked every turn and approval poll, context thresholds (`MASK_AT = 0.75`, `COMPACT_AT = 0.85`, `observation_budget`), bounded repair limits (`text_only_repairs >= 3`, `malformed_repairs > 1`, `plan_repairs > 1`, `verify_failures >= 3`), and subprocess timeouts (`30s` default / `120s` max for `run_command`, `120s` for `CommandVerifier`).
- **Repeat guard (`src/loop_engine/repeat_guard.rs`):** identical `(tool, canonical_json(args))` calls (excluding `submit_plan`) append a system note to `ToolResult.next_actions` on the 3rd call and hard-escalate (`TOOL_REPEAT_LIMIT_CODE`) on the 4th (tighter than OpenClaw's 3/20 because our step cap is 12).
- **No-answer is failure:** empty final or tool-free text that never calls `finish`/`escalate` triggers `TextOnlyRepair` and escalates on the 3rd occurrence, never a done state.
- **Human last switch:** in `ask` mode, medium/high-risk tools (`apply_patch`, `run_command`) pause on `Approver`; `finish` triggers external verification, never an irreversible side effect.

### 4.6 Verification / goal definition

A goal is accepted only if a machine can answer yes/no in one command:

- ✅ "`cargo test` exits 0 AND `src/tax.rs` contains `fn calc_total` AND `git status --porcelain` shows changes only under `src/`"
- ✅ "`report.csv` exists, header `a,b,c`, exactly 12 data rows"
- ❌ "make the code better", "check if it looks ok"

Ship **boundaries** with every goal (what it must NOT do) — the anti-Goodhart guard. Prefer reconciliation against an external reference over the agent's own assertions. When a structured final answer is needed, use a **separate tool-free request** with `response_format` JSON schema (never combine grammar with tools on this model).

---

## 5. Safety & observability

- **Modes:** `dry-run` (`RunMode::DryRun` — blocks mutations/commands), `ask` (`RunMode::Ask` — interactive approval for `apply_patch` and `run_command`), `auto` (`RunMode::Auto` — allowlist only, no interactive prompt).
- **Audit:** `.tinyloop/audit.jsonl` — each JSON line carries `{ ts_ms, run_id, kind, detail }` for `plan`, `tool_call`, `tool_result`, `verify`, `finished`, `escalated`, `repair`, `approval_required` (`id`, `tool`, `args`, `hash`), `observation_mask`, `compaction`, `prefix_reset` (`reason`, `before_tokens`, `after_tokens`, `compactions`), and `report` (including `masked_turns`, `compactions`, tokens, cache hits, timings, errors).
- **Stop:** `RunConfig::cancel` (`Arc<AtomicBool>`) halts at the next step boundary and `ApprovalRegistry::deny_all` unblocks pending desktop approval gates immediately; no half-applied patch.
- **Inspection:** `.tinyloop/audit.jsonl` (`AuditLog::read_all`) and `.tinyloop/state.md` provide full run replay and compacted state inspection.
- **Eval:** `tests/eval/tasks/*.toml` (5 smoke + 10 full tasks) with decidable goals and `CommandVerifier` grading on the final sandbox state (`src/bin/eval.rs`, `docs/EVAL_RESULTS.md`). Track tool-call validity separately from task success; log retries/task and tokens per successful task.

---

## 6. Deliberately NOT copied from the researched systems

- DSH's Cordis profile/bundle/plugin tree and hook bridge — Node/Cordis-specific; implement the equivalent checks as Rust code at explicit pipeline stages.
- Claude-Code slash commands, subagent dispatch surfaces, and always-loaded 300+ skill catalogs.
- Cloud-model routing (Opus/Sonnet/Haiku tiers) — there is one 4B model here; context isolation is achieved with fresh sequential phases and summary files instead.
- DeepSeek server-side disk-cache economics — local llama.cpp has different flags and determinism trade-offs.
- Permissive multi-format tool-output parsers: one canonical native channel plus strict validation is the primary path.

---

## 7. Dependencies & Adapted Modules

| Layer / Need | Crate / Package |
|---|---|
| Serialization & Config | `serde`, `serde_json`, `toml` |
| HTTP Client (`llama-server`) | `reqwest` (`blocking`, `json`, `rustls-tls`) |
| Subprocess Timeout & Search | `wait-timeout`, `ignore` |
| Approval Hashing | `sha2` |
| Desktop Shell & UI | `tauri` v2 (`desktop/src-tauri/`), React 19 + `motion` + Vite + Vitest (`desktop/`) |

Adapted modules from audited OSS sources: `src/tools/observation.rs` and `src/loop_engine/repeat_guard.rs` (see [`REUSE_REGISTER.md`](REUSE_REGISTER.md) for sources and licenses).

Deferred: `MlxClient` (Apple Silicon) and in-process `llama-cpp-2` bindings. HTTP decouples the agent from fast-moving llama.cpp bindings.

---

## 8. Project layout

```
tinyloop/
├── Cargo.toml                        (`tinyloop` package, `tinyloop_core` lib)
├── docs/
│   ├── AGENT_BOARD.md                ← orchestration cards & merge gates
│   ├── ARCHITECTURE.md               ← this file
│   ├── DECISIONS.md                  ← architecture decision log
│   ├── DESIGN.md                     ← product & UI/UX spec
│   ├── DESKTOP_UI.md                 ← Tauri desktop architecture
│   ├── EVAL_RESULTS.md               ← 15-task real-model eval metrics
│   ├── FLOW_REASONING.md             ← stage & latency rationale
│   ├── MODEL_PROFILE.md              ← llama-server command, flags, GGUF files
│   ├── REUSE_REGISTER.md             ← vendored & ported OSS attributions
│   ├── RULES.md                      ← coding, testing, and never-do standards
│   ├── SYSTEM_DESIGN.md              ← storage, APIs, bounds, security
│   ├── TASKS.md                      ← living checklist
│   ├── WORKFLOW.md                   ← per-task engineering loop
│   └── research/                     ← 140+ pinned research citations
├── src/
│   ├── lib.rs, audit.rs, context.rs
│   ├── model/{mod.rs, llama_cpp.rs}
│   ├── loop_engine/{mod.rs, engine.rs, repeat_guard.rs}
│   ├── tools/{mod.rs, fs_tools.rs, observation.rs, repo_map.rs, shell.rs}
│   └── bin/{probe.rs, run.rs, eval.rs}
├── desktop/
│   ├── src/{App.tsx, main.tsx, styles.css, App.test.tsx, App.tauri.test.tsx}
│   ├── prototype.html
│   └── src-tauri/{Cargo.toml, tauri.conf.json, capabilities/default.json, src/main.rs}
├── tests/eval/{task.toml, tasks/*.toml}
├── vendor/                           ← upstream reference originals & licenses
└── .tinyloop/                        (runtime, gitignored: state.md, artifacts/, audit.jsonl)
```

---

## 9. Roadmap & Status

| Phase | Deliverable | Status |
|---|---|---|
| 0 | Research + design docs (`docs/research/`, `MODEL_PROFILE.md`, `REUSE_REGISTER.md`) | Done ✅ |
| 1 | `tinyloop_core` (`ModelClient`/`ChatTransport`, 7 tools, bounded loop, `audit.jsonl`), `desktop/` Tauri + React UI, 15-task eval suite, P0 safety/pipe hardening | Done ✅ |
| 2 | Protocol-safe compaction, real `prompt_tokens + delta` budget, Tier 1 block observation masking (`75%`) → Tier 2 compaction (`85%`), sparse `ToolResult` JSON, `context_tokens` wiring, `prefix_reset` audit, ACI tools & deterministic `repo_map` | Done ✅ |
| Backlog | Real-window `tauri-driver` Allow/Deny click-through, `llama_kv_cache` / `enable_thinking: false` content log capture, optional `MlxClient` | Open (`docs/TASKS.md`) |

---

## 10. Verified Runtime Baseline & Remaining Gaps

1. **Verified model & server**: `Spark-X2.6-4B-Q4_K_M.gguf` served by `llama-server` build `12652` on `127.0.0.1:9941` (`-c 8192 -np 1 --jinja --temp 1.0 --top-p 0.95 --top-k 0 --repeat-penalty 1.0`).
2. **Startup probe & thinking**: `enable_thinking: false` in the loop and `PROBE_TEMPERATURE = 0.0` in `run_startup_probe` passed tool visibility (`23 → 130` prompt tokens) and native tool calling; 15/15 eval tasks passed (`docs/EVAL_RESULTS.md`).
3. **Remaining verification gaps**: tracked in `docs/TASKS.md` (`Backlog & Open Verification Gaps`) — live-window WebDriver Allow/Deny click-through, explicit `llama_kv_cache` startup log + non-empty `content` capture, and optional Apple Silicon MLX backend.

---

## Sources

Primary research is in [`docs/research/`](research/INDEX.md) with 140+ pinned citations:

- OpenClaw: canonical `openclaw/openclaw`, commit `58c8966…` — [`research/openclaw.md`](research/openclaw.md)
- DeepSeek Harness + ECC — [`research/deepseek-harness.md`](research/deepseek-harness.md)
- pi-mono, Codex CLI, Aider, OpenCode, Goose, SWE-agent, 12-Factor Agents — [`research/small-model-harnesses.md`](research/small-model-harnesses.md)
- Spark-X2.6-4B GGUF/llama.cpp/MLX primary sources — [`research/model-runtime.md`](research/model-runtime.md), [`MODEL_PROFILE.md`](MODEL_PROFILE.md)

*No benchmark numbers in these docs are invented; quoted figures come from cited public sources and verified runs.*
