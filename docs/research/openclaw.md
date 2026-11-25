# OpenClaw architecture findings for `tinyloop`

**Scope.** Only architecture patterns transferable to a small Rust CLI agent driving a 4B local model through llama-server's OpenAI-compatible `/v1/chat/completions` tools API with an 8K context. Each substantive claim is cited with a numbered source; full URLs are in `## Sources`. Derived numbers are labelled *computed* and come only from the cited formulas. No code was changed outside this file.

**Which repo.** "OpenClaw" is ambiguous: the search results include many forks (`vercel-labs/openclaw`, `mitsuhiko/openclaw`, etc.) and an unrelated skill/plugin registry. I chose the canonical personal-AI-assistant framework `openclaw/openclaw` (MIT, self-hosted gateway + agent runtime), pinned at commit `58c8966332d1563e8508142394f53b14a31147c1` (2026-11-25) [1]. It is a TypeScript pnpm workspace; its Rust crates are support components (gateway client, node host, iroh), not the agent loop [1]. All paths below refer to that commit.

---

## 1. Agent loop / state machine and termination

- OpenClaw defines the loop as a **serialized, per-session run**: intake → context assembly → model inference → tool execution → streaming → persistence [2]. The gateway accepts the turn immediately with `{ runId, acceptedAt }`, then `agentCommand` → `runEmbeddedAgent` performs the work; `agent.wait` returns `ok | error | timeout` and does not cancel the run [2].
- Termination is **host-owned, not model-self-declared**: a confirmed final send/reply in the source conversation ends the run; progress sends (`final: false`) and unconfirmed actions do not [2][3]. The core loop stops when the provider reports `stopReason: "stop"`, no unexecuted tool calls remain, and no batch `terminate` hint is set; `toolUse` keeps it looping [4]. Early exits are enumerated: agent timeout, `AbortSignal`, gateway disconnect, wait timeout [2].
- **No-answer is a failure, never a fabricated success**: an empty final after bounded retries records a failed outcome and, for admitted user turns, posts one short system notice instead of inventing an assistant answer [3]. A tool-free finalization pass can recover from a settled tool batch that returned no composed text; it does not repeat completed tools [3]. Whole `NO_REPLY` is the explicit silence token [3].
- **Bounded recovery for weak tool calls** (highly relevant to a 4B model): (a) a provider-rejected malformed tool call can be continued *once* from the current transcript, but only when host evidence shows no side effects began [2]; (b) if the model prints a tool call as text, the runner can request *one* native correction, and a repeated text call becomes a visible failure instead of a fake success [2]; (c) an output-limit stop while generating a tool call finishes already-admitted tools and retries from recorded results — the unfinished call never executes [3].
- **Repeat guard**: identical consecutive calls (tool name + canonical args) get a system note on the 3rd call and a hard `tool_repeat_limit` stop on the 20th; different calls and new user input reset the streak, and polling tools declare `repeatable` to opt out [5]. Source constants: `TOOL_REPEAT_NOTE_AT = 3`, `TOOL_REPEAT_STOP_AT = 20` [5].
- Concurrency control: one lane per physical session plus an optional global lane; before streaming, the run records a durable `activeWriterRunId`, and every transcript append/rewrite must match it, so a superseded run cannot commit stale history [2].

**tinyloop inference:** keep Clarify→Plan→Act→Observe→Verify→Done|Escalate as a Rust enum-driven state machine; only a deterministic verifier or explicit user turn may emit `Done`. Reuse the 3rd-call nudge / 20th-call stop, one-shot malformed-call recovery, and "empty answer = failure" rule.

## 2. Tool registry shape, schemas, and result formatting/truncation

