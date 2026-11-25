# DeepSeek Harness (DSH) + Everything Claude Code (ECC): findings for tinyloop

Research date: 2026-11-25. Scope: what DSH/ECC actually are, which of their harness patterns are model-independent, hook-event bridging limits, and prompt-cache/context-stability evidence. Sources are primary docs in the `deepseek-ai/deepseek-harness`, `affaan-m/everything-claude-code`, `ggml-org/llama.cpp` repos plus the DeepSeek API docs; community reports are labelled as such. Citations are numbered and collected at the end. Where docs conflict across DSH versions, the conflict is stated rather than smoothed over.

## 1. What DSH actually is

**Definition.** DeepSeek Harness (`dsh`) is an open-source agent harness built on the general-purpose plugin framework **Cordis**: "everything is a plugin" — CLI, Web UI, tools, slash commands, skills, MCP connections and hook listeners are all Cordis plugin rows rather than separate manifest formats. A plugin is a JS/TS module exporting `apply(ctx, config)`; it becomes whatever it registers on `ctx` [1][2][7][38].

**Composition model.** A *profile* is a named runnable composition under `$DSH_HOME/profiles/<name>` (shipped templates: `web`, `headless`, `sdk`, `sdk-minimal`, `acp`). A *bundle* is an npm package that contributes an ordered Cordis config patch (`dsh.bundle.patch`); a profile manifest lists its bundles (`dsh.profile.bundles`) and owns a user `cordis.patch.yml`. Effective config composes over an empty root: bundle patches in list order → profile patch → `$DSH_HOME/cordis.patch.yml` → each `--patch` overlay in argv order. Later layers win **per row**; a patch replaces the whole `config` of the targeted row (no deep merge). `--dump-config` shows the composed tree; `dsh plugin --profile <p> add|remove|why|update` forwards to pnpm and reconciles bundle declarations [1][2][3].

**Loop spine.** The default loop lives in `agent`/`agent-loop`: claim queued prompt → open turn on an append-only `SessionEvent` log → assemble system prompt + tool schemas → derive messages from the log → stream model response → dispatch tool calls through a guarded pipeline → append model-visible facts back before the next step. The loop is swappable; extensions depend on the public `Agent` contract, not on the concrete loop [5]. Tool execution is a fixed graph: `tools/pre-execute` waterfall → monotonic guards → `ctx.approval` (deny if unanswered) → `tools/execute` → tool body → `fs/*-intent` gates → `tools/post-execute` waterfall → `tools/result` [6].

**Extension surfaces (all plugin registrations, no separate manifest language) [1][7]:**

| Surface | Mechanism |
|---|---|
| Model provider | `ctx.llm.registerAdapter()` (`dsh-llm-deepseek`, `dsh-llm-pi-ai`) |
| Tool | `ctx.tools.register()`; schema joins prompt assembly; `ctx.tools.restrict()` for progressive disclosure |
| Slash command / UI | `ctx.commands`; client `ConversationNodeDefinition` |
| Skill | section + tool registration; `inject()` on invocation |
| Hook / policy | typed listeners on `agent/created`, `agent/pre-step`, `tools/pre-execute`, `tools/post-execute`, `agent/turn-stopping` returning typed decisions |
| Goals / loops | `ctx.goals` + `dsh-goal-round-driver`; `/loop` = `followup()` on `turn/end` |
| Context compaction | `ctx.compaction` seam + `dsh-compaction-basic`; auto-pressure via `agent/pre-step`, overflow recovery via `agent/request-error` |
| Subagents | `ctx.subagents` provider registry (in-process spawn/fork, ACP, Codex, Claude Code, SDK) behind `dsh-tool-subagent` |
| MCP | one plugin per server, tools discovered into `ctx.tools.register()` |
| Scheduling | model-callable scheduling tools → `followup(..., {source: {kind: 'schedule'}})` |

**Skills.** Local discovery priority is `project/.dsh/skills` → `project/.agents/skills` → custom dirs → `~/.dsh/skills` → `~/.agents/skills` → bundled; names are kebab-case; directory bundles (`<name>/SKILL.md`) and flat `<name>.md` are accepted, recursive `**/SKILL.md` is not. ECC's `SKILL.md` frontmatter (`name`, `description`, `origin`) matches DSH's reader, so ECC skills need no conversion; because ECC ships `.agents/skills/`, opening a session from an ECC checkout already exposes a subset [4][21].

