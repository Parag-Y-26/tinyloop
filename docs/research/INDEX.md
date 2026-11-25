# Research index

Deep-dive findings used to build `tinyloop`'s design. All four files carry numbered citations to primary sources; community measurements are labelled as such.

| File | What it covers | Most useful takeaway |
|---|---|---|
| [`openclaw.md`](openclaw.md) | Canonical `openclaw/openclaw` (commit `58c8966…`): loop, tools, context, permissions, persistence | Host-owned termination; empty final = failure; bounded recovery; repeat guard; context-derived result caps; two-layer tool gate |
| [`deepseek-harness.md`](deepseek-harness.md) | DeepSeek Harness (Cordis microkernel) + Everything Claude Code: hooks, skills, loops, quality gates, prompt caching | Machine-decidable goals + boundaries; plan/build/judge; phase-boundary compaction with state-on-disk first; append-only context; quality gates in code |
| [`small-model-harnesses.md`](small-model-harnesses.md) | pi-mono, Codex CLI, Aider, OpenCode, Goose, SWE-agent, 12-Factor Agents | Probe native tools; ~7 tool cap; constrain only the action boundary; bounded observations; exact recent tail; frozen prefix; bounded retry; final-state evals |
| [`model-runtime.md`](model-runtime.md) | Spark-X2.6-4B Q4_K_M: exact llama-server flags, thinking control, tool format, GBNF caveat, KV cache, GGUF files | llama.cpp ≥ b10828 + `--jinja`; `enable_thinking: false`; grammar ≠ tools; `-np 1`; keep `--swa-full` off |

## Consolidated rules adopted in `ARCHITECTURE.md` v0.2

1. Native `tools` + `--jinja`, with a startup capability probe (silent tool-drop is real).
2. Rust-owned state machine; only deterministic verifier or user turn can produce `Done`.
3. Empty answer / truncated tool call = failure with bounded retry, never a fabricated success.
4. Repeat guard: nudge at 3 identical calls, hard stop at 4 (our 12-step cap).
5. Tool result caps derived from context: ~9,828 chars/result, ~8,192 chars total at 8K; head+tail truncation; disk spill.
6. Frozen prompt/tool-schema prefix; append volatile facts; log every prefix reset; `timings.cache_n` instrumentation.
7. Compact at 85% window or phase boundary, state on disk first, exact recent tail kept.
8. 7 tools max, one call per turn, dict arguments, error contracts with root cause + safe retry + stop condition.
9. Two-layer permission gate: capability (schema visibility) then allowlist + approval bound to args/path/hash.
10. Verifier is external and deterministic; eval grades final state with an oracle, not console narrative.

## Explicitly not transferred

DSH's Cordis plugin/hook tree, Claude-Code slash commands, cloud-model tier routing, server-side disk-cache economics, permissive multi-format tool parsers.
