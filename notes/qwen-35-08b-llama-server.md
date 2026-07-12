# Notes: Qwen 3.5 0.8B and llama-server on CPU

## Model: Qwen3.5-0.8B-UD-Q8_K_XL.gguf

- **Family**: Qwen 3.5 (not Qwen 3)
- **Size**: ~752M params (0.8B)
- **Quant**: Q8_K_XL
- **Speed**: ~17–20 tok/s on CPU (Intel AVX2 / Ryzen)

### Chat template

The model's embedded template is a **vision template** that breaks text-only chat. Override it:

```bash
llama-server -m Qwen3.5-0.8B-UD-Q8_K_XL.gguf \
  --chat-template '{{#if .tools}}<|tool_list_start|>{{.tools}}<|tool_list_end|>{{/if}}{{#each .messages}}<|{{.role}}|>{{.content}}<|end|>{{/if}}<|assistant|>' \
  --ctx-size 8192 -ngl 0 --no-warmup --host 0.0.0.0 --port 8080
```

Without this, the server crashes with "the supplied chat template is not supported."

## llama-server build

**Homebrew bottle (`llama.cpp@9960`) lacks CPU backend** — it only builds Vulkan/Metal. On CPU-only machines it fails with:

```
no backends are loaded. hint: use ggml_backend_load() or ggml_backend_load_all()
```

### Build from source (with CPU backend)

```bash
git clone --depth 1 https://github.com/ggml-org/llama.cpp.git
cd llama.cpp
cmake -B build -DGGML_CPU=ON -DCMAKE_BUILD_TYPE=Release
cmake --build build --config Release -j$(nproc)
```

Binaries land in `build/bin/llama-server` and `build/bin/llama-cli`.

### Old build (`llama-b9888`)

The previous commit `b9888` (from the `llama-b9888` directory) had full CPU support but **no Qwen 3.5 architecture support** — it segfaults on this model. The new build (`llama.cpp` commit `6b4dc21` / `0c4fa7a`) adds Qwen 3.5 but requires the manual build above.

## Tool calling with this model

Qwen 3.5 0.8B **does not reliably emit tool calls natively** (0/4 in benchmark). Requires:

- `OPENHARN_PROMPT_TOOLS=1` (describe tools in prompt)
- `OPENHARN_STRICT_TOOLS=1` (grammar-force valid calls)
- `OPENHARN_TOOLS=read` (restrict to one tool at a time)

With these, it makes valid `read` calls through the grammar path.

### What doesn't work

- Native tool calling (`tool_calls` field) — model never uses it
- Multiple tools in strict mode — full 12-tool grammar fails to parse on new llama.cpp
- Reasoning (`OPENHARN_NO_THINK=1` required for speed; `STRICT_TOOLS` + `NO_THINK` incompatible)

## Circuit breaker with this model

`OPENHARN_MAX_CALLS=1` (default) + `OPENHARN_TOTAL_MAX=5` (default) works well:
- Model makes 1 call → grounding fires → model answers
- `OPENHARN_NO_THINK=1` keeps it fast (~20 tok/s)