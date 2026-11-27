# tinyloop — Architecture & Design (v0.2)

**Status:** design only. No application code written yet.
**Target:** a desktop agent (Tauri 2 + React 19, Rust core) that runs a small local model and performs real computer-use tasks under a bounded, verifiable loop. The CLI becomes a thin headless wrapper over the same core — see [`DESKTOP_UI.md`](DESKTOP_UI.md).
**First test model:** Spark-X2.6-4B Q4_K_M, 8K context, served by `llama-server`.
**v0.2 change:** folded in deep research on OpenClaw, DeepSeek Harness / ECC, small-model harnesses, and verified Spark runtime settings. Full findings: [`docs/research/`](research/INDEX.md).
**Rationale:** [`FLOW_REASONING.md`](FLOW_REASONING.md) explains why each stage exists and how to make it fast without losing accuracy.

---

## 1. What this is

`tinyloop` is a CLI that:

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
| C2 | llama.cpp **≥ b10828** with `spark2_5` support | Older builds fail with `unknown model architecture: 'spark2_5'` | Version-check at startup; fail with a clear message |
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
│ CLI (clap)                                                    │
│ tinyloop run "task" --cwd DIR --context 8192 --mode ask       │
└───────────────────────────┬───────────────────────────────────┘
                            v
┌───────────────────────────────────────────────────────────────┐
│ CLARIFY  (once, before any tool runs; Rust CLI, not a tool)    │
│ • collect all ambiguities into one question batch             │
│ • write machine-decidable DONE condition                       │
│ • write BOUNDARIES (what it must NOT do)                       │
│ • you confirm; no mid-run clarifications                       │
└───────────────────────────┬───────────────────────────────────┘
                            v
┌───────────────────────────────────────────────────────────────┐
│ LOOP ENGINE  (Rust state machine)                              │
│  PLAN → ACT → OBSERVE → VERIFY → DONE | ESCALATE               │
│  BudgetGovernor: steps ≤ 12, tokens ≤ N, wall clock ≤ 10min    │
│  Repeat guard: nudge at 3 identical calls, hard stop at 4      │
└──────┬──────────────────┬──────────────────┬───────────────────┘
       │                  │                  │
       v                  v                  v
┌──────────────┐  ┌────────────────┐  ┌──────────────────────┐
│ ModelClient  │  │ ToolRegistry   │  │ ContextManager       │
│ (HTTP)       │  │ (sandboxed)    │  │ (8K budget)          │
└──────┬───────┘  └───────┬────────┘  └──────────┬───────────┘
       │                  │                      │
       v                  v                      v
 llama-server        filesystem / shell     .tinyloop/state.md
 --jinja :9941       (cwd-confined)         audit.jsonl
```

---

## 4. Components

### 4.1 CLI

```
tinyloop run "find why calc_total uses 8% tax and patch it"
    --cwd C:/work/myrepo
    --model C:/models/Spark-X2.6-4B-Q4_K_M.gguf
    --context 8192
    --mode ask            # dry-run | ask | auto
    --max-steps 12
    --verify "cargo test"
```

Config defaults in `%APPDATA%/tinyloop/config.toml` (Windows) / `~/.config/tinyloop/config.toml` (Unix). Flags override config.

### 4.2 ModelClient

- **Trait** `ModelClient`; v1 implementation `LlamaCppClient`, later thin `MlxClient` (same OpenAI-style endpoint on Apple Silicon).
- Endpoint `POST http://127.0.0.1:9941/v1/chat/completions`. Exact server flags in [`MODEL_PROFILE.md`](MODEL_PROFILE.md).
- Request essentials (frozen for the whole session):
  - `tools`: the 7 schemas from §4.3, byte-identical names/descriptions/parameters/order
  - `tool_choice: "auto"`, `parallel_tool_calls` left off
  - vendor sampling: `temperature: 1.0`, `top_p: 0.95`, `top_k: 0`, `repeat_penalty: 1.0` — determinism comes from the verifier, not from decoding
  - `max_tokens: 512`
  - `chat_template_kwargs: {"enable_thinking": false}`
  - tool argument values sent as **JSON objects**, not JSON strings (the template requires dicts)
- Response handling:
  - `message.tool_calls` → validate schema → one call to ToolRegistry
  - text only → a **proposal**, never success; require explicit `submit_plan` / `finish`
  - empty final / `finish_reason: "length"` with no content → **failure with bounded retry**, never a fabricated answer (OpenClaw rule)
  - `usage.prompt_tokens`, `completion_tokens`, `timings.cache_n` → BudgetGovernor + audit log
