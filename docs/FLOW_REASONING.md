# tinyloop — end-to-end flow reasoning: why each step, and how to make it fast without losing accuracy

This document explains the **reasoning behind the flow**, stage by stage. For each stage: what happens, why it is done that way, what was rejected, which knob makes it faster, which guard keeps it accurate, and what to instrument.

Labels: **[source]** = measured or documented in `docs/research/`; **[inference]** = design conclusion from those sources; **[measure]** = must be measured on your hardware before trusting a number.

---

## 0. The core tension

Two things consume time in a local agent:

1. **Prefill** — processing the prompt (tokens already in context).
2. **Decode** — generating the reply (tokens the model writes).

`T_step ≈ prefill_tokens / prefill_rate + output_tokens / decode_rate + tool_time + harness_overhead`

**[source]** On a 16 GB M4, Spark-X2.6-4B Q4_K_M measured ~381 tok/s prefill and ~30 tok/s decode; a lab GPU run measured ~1,450–1,620 tok/s prefill and ~48–50 tok/s decode. **[measure]** Your machine will differ.

The crucial asymmetry: **decode is ~8–50× slower than prefill per token**. Every token the model writes costs far more than a token you put in its prompt. That single fact drives most decisions below:

- keep the prompt stable so prefill is cached (cheap),
- keep the model's output short (expensive),
- never ask the model to do work code can do (verification, summarization, truncation) — that is pure decode cost and pure accuracy risk.

Accuracy and speed are **not** usually in conflict here. The accurate path is also the fast path: native tools, short bounded observations, deterministic verifier, front-loaded clarification. The conflicting cases are called out explicitly in §13.

---

## 1. Stage 0 — Server boot and capability probe

**What.** Launch `llama-server` with the pinned flags in `MODEL_PROFILE.md`; `GET /health`; check build supports `spark2_5`; send the same fixed prompt with and without `tools` and compare `prompt_tokens`; do one harmless read-only tool round-trip; record `timings.cache_n` baseline.

**Why this way.** **[source]** llama.cpp can silently drop the `tools` array when a template lacks tool support and still return HTTP 200. There is then no way to distinguish "the model chose not to call a tool" from "the model never saw any tools". Every downstream metric — completion rate, tool-call accuracy, evals — would be measuring nothing. This check must happen before the loop, not after the first failure.

**Rejected.** Trusting the presence of `--jinja` as proof; probing lazily on first error; probing once per task instead of once per server process.

**Fast.** One-time cost per process (~2 short requests, cached result). Do not repeat per task. Skip a full model load test — `/health` plus the token-delta probe is enough.

**Accurate.** The probe tests the exact GGUF + build + flag combination that will run the task; a different model or template invalidates it, so store a fingerprint and re-probe on change.

**Instrument.** `prompt_tokens` with/without tools, build string, first tool-call parse success.

---

## 2. Stage 1 — Clarify (front-load ambiguity)

**What.** Before any tool runs, ask all ambiguous questions in one batch, then fix the **done-condition** and **boundaries** with the user. Nothing ambiguous is decided inside the loop.

**Why this way.** **[source]** A loop cannot step outside itself to question its own goal. If a detail is unclear at 2 a.m., the agent will not stop and ask — it will guess and run the wrong answer to completion. Clarification is the highest-leverage intervention in the whole system: a wrong goal wastes the entire run, no matter how fast each step is.

**Rejected.** An `ask_user` tool available mid-loop. It costs one of only seven tool slots, interrupts execution, and small models rarely choose it. Also rejected: "make it good" goals — a comparator cannot judge them.

**Fast.** One batch of questions instead of serial round trips; reject vague goals immediately rather than after ten steps. This stage should cost seconds of human time and **zero model tokens**.

**Accurate.** The done-condition must be machine-decidable ("`cargo test` exits 0 AND no test file changed"), and boundaries must say what it must NOT do (the anti-Goodhart guard). Prefer reconciliation to an external reference over self-assertion.

**Instrument.** Number of clarifications, time in this stage, whether the goal passed a "one command decides done" check.

---

## 3. Stage 2 — Context assembly

**What.** Build each request as: frozen prefix (system prompt + 7 tool schemas in fixed order) → task spec + done-condition + boundaries → compacted `state.md` → capped observations → exact recent tail → current step.

