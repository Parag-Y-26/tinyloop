# Reuse register — third-party files in tinyloop

Every vendored file and every ported algorithm is listed here with its source, license, and the reason it is better than writing our own. Vendored originals live in `vendor/`; ports live in `src/` and carry attribution headers.

| Item | Source (pinned) | License | Used in tinyloop | Why this is better than writing it ourselves |
|---|---|---|---|---|
| `vendor/llama-cpp/grammars/json.gbnf` | ggml-org/llama.cpp, `master` (`grammars/json.gbnf`) | MIT (`vendor/llama-cpp/LICENSE`) | Constrained final answers via `response_format` on tool-free requests | Maintained upstream by the grammar engine authors; a hand-written grammar risks subtle JSON edge cases. Directly usable — no port needed. |
| `vendor/openclaw/agents/tool-result-limits.ts` | openclaw/openclaw, `main` (`src/agents/tool-result-limits.ts`) | MIT (`vendor/openclaw/LICENSE`) | Budget formula in `src/tools/observation.rs` | Production-tuned caps with context-derived tiers. Our 8K numbers (9,828 / 8,192 chars) come straight from these formulas, not from a guess. |
| `vendor/openclaw/agent-core/harness/utils/truncate.ts` | openclaw/openclaw, `main` (`packages/agent-core/src/harness/utils/truncate.ts`) | MIT | Line/byte ceiling logic in `src/tools/observation.rs` | Explicit metadata (`truncatedBy`, line/byte counts) and complete-line preservation; reimplementing this from scratch is where silent truncation bugs live. |
| `vendor/openclaw/utf8-truncate.ts` | openclaw/openclaw, `main` (`src/utils/utf8-truncate.ts`) | MIT | UTF-8-safe head/tail boundaries in `src/tools/observation.rs` | Correct handling of split multibyte code points and surrogates. Rust uses `is_char_boundary`, but the source documents the edge cases we must preserve. |
| `vendor/openclaw/text-truncate.ts` | openclaw/openclaw, `main` (`src/shared/text-truncate.ts`) | MIT | Reference for marker/ellipsis behavior | Kept as provenance for how OpenClaw reports truncation to the model. |
| `vendor/openclaw/agent-core/tool-repeat-guard.ts` | openclaw/openclaw, `main` (`packages/agent-core/src/tool-repeat-guard.ts`) | MIT | `src/loop/repeat_guard.rs` | Battle-tested nudge-then-stop pattern with canonical argument comparison. We change only the stop threshold (20 → 4) to fit a 12-step cap. |
| `vendor/terminal-bench/docs/task-template.toml` | harbor-framework/terminal-bench, `main` (`docs/task-template.toml`) | Apache-2.0 (`vendor/terminal-bench/LICENSE`) | `tests/eval/task.toml` schema | Field-tested task schema used by a real agent benchmark. We drop Docker/network fields and add `goal`, `boundaries`, and `oracle` for our local verifier. |
| ECC skills (`deepseek-harness-setup`, `loop-design-check`, `agent-harness-construction`, `autonomous-loops`) | local Hermes skill library, origin ECC | ECC skill files (used as design reference only, not copied) | `docs/ARCHITECTURE.md`, `docs/FLOW_REASONING.md` | Encodes loop-failure modes (spinning, Goodhart-gaming, wrong-answer-to-completion) that are expensive to rediscover. Summarized, not copied verbatim. |

## Adaptation rules

1. **License files travel with the code.** Each `vendor/<source>/LICENSE` is the upstream license; no license text was rewritten.
2. **Ports are marked.** `src/tools/observation.rs` and `src/loop/repeat_guard.rs` name their upstream file in the module doc comment and state the adaptation.
3. **Do not vendor TypeScript into the build.** The `.ts` files are reference originals under `vendor/`; the Rust ports in `src/` are what compile.
4. **No CC BY-SA content copied.** The 12-factor-agents prose is CC BY-SA 4.0; only its principles were summarized in our own words, so no share-alike obligation attaches to tinyloop docs.
5. **Re-pin on update.** If an upstream file changes, fetch it into `vendor/`, re-read it, update the port, and update this register. Do not silently patch a port without recording why.

## Deliberate non-reuse

- **pi / pi-mono (MIT)** — its agent core is TypeScript and its defaults (subagents, plan mode) are explicitly things we do not want at 4B/8K. Patterns were studied, no files vendored.
- **OpenClaw gateway/session layer** — Node/Cordis-style persistence and hook plumbing; far larger than a Rust CLI needs. Only the small, well-defined utility algorithms above were taken.
- **12-factor-agents content** — CC BY-SA 4.0; principles summarized rather than copied.

## Verification status

`vendor/` files were fetched over HTTPS and their byte sizes and license headers checked. The ported algorithms were numerically verified with a Python mirror (see the verification output recorded in this session). **The Rust modules have not been compiled**: no `cargo`/`rustc` is installed on this machine. Run `cargo test` in Phase 1 once the toolchain is available.