- **Startup capability probe** (before the loop):
  1. `GET /health`
  2. version/build check (`spark2_5` supported, `--jinja` active)
  3. fixed prompt with and without `tools`; compare `prompt_tokens` — if identical, tools were silently dropped → abort
  4. one tool-call round-trip with a harmless read-only tool; if it fails, abort
- **Bounded recovery for weak calls:**
  - malformed call, no side effects begun → one continuation from current transcript
  - tool call printed as prose → one explicit native correction request; a second occurrence is a visible failure
  - output-limit stop mid-tool-call → finish already-admitted tools, never execute the truncated call

### 4.3 ToolRegistry — 7 tools, risk-tiered

| Tool | Risk | Execution | Notes |
|---|---|---|---|
| `submit_plan(steps[3..6])` | low | auto | Rust validates step count (`3..=6`) and that each step maps to one of the 7 declared tools |
| `read_file(path, offset, limit)` | low | auto | UTF-8 file reader (`offset`, `limit <= 400`, default 160 lines); caps excerpt via `ToolContext::context_tokens` + artifact spill |
| `search_files(pattern, path, max_results)` | low | auto | Literal case-insensitive workspace search, `<= 50` hits, skips `.git`/`target`/`node_modules`/`dist`/`.tinyloop` and files > 1 MB |
| `run_command(command, timeout_sec)` | **high** | approval | Allowlisted binaries (`cargo`, `git`, `rg`, `pnpm`, `npm`, `node`, `python`, `python3`), 30 s default (120 s max), pipe-drained (`2 MiB` capture cap/stream) |
| `apply_patch(path, old_string, new_string)` | medium | approval | Unique exact string replacement inside canonicalized workspace (symlinks rejected) |
| `finish(summary)` | low | auto | Requests deterministic external verifier run; only verifier pass yields `Done` |
| `escalate(reason)` | low | auto | Explicit stop handing control back to the human |

Clarification is handled before the loop (`RunConfig` / desktop Clarify modal), not by an `ask_user` tool — that keeps the 7-tool budget and front-loads ambiguity.

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
- **Context-derived live caps:** `ToolContext` carries `context_tokens` (set by `run_agent` from `RunConfig::context_tokens` via `with_context_tokens`, no hardcoded `8192` inside `finish_with_artifact`), passed into `observation::resolve_tool_result_budget(ctx.context_tokens)`. At `8192` tokens this yields `max_chars = 9,828` chars per result and `max_context_chars = 8,192` total observation chars. Truncation is **head + tail** (UTF-8 char-boundary safe) with `[... N chars omitted; full output at <path> ...]` pointing to `.tinyloop/artifacts/tool-<ts_ms>.txt`.
- Tool output is treated as **untrusted data**, never as instructions.

**Two-layer permission gate** (per OpenClaw):
1. **Capability / Mode** — `RunMode::DryRun` blocks mutations/commands; `apply_patch` and `run_command` in `RunMode::Ask` require interactive approval.
2. **Allowlist + approval** — `deny` is the fallback; approval binds `sha256(tool + "\0" + canonical_json(args))` shown to the user in the inspector card.

**Sandbox rules**

- `ToolContext::root` (`--cwd`) is the root; all paths are canonicalized and rejected if they escape it, and `resolve_for_write` rejects writes through **any** symlink (via `symlink_metadata`).
- Command policy (`src/tools/shell.rs`): rejects shell metacharacters, validates quotes via `split_command`, blocks destructive `git` subcommands (`push`, `reset`, `clean`, `rebase`, `filter-branch`, `restore`, `commit`, `config`, `remote`, `checkout --`) and escaping flags (`-C`, `--git-dir`, `--work-tree`, `-c`), blocks `python -m http.server`, stdin `-`, default `python -c` (unless `allow_inline_code`), and inline `node` flags, and requires script paths to canonicalize inside the workspace. Environment is cleared (`env_clear()`) before spawning, keeping only safe OS/PATH vars.
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

Phase 2 Context Rules (`src/context.rs` + `src/loop_engine/engine.rs`):

