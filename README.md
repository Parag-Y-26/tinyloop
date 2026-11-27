# tinyloop

A Rust CLI computer-use agent that runs a **small local model** and does real tasks on your machine under a bounded, verifiable loop.

**Status:** implemented — Rust core plus Tauri/React desktop app, verified against the real model. Card status lives on [`docs/AGENT_BOARD.md`](docs/AGENT_BOARD.md).

## Decisions so far

| Choice | Value |
|---|---|
| Job | General computer-use tasks (repo work, file/data tasks), Claude-Cowork style |
| Model | Spark-X2.6-4B, Q4 quantization, 8K context |
| Runtime | Local `llama-server` (llama.cpp), OpenAI-compatible API; MLX backend later |
| Interface | CLI |
| Language | Rust |
| Deliverable now | Architecture + design docs |

## Why it is built this way

The model proposes actions. **Rust owns the loop, the budget, the sandbox, and the verifier.**

- Native OpenAI `tools` array + `--jinja` — this model fails badly with prompt-encoded action formats.
- Thinking disabled / lowered for the 8K token budget.
- Machine-decidable done-condition + boundaries, fixed before the loop starts.
- One tool call per turn, deterministic tool-result shape, large output to disk.
- Human approval for writes and shell commands; no auto-commit / auto-push.

## Layout

```
tinyloop/
├── src/                      # tinyloop-core: loop engine, tools, model client, audit, context
├── src/bin/                  # run (headless), probe (capability), eval (15-task suite)
├── tests/eval/               # deterministic eval tasks
├── desktop/                  # Tauri 2 shell + React 19 UI
├── docs/                     # architecture, model profile, eval, agent board, research
└── README.md
```

## Docs

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — v0.2 design, updated with research findings
- [`docs/FLOW_REASONING.md`](docs/FLOW_REASONING.md) — why each stage exists, and the speed/accuracy tradeoff at each step
- [`docs/REUSE_REGISTER.md`](docs/REUSE_REGISTER.md) — vendored files, licenses, and why each is better than writing it ourselves
- [`docs/DESKTOP_UI.md`](docs/DESKTOP_UI.md) — Tauri + React desktop decision, Apple-like UI spec, core↔UI contract
- [`docs/AGENT_BOARD.md`](docs/AGENT_BOARD.md) — card status, evidence, and remaining gaps
- [`docs/EVAL_RESULTS.md`](docs/EVAL_RESULTS.md) — real-model eval numbers and limits

## Interface

Desktop app (Tauri 2 + React 19). Open [`desktop/prototype.html`](desktop/prototype.html) for the clickable UI prototype; `desktop/` holds the real app. The CLI is retained as a thin headless wrapper over `tinyloop-core`.

**Run it:** start the model server first, then the app.

```bash
# terminal 1
cd ~/.llama/models
llama serve -m Spark-X2.6-4B-Q4_K_M.gguf -a spark-x26 -c 8192 -np 1 -ngl all -fa on --jinja --host 127.0.0.1 --port 9941

# terminal 2
cd desktop && pnpm tauri dev
```

Or launch the built binary at `desktop/src-tauri/target/release/tinyloop-tauri.exe`.

**Verified against the real model:** `llama serve` build 12652, Spark-X2.6-4B Q4_K_M, 8192 ctx. Capability probe: tools visible (23 → 130 prompt tokens), native tool call parsed. Eval suite: **15/15 tasks**, 4,551 tok/s prefill, 40.9 tok/s decode, 88.9% cache hits, 0 tool errors — see [`docs/EVAL_RESULTS.md`](docs/EVAL_RESULTS.md).

**Installers:** MSI and NSIS builds are in `desktop/src-tauri/target/release/bundle/`.
- [`docs/MODEL_PROFILE.md`](docs/MODEL_PROFILE.md) — exact `llama-server` command and gotchas
- [`docs/research/INDEX.md`](docs/research/INDEX.md) — OpenClaw, DeepSeek Harness/ECC, small-model harnesses, runtime verification

## Next

Core, desktop shell, approval gate, audit trail, context compaction, eval suite, and installers are landed (see the agent board). Tests: `cargo test --lib` (30), `pnpm test` in `desktop/` (10).

The one open surface is automated click-through of Allow/Deny inside the native WebView, which needs `tauri-driver` plus a matching Edge WebDriver. The logic is covered in jsdom; the real-window click is still manual.