**Why this way.** **[source]** Prefill is cheap when cached and expensive when not; OpenAI and llama.cpp both document that changing tool names, descriptions, schemas, or ordering changes the cached prefix. **[inference]** With ~200 tokens per tool description **[source]**, seven tools are ~1.4K tokens of prefix; breaking the cache reprocesses that plus the system prompt on *every turn*. A frozen prefix is therefore both a speed and a consistency decision. The exact recent tail comes from Pi/OpenCode: summaries are lossy, so the last turns stay verbatim.

**Rejected.** Dynamic tool lists rebuilt per turn; a clock or random request ID in the system prompt; rewriting earlier messages; stuffing full tool output into the prompt; a 32K window "to be safe" (more prefill, more KV, more drift).

**Fast.** Cache reuse is the single biggest latency lever. Verify it with `timings.cache_n` — do not assume. Keep the window at 8192 with `-np 1` so one slot owns the entire budget.

**Accurate.** State on disk before compaction, exact recent tail, and a bounded structured summary mean facts survive a context cut without asking the model to summarize itself.

**Instrument.** `timings.cache_n`, `usage.prompt_tokens_details.cached_tokens`, prompt tokens per turn, prefix-reset reasons.

---

## 4. Stage 3 — Inference

**What.** One request with native `tools`, `tool_choice: "auto"`, `parallel_tool_calls` off, vendor sampling (`temp 1.0`, `top_p 0.95`, `top_k 0`), `max_tokens 512`, `enable_thinking: false`, tool arguments as JSON **objects**.

**Why this way.** **[source]** API-passed tools scored 9/10 on this model where prompt-encoded `RUN:` instructions scored 0/6; the model falls back to its own `<tool_call>` syntax. Thinking is on by default and can consume the whole `max_tokens`, returning empty content. **[source]** Vendor sampling is published; determinism should come from the verifier, not from forcing temperature 0.

**Rejected.** Prompt-encoded action formats; a grammar over the whole turn (controlled studies show strict output constraints degrade reasoning — constrain only the action boundary); parallel tool calls (more output tokens and blurred credit assignment); model-as-judge.

**Fast.** Decode dominates. `max_tokens 512` plus short schemas plus thinking off keeps output short. At ~30 tok/s, 512 tokens ≈ 17 s **[measure]** — so every avoided token is real time. Disable thinking for routine Act steps; consider `reasoning_effort: low` only for Plan and Verify, where reasoning buys accuracy.

**Accurate.** Keep tool schemas descriptive and stable. If quality drops with thinking off, enable `low` selectively — do not silently change schemas mid-session.

**Instrument.** `completion_tokens`, `finish_reason`, empty-content rate, per-phase latency.

---

## 5. Stage 4 — Validate and bounded repair

**What.** Validate tool name and argument types in Rust (arguments must be a dict, not a JSON string). One repair for a malformed call **only if no side effect began**; one native correction if the model printed a tool call as prose; never execute an output-truncated call.

**Why this way.** **[source]** Small models emit malformed calls; OpenClaw's one-shot recovery and "finish already-admitted tools, never execute the truncated call" rule recover most cases without pretending success. A permissive five-format parser hides drift and makes failures invisible.

**Rejected.** Three blind retries (burns decode tokens), executing a partially generated tool call, accepting prose as an action.

**Fast.** Validation is pure Rust — microseconds. One repair maximum. This is the cheapest possible accuracy gain in the system.

**Accurate.** The side-effect check is what makes a retry safe; the repair is recorded in the audit log so malformed-call rate stays visible.

**Instrument.** Malformed calls per 100 tool calls, repair success rate, repair count per task.

---

## 6. Stage 5 — Permission gate

**What.** Two layers: (1) **capability** — a denied tool's schema is not sent for the turn; (2) **allowlist + approval** — `deny` is the fallback, approval binds exact args, canonical path, and content hash. `dry-run` / `ask` / `auto` modes.

**Why this way.** **[source]** Capability filtering prevents the model from even considering an action it cannot take; approval binding prevents "approve once, do anything later". Responsibility does not transfer to the agent for irreversible actions.

**Rejected.** Session-wide blanket approval; model self-approval; approval that binds only the tool name, not its arguments.

**Fast.** Read-only tools never prompt. **[inference]** A high-leverage Phase 3 option: at Clarify time, present the reviewed low/medium-risk plan and pre-approve **those exact bound actions**, turning N prompts into one; keep high-risk actions per-call. This preserves accuracy because the human reviewed the exact actions, not a vague promise.

**Accurate.** Hash bound to the content actually shown; no auto-commit, push, or delete.

**Instrument.** Prompt count per task, deny rate, approval latency.