**Hooks are a compatibility bridge, not the core.** `dsh-hook-protocol` is a shared library (matcher, stdin/stdout codec, `ctx.shell`, most-restrictive merge, `hook/invoked`/`hook/result` events). `@deepseek-ai/dsh-hooks-claude-code` and `-codex` translate existing `hooks.json` command hooks onto the typed interception points. Native plugins on those same points are strictly more powerful; the bridge is explicitly a compatibility adapter [8][9][10]. Config is read once at process start (`configPath` is process-level); read/parse failure logs a warning and registers zero hooks [8][9].

**ECC on DSH, concretely.** ECC has no dedicated DSH installer. Setup = symlink curated skills into `~/.dsh/skills`, generate a DSH-format hooks config with `scripts/dsh/generate-hooks-config.js` (lower-cases tool matchers, prefixes `CLAUDE_PLUGIN_ROOT`/`ECC_PLUGIN_ROOT` into commands because the bridge does not export them, preserves timeouts, skips unsupported events), then mount a **bare** `- insert:` row for `@deepseek-ai/dsh-hooks-claude-code` in the profile patch. A plain top-level `- id:` row is silently dropped, `insert` under a non-group id is rejected, and compose exit status stays 0 on both — verify with `--dump-config` plus empty stderr. ECC's matrix calls this "adapter-backed": 23 command hooks on 5 of 7 bridged events; slash commands and ECC subagent surfaces have no DSH equivalent; `PreCompact`, `PostToolUseFailure`, `SessionEnd` have no bridge counterpart [19][20][21].

## 2. ECC harness patterns

- **Portability doctrine.** ECC is the reusable workflow layer; harnesses are execution surfaces. Durable assets live in `skills/`, `rules/`, `hooks/`, `scripts/`, MCP configs and harness-neutral memory; adapters stay thin and only translate loading, event shape, command names and platform limits. `SKILL.md` is the most portable unit; hooks may be runtime-backed (Claude, OpenCode, Cursor) or merely instruction-backed (Codex, Gemini) [19][20].
- **Loops.** `loop-design-check` requires a machine-decidable done-condition **plus** explicit boundaries ("what it must NOT do"), a retry cap with human escalation, layered goals, and reconciliation against external facts over self-assertion. Servo vs regulator vs regulator-with-exit; greenfield work uses plan/build/judge with a deterministic, **independent** judge and a Build role forbidden from editing acceptance conditions. Three retries then escalate. Five failure modes: vague goal spins; self-judged "looks right" verification; "all tests pass" invites deleting tests; relying on mid-run clarification; stale bloated memory. Human keeps the last switch; responsibility does not transfer [22].
- **Mechanism layer.** `continuous-agent-loop` (v1.8+ canonical; `autonomous-loops` retained for compatibility) selects continuous-pr / RFC-DAG / infinite / sequential, and recommends decomposition → quality gates → eval loop → session persistence. Failure modes: churn without measurable progress, repeated same-root-cause retries, merge stalls, cost drift; recovery = freeze loop, harness audit, reduce scope, replay with explicit acceptance criteria [23][24]. `autonomous-loops` adds concrete mechanisms: isolated fresh contexts per `claude -p` step, `SHARED_TASK_NOTES.md` as the cross-iteration context bridge, completion signals with consecutive-signal thresholds, exit codes that propagate, separate de-sloppify pass instead of negative instructions, separate contexts per stage so the reviewer never wrote the code, worktree isolation and eviction context on merge conflict [24].
- **Verification.** `verification-loop` is six phases — build (stop on failure), type check, lint, tests with coverage, security grep, diff review — followed by a PASS/FAIL report; hooks catch issues immediately, the full loop provides comprehensive review. Newer ECC text separates local checks from *target acceptance* and forbids treating merge/release as evidence of target verification [25].
- **Context compaction.** ECC's `strategic-compact` recommends `/compact` at phase boundaries (after exploration before implementation, after milestones, after debugging, before a context shift), never mid-implementation; its hook combines a window-scaled token threshold (160k on a 200k window) with tool-call count, and notes that call count alone is a weak proxy. Critical rule: **write the plan/state to a file before compacting** — task-list tools may not exist, so files are the durable record [27]. `token-optimization` repeats the phase-boundary rule and notes subagents protect main context by returning summaries [29]. ECC's `UserPromptSubmit` context gate is advisory by default; `ECC_CONTEXT_GATE_MODE=enforce` orders a clean stopping point plus a checkpoint at high usage and suppresses conflicting compact/warning hooks [28].
- **Subagents.** Delegate exploration to a cheap model and ingest only the summary; the sub-agent lacks the orchestrator's *purpose*, so ECC's iterative-retrieval pattern re-queries up to 3 cycles with objective context, one clear input/one clear output per phase, outputs on disk between phases [29][30].
- **Quality gates.** Post-edit `PostToolUse` quality gate, pre-commit checks, dev-server/destructive-command blockers, GateGuard fact-forcing, secret/console.log scans, and runtime hook profiles (`minimal`/`standard`/`strict`) put enforcement outside the prompt [28]. ECC's agent-harness guidance adds narrow schema-first tool inputs, deterministic result shapes (`status`/`summary`/`next_actions`/`artifacts`), per-error root-cause + safe-retry + stop condition, minimal invariant system prompt, and "compact at phase boundaries, not arbitrary token thresholds" [26].

