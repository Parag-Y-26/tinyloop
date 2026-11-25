# tinyloop

A Rust CLI computer-use agent that runs a **small local model** and does real tasks on your machine under a bounded, verifiable loop.

**Status:** design phase. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

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
├── README.md
└── docs/
    ├── ARCHITECTURE.md       # main design
    ├── MODEL_PROFILE.md      # exact model/server settings (pending)
    └── research/             # deep-dive findings from reference repos (in progress)
```

## Docs

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — v0.2 design, updated with research findings
- [`docs/FLOW_REASONING.md`](docs/FLOW_REASONING.md) — why each stage exists, and the speed/accuracy tradeoff at each step
- [`docs/REUSE_REGISTER.md`](docs/REUSE_REGISTER.md) — vendored files, licenses, and why each is better than writing it ourselves
- [`docs/DESKTOP_UI.md`](docs/DESKTOP_UI.md) — Tauri + React desktop decision, Apple-like UI spec, core↔UI contract

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

**Verified against the real model:** `llama serve` build 12652, Spark-X2.6-4B Q4_K_M, 8192 ctx. Capability probe: tools visible (23 → 130 prompt tokens), native tool call parsed. End-to-end task: read `Cargo.toml` → patch `answer.txt` → verifier passed → `Done` in 4 steps, 3,924 prompt / 139 completion tokens, 3,535 cache hits.
- [`docs/MODEL_PROFILE.md`](docs/MODEL_PROFILE.md) — exact `llama-server` command and gotchas
- [`docs/research/INDEX.md`](docs/research/INDEX.md) — OpenClaw, DeepSeek Harness/ECC, small-model harnesses, runtime verification

## Next

Phase 1: Cargo skeleton, `ModelClient`, and the startup capability probe (health check + verify that native tools actually reach the prompt). Waiting on: exact GGUF path, llama.cpp build version, and GPU availability.
