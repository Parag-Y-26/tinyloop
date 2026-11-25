# Eval results — real model run

Date: 2026-11-25. All numbers below are from actual runs, not estimates.

## Setup

- Windows 11, Rust 1.97.1.
- `llama serve` build **12652**, `Spark-X2.6-4B-Q4_K_M.gguf`, context **8192**, `-np 1`, `--jinja`, `--host 127.0.0.1 --port 9941`.
- Thinking disabled per request via `chat_template_kwargs.enable_thinking=false`.
- Harness: `target/debug/eval.exe`, Auto mode, max 6 steps per task, deterministic PowerShell verifier per task.
- 15 tasks: 5 smoke + 10 full, each a tiny read → patch → verify task with a unique token.

## Aggregate

| Metric | Value |
|---|---:|
| Tasks passed | **15 / 15 (100%)** |
| Average steps | 3.0 |
| Prompt tokens total | 38,127 |
| Completion tokens total | 1,858 |
| Cache-hit share | 88.9% (33,893 tokens) |
| Tokens per successful task | 2,666 |
| Prefill rate (server `prompt_ms`) | 4,551 tok/s |
| Decode rate (server `predicted_ms`) | 40.9 tok/s |
| Average wall time | 4.09 s |
| Total wall time | 61.4 s |
| Tool errors | 0 |

## Per task

| Task | Result | Steps | Prompt | Completion | Wall ms |
|---|---|---:|---:|---:|---:|
| full-06-foxtrot | PASS | 3 | 2517 | 115 | 3557 |
| full-07-golf | PASS | 3 | 2573 | 134 | 3801 |
| full-08-hotel | PASS | 3 | 2561 | 131 | 3629 |
| full-09-india | PASS | 3 | 2527 | 121 | 3522 |
| full-10-juliet | PASS | 3 | 2515 | 116 | 3799 |
| full-11-kilo | PASS | 3 | 2573 | 134 | 4685 |
| full-12-lima | PASS | 3 | 2581 | 136 | 6979 |
| full-13-mike | PASS | 3 | 2587 | 136 | 4574 |
| full-14-november | PASS | 3 | 2565 | 133 | 3957 |
| full-15-oscar | PASS | 3 | 2519 | 117 | 4774 |
| smoke-01-alpha | PASS | 3 | 2513 | 115 | 3736 |
| smoke-02-bravo | PASS | 3 | 2495 | 110 | 3208 |
| smoke-03-charlie | PASS | 3 | 2519 | 116 | 3347 |
| smoke-04-delta | PASS | 3 | 2557 | 126 | 3881 |
| smoke-05-echo | PASS | 3 | 2525 | 118 | 3962 |

## Phase-1 measurements

| Required measurement | Result |
|---|---|
| Prefill rate | **4,551 tok/s** (sum of server `prompt_ms`) |
| Decode rate | **40.9 tok/s** |
| Tools-reached-prompt delta | **23 → 130** prompt tokens in the startup probe |
| `cache_n` turn 2 vs 6 | Turn 2 ≈ 2.2K–2.4K cached tokens; tasks finish in 3 steps, so a 6th turn was not reached. Cache-hit share 88.9%. |
| Malformed-call rate | **0 / 45 tool calls** (0%) |

## Limits

- These are deliberately small patch tasks. They measure harness reliability and speed, not hard reasoning.
- The server log reports `failed to fit params to free device memory: n_gpu_layers ... -2, abort`; no CUDA/Metal backend line appears. Treat the rates as this machine's measured path, not a GPU claim.
- Timing comes from llama.cpp's own `timings` fields; wall time includes verifier and harness overhead.
- The eval tasks share a warm server, so the first-request cold-start cost is excluded.

## Reproduce

```bash
cd ~/.llama/models
llama serve -m Spark-X2.6-4B-Q4_K_M.gguf -a spark-x26 -c 8192 -np 1 -ngl all -fa on --jinja --host 127.0.0.1 --port 9941

cd C:/Users/parag/Downloads/tinyloop
cargo run --bin eval
```
