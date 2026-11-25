# Efficient agent harnesses for small local models — findings

Scope: techniques that make a ~4B local model reliable at an 8K context, with native tool calling, at most seven tools, and one tool call per turn. Research date: 2026-11-25. Claims below are tied to the sources in the `## Sources` section; recommendations for `tinyloop` are explicitly labelled as design inferences, not measured results. No benchmark numbers are reported unless the cited source states them.

The project-provided observation — 9/10 with API-passed tools versus 0/6 with a prompt-encoded action format — is treated as given project context, not as an external finding. It is consistent with the general format-sensitivity literature but no source here measures it directly.

## Bottom line: ten concrete techniques

1. **Send tools in the native `tools` array through llama-server with `--jinja`, and verify the tools actually reached the prompt.** llama.cpp documents OpenAI-style function calling via `--jinja` plus native and generic chat-template handlers [10]. A filed llama.cpp bug reports that when a template has no tool support, `tools` is silently dropped and the client cannot distinguish "no tool chosen" from "no tools shown" [37]. For a local harness, a prompt_tokens delta probe on a fixed request is therefore a prerequisite, not an optional check [37].
2. **Keep action schemas as JSON, with fixed names, descriptions, and ordering; do not invent prompt-only action syntax.** BFCL V4 tests 26 prompt variations and reports that function-call output format and function-documentation format change accuracy, with JSON documentation best across nearly all tested models and Python/JSON return formats better than XML for smaller models [36]. A community local-model tool-calling report had to add layered fallback parsers (tags, bare JSON, bracket notation, bare function calls) precisely because raw-format outputs diverge [13].
3. **Constrain only the action boundary, not the reasoning.** llama.cpp can convert a subset of JSON Schema to GBNF grammars and apply them per request through the server [11]; OpenAI's Codex prompting guide illustrates a context-free grammar for the `apply_patch` freeform tool [5]. But a controlled study found reasoning performance declines under strict output-format restrictions, with stronger restrictions causing larger degradation [29]. Use grammar-constrained tool arguments, not a grammar over the whole turn [29][11].
4. **Cap the visible tool set and make it an allow-list.** A 2026 study of tool shortlist depth notes that roughly 200 tokens per tool description makes a 100-tool shortlist consume about 20K tokens before the query, and reports comparable BFCL coverage at K≈7.4 versus fixed K=50 [30]. Goose recipes support an explicit `available_tools` allow-list per extension [14]; OpenCode agents can deny `edit` and `shell` per resource [18].
5. **Own the loop in Rust; the model only emits one structured next step.** The 12-Factor Agents pattern is: LLM emits structured JSON ("tool calling"), deterministic code executes it, the result is appended to context, and the loop repeats until the next step is "done" [6]. Tools are just structured outputs that trigger deterministic code [25], and the control flow — summarization, compaction, rate limits, pause, human approval — stays in harness code [26].
6. **Separate plan / act / verify instead of asking one turn to do all three.** The Codex prompting guide asks the model to acknowledge and plan before any tool calls and to give updates on a cadence rather than every tool call [5]. OpenCode supports a read-only `plan` subagent and per-agent permissions, with subagents running in fresh context [18]. SWE-agent's loop emits a thought and an action, then incorporates environment feedback; edits are linted before being accepted [7][32].
7. **Truncate observations at the source, not in the prompt assembler.** OpenCode's `read` caps pages at 2,000 lines / 50 KiB and shortens individual lines after 2,000 characters, while `grep` and `glob` default to 100 results [19]. SWE-agent's file viewer shows at most 100 lines, search returns at most 50 results with a suggestion for a more specific query, and empty output becomes an explicit success message [7][32]. Aider sends a graph-ranked repo map fitted to a token budget (`--map-tokens` defaults to 1k) instead of whole files [8][9]; Goose summarizes verbose command output [35].
8. **Compact older history but keep an exact recent tail.** Pi triggers compaction when `contextTokens > contextWindow - reserveTokens` (default `reserveTokens` 16,384), walks backward to a `keepRecentTokens` default of 20k, produces an iterative structured summary, and records `firstKeptEntryId` [1]. OpenCode summarizes the conversation and keeps about 15,000 recent tokens beside the summary by default; compaction is lossy and has a configurable free-buffer [16][17].
9. **Freeze a cache-friendly prefix and put volatile content last.** OpenAI documents that changing tool names, descriptions, schemas, or ordering affects the cached prefix, and recommends preserving earlier messages and tool results so later turns reuse the shared prefix [4]. An independent harness survey describes the general rule as static content first, volatile content last, and a stable per-session key; it also notes Pi uses no date in its system prompt and a frozen tool list, while Codex CLI snapshots context sections and appends later diffs [12].
10. **Give failures a bounded, machine-readable retry contract.** Compact Error factor 9 appends a formatted error event to the thread, suggests an error counter limiting a tool call to roughly three attempts, and escalates on a consecutive-error threshold [21]. Goose returns invalid JSON and missing-tool errors to the model as tool responses so it can recover [35]. SWE-agent rejects syntactically invalid edits and repeats malformed generations until valid [7][32]. Evals should be deterministic: SWE-bench applies the patch and runs the repository test suite in Docker [31], and Terminal-Bench verifies the final container state with tests rather than inspecting the agent's commands [33].