- A tool definition is `{ name, description, parameters }` with a JSON-Schema-compatible `TSchema` [6]. The runtime adds `label`, optional `outputSchema`, `prepareArguments` (a compatibility shim **before** validation), `execute(toolCallId, params, signal?, onUpdate?)`, `executionMode` (`sequential`/`parallel`), `interruptible`, and `repeatable`; the result is `{ content: (TextContent|ImageContent)[], details, progress?, terminate? }` [7][4]. `details` is structured state for logs/UI and is **stripped before provider conversion** — only `content` is tokenized [10].
- Tool results are normalized through `textResult` / `jsonResult` / `payloadTextResult`: `jsonResult` serializes the payload into text *and* keeps it as `details`; errors become `{ status: "error", tool, error, permissionMissing? }` rather than thrown into the model transcript [8]. Tool arguments are validated against the schema before execution, with `anyOf`/`oneOf` branch-aware type recovery (e.g. `"00123"` stays a string while `"2"` becomes `2`) [2][7].
- Policy is applied **before** the model call: a denied tool's schema is never sent for the turn [34]. Tool start/update/end events stream on a separate channel, and results are sanitized for size and image payloads before logging/emitting [2].
- **Context-derived live caps** (source formulas [9]): per-result cap = `min(floor(contextWindowTokens * 0.3) * 4, 16000)` chars; at 8,192 tokens this is **9,828 chars** (*computed*). A second context guard budget is `max(1024, floor(contextTokens * 2 * 0.5))` = **8,192 chars** at 8K (*computed*) [9]. Tool-result text is costed at **2 chars/token** and an image at 16,000 chars, so images are dropped first with an explicit `[N images omitted from context ... rerun with fewer images]` notice [10][13].
- **Truncation is head+tail, not head-only**: if the tail matches `error|exception|failed|fatal|traceback|panic|errno|exit code|exited with code|timed out|...|total|summary|result|done`, the code reserves up to `min(30% of budget, 4000)` chars for the tail and inserts `⚠️ [... middle content omitted — showing head and tail ...]`; otherwise it keeps a prefix cut at a newline boundary [11]. The default suffix is `[... N more characters truncated; rerun with narrower args if needed]` [12]. Oversized results can be spilled to disk with a durable pointer of the form `[... N chars truncated; full output at PATH]`, so the model can re-read selectively without replaying the command [11].
- Projections are **non-destructive**: truncation changes what the model sees, while the transcript keeps the full result; the aggregate projection has its own cap and protects the trailing tool results of the current turn [13][11].
- **Deferred schemas for small windows**: Tool Search exposes three compact controls (`tool_search`, `tool_describe`, `dispatch_action`) plus a capability directory capped at **18,000 chars**, sorted by name and cached above the prompt-cache boundary [14]. Direct-only tools stay visible; the directory scales with the model's context window and shortens descriptions before omitting names [14]. OpenClaw explicitly warns compaction is **not always cheaper** — small catalogs gain schema overhead and discovery turns can erase the saving — and notes that native Ollama keeps four tools direct while generic OpenAI-compatible transports use the general default [14].

