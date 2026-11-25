# Spark-X2.6-4B Q4_K_M at 8192 tokens — exact runtime findings

Research date: 2026-11-25. Target endpoint for tinyloop: `http://127.0.0.1:9941/v1/chat/completions` with a native OpenAI `tools` array. Claims below are tied to the numbered sources at the end; community measurements are labelled as such and are not vendor claims. The Transformers-format original weights are in `XHToken/Spark-X2.6-4B`, while the GGUF builds are in a separate vendor repo.[2][1] HF's GGUF metadata records `architecture: spark2_5` and `context_length: 1048576` for that repo.[9]

## 1. Recommended server command

```bash
# Official GGUF (3.54 GB decimal / 3.30 GiB); build MUST be >= b10828
llama-server ^
  -m C:\models\Spark-X2.6-4B-Q4_K_M.gguf ^
  --host 127.0.0.1 --port 9941 ^
  -c 8192 -np 1 ^
  -ngl 99 -fa on ^
  --jinja ^
  --temp 1.0 --top-p 0.95 --top-k 0 --repeat-penalty 1.0
```

Why each flag:

- `-m`: the file above is the only Q4_K_M in the vendor GGUF repo; Q8_0 (4.38 GB) and untagged BF16 (8.23 GB) are the only other weights there.[8]
- `--host`/`--port`: server defaults are `127.0.0.1` and port `9931` on current master, so port `9941` must be set explicitly.[10]
- `-c 8192`: context size flag; omitting it loads the model's trained context, which is `max_position_embeddings: 1048576` in the model config — i.e. a ~1M-token KV allocation, not what an 8K agent wants.[7][10]
- `-np 1`: llama-server's `--parallel` defaults to `-1` (auto), which shares the context among several slots; one slot keeps the whole 8192 budget for the agent.[10]
- `-ngl 99`: full GPU offload (max layers in VRAM); community 4B runs report ~3.3 GiB resident weights and ~30 tok/s decode on a 16 GB M4 with this setting.[10][30]
- `-fa on`: Flash Attention on; supported values are `on|off|auto`, default `auto`. With quantized KV, CUDA/Metal default builds have kernels for f16/q8_0 (and q4_0/bf16 on CUDA), while other cache types can silently fall back to CPU attention.[10][29]
- `--jinja`: mandatory for tool calling. llama.cpp's function-calling docs say `--jinja` is what enables OpenAI-style tool calling; current master's server README shows Jinja as default-enabled, but older builds defaulted off, so pass it explicitly. Without it, `tools` in the request raises `tools param requires --jinja flag`.[11][10][15]
- Sampling: `temperature=1.0, top_p=0.95, top_k=0/-1` are the vendor's published settings; `top_k: -1` in the vendor SGLang request means disabled, equivalent to llama.cpp's `0`.[1][3][31]
- KV cache type: leave at f16 default at 8192 tokens. `-ctk/-ctv` accept `f16`, `q8_0`, `q4_0`, `q4_1`, `iq4_nl`, `q5_0`, `q5_1`, etc., default f16; use `-ctk q8_0 -ctv q8_0` only if VRAM is tight and your backend has FA kernels for it.[10][29][16]

Do **not** add `--chat-template-file` or `--chat-template`: the GGUF's embedded chat template already handles thinking and tools, and replacing it changes the tool-call grammar the parser derives.[6][13][22]

## 2. Build requirements / known hard failures

- Native `spark2_5` GGUF architecture needs llama.cpp **b10828 or later** (vendor card, Ollama card, GitHub README all state this).[1][5][3] Support landed in PR #27868, merged 2026-09-06 as commit `3ad1ba7`; the release page for b10828 is dated the same day.[17][20]
- Older runtimes fail at load with `unknown model architecture: 'spark2_5'`; this was tracked for Ollama before v0.34.x.[28] Qualitatively newer artifact providers also pin builds: AtomicChat built/tested at b12015 (`a34de8618`) and notes thinking-on support from b11946; bartowski quantized with b11716.[21][4]
- `tool_call.function.arguments` must be a **dictionary** in the template; a JSON string raises `tool_call.function.arguments must be a dictionary; normalize JSON strings before apply_chat_template`. llama.cpp applies a `func_args_not_string` workaround for templates that declare object-argument support, but passing objects from your client avoids reliance on that path.[6][14]
- `reasoning_effort` values other than `high`/`low` are rejected by the embedded template (`Unexpected reasoning effort ... Supported types are high (default) and low.`); llama.cpp additionally rejects values listed in the template's rejected-efforts capability with HTTP 400-style errors.[6][15]
- Do not rely on context shifting for a long-running agent: llama.cpp's interleaved-SWA cache cannot remove/shift tokens once the window has slid; context-shift is disabled by default anyway.[18][10]
- Keep `--swa-full` off (default false). It expands the sliding-window cache to full context and destroys the memory saving.[10][18]
- The 1M context is a configuration ceiling, not a quality guarantee: the vendor's own long-context table shows MRCR 512k–1M at 26.0, and independent benchmarks record weak spontaneous tool selection on this model.[1][32]