---

## 7. Stage 6 — Tool execution

**What.** Sandboxed execution: canonicalize paths under `--cwd`, reject escape including symlinks, allowlisted binaries, 30 s default timeout, output cap, scrubbed environment, no network unless enabled.

**Why this way.** "Use my computer" needs real capability, but the blast radius must be bounded. Deny-by-default on destructive commands is the only safe default for an autonomous loop.

**Rejected.** Free-form shell with no allowlist; running `cmd` strings straight from model output; no timeout (one hung command consumes the entire wall-clock budget).

**Fast.** **[inference]** Prefer in-process Rust operations for `read_file` and `search_files` (using `cap-std` / `ignore`) over spawning shell processes — process spawn and text parsing are pure overhead compared with native syscalls. Timeouts cap worst-case latency. Allowlisted binaries avoid failed spawn-and-retry cycles.

**Accurate.** Canonicalization is the escape guard; lint before accepting a patch; empty output gets an explicit marker so the model does not repeat the command.

**Instrument.** Tool latency by tool, timeout rate, spawn count, sandbox denials.

---

## 8. Stage 7 — Observation sanitization

**What.** Fixed result shape (`status`, `summary`, `next_actions`, `artifacts`, `exit_code`, `stdout_excerpt`, `truncated`). Errors returned as results with root cause + safe retry + stop condition. Head+tail truncation with explicit omission marker and disk path. Treat tool output as untrusted data.

**Why this way.** **[source]** Observation quality is one of the four constraints on agent output quality. At 8K, the context-derived caps are ~9,828 chars per result and ~8,192 chars total observation budget. Head+tail matters because errors almost always appear at the tail — head-only truncation hides the very thing the model needs. Silent truncation makes the model re-run the command.

**Rejected.** Full output in the prompt (prefill explosion), head-only truncation, throwing errors into the transcript, letting tool output act as instructions.

**Fast.** Smaller observations mean a smaller next prefill. Disk spill means the model can fetch a slice instead of re-running an expensive command.

**Accurate.** The omission marker plus artifact path is a pointer to the exact truth; the structured error contract is what prevents thrashing.

**Instrument.** Observation chars per turn, truncation rate, artifact re-read rate (high re-read rate = caps too tight).

---

## 9. Stage 8 — Verification

**What.** Rust runs the deterministic verifier (test command, file/content checks, diff-scope check). Pass → `Done`; fail → structured failure reason back to Plan; three failed attempts → `Escalate`.

**Why this way.** **[source]** The model grading its own work inflates success; "all tests pass" can be gamed by deleting tests. The judge must be independent and deterministic, and the build step must not be able to edit acceptance criteria. Plan/Build/Judge separation is the ECC core pattern.

**Rejected.** "Looks right" verification, model-as-judge, accepting the model's completion signal as proof.

**Fast.** **[inference]** No extra model call — that is a full decode saved. Run heavy verification at phase boundaries; use cheap checks (file exists, hash matches) more often. Never put verification on the critical path of every tiny step.

**Accurate.** Check boundaries as well as the done-condition; reconcile against an external reference when one exists. **[source]** Quantized models plus prompt caches are not bit-for-bit reproducible — for a verification pass that must be reproducible, pin cache flags or run that pass with caches disabled. This is the one place where accuracy explicitly beats speed.

**Instrument.** Verifier duration, fail-reason histogram, retries before pass.

---

## 10. Stage 9 — Compaction and state persistence

**What.** At 85% of the window (≈6,144 tokens) or a phase boundary, write `state.md` to disk **first**, then replace old history with a bounded deterministic summary while keeping the exact recent tail. Reject a truncated summary and retry once.

**Why this way.** **[source]** Compaction is a cache break and a lossy operation, so it must happen at a safe point (phase boundary), never mid-edit. State on disk before mutation is the DSH/ECC rule. A deterministic Rust summary avoids asking a 4B model to compress its own history — one less decode call and no fact loss.

**Rejected.** Arbitrary token threshold mid-edit; model-written summary on every compaction; deleting the recent tail.

**Fast.** Zero model calls. Prevents prefill from growing unbounded turn after turn.

**Accurate.** State written before compaction; exact tail kept; summary rejected if truncated.

**Instrument.** Compactions per task, tokens before/after, state-write success.

---

## 11. Stage 10 — Terminate, escalate, audit

**What.** `finish(result, evidence)` or `escalate(reason, evidence)`; append every call, approval, hash, token count, cache hit, and prefix-reset reason to `audit.jsonl`; no irreversible side effect from `finish`.