## Implementation comparison

| Implementation | Loop and control | Tool surface | Context / observation strategy | Retry and verification |
|---|---|---|---|---|
| pi-mono / pi coding agent | Session tree; one turn = request, streamed response, tool execution, results; next turn only if results or queued messages require it [2] | Built-ins `read`, `bash`, `edit`, `write` by default; extensions add tools [3] | JSONL tree sessions with parent IDs; branch summarization; auto-compaction with reserve/keep-recent thresholds; tool results truncated for summary requests [1][2] | `session_compact_failed` event exposes reason, error, aborted, `willRetry`, `fromExtension` [1] |
| OpenAI Codex CLI (prompting guide) | Acknowledge/plan before tools; short progress updates on a cadence; `parallel_tool_calls=False` in the shown example [5] | First-class `apply_patch` or a freeform tool with a context-free grammar; dedicated terminal-wrapping tools [5] | `AGENTS.md` discovered from home through repo root to CWD, merged in order as user-role messages; first-class compaction support [5][20] | Backed by internal evals for correctness, completeness, tool usage, parallelism, bias for action [5] |
| Aider | Edit-format-specific coders; map sent with each change request; lint/test commands configurable [9] | Edit formats; `--weak-model`, `--auto-lint`, `--auto-test`, `--test-cmd` [9] | Concise repo map of key symbols ranked by a dependency graph and fitted to `--map-tokens` (default 1k); prompt caching opt-in [8][9] | `--lint`, `--lint-cmd`, `--auto-lint`, `--auto-test` provide deterministic feedback [9] |
| OpenCode | Primary and `plan`/reviewer subagents; permissions per action/resource; fresh context for subagents [18] | `read`, `glob`, `grep`, `edit`, `write`, `patch`, `shell` with explicit per-tool limits [19] | Summary plus kept recent tail; background compaction; automatic recovery once on a context-length rejection; full conversation reused for the summary request to preserve prompt cache [16] | Manual compaction runs at a safe point, duplicate requests merge, idempotency key supported [16] |
| Goose | Interface → agent loop → MCP extensions; tool calls formatted in JSON and executed by goose [35]; the README describes loading local models directly and connecting tools through MCP [15] | Built-in Developer extension (`shell`, `write`, etc.); recipes can restrict `available_tools` [34][14] | Content Revision: smaller-model summarization, deletion of old/irrelevant content, find-and-replace instead of rewrites, ripgrep, verbose-output summarization [35] | Invalid JSON and missing-tool errors are returned as tool responses [35] |
| SWE-agent | Thought + action, environment feedback, ACI commands for search/view/edit/bash [32] | Purpose-built file viewer/editor and directory string search over a shell [7] | 100-line viewer window, at most 50 search results, explicit empty-output message, history processors [7][32] | Linter blocks invalid edits; malformed generations get an error and are retried; all past errors but the first omitted [7][32] |
| 12-Factor Agents (principles, not an implementation) | Deterministic outer loop; model chooses next step; harness owns control flow, pause/resume, human contact [6][26] | Tools are structured outputs that trigger deterministic code [25] | Own the context window; unify execution and business state in one serializable thread [22][28] | Compact errors into context; bounded retries; stateless reducer over events [21][24] |

## Native tool calling versus prompt formats

llama.cpp's function-calling document says OpenAI-style function calling is used by `llama-server` when started with `--jinja`, lists model families with native handlers, and describes a generic handler for unrecognized templates that "may consume more tokens and be less efficient" than the native format [10]. The practical hazard is configuration: the llama.cpp issue reports that with `--jinja`, a template lacking tool support causes the `tools` array to be silently dropped with HTTP 200 and no warning, leaving the client unable to tell tool refusal from tool absence [37]. A harness should therefore treat native tool calling as a capability to be probed at startup and re-probed after any model or template change [37].

Prompt-encoded formats are not equivalent [36][13]. BFCL V4 varies return format, function documentation format, tool-call tags, prompt format, and style, and reports both cross-model trends and model-specific failure modes [36]. The community local-model benchmark documents the cost of format drift: models emitted bare JSON without the expected opening tag, bracket notation, function calls inside tags, and bare function calls, requiring a five-layer parser fallback chain [13]. That is evidence for a single canonical native channel plus strict validation, not for permissive multi-format parsing as a primary path [13][36].

