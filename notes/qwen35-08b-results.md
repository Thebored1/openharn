# Notes: Qwen 3.5 0.8B behavioral test results

## Test Results (Linux, llama-server 9585, --jinja)

| Mode | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding | Total |
|---|---|---|---|---|---|---|---|
| **Default (native tools)** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |
| YESNO + NO_THINK | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | **5/6** |
| PROMPT_TOOLS + STRICT | timeout | — | — | — | — | — | — |
| YESNO + STRICT | timeout | — | — | — | — | — | — |
| NARROW | timeout | — | — | — | — | — | — |

## Setup

- Model: `Qwen3.5-0.8B-UD-Q8_K_XL.gguf` (~1.2 GB, Q8_0 quant)
- Server: llama-server 9585, `--jinja --ctx-size 8192 -ngl 0`
- ~150 tok/s prompt, ~16 tok/s generation on CPU

## Key findings

1. **Native tools work** — Qwen 3.5 0.8B scores 6/6 in default mode. The model emits
   native tool calls when the server supports them (llama-server 9585 with `--jinja`).
   No special flags needed.

2. **Grammar modes timeout** — PROMPT_TOOLS+STRICT, YESNO+STRICT, and NARROW all
   timeout. The GBNF grammar is too complex for the 0.8B model's context window. The
   model gets stuck trying to generate valid `<tool_call>` output.

3. **YESNO without grammar works** — 5/6, misses glob_system (model searches but doesn't
   use the dedicated tool).

4. **Fast** — ~16 tok/s on CPU, simple queries complete in ~5s.

## Comparison with other models

| Model | Params | Default | Best mode | Notes |
|---|---|---|---|---|
| MiniCPM-V-4.6 | ~4B | 6/6 | any | Best overall |
| LFM2-8B-A1B | 8B (1B active) | 3/6 | PROMPT_TOOLS+STRICT | Needs grammar |
| LFM2-1.2B-Tool | 1.2B | 4/6 | YESNO+STRICT | Tool-tuned |
| **Qwen 3.5 0.8B** | **0.8B** | **6/6** | **default** | Native tools work! |

## Conclusion

**Qwen 3.5 0.8B is viable for CPU agents.** It scores 6/6 in default mode — no special
flags, no grammar, no YES/NO. The model emits native tool calls when the server supports
them (llama-server 9585 with `--jinja`).

For CPU agents, use:
- **Qwen 3.5 0.8B** (6/6 default) — fastest, smallest, simplest
- **MiniCPM-V-4.6** (6/6 all modes) — best overall
- **LFM2-8B-A1B** (6/6 with PROMPT_TOOLS+STRICT) — needs grammar
- **LFM2-1.2B-Tool** (6/6 with YESNO+STRICT) — tool-tuned