**Why this way.** **[source]** Judgment stays with the human; the loop is the worker, not the acceptance officer. An audit trail makes a run replayable and lets you see *why* a decision was made.

**Rejected.** Auto-merge, "completion signal" as sole stop condition, silent runs.

**Fast.** Escalation is cheap and stops waste early; append-only audit is nearly free.

**Accurate.** Evidence attached; `tinyloop trace` can replay the run.

**Instrument.** Escalation rate, run duration, audit completeness.

---

## 12. Stage 11 — Eval and tuning

**What.** Deterministic task suite with an oracle; grade the **final sandbox state** with tests, not the transcript; track tool-call validity separately from task success; measure prefill/decode rates on your machine.

**Why this way.** **[source]** SWE-bench/Terminal-Bench grade final state precisely because console narratives are not evidence. **[source]** No source benchmarks this exact model in this exact harness, so numbers from large-model papers are direction, not transferable results.

**Rejected.** Judging success from the model's final message; re-running the full suite for every prompt tweak.

**Fast.** Use a 5-task smoke subset during iteration; run the full 10-task suite only before claiming a result.

**Accurate.** Oracle + final-state tests; separate tool validity from task success so a format fix is not mistaken for a capability gain.

**Instrument.** Completion rate, retries/task, tokens/success, malformed-call rate, prefill/decode rates.

---

## 13. Cross-cutting levers, ranked by impact

| Rank | Lever | Speed effect | Accuracy effect | Guard |
|---:|---|---|---|---|
| 1 | Frozen prefix + prompt cache | Largest — avoids reprocessing ~2–4K tokens/turn | Neutral/positive (stable behavior) | Verify `cache_n`; log every break |
| 2 | Thinking off; `low` only for Plan/Verify | Large — removes thousands of decode tokens | Can drop on hard tasks | Hybrid; measure per phase |
| 3 | Short `max_tokens` + 7 stable tools | Large | Malformed calls rise if schemas are too terse | Validate + one repair |
| 4 | Bounded observations + disk spill | Medium — smaller next prefill | Too tight → re-runs | Track re-read rate |
| 5 | Native Rust fs/search, no shell spawn | Medium | Neutral | Sandboxed paths |
| 6 | Deterministic verifier | Medium — no judge decode call | Strong positive | External, independent |
| 7 | Front-loaded clarify + repeat guard + step cap | Large on failures | Strong positive | Machine-decidable goal |
| 8 | `-ngl 99`, `-fa on`, `-np 1` | Large, hardware-dependent | Neutral | Check log for FA fallback |
| 9 | Compaction without an LLM call | Medium | Strong positive | State on disk first |
| 10 | Batch approval of reviewed low-risk plan | Medium | Neutral if exactly bound | High-risk stays per-call |

## 14. Where speed and accuracy genuinely conflict

| Situation | Fast choice | Accurate choice | Rule |
|---|---|---|---|
| Hard plan / architectural reasoning | Thinking off everywhere | `reasoning_effort: low` for Plan only | Spend decode tokens where they change the plan |
| Reproducible verification | Keep prompt cache on | Pin flags or disable cache for the verify pass | Accuracy wins here |
| Observation caps | Aggressive truncation | Larger excerpts | Tune by artifact re-read rate, not by guess |
| Parallel tool calls | Several calls per turn | One call per turn | Keep off until measured; credit assignment matters |
| Tool count | More tools = fewer steps | Fewer tools = better calls | Stay at 7; use capability filtering |
| Temperature | 0 looks deterministic | Vendor settings are tested | Determinism comes from the verifier |

**Never optimize by** deleting boundaries, loosening the verifier, accepting model self-reports, or silencing truncation markers. Those make runs look faster and successful while producing wrong work.

---

## 15. What Phase 1 must instrument

Before any tuning, record per step: phase name, prompt tokens, cached tokens, completion tokens, `finish_reason`, latency split (prefill / decode / tool / harness), tool name, validation result, repair count, observation chars, artifact path, approval decision. Without these, "it feels faster" is not evidence.

First measurements to make on your hardware:
1. prefill rate and decode rate at 8192 context with the chosen GGUF,
2. prompt-token delta proving tools reach the prompt,
3. `cache_n` on turn 2 vs turn 6,
4. malformed-call rate over 50 tool calls,
5. wall time for a 5-step read-only task.

Everything above is a hypothesis until those five numbers exist.