## Constrained decoding, JSON Schema, GBNF

GBNF is llama.cpp's grammar format for constraining generation, supports JSON output and token-level rules, can be supplied as a `grammar` field to server completion endpoints, and supports conversion of a JSON Schema subset to a grammar [11]. The guide also notes performance gotchas and links an upstream issue [11]. Codex's prompting guide shows the same idea at the tool level: a context-free grammar describes the patch format for a freeform `apply_patch` tool [5]. The counterweight is empirical: format restrictions significantly reduced reasoning performance in the "Let Me Speak Freely?" study, with stricter constraints degrading reasoning more [29]. Design implication for tinyloop: let the 4B model think in ordinary text, then constrain only the final structured action and its argument values [29][11].

## Tool count and selection

Tool descriptions are not free: the shortlist-depth paper estimates about 200 tokens each and reports an RL depth agent reaching roughly 90.3% coverage at K≈7.4 on BFCL compared with about 90.8% at fixed K=50 [30]. It also warns that fixed K cannot distinguish easy from hard queries [30]. Concrete harness controls already exist: Goose recipes can name `available_tools` per extension [14], and OpenCode agents can deny edit and shell per resource [18]. For a seven-tool budget, a static allow-list is simpler than learned retrieval and keeps the tool-definition prefix stable [14][4].

## Observation truncation

The strongest pattern is to make tools return small, bounded, informative observations [19][7]. OpenCode pages reads and caps result counts [19]; SWE-agent's ACI suppresses verbose search results, uses a 100-line window, and explicitly says when a command produced no output [7][32]. The SWE-agent paper's ablations also show the file-viewer window size and history processing matter, and that block edits with syntax/compile errors help stop cascading errors [32]. Aider's repo map is the complementary technique: a concise, ranking-selected symbol map fits a 1k-token default budget and tells the model which files to request next [8][9].

## Compaction, state files, and prefix stability

Pi stores sessions as JSONL trees, with each entry carrying an ID and parent; the active branch supplies history for the next request [2]. Compaction writes a structured entry with a summary and `firstKeptEntryId`, passes the previous summary forward as iterative context, disables prompt-cache writes for one-off summarization requests, and truncates tool results before summarizing [1]. OpenCode instead sends the full conversation in the summary request so the summary can reuse the normal request's prompt cache, keeps a recent tail, runs background compaction by default, and recovers once from a too-long request [16]. These are two different tradeoffs for the same problem; for a local model where prefill dominates, the OpenCode cache-reuse idea and the Pi keep-recent boundary are complementary [1][16].

The 12-Factor framing is that the context window is the state: unify execution state and business state into one serializable thread so a run can resume, fork, and be shown to a human [22][28][24]. Prompt-cache documentation reinforces the mechanics: model, tools, their ordering, and parallel-tool-call instructions affect the cached prefix [4]. The independent harness survey reports Pi keeps no date in the system prompt and a frozen tool list, Codex CLI snapshots context sections and appends diffs, and OpenCode sends date changes as new messages [12]. Do not put clocks, random IDs, or per-turn tool reordering in the stable prefix [4][12].

## Error contracts, plan/act/verify, evals

Bounded retries beat unbounded loops: the 12-Factor error pattern appends a formatted error to the thread, retries a few times, then escalates [21]. Goose feeds execution errors back as tool responses [35]. SWE-agent blocks invalid edits and repeats malformed generations [32]. Planning should be cheap and separate from execution — Codex asks for a one-sentence acknowledgement and a one-to-two-sentence plan before tools [5], while OpenCode's plan agent is read-only and runs in fresh context [18]. Verification should be external and deterministic: SWE-bench grades by applying the patch and running tests in Docker [31], and Terminal-Bench tests the final container state, not console output, with oracle solutions and explicit solvability checks [33].

## Design inferences for tinyloop (not measured results)

- Use seven native tools at most: `plan`, `read`, `bash`, `apply_patch`, `verify`, `done`, `escalate` [4][14]. Keep each invocation short enough that a task stays within roughly 3–10 steps, with 20 as an upper bound [23]. Keep names, descriptions, parameter JSON Schemas, and array order byte-identical for the whole session [4][14][10].
- Freeze system prompt prefix; append project instructions (`AGENTS.md`-style) once at session start, then append observations; never rewrite earlier messages [20][4][12]. Treat prompts as first-class owned code, not a framework black box [27].
- Budget 8K roughly as: frozen prefix + compact tool schemas, small repo/task map in the style of Aider's ranked map, exact recent tail, then the current observation [8][1][16].
- Make each tool return a bounded string with line numbers, an explicit empty-result marker, and a structured error object; cap lines, bytes, and result counts in Rust before the observation enters context [19][7].
- Compaction: summarize when free space falls below a configured reserve, keep a fixed recent tail, persist summary and cut point to a JSONL state file, and disable cache writes for the one-off summary call [1][16].
- Retry contract: at most three consecutive failures for one step, append the error event each time, then `escalate`; deterministic `verify` decides `done`, never the model's self-report [21][31][33].
- Evals: build a small deterministic suite with a container or sandbox snapshot, an oracle solution, and tests over final state [31][33]. Track tool-call validity and task success separately [13][30]. Start by probing whether `--jinja` actually renders the tools array for the exact Spark GGUF build, since a silent drop invalidates every other metric [37].