**tinyloop inference:** with ≤7 tools, prefer all schemas direct (Tool Search's own warning), but budget them: OpenClaw's own `/context` example shows tool JSON schemas consuming ~8k tokens *before* the conversation [19]. Cache a stable directory prefix, validate arguments in Rust, and never let `details` leak into the 8K window. Truncate observations at the source with head+tail and an explicit omission marker; spill large output to a file and hand the model the path.

## 3. Context management and compaction

- Context = system prompt + history + tool calls/results + attachments; memory on disk is not context [19]. Bootstrap files are capped per file at 20,000 chars and in total at 60,000 chars, with a built-in truncation notice [19]. Skills are injected as **metadata only** (name/description/location) and loaded on demand [19].
- Prompt layout is split at a **cache boundary**: stable sections (workspace/project context, runtime instructions, execution policy) above it; volatile sections (tool list/schemas, skills, sandbox, per-turn guidance) below it, so a catalog refresh does not invalidate the cached workspace prefix [18]. Tool schema descriptors are immutable and reused while tools/config/model are unchanged; authority is recreated per attempt [18].
- Auto-compaction is on by default and also triggers on recognized context-overflow errors (including `ollama error: context length exceeded`), followed by a retry [15][16]. Maintenance starts at `min(85% of active window, window - compaction reserve)`; the reserve is capped at **25% of the window**, leaving at least 75% for prompts, and the default 20,000-token reserve applies only to windows ≥80,000 [15][17][16]. At 8K that is `min(6,963, 8,192 - 2,048) = 6,144` tokens (*computed*) [15][17].
- Summarizer input is capped at 160,000 chars (~40k tokens) **or less when the summarizer's own context is smaller**. When history exceeds it: newest messages verbatim get half the budget, older user asks/decisions/corrections a quarter (trimmed to 2,000 chars each), oldest a tenth, and evenly spaced runs of the rest trimmed to 6,000 chars; every gap is marked with its size, the summarizer is told not to guess omitted content and to keep prior summary facts, and CJK gets approximate token weighting [15]. Long tool results get a 2,000-char summary projection with an explicit truncation marker that keeps head and tail; over-cap histories use 500 chars [15].
- Split points **never separate a tool call from its `toolResult`** [15][16]. The persisted summary is capped at 16,000 chars by default; a deterministic, model-free record of recent user messages, frequent identifiers, and last replies is appended under its own ≤6,000-char budget [15]. Exact identifier retention is requested by default (`strict`) and quality-audited; degraded summaries are labelled `qualityDegraded` and warned about [15].
- Full history stays on disk; compaction only changes the next model view, and obsolete pre-boundary history is not reloaded [15]. A pre-compaction **memory flush** writes durable notes to workspace files; failure is non-fatal and never resets the session [15]. Built-in summarization rejects summaries cut off by the output token limit [15]. Native Ollama prefers `thinking: off` for summary calls to stay inside the smaller request budget [16].

**tinyloop inference:** compact deterministically in Rust before the model complains (6K of 8K), keep the exact recent tail and tool pairs intact, write a bounded structured summary (facts, decisions, pending ask, exact identifiers), and keep the full JSONL transcript on disk. Never let a 4B summarizer emit an unbounded summary; reject truncated output and retry once.

## 4. Permission / approval / sandbox model

- Host exec permission modes are a normalized pair of `security` (allowlist strictness) and `ask`: `deny`, `allowlist`, `ask` (`on-miss`), `auto` (`on-miss` + native auto-reviewer), `full` [20]. `askFallback` defaults to `deny` when no approver UI is reachable [21]. Effective policy is the **stricter** of tool config and the host-local approvals document; approvals can only tighten, never loosen [21][20].
- Approvals bind the exact execution context: executable real-path identity, content hash for writable executables, cwd/argv/env, and a best-effort single file operand whose drift denies the run [21]. A late approval cannot revive a closed turn [21].
- Sandboxing is **off by default**, is configured independently by mode (`off | non-main | all`), scope (`agent | session | shared`) and backend (Docker default), and is explicitly documented as "not a perfect security boundary" [22][24]. The gateway stays on the host; only tool execution (`exec`, `read`, `write`, `edit`, `apply_patch`, `process`, optional browser) moves inside [22][23]. Tool allow/deny policy is applied **before** sandbox rules, and `tools.elevated` is an explicit escape hatch that still cannot bypass a creator-role-required sandbox [22][23].
- For agents touching untrusted content, OpenClaw recommends denying `gateway`, `cron`, `sessions_spawn`, `sessions_send` and treating network tool results as a turn taint that must not grant new authority [25]. External content is wrapped in `<<<EXTERNAL_UNTRUSTED_CONTENT ...>>>` boundaries, and self-hosted chat-template special-token literals are stripped so fetched text cannot forge a `system`/`assistant` role boundary [28].

**tinyloop inference:** mirror the two-layer decision — capability (schema exposed or not) *then* runtime gate (allowlist, human approval for writes/exec, fallback = deny). Bind an approval to (tool, canonical args, workspace root, file hash) with a TTL, re-check before execution, and keep the verifier outside the model's control. Wrap all tool output as untrusted data, especially with a 4B model.

## 5. Session persistence and state files

- Runtime session rows and transcripts default to `~/.openclaw/agents/<agentId>/agent/openclaw-agent.sqlite`; archived transcripts live in `~/.openclaw/agents/<agentId>/sessions/` [26]. Separate timestamps matter: `sessionStartedAt` (daily reset), `lastInteractionAt` (idle reset), and `updatedAt` (bookkeeping only) [26].
- Session reuse is the default; daily/idle/manual (`/new`, `/reset`) resets roll the session id, but compaction manages the active context between resets [26]. Maintenance defaults are 30-day prune, 5,000 unarchived entries, archive-not-delete for eligible sessions, with active/pinned sessions protected [26].
- Graceful shutdown writes a small marker per running root session; startup clears it and sends **one** ordinary resume input asking the agent to inspect files/processes/sub-agents before repeating effects. A crash uses the persisted nonterminal run identity to resume once [26].
- Cross-actor coordination uses a durable signal log with **one-line summaries only, never message content**; watchers get one coalesced notice and reconcile via `changesSince` [27]. History is bounded to 30 days / 50,000 rows and heads stay monotonic after pruning [27].

**tinyloop inference:** one JSONL transcript per session (append-only, `tool_call_id`-linked), one small state file with run id, phase, budget counters and compaction boundary; on restart, write a resume marker and ask the model to *inspect before repeating*. Keep any status log metadata-only.

## 6. Small/local models and tool-call reliability

- OpenClaw's guidance is blunt: smaller/heavily quantized models are more vulnerable to prompt injection; use large models for tool-access bots, and if a small model is unavoidable, enable sandboxing and strict tool allowlists [29]. "Prompt-injection risk with older/smaller models is often too high. Do not run those workloads on weak model tiers"; if forced, prefer read-only tools, strong sandbox, minimal filesystem access, and disable web/browser tools [28]. Chat-only small models with trusted input are fine [30][28].
- Format fragility is acknowledged in the loop itself: "Smaller models can be unreliable at finding a fact that is only in the attachment" when an oversized request is moved to `current-request.txt` [2]. Tool calls emitted as prose, malformed calls, and tool calls truncated by the output limit each get narrowly scoped, bounded recovery rather than permissive re-parsing [2][3].
- A dedicated lean surface exists for local/small models: `--local-model-lean` removes high-latency/channel-dependent tools and enables bounded Tool Search defaults whose directory lists names and owners but not descriptions [31]. One-shot `agent exec` also lets the caller force direct vs. code-mode tool surface for model comparison [31].
- The OpenAI-compatible HTTP surface documents the exact protocol tinyloop uses: `finish_reason: "tool_calls"`, `tool_calls[].function.arguments` as a JSON string, then a follow-up request carrying the assistant tool-call message plus `role: "tool"` messages with matching `tool_call_id`; **even an empty result must be sent** (`content: ""` or empty text parts) because omitting it does not complete the call [32].

**tinyloop inference:** treat reliability as a harness property. Probe at startup that llama-server actually honoured the `tools` array, keep tool names/descriptions/order frozen, send one call per turn, always return a tool result (even empty), and put every recovery decision behind a bounded Rust retry counter.

---

## Top 8 transferable ideas (ranked for `tinyloop`)

1. **Host-owned state machine**: Rust decides Done/Verify/Escalate; text from the 4B model is never completion evidence. Model-only `NO_REPLY`-style silence and empty finals become failures with a short notice [2][3][4].
2. **Bounded malformed-call recovery**: one continuation for a rejected call with no side effects, one native correction for a printed-as-text call, and never execute an output-truncated tool call [2][3].
3. **3rd-call nudge / 20th-call hard stop** on identical `(tool, canonical args)`; polling tools opt out explicitly [5].
4. **Context-derived result budgets**: at 8K, ~9.8k chars/result and ~8.2k weighted context chars from OpenClaw's formulas; head+tail truncation with an explicit omission marker, disk spill with a path pointer, full transcript untouched [9][10][11][13].
5. **Compact at ~6.1k tokens** (85% / window−25%-cap formula), cut only between tool-call/tool-result pairs, keep a bounded structured summary plus recent tail, reject truncated summaries, and keep full history on disk [15][16][17].
6. **Stable-prefix / volatile-suffix prompt layout** with immutable, frozen tool schemas above the cache boundary and per-turn context below it; budget the schemas themselves (~8k tokens in OpenClaw's own example) [18][19].
7. **Two-layer permission gate**: capability first (no schema if denied), then allowlist + approval with `deny` fallback and `(tool, args, path, hash)` binding; wrap all tool output as untrusted data [20][21][25][28].
8. **Durable, minimal state**: append-only transcript + small state file + resume marker; on restart, inspect before repeating effects; keep watcher/status logs metadata-only [26][27].

## What not to copy

OpenClaw's gateway/WebSocket multi-channel control plane, plugin/MCP catalogs, sub-agent swarms, code mode, and safeguard quality-audit loops are far beyond a solo Rust CLI at 8K [33][14][34]. Its own docs warn that even Tool Search can cost more than direct schemas for small catalogs [14], that sandboxing is not a perfect boundary [22], and that weak tool-enabled models are a known high-risk configuration [28][29].

---

## Sources

All URLs are pinned to the cloned commit `58c8966332d1563e8508142394f53b14a31147c1` unless noted.

1. Repository root / README — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/README.md ; repo identity — https://github.com/openclaw/openclaw
2. Agent loop — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/concepts/agent-loop.md
3. Agent loop, reply shaping and compaction/retries — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/concepts/agent-loop.md#reply-shaping
4. Core loop (`packages/agent-core/src/agent-loop.ts`) — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/packages/agent-core/src/agent-loop.ts
5. Repeat guard source + doc — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/packages/agent-core/src/tool-repeat-guard.ts ; https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/tools/loop-detection.md
6. `Tool` interface (`packages/llm-core/src/types.ts`) — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/packages/llm-core/src/types.ts
7. `AgentTool` / `AgentToolResult` (`packages/agent-core/src/types.ts`) — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/packages/agent-core/src/types.ts
8. Tool result helpers — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/src/agents/tools/tool-results.ts
9. Live tool-result caps — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/src/agents/tool-result-limits.ts
10. Char cost estimator — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/src/agents/embedded-agent-runner/tool-result-char-estimator.ts
11. Truncation implementation — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/src/agents/embedded-agent-runner/tool-result-truncation.ts
12. Truncation notice text — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/src/agents/embedded-agent-runner/context-truncation-notice.ts
13. Mid-turn tool-result context guard — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/src/agents/embedded-agent-runner/tool-result-context-guard.ts
14. Tool Search — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/tools/tool-search.md
15. Compaction — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/concepts/compaction.md
16. Compaction reference — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/reference/session-management-compaction/compaction.md
17. Compaction reserve cap — https://raw.githubusercontent.com/openclaw/openclaw/58c8966332d1563e8508142394f53b14a31147c1/src/agents/agent-compaction-constants.ts
18. System prompt / cache boundary — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/concepts/system-prompt.md
19. Context accounting — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/concepts/context.md
20. Permission modes — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/tools/permission-modes.md
21. Exec approvals — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/tools/exec-approvals.md
22. Sandboxing — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/gateway/sandboxing.md
23. What gets sandboxed — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/gateway/sandboxing/what-gets-sandboxed.md
24. Sandbox modes/scope/backend — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/gateway/sandboxing/modes-scope-and-backend.md
25. Tool and agent permissions — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/gateway/security/tool-permissions.md
26. Session management — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/concepts/session.md
27. Session state awareness — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/concepts/session-state.md
28. Prompt injection / external-content wrapping — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/gateway/security/prompt-injection.md
29. FAQ: self-hosted models and small-model risk — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/help/faq-models.md
30. FAQ: cheaper models / access control — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/help/faq/security-and-access-control.md
31. CLI `agent` / `--local-model-lean` — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/cli/agent.md
32. OpenAI-compatible HTTP API / tool follow-up loop — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/gateway/openai-http-api.md
33. Architecture overview — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/concepts/architecture.md
34. Tools overview — https://github.com/openclaw/openclaw/blob/58c8966332d1563e8508142394f53b14a31147c1/docs/tools/index.md