## 3. Hook events: what bridges, what cannot, and why

**Supported by the Claude bridge (7) and their DSH points [8][9][35]:**

| CC hook | DSH point | Behavior |
|---|---|---|
| `SessionStart` | `agent/created` (master README: awaited init; pinned-commit README: `agent/session-start` emit — **version drift**) | attach context; cannot block |
| `UserPromptSubmit` | `agent/pre-step` waterfall | `deny` rejects prompt; context appends after `next()` |
| `PreToolUse` | `tools/pre-execute` waterfall | `deny` → deny; `ask` → approval request |
| `PostToolUse` | `tools/post-execute` waterfall | `deny` → blocked result + model-visible feedback; context prepends |
| `Stop` | `agent/turn-stopping` serial | blocking result forces another step via `steer()` |
| `SubagentStart` | `subagent/start` emit | injects into a live **in-process** child only |
| `SubagentStop` | `subagent/end` emit | observe only |

Matchers: tool name / session source / constant `agent_type: general-purpose`; `UserPromptSubmit` and `Stop` ignore matchers. Matched hooks run **serially in config order**; decisions fold most-restrictively (`deny > ask > allow`), order-independent. Context-only hooks call `next()` first so a later listener can still reject or rewrite — "adding context is not a veto." Emit points are detached and drained on disposal. Hook-provided context is logged, re-sent until compaction, and **append-only** after the reusable prefix [8][9].

**Partial semantics (do not assume CC parity):** `PreToolUse` `allow` does not pre-approve, `defer` is unsupported, `additionalContext` ignored, `updatedInput` logged/warned but not honored; `PostToolUse` `updatedToolOutput`/`updatedMCPToolOutput` unsupported and `tool_response` flattened to text; `systemMessage` not model-visible; `transcript_path` is always `''` in the bridge payload because the compressed session log is not hook-readable; `SessionStart` JSON `additionalContext` works but plain stdout context, `initialUserMessage`, `CLAUDE_ENV_FILE` etc. do not, and detached delivery can miss the first request; a blocked prompt is discarded with no model-visible message [8]. A community port reports that at DSH `0.2.0-rc.2`, exit code 2 did **not** block (recorded as `pass`) while structured stdout `permissionDecision: deny` with exit 0 did — treat as a version-specific community observation, not a documented contract [31].

**Unsupported: 23 of CC's 30 events** — `Setup`, `InstructionsLoaded`, `UserPromptExpansion`, `MessageDisplay`, `PermissionRequest`, `PostToolUseFailure`, `PostToolBatch`, `PermissionDenied`, `Notification`, `TaskCreated`, `TaskCompleted`, `StopFailure`, `TeammateIdle`, `ConfigChange`, `CwdChanged`, `FileChanged`, `WorktreeCreate`, `WorktreeRemove`, `PreCompact`, `PostCompact`, `SessionEnd`, `Elicitation`, `ElicitationResult`. Their config is ignored before group parsing, so it can neither register hooks nor invalidate the config [8][35]. Why they cannot be bridged, grouped by root cause:

1. **No matching typed interception point.** The bridge only targets the five loop points plus two subagent emits [7][8]. `compaction/*` (`start`, `summary`, `end`) exists as durable session events behind the `ctx.compaction` capability seam, but there is no documented user-facing pre/post-compaction hook point; users are still requesting one, and `SessionStartSource: 'compact'` is reserved but never emitted [15][16][17]. Hence `PreCompact`/`PostCompact`, and no state-save hook before context mutation.
2. **The harness owns the decision internally.** Permission is `ctx.approval` inside `tools/pre-execute` (allowed once or denied), not a CC-style `PermissionRequest`/`PermissionDenied` event; turn failure and batch dispatch are handled by built-in executors, not hook-shaped points [6][7]. Hence the permission, `StopFailure` and `PostToolBatch` family.
3. **Concept has no DSH equivalent.** DSH dispatches subagents from a declarations catalog with no shared task board or named teammates (the `TeammateIdle` row in one community stack only observes idle transitions); file/worktree/cwd changes are observation or platform concerns, not CC lifecycle hooks; a `SessionEnd` support request is still open [6][36][37]. Hence task/teammate/worktree/config/cwd/file events and `SessionEnd`.
4. **Frontend/editorship concepts.** `Setup`, `InstructionsLoaded`, `UserPromptExpansion`, `MessageDisplay`, `Notification`, `Elicitation*` are Claude-Code-specific UI or setup semantics; the extension cookbook's feature→mechanism map has no corresponding row [7][8]. This grouping is an inference from the documented maps, not a verbatim maintainer statement.