## Limits of this research

No primary source found here benchmarks Spark-X2.6-4B specifically, so the 9/10 versus 0/6 result remains project-provided evidence and should be re-run under one controlled harness [37][13]. Several cited results come from much larger models (SWE-agent, BFCL, format-restriction study); they support design direction, not a numeric transfer to 4B [32][36][29]. The community tool-calling report is useful as failure-mode evidence but is not an authoritative leaderboard [13]. One candidate BFCL "supported models" file returned HTTP 404 during retrieval and was excluded; no claim rests on it [unverified].

## Sources

[1] https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/compaction.md
[2] https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/how-pi-works.md
[3] https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/settings.md
[4] https://developers.openai.com/api/docs/guides/prompt-caching
[5] https://developers.openai.com/cookbook/examples/gpt-5/codex_prompting_guide
[6] https://raw.githubusercontent.com/humanlayer/12-factor-agents/main/README.md
[7] https://raw.githubusercontent.com/SWE-agent/SWE-agent/main/docs/background/aci.md
[8] https://raw.githubusercontent.com/Aider-AI/aider/main/aider/website/docs/repomap.md
[9] https://aider.chat/docs/config/options.html
[10] https://raw.githubusercontent.com/ggml-org/llama.cpp/master/docs/function-calling.md
[11] https://raw.githubusercontent.com/ggml-org/llama.cpp/HEAD/grammars/README.md
[12] https://www.maximem.ai/blog/prompt-caching-coding-agent-harnesses
[13] https://github.com/MikeVeerman/tool-calling-benchmark/blob/master/ROUND2_REPORT.md
[14] https://raw.githubusercontent.com/block/goose/main/documentation/docs/guides/recipes/recipe-reference.md
[15] https://raw.githubusercontent.com/block/goose/main/README.md
[16] https://opencode.ai/docs/compaction
[17] https://opencode.ai/docs/config
[18] https://opencode.ai/docs/agents
[19] https://opencode.ai/docs/tools
[20] https://developers.openai.com/codex/agent-configuration/agents-md
[21] https://raw.githubusercontent.com/humanlayer/12-factor-agents/main/content/factor-09-compact-errors.md
[22] https://raw.githubusercontent.com/humanlayer/12-factor-agents/main/content/factor-03-own-your-context-window.md
[23] https://raw.githubusercontent.com/humanlayer/12-factor-agents/main/content/factor-10-small-focused-agents.md
[24] https://raw.githubusercontent.com/humanlayer/12-factor-agents/main/content/factor-12-stateless-reducer.md
[25] https://raw.githubusercontent.com/humanlayer/12-factor-agents/main/content/factor-04-tools-are-structured-outputs.md
[26] https://raw.githubusercontent.com/humanlayer/12-factor-agents/main/content/factor-08-own-your-control-flow.md
[27] https://raw.githubusercontent.com/humanlayer/12-factor-agents/main/content/factor-02-own-your-prompts.md
[28] https://raw.githubusercontent.com/humanlayer/12-factor-agents/main/content/factor-05-unify-execution-state.md
[29] https://arxiv.org/pdf/2408.02442v3
[30] https://arxiv.org/html/2605.24660v2
[31] https://raw.githubusercontent.com/SWE-bench/SWE-bench/02e7a74ffd0b707aab73d203fe87bdc7c76afc8e/docs/reference/harness.md
[32] https://proceedings.neurips.cc/paper_files/paper/2024/file/5a7c947568c1b1328ccc5230172e1e7c-Paper-Conference.pdf
[33] https://arxiv.org/html/2601.11868
[34] https://raw.githubusercontent.com/aaif-goose/goose/2bcb8583a18a0623eb78c9ed845d597ed99fae48/documentation/docs/mcp/developer-mcp.md
[35] https://goose-docs.ai/docs/goose-architecture
[36] https://gorilla.cs.berkeley.edu/blogs/17_bfcl_v4_prompt_variation.html
[37] https://github.com/ggml-org/llama.cpp/issues/27129
