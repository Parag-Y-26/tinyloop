# MODEL_PROFILE — Spark-X2.6-4B Q4_K_M at 8192 tokens

Verified runtime settings for `tinyloop`. Full citations: [`research/model-runtime.md`](research/model-runtime.md).

## Recommended server command (Windows)

```bat
llama-server ^
  -m C:\models\Spark-X2.6-4B-Q4_K_M.gguf ^
  --host 127.0.0.1 --port 9941 ^
  -c 8192 -np 1 ^
  -ngl 99 -fa on ^
  --jinja ^
  --temp 1.0 --top-p 0.95 --top-k 0 --repeat-penalty 1.0
```

POSIX: replace `^` with `\`.

| Flag | Why |
|---|---|
| `-c 8192` | Without it the model loads its ~1M trained context — wrong KV allocation for an agent |
| `-np 1` | One slot keeps the whole 8192 budget; `--parallel` default `-1` shares it |
| `-ngl 99` | Full GPU offload; omit/adjust for CPU-only |
| `-fa on` | Flash attention; q8_0 KV without FA kernels can silently fall back to CPU attention |
| `--jinja` | **Mandatory** for tool calling; without it `tools` returns `tools param requires --jinja flag` |
| `--temp 1.0 --top-p 0.95 --top-k 0` | Vendor-published sampling; keep determinism in the verifier, not decoding |
| `--host 127.0.0.1` | Do not expose this server; mlx-lm's server warns it has only basic security checks |

**Do not** pass `--chat-template-file` / `--chat-template`: the GGUF's embedded template handles thinking and tools, and replacing it changes the tool-call grammar.

## Build requirements

- llama.cpp **≥ b10828** (support for `spark2_5` landed in PR #27868). Older builds: `unknown model architecture: 'spark2_5'`.
- Use a build that includes the Spark2.5 native tool parser (`TAG_WITH_TAGGED` format).

## Request shape (client)

```json
{
  "model": "spark-x26",
  "messages": [...],
  "tools": [{"type":"function","function":{"name":"read_file","description":"...","parameters":{...}}}],
  "tool_choice": "auto",
  "chat_template_kwargs": {"enable_thinking": false},
  "temperature": 1.0, "top_p": 0.95, "top_k": 0,
  "max_tokens": 512
}
```

- `tool_call.function.arguments` must be a **dictionary**, not a JSON string.
- Raw model output is `<tool_call>name<arg_key>k</arg_key><arg_value>v</arg_value></tool_call>`; llama.cpp parses it into OpenAI `tool_calls`.
- `reasoning_effort` accepts only `high` / `low`; `medium` → 400. To disable thinking use `enable_thinking: false` or top-level `reasoning_effort: "none"`.
- Thinking does **not** break tool calling; the `<tools>` block is emitted regardless. But if thinking consumes all `max_tokens`, `content` comes back empty with `finish_reason: length`.
- Keep assistant `reasoning_content` in multi-turn history so the template re-renders consistently.
- `parallel_tool_calls` is off by default — keep it off for tinyloop.
- **Do not combine `response_format` schemas with tools** (`Cannot specify grammar with tools`). Use a separate tool-free request when a constrained final answer is needed.
- `response_format` supports `json_object`, `json_object + schema`, `json_schema`; schemas become GBNF. A schema constrains output but is **not** injected into the prompt — describe the structure in messages.

## KV cache / memory

- 36 layers: 27 sliding-window (512 tokens) + 9 full attention. Only the 9 grow with context: ~36 KiB/token at f16.
- Keep `--swa-full` **off** (it expands SWA cache to full context and destroys the saving).
- At 8192 tokens, f16 KV is fine (~0.36 GB per a community measurement). Only use `-ctk q8_0 -ctv q8_0` when VRAM is tight and your backend has FA kernels.
- Verify actual allocation in startup log: `llama_kv_cache: size = ... MiB`.
- Prefix reuse (`--cache-prompt`) is on by default; confirm with `timings.cache_n` / `usage.prompt_tokens_details.cached_tokens`. Cache hits can make logits nondeterministic on quantized models — pin flags or disable caches for reproducible verification passes.

## GGUF files

Vendor repo `XHToken/Spark-X2.6-4B-GGUF`:

| File | Size | Notes |
|---|---|---|
| `Spark-X2.6-4B-Q4_K_M.gguf` | 3,543,441,312 B (3.30 GiB) | **recommended**; keeps BF16 embeddings + Q8_0 output tensor |
| `Spark-X2.6-4B-Q8_0.gguf` | 4,375,021,824 B (4.08 GiB) | near-lossless |
| `Spark-X2.6-4B.gguf` | 8,229,921,024 B (7.67 GiB) | untagged BF16 |

```bat
curl -L -o Spark-X2.6-4B-Q4_K_M.gguf ^
  https://huggingface.co/XHToken/Spark-X2.6-4B-GGUF/resolve/main/Spark-X2.6-4B-Q4_K_M.gguf
```

Smaller community quant: `bartowski/Spark-X2.6-4B-GGUF` Q4_K_M ≈2.62 GB (imatrix, b11716). AtomicChat `AD-Q4_K_M` ≈2.79 GB.

## MLX (later, Apple Silicon only)

Official `XHToken/Spark-MLX-LLM` works on original HF checkpoints with `spark-mlx-server`; thinking control is `--no-thinking` or `--thinking --reasoning-effort low|high`; default KV is BF16 with opt-in `--kv-bits 8`. Requires macOS ≥26.2 / Metal 4 — not a drop-in for Windows llama.cpp. Plain `mlx-lm` users get `mlx_lm.cache_prompt` for prefix reuse.

## Phase 1 acceptance checklist

- [ ] `llama-server` build reports `spark2_5` support
- [ ] `/health` returns OK on port 9941
- [ ] fixed prompt with/without `tools` yields different `prompt_tokens` (tools not silently dropped)
- [ ] one tool call round-trips into `message.tool_calls` with dict arguments
- [ ] `enable_thinking: false` returns non-empty `content` and valid tool calls
- [ ] 8192 context fits in memory; startup log confirms KV size
- [ ] `timings.cache_n` shows prefix reuse across turns