- **Prefix is frozen & single-system-message invariant**: `messages[0]` (`role: "system"`) is kept untouched for the entire run — no clock, no random IDs, no tool reordering, and **never** a second `role: "system"` message. Every prefix reset (e.g., `"compaction"`) is logged as a `prefix_reset` event in `.tinyloop/audit.jsonl`.
- **Real token measurement (fallback only when `== 0`)**: `effective_prompt_tokens(messages, reported_prompt_tokens, new_turn_tokens)` uses `(pt as usize).saturating_add(new_turn_tokens)` whenever `reported_prompt_tokens` is `Some(pt)` with `pt > 0`. It falls back to `estimate_messages(messages) + TOOL_SCHEMA_TOKENS` (`1200`) **only** when `reported_prompt_tokens` is `None` or `Some(0)` (so naive `chars / 4` never triggers premature compaction over real server counts).
- **Two-tier context protection (`75% → 85%`)** via `manage_context`:
  1. **Tier 1 — Block Observation Masking (`MASK_AT = 0.75`, ~6,144 tokens at 8K)**: Preserves the last `KEEP_RECENT_TURNS = 2` atomic turns verbatim; in one single batch across all older `role: "tool"` messages (index `>= 1`), parses `ToolResult` JSON and replaces non-empty `stdout_excerpt` (or non-JSON text) with `"[omitted — see summary and artifact path]"` (`MASKED_EXCERPT_MARKER`), keeping `status`, `summary`, `artifacts`, `next_actions`, `exit_code`, and `truncated` intact.
  2. **Tier 2 — Full Deterministic Compaction (`COMPACT_AT = 0.85`, ~6,963 tokens at 8K, or when Tier 1 masks 0 turns / leaves tokens `>= 75%`, or on observation-budget overflow)**:
     - Increment `state.compactions += 1` **before** calling `write_state(root, state)` and `state.render()`, so `.tinyloop/state.md` reflects the true count.
     - Persist `.tinyloop/state.md` first (`goal`, `done`, `boundaries`, up to 16 recent `facts`, `next_step`, `compactions`).
     - Slice `KEEP_RECENT` (`4` messages minimum) walking backward in **atomic turn units**, pairing each `role: "tool"` message (including consecutive tool results) with its owning `role: "assistant"` (`tool_calls`) message so the kept tail never starts with an orphaned `role: "tool"` message.
     - Insert the deterministic recap as a `role: "user"` message: `[Compacted state — prior turns persisted to .tinyloop/state.md]\n...`.
- **Observability & Metrics**: `RunReport` tracks both `masked_turns` and `compactions`; `AuditLog` records `compaction` and `prefix_reset` entries; `timings.cache_n` / `usage.prompt_tokens_details.cached_tokens` measure prompt-cache reuse.
- Full raw tool output goes to `.tinyloop/artifacts/tool-<ts_ms>.txt`; the model re-reads slices with `read_file(offset, limit)`.

### 4.5 LoopEngine

States: `Clarify → Plan → Act → Observe → Verify → Done | Escalate`.