## 3. Thinking control — and tool calling

Thinking is **on by default** in both the chat template and the vendor's SGLang `qwen3` reasoning parser (`reasoning_effort` defaults to `high`).[1][3][6]

Per request (on llama.cpp):

```json
{
  "model": "spark-x26",
  "messages": [...],
  "tools": [...],
  "chat_template_kwargs": { "enable_thinking": false },
  "temperature": 1.0, "top_p": 0.95, "top_k": 0,
  "max_tokens": 512
}
```

- `chat_template_kwargs.enable_thinking = false` is the verified switch: the template branches on it and pre-fills `</think>` instead of `<think>`; llama.cpp's server reads this kwarg explicitly and also accepts top-level `reasoning_effort: "none"` as "disable thinking".[6][10][15]
- Lower but not off: `{"chat_template_kwargs": {"reasoning_effort": "low"}}`. The template only renders `high` (default) and `low`; `medium`, `xhigh`, `none` inside `chat_template_kwargs` are invalid here.[6][15] The vendor reports that high→low costs 1.7 average score points but cuts output length by 22.0% for the 4B model.[1]
- Thinking does **not** break tool calling: the `<tools>` block is emitted in the system turn regardless of `enable_thinking`, and llama.cpp's tool parser is generated from that same template.[6][13] Community cards report `tool_calls` parsed with `--jinja` and also verify thinking-off replies.[21][31][22] Keep assistant `reasoning_content` in multi-turn history so the template re-renders `<think>...</think>` consistently.[6][10]
- Give thinking room when enabled: independent users report empty `content` with `finish_reason=length` when the whole `max_tokens` budget is consumed inside `<think>`; `low` is not guaranteed to be shorter than `high` on llama.cpp.[31][6]
- Ollama alternative uses a different wire field: `"think": false` in `/api/chat`, not `chat_template_kwargs`.[25][5]

## 4. Native tools format and tool_call shape

Request: standard OpenAI array — `tools: [{"type":"function","function":{"name":"get_weather","description":"...","parameters":{...JSON Schema...}}}]`. The template serializes each `tool.function` with compact JSON into a `<tools>` block in the initial system turn.[6][11]

Raw model output (native Spark format, parsed by llama.cpp into OpenAI `tool_calls`):

```text
<tool_call>get_weather<arg_key>city</arg_key><arg_value>Paris</arg_value><arg_key>days</arg_key><arg_value>3</arg_value></tool_call>
```

