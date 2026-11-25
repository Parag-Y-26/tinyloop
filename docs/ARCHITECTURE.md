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
| `submit_plan(steps[3..6])` | low | auto | Rust validates count and tool mapping |
| `read_file(path, offset, limit)` | low | auto | ≤8K chars excerpt + artifact path; also lists directories |
| `search_files(pattern, path, glob)` | low | auto | ripgrep-style, ≤50 hits, ranked |
| `run_command(cmd, cwd, timeout)` | **high** | approval | allowlisted binaries, 30 s default |
| `apply_patch(diff)` | medium | approval | unified diff only; preview first; lint before accepting |
| `finish(result, evidence)` | low | auto | structured final answer + verifier evidence |
| `escalate(reason, evidence)` | low | auto | explicit stop to the human |

Clarification is handled by the CLI before the loop, not by an `ask_user` tool — that keeps the 7-tool budget and front-loads ambiguity.

Every tool result has one deterministic shape, sanitized **before** it enters the model transcript:

```json
{
  "status": "success | warning | error",
  "summary": "one-line result",
  "next_actions": ["..."],
  "artifacts": ["C:/.../.tinyloop/artifacts/07.txt"],
  "exit_code": 0,
  "stdout_excerpt": "...",
  "truncated": true
}
```

- Errors are returned as results (`status: "error"`, root-cause hint, safe retry, stop condition), never thrown into the transcript as exceptions.
- Empty output gets an explicit `"command produced no output"` marker; SWE-agent/OpenCode both do this to stop the model re-running a command.
- Only `stdout_excerpt` is tokenized; `artifacts` and internal details stay out of the prompt (OpenClaw's `details` rule).
- **Context-derived live caps at 8K:** per-result ≤ **9,828 chars** (`floor(8192 × 0.3) × 4`, capped at 16,000); total observation guard **8,192 chars** (`max(1024, 8192 × 2 × 0.5)`). Truncation is **head + tail** with an explicit `[N bytes omitted — full output at <path>]` marker; the transcript keeps the pointer, not the bulk.
- Tool output is treated as **untrusted data**, never as instructions.

**Two-layer permission gate** (per OpenClaw):
1. **Capability** — a denied tool's schema is not even sent for that turn.
2. **Allowlist + approval** — `deny` is the fallback; approval binds the exact args, canonical path, and content hash shown to the user.

**Sandbox rules**

- `--cwd` is the root; all paths canonicalized and rejected if they escape it (including via symlink).
- Deny-by-default: `sudo`, `rm -rf /`, `git push`, `git reset --hard`, `diskpart`, `format`, registry writes, etc. Each requires `--allow-high-risk` plus confirmation.
- No network tools unless `--allow-network` is passed. Environment scrubbed of credentials before `run_command`.
- Every call, approval decision, and result appended to `.tinyloop/audit.jsonl`.

### 4.4 ContextManager — the 8K budget

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

Rules:

- **Prefix is frozen**: no clock, no random IDs, no tool reordering, no mid-session prompt rewrite. Volatile facts are appended after the prefix. Every prefix reset is logged with a named reason (`compaction`, `config_change`, `process_restart`).
- Compaction triggers at **85% of window** (≈6,144 tokens) or at a phase boundary — whichever comes first, never mid-edit.
- **Write state to disk before compacting**; then replace old history with a bounded structured summary while keeping an exact recent tail. Reject a truncated summary and retry once.
- Compaction is deterministic Rust rendering of a structured state object — no extra LLM call, so a small model cannot lose facts in a summarization step. (If later a model-written summary is needed, make it a tool-free request with `response_format` and full tools forwarded to preserve prefix.)
- `.tinyloop/state.md` holds: goal, done-condition, boundaries, facts, decisions, next step, cut point.
- Prefix reuse is on by default in llama-server; verify via `timings.cache_n` / `usage.prompt_tokens_details.cached_tokens`. **Caution:** quantized local models plus caches are not bit-for-bit request-reproducible. For a verification pass that must be reproducible, pin cache flags or run that pass with caches disabled.
- Full output goes to `.tinyloop/artifacts/<seq>.txt`; the model re-reads a slice with `read_file(offset, limit)`.

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