- **Plan:** `submit_plan(steps[3..6])`; Rust checks the count and that each step maps to a known tool. Invalid plan → one repair attempt → escalate.
- **Act:** exactly one tool call per turn; `parallel_tool_calls` off.
- **Observe:** bounded, sanitized result appended; artifact path recorded.
- **Verify:** Rust runs `verify_cmd` (or checks files/content/diff scope). The model cannot edit acceptance conditions. Pass → `Done`; fail → failure reason returned to Plan. **3 failed attempts → `Escalate`.**
- **Damping:** step cap 12, token cap, wall-clock cap 10 min, no-op detector.
- **Repeat guard:** identical `(tool, canonical args)` calls get a system nudge on the 3rd and a hard `escalate` on the 4th (tighter than OpenClaw's 3/20 because our step cap is 12). New user input resets the streak.
- **No-answer is failure:** empty final or tool-free text that never calls `finish`/`escalate` is a failed step, not a done state.
- **Human last switch:** in `ask` mode, medium/high-risk tools prompt you; `finish` produces a result, never an irreversible side effect.

### 4.6 Verification / goal definition

A goal is accepted only if a machine can answer yes/no in one command:

- ✅ "`cargo test` exits 0 AND `src/tax.rs` contains `fn calc_total` AND `git status --porcelain` shows changes only under `src/`"
- ✅ "`report.csv` exists, header `a,b,c`, exactly 12 data rows"
- ❌ "make the code better", "check if it looks ok"

Ship **boundaries** with every goal (what it must NOT do) — the anti-Goodhart guard. Prefer reconciliation against an external reference over the agent's own assertions. When a structured final answer is needed, use a **separate tool-free request** with `response_format` JSON schema (never combine grammar with tools on this model).

---

## 5. Safety & observability

- **Modes:** `dry-run` (print calls, execute nothing), `ask` (default), `auto` (explicit opt-in, allowlist only).
- **Audit:** `.tinyloop/audit.jsonl` — timestamp, phase, tool, bound args, content hash, result, tokens, cache hits, approval decision, prefix-reset reason.
- **Stop:** Ctrl-C halts at the next step boundary; no half-applied patch.
- **Trace:** `tinyloop trace <run-id>` replays the JSONL; `--explain` prints the model's short rationale per action.
- **Eval:** `tests/eval/tasks/*.toml` with decidable goals and an **oracle solution**. Grade the **final sandbox state** with tests, not the agent's console narrative (Terminal-Bench/SWE-bench pattern). Track tool-call validity separately from task success; log retries/task and tokens per successful task. v1 target: ≥8/10 on read-only repo-investigation tasks with 8K context and thinking off.

---

## 6. Deliberately NOT copied from the researched systems

- DSH's Cordis profile/bundle/plugin tree and hook bridge — Node/Cordis-specific; implement the equivalent checks as Rust code at explicit pipeline stages.
- Claude-Code slash commands, subagent dispatch surfaces, and always-loaded 300+ skill catalogs.
- Cloud-model routing (Opus/Sonnet/Haiku tiers) — there is one 4B model here; context isolation is achieved with fresh sequential phases and summary files instead.
- DeepSeek server-side disk-cache economics — local llama.cpp has different flags and determinism trade-offs.
- Permissive multi-format tool-output parsers: one canonical native channel plus strict validation is the primary path.

---

## 7. Suggested Rust crates

| Need | Crate |
|---|---|
| CLI | `clap` |
| Async HTTP | `tokio`, `reqwest` |
| Tool schemas / JSON | `schemars`, `serde`, `serde_json` |
| Sandboxed paths | `cap-std` |
| File search | `ignore` |
| Approvals | `dialoguer` |
| Logging | `tracing`, `tracing-subscriber` |
| Config | `toml`, `dirs` |
| Hashes (audit) | `sha2` |

First modules already adapted from audited OSS code: `src/tools/observation.rs` and `src/loop/repeat_guard.rs` (see [`REUSE_REGISTER.md`](REUSE_REGISTER.md) for sources and licenses).

Deferred: `llama-cpp-2` for in-process inference. HTTP first decouples the agent from fast-moving llama.cpp bindings.

---

## 8. Project layout

```
tinyloop/
├── README.md
└── docs/
    ├── ARCHITECTURE.md          ← this file
    ├── MODEL_PROFILE.md         ← exact llama-server command, flags, GGUF files
    └── research/
        ├── INDEX.md
        ├── openclaw.md
        ├── deepseek-harness.md
        ├── small-model-harnesses.md
        └── model-runtime.md
src/                              (to be created in Phase 1)
├── main.rs, cli.rs, loop_engine.rs
├── model/{mod.rs, llama_cpp.rs, mlx.rs}
├── tools/{mod.rs, registry.rs, fs_tools.rs, shell.rs}
├── context.rs, verify.rs, budget.rs, audit.rs
tests/eval/tasks/*.toml
.tinyloop/                        (runtime, gitignored)
├── state.md, artifacts/, audit.jsonl
```

---

## 9. Roadmap

| Phase | Deliverable |
|---|---|
| 0 | Research + design docs ✅ |
| 1 | Cargo skeleton, `ModelClient`, `/health` + tool-capability probe against Spark-X2.6-4B Q4 @ 8K |
| 2 | Read-only tools, CLARIFY→PLAN→ACT→OBSERVE, deterministic verifier; 5 repo-Q&A tasks |
| 3 | Write/patch tools behind approval, sandbox hardening, audit JSONL |
| 4 | Budget governor, prefix-stability checks, 10-task eval suite with oracle |
| 5 | Optional MLX backend, in-process `llama-cpp-2`, TUI progress |

---

## 10. Open questions before Phase 1

1. Which GGUF exactly — official `XHToken/Spark-X2.6-4B-Q4_K_M.gguf` (3.54 GB / 3.30 GiB) or a smaller community Q4_K_M (2.6 GB)? Give the local path.
2. Is llama.cpp installed on this PC, and what build? Must be ≥ b10828 for `spark2_5`.
3. GPU (CUDA/Metal) or CPU-only? Affects whether 8K feels fast.
4. Does `enable_thinking: false` still produce reliable tool calls, or is `reasoning_effort: low` needed? The probe will answer this.
5. First eval domain: repo maintenance or general file/data tasks?

---

## Sources

Primary research is in [`docs/research/`](research/INDEX.md) with 140+ pinned citations:

- OpenClaw: canonical `openclaw/openclaw`, commit `58c8966…` — [`research/openclaw.md`](research/openclaw.md)
- DeepSeek Harness + ECC — [`research/deepseek-harness.md`](research/deepseek-harness.md)
- pi-mono, Codex CLI, Aider, OpenCode, Goose, SWE-agent, 12-Factor Agents — [`research/small-model-harnesses.md`](research/small-model-harnesses.md)
- Spark-X2.6-4B GGUF/llama.cpp/MLX primary sources — [`research/model-runtime.md`](research/model-runtime.md), [`MODEL_PROFILE.md`](MODEL_PROFILE.md)

*No benchmark numbers in these docs are invented; quoted figures come from cited public sources and should be re-measured on your hardware.*