**tinyloop implication:** do not plan to inherit hook semantics. Implement the equivalent checks as Rust code at explicit pipeline stages (pre-tool approval, post-tool observation, phase-boundary state save, pre-done verification), where the decision is deterministic and auditable.

## 4. Prompt caching and context-stability evidence

- **DeepSeek server side:** disk context caching is on by default. A hit requires a *fully* matching persisted **prefix unit**; units are persisted at request boundaries (end of user input and model output), by common-prefix detection, and at fixed token intervals for long inputs/outputs. Partial middle matches do not hit; `A+B` then `A+C` cannot hit `A+B`, but may persist `A` for a later `A+D` [11].
- **DSH request discipline:** "model-visible ⟺ durably referenced." An append-only event log projected once per node through a pure `deriveEventMessage()` yields requests that are append-extensions of predecessors whenever the header is unchanged — cache stability is *emergent*, not managed. Full-price points are named: compaction, a real prompt/tool/config change (`request/header` reason `change`), and process-boundary drift. Deep-frozen messages make mutation of logged history unrepresentable [12].
- **Compaction itself is cache-aware:** the summarizer used to send a bespoke system prompt + flattened transcript (cache miss at token 0). It now replays the last routed `system` + `tools` + derived history verbatim and appends the compaction instruction as the **final user message**; tool schemas are forwarded even though the summarizer calls none, because omission would misalign the prefix. Auto-compaction anchors at the surface head (guaranteed hit); manual mid-range compaction and a different summarization model forgo reuse [13].
- **Prompt changes move rather than rewrite:** rewriting leading prompt bytes costs the whole prefix; DSH logs changed instructions in-history, with `systemPromptUpdate = 'in-history'` a *declared* model capability validated by catalogs — not inferred from model name or protocol. Resume retains committed instructions and appends changes, so a process restart alone need not invalidate caches [14]. Hook-injected context is likewise append-only after the reusable prefix [8].
- **Local llama.cpp reality (tinyloop's runtime):** `cache_prompt` (prefix reuse of the previous request) is enabled by default; only the differing suffix is reprocessed. But the server documents that logits are not guaranteed bit-for-bit identical across different batch sizes, so cache hits can make results nondeterministic. `--cache-reuse N` (default 0/off) can shift and reuse chunks after a divergence; host-memory prompt caching (`--cache-ram`, default 8192 MiB in some builds) is a separate layer that selects stored states by prefix similarity [32][33]. A 2026 study on Q4_K_M GGUF models found prefix reuse changed trajectories vs recompute and that the host-memory layer produced 38.8% run-to-run divergence vs 1.2% with it disabled in their measured configuration — evidence that a quantized local model plus caches is not request-reproducible unless server flags and cache state are pinned [34]. Treat those numbers as that paper's configuration, not a universal constant.

**tinyloop implication:** freeze system prompt + tool schema bytes for a session; append volatile facts rather than rewriting the head; put compaction/state snapshots at phase boundaries and log the reason for every prefix reset; record `cache_prompt`/`cache-reuse`/`cache-ram` settings and, if the verifier must be reproducible, consider a pinned deterministic verification pass with caches disabled.

## 5. Top 8 model-independent ideas that transfer to tinyloop

All eight are mechanisms of the *harness*, not properties of a large cloud model; each is cited above and adapted to a 4B/8K Rust CLI.

1. **Machine-decidable done-condition + boundary + retry cap + human final switch** — make the verifier external and deterministic; three failures → escalate [22].
2. **Plan / Build / Judge with author-bias elimination and disk artifacts between phases** — in tinyloop the Rust verifier is the independent judge; Build cannot edit acceptance criteria [22][24][30].
3. **Narrow typed tools with deterministic result shape and recovery hints** — `status`/`summary`/`next_actions`/`artifacts`, one tool call per turn, root cause + safe retry + stop condition [26].
4. **Front-load clarification; never rely on mid-run questions** — ambiguity is escalated before the loop starts [22].
5. **Phase-boundary compaction with state written to files first** — with an 8K window, compact earlier than a 200k-window recipe; never mid-edit [27][29].
6. **Append-only context model with attributable cache breaks** — freeze header bytes, append updates, log compaction/config/process drift as named reset reasons [11][12][14][32].
7. **Subagent-style context isolation without model routing** — sequential fresh phases each write a summary file; with one 4B model, parallel Haiku/Opus routing does not apply, but context isolation and iterative re-query do [24][30][29].
8. **Quality gates outside the prompt** — build/test/lint/security/diff phases and post-edit checks run in Rust, with human approval for writes/shell; report target acceptance separately from local checks [25][28].

**Does not transfer:** DSH's Cordis profile/bundle/plugin tree and hook bridge (Node/Cordis-specific) [1][8]; CC slash commands and subagent surfaces [20][21]; model-alias routing across Opus/Sonnet/Haiku [29][30]; DeepSeek's disk-cache economics (local llama.cpp has different flags and determinism trade-offs) [11][32][34]; the full 300+ skill catalog as always-loaded context [18][26].

## 6. Citations

1. https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md
2. https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/cli/reference/README.md
3. https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/user/develop/basic/publish.md
4. https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/subsystems/skills.md
5. https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/subsystems/core.md
6. https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/tool-execution-pipeline.md
7. https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/cookbook/extension-cookbook.md
8. https://raw.githubusercontent.com/deepseek-ai/deepseek-harness/master/packages/experimental/hooks-claude-code/README.md (see also pinned https://github.com/deepseek-ai/deepseek-harness/blob/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e/packages/hooks/hooks-claude-code/README.md )
9. https://deepseekdocs.com/en/docs/features/hooks
10. https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/hooks/README.md
11. https://api-docs.deepseek.com/guides/kv_cache
12. https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/notes/implemented/architecture/2026-07-05-reconstructable-requests.md
13. https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/notes/implemented/bug-fix/2026-07-21-compaction-summary-prefix-cache-reuse.md
14. https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/notes/implemented/feature/2026-09-02-in-history-system-prompt-replacement.md
15. https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/subsystems/compaction.md
16. https://github.com/deepseek-ai/deepseek-harness/discussions/10702
17. https://github.com/deepseek-ai/deepseek-harness/discussions/5890
18. https://github.com/affaan-m/ECC
19. https://github.com/affaan-m/ECC/blob/main/docs/architecture/cross-harness.md
20. https://github.com/affaan-m/ECC/blob/main/docs/architecture/harness-adapter-compliance.md
21. https://github.com/affaan-m/ECC/blob/main/docs/DEEPSEEK-HARNESS-GUIDE.md
22. https://github.com/affaan-m/ECC/blob/main/skills/loop-design-check/SKILL.md
23. https://github.com/affaan-m/ECC/blob/main/skills/continuous-agent-loop/SKILL.md
24. https://github.com/affaan-m/ECC/blob/main/skills/autonomous-loops/SKILL.md
25. https://github.com/affaan-m/ECC/blob/main/skills/verification-loop/SKILL.md
26. https://github.com/affaan-m/ECC/blob/main/skills/agent-harness-construction/SKILL.md
27. https://github.com/affaan-m/ECC/blob/main/skills/strategic-compact/SKILL.md
28. https://github.com/affaan-m/ECC/blob/main/hooks/README.md
29. https://github.com/affaan-m/ECC/blob/main/docs/token-optimization.md
30. https://github.com/affaan-m/ECC/blob/main/the-longform-guide.md
31. https://github.com/denceee/dsh-everything-claude-code (community port; version-specific observations)
32. https://github.com/ggml-org/llama.cpp/blob/b12627/tools/server/README.md
33. https://github.com/ggml-org/llama.cpp/discussions/20574
34. https://arxiv.org/html/2609.04748v2
35. https://docs.claude.com/en/docs/claude-code/hooks
36. https://github.com/dsh-cc/dsh-cc/blob/main/docs/cc-parity-matrix.md
37. https://github.com/deepseek-ai/deepseek-harness/discussions/1595
38. https://findharness.com/blog/what-is-deepseek-harness (secondary overview only)

**Verification note:** claims were checked against the fetched primary docs on 2026-11-25; DSH and ECC are fast-moving, so re-check the bridge event list and version-specific hook behavior against the installed `dsh --version` before relying on them.