- llama.cpp classifies Spark2.5 as a `TAG_WITH_TAGGED` tool format with exactly this `<tool_call>name<arg_key>...<arg_value>...` shape (added in PR #27868), and returns OpenAI-shaped `tool_calls` with `type: "function"`, `function.name` and `function.arguments` (a string, or object when the template requires).[13][17][14]
- Tool results go back as role `tool`; the template wraps consecutive tool messages in `<|Tool|>` and `<tool_response>...</tool_response>`.[6][27]
- `parallel_tool_calls` is off by default and only enabled per request; Spark's template can render several calls per turn, so enable it deliberately if your loop needs it.[10][11][27]
- `tool_choice: "none"` suppresses tools; other `tool_choice` values require `--jinja` and native template support.[15]
- Practical agent lesson: pass tools via the API `tools` array — in one reported test, prompt-only "reply with `RUN: ...`" instructions produced malformed `<tool_call>` wrappers 26 times in 6 attempts, while OpenAI-style tools scored 9/10 on the same task.[30]
- Independent benchmark caveat: on one lab's tool-*perception* suite (does it decide to call a tool unprompted), Spark-X2.6 scored 0/60 even with thinking off; when a tool was directly requested it chained tools at 86.7%. Treat tool-use policy as something tinyloop must enforce, not infer.[32]

## 5. JSON Schema / GBNF constrained decoding on /v1/chat/completions

- `response_format` supports `{"type":"json_object"}`, `{"type":"json_object","schema":{...}}`, `{"type":"json_schema","schema":{...}}`, and the OpenAI wrapper `{"type":"json_schema","json_schema":{"name":...,"strict":...,"schema":{...}}}`; schemas are converted to GBNF grammars for constrained sampling.[10][12]
- `/completion` and `/v1/completion` also accept a top-level `json_schema` or raw `grammar` field; `json_schema` and `grammar` cannot both be set.[10][15]
- Important: the schema constrains output only — it is **not** injected into the prompt, so describe the expected structure in your messages (tool schemas are the exception: they are injected into the prompt).[12]
- Interaction with tools: llama.cpp throws `Cannot specify grammar with tools` when tools are active with a non-`none` tool_choice and a grammar, and the server rejects `grammar` alongside tools with `Cannot use custom grammar constraints with tools`.[14][15] The autoparser's content branch handles `json_schema`, while tools dispatch to the tool parser; coexistence is parser-dependent and not documented as supported for Spark2.5.[13] For tinyloop: use `tools` only during action selection, and make a separate tool-free request with `response_format` when you need a schema-constrained final answer.
- Parser-specific bugs have occurred in llama.cpp where a model's native chat handler ignored `response_format` entirely (that upstream fix was for a different "Muse Spark" model, not XHToken Spark); verify your build with a one-token schema probe before relying on it.[34]

## 6. KV cache, sliding-window saving, prompt caching

- Architecture: 36 layers in a strict 3:1 pattern — 27 sliding-window layers (`sliding_window: 512`) and 9 full-attention layers; 16 query heads, 4 KV heads, head dim 256, tied 131072 vocab.[7][22]
- Only the 9 full layers grow with context: 9 × 4 × 256 × 2 (K+V) × 2 bytes ≈ **36 KiB/token at f16** (derived from config [7]); the 27 SWA layers are capped by llama.cpp's interleaved-SWA cache at roughly the window plus one micro-batch.[7][18] A community card tabulates ~0.36 GB at 8K with f16 and ~0.19 GB with q8_0 for this architecture, so f16 at 8192 is not a VRAM problem.[22]
- Check the actual allocation in the startup log (`llama_kv_cache: size = ... MiB`); `-np`, `-ub`, `--swa-full`, `--kv-unified` and cache dtype all change it.[10][18] A pending upstream PR proposes separate `--cache-type-k-swa`/`--cache-type-v-swa` flags so the bounded SWA cache can stay at higher precision than the growing full cache; do not assume they exist in older builds.[19]
- Prefix reuse: `--cache-prompt` is enabled by default; `--cache-reuse N` sets the minimum chunk for KV-shift reuse; `--cache-ram` defaults to 8192 MiB; `--cache-idle-slots` saves idle slots; `/slots` exposes per-slot state; `--slot-save-path` enables slot KV save/load.[10] Response `timings.cache_n` is the number of prompt tokens reused, and `usage.prompt_tokens_details.cached_tokens` is the OpenAI-style equivalent — instrument these to verify tinyloop's system prompt is actually being reused.[10] With SWA, KV-shift-style reuse has limits because slid tokens are pruned.[18]

## 7. GGUF files, sizes and download locations

Vendor repo `XHToken/Spark-X2.6-4B-GGUF` contains exactly three weight files (byte sizes from the HF tree API):[8]

| File | Bytes | Approx size | Notes |
|---|---|---|---|
| `Spark-X2.6-4B-Q4_K_M.gguf` | 3,543,441,312 | 3.30 GiB / 3.54 GB | recommended; vendor quant keeps BF16 embeddings + Q8_0 output tensor, hence larger than a stock Q4_K_M [8][21] |
| `Spark-X2.6-4B-Q8_0.gguf` | 4,375,021,824 | 4.08 GiB / 4.38 GB | near-lossless [8] |
| `Spark-X2.6-4B.gguf` | 8,229,921,024 | 7.67 GiB / 8.23 GB | untagged BF16 [8] |

Direct downloads:

```bash
# vendor Q4_K_M
curl -L -o Spark-X2.6-4B-Q4_K_M.gguf \
  https://huggingface.co/XHToken/Spark-X2.6-4B-GGUF/resolve/main/Spark-X2.6-4B-Q4_K_M.gguf

# smaller community Q4_K_M (2.62 GB, b11716 imatrix)
hf download bartowski/Spark-X2.6-4B-GGUF --include "Spark-X2.6-4B-Q4_K_M.gguf" --local-dir ./
```

Other sources: bartowski Q4_K_M 2.62 GB and AtomicChat `Spark-X2.6-4B-AD-Q4_K_M.gguf` 2.79 GB.[4][21] Sharp-Spark `Q4_K_XL` is 2.68 GB; Ollama tags `:q4_k_m` (3.5 GB) and `:q8_0` (4.4 GB) exist, and ModelScope hosts the original format.[22][25][26] A separate 624M DSpark draft GGUF exists for speculative decoding, not needed for a first tinyloop build.[33]

## 8. MLX alternative (later option)

Official XHToken Spark-MLX-LLM runs the original HF checkpoints (no GGUF conversion) with separate `spark-mlx-convert`, `spark-mlx-generate`, `spark-mlx-chat`, `spark-mlx-server` entry points; thinking control is `--no-thinking` or `--thinking --reasoning-effort low|high`; default KV is unquantized BF16 with an opt-in `--kv-bits 8` for full-attention layers.[23] It targets Apple Silicon with macOS >= 26.2 and Metal 4 (plus Linux CPU/CUDA paths), so it is not a drop-in for a Windows llama.cpp server.[23] Upstream mlx-lm provides `mlx_lm.cache_prompt` for prefix reuse if you later move to plain mlx-lm tooling.[24]

## Sources

[1] https://huggingface.co/XHToken/Spark-X2.6-4B-GGUF
[2] https://huggingface.co/XHToken/Spark-X2.6-4B
[3] https://github.com/XHToken/Spark-X2.6
[4] https://huggingface.co/bartowski/Spark-X2.6-4B-GGUF
[5] https://ollama.com/SparkLLM/Spark-X2.6-4B
[6] https://huggingface.co/XHToken/Spark-X2.6-4B/raw/main/chat_template.jinja
[7] https://huggingface.co/XHToken/Spark-X2.6-4B/raw/main/config.json
[8] https://huggingface.co/api/models/XHToken/Spark-X2.6-4B-GGUF/tree/main?recursive=true
[9] https://huggingface.co/api/models/XHToken/Spark-X2.6-4B-GGUF
[10] https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md
[11] https://github.com/ggml-org/llama.cpp/blob/master/docs/function-calling.md
[12] https://github.com/ggml-org/llama.cpp/blob/master/grammars/README.md
[13] https://github.com/ggml-org/llama.cpp/blob/master/docs/autoparser.md
[14] https://github.com/ggml-org/llama.cpp/blob/master/common/chat.cpp
[15] https://github.com/ggml-org/llama.cpp/blob/master/tools/server/server-common.cpp
[16] https://github.com/ggml-org/llama.cpp/blob/master/tools/cli/README.md
[17] https://github.com/ggml-org/llama.cpp/pull/27868
[18] https://github.com/ggml-org/llama.cpp/pull/13194
[19] https://github.com/ggml-org/llama.cpp/pull/32240
[20] https://github.com/ggml-org/llama.cpp/releases/tag/b10828
[21] https://huggingface.co/AtomicChat/Spark-X2.6-4B-GGUF
[22] https://huggingface.co/peculiar-ragdoll/Sharp-Spark-X2.6-4B-GGUF
[23] https://github.com/XHToken/Spark-MLX-LLM
[24] https://github.com/ml-explore/mlx-lm
[25] https://ollama.com/SparkLLM/Spark-X2.6-4B:q4_k_m
[26] https://www.modelscope.cn/models/XHToken/Spark-X2.6-4B
[27] https://github.com/XHToken/Spark-plugin
[28] https://github.com/ollama/ollama/issues/18195
[29] https://github.com/ggml-org/llama.cpp/pull/28456
[30] https://note.com/hacklog_stealth/n/n59f8e4223831?hl=en
[31] https://apisitlee.com/spark-x2-6-4b-ondevice-llamacpp-mac-guide
[32] https://freshcontext.io/benchmarks/models/spark-x2.6-4b
[33] https://huggingface.co/XHToken/Spark-X2.6-4B-DSpark-GGUF
[34] https://github.com/ggml-org/llama.cpp/pull/32017
