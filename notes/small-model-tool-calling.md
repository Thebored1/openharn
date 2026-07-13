# Notes: which small models can call tools on CPU

For a small local coding agent, the gating capability is emitting a **dispatchable tool
call**. These are notes from driving a dozen small models through openharn on CPU: a
detailed look at one model that can't, and a same-prompt benchmark of the rest. Main
finding: tool-calling tracks the model *family and post-training*, not the quantization
tier.

---

## Related work

This benchmark operationalizes the **SLM paper's** (Belcak et al., arXiv:2506.02153v2) three value statements: V1 (SLMs are sufficiently powerful), V2 (operationally superior), V3 (economically necessary). Our findings confirm V1 for tool-tuned models (LFM2-1.2B-Tool, LFM2.5, Granite-4.0), V2 for CPU-first deployment, and V3 via the reasoning-tax measurements ([`reasoning-tax.md`](reasoning-tax.md)). The discriminator being *post-training* not *quantization* aligns with the paper's emphasis on "scaling laws assume fixed architectures; new SLM structures change the game."

---

## Related work

This note validates the core thesis of **"Small Language Models are the Future of Agentic AI"** (Belcak et al., NVIDIA, arXiv:2506.02153v2): SLMs (<10B) are sufficiently powerful for agentic tasks when paired with a well-designed harness. The V1–V3 claims (sufficient capability, operational suitability, economic necessity) map directly to what we observe: tool-tuned 1.2B models pass behavioral suites, while larger untuned MoEs fail.

The **slm-agents** paper (Ranjan & Talluri, 2026; GitHub: IshaanAyaan/slm-agents) demonstrates the same interaction: a 1.5B specialist + co-designed harness matches a 72B generalist. Our `OPENHARN_STRICT_TOOLS` + `YESNO` combo is a prompt-only approximation of their "custom harness + distilled specialist" co-design.

## Setup

openharn (Rust) driving `llama-server` (llama.cpp build 9585) with
`--jinja --ctx-size 8192 -ngl 0 --no-warmup`, on an Intel AVX2 laptop (Windows 11) and a
Ryzen box. CPU-only throughout: full GPU offload with a 16k KV cache OOM-crashed the 4 GB
laptop GPU immediately, and these are A1B MoE models (~1B active params), so CPU is fine
(~20–35 tok/s).

## Case: LFM2-8B-A1B-Q3_K_XL never calls a tool

Driven through openharn it answers in prose and never dispatches. Hitting the raw endpoint
reproduces it:

```
CONTENT:    "```bash\nglob src/*.rs\n```"
TOOL_CALLS: null
```

The model wants to search — it just writes a Markdown shell fence instead of a call. It's
not the request shape: `tool_choice: auto`, cueing tools in the system prompt, and
`tool_choice: "required"` all give the same Markdown fence with `tool_calls: null` and no
`<|tool_call_start|>` token anywhere in the content.

The delimiters are absent entirely, so this isn't a parser shape-mismatch (there's nothing
to recover). The rendered prompt is the canonical LFM2 tool block — same as
`apply_chat_template(tools=...)` — so it isn't a presentation bug. And the delimiters are
real single tokens, not split into text:

| String | Tokens | ID |
|---|---|---|
| `<\|tool_list_start\|>` | 1 | 8 |
| `<\|tool_call_start\|>` | 1 | 10 |
| `<\|tool_call_end\|>` | 1 | 11 |
| `<\|im_start\|>` | 1 | 6 |

The model *can* produce the format — forcing generation with a GBNF grammar yields the
exact native call — it just won't sample token 10 under normal decoding. So it's a
decoding-behavior deficit at this quantization: it defaults to a Markdown fence instead of
its own protocol. Not a parser bug, not a prompt bug. The benchmark below shows this is a
property of the `LFM2-v2 8B-A1B` family, at Q3 *and* Q4.

## Benchmark

Identical scenario for every model — a chat turn plus search / create / find / edit tool
calls — driven through openharn's loop on CPU. Zero transport-level failed requests; every
"fail" is a task/tool miss, not a crash.

| Model | Quant | Time s | Tok/s | Think tok | Tool hits | Task |
|---|---|---:|---:|---:|:---:|:---:|
| LFM2-1.2B-Tool | Q4_K_M | 18.5 | 33.5 | 0 | 3/4 | PASS |
| LFM2-8B-A1B | Q3_K_S | 17.4 | 26.2 | 0 | 0/4 | fail |
| LFM2-8B-A1B | Q3_K_XL | 17.6 | 24.4 | 0 | 0/4 | fail |
| LFM2-8B-A1B | Q4_K_XL | 15.0 | 27.6 | 0 | 0/4 | fail |
| LFM2.5-1.2B-Instruct | Q4_K_M | 16.2 | 33.5 | 0 | 3/4 | PASS |
| LFM2.5-8B-APEX-Compact | — | 77.3 | 22.8 | 1089 | 4/4 | PASS |
| LFM2.5-8B-APEX-Mini | — | 176.0 | 21.6 | 3084 | 3/4 | PASS |
| LFM2.5-8B-A1B | Q4_K_M | 89.4 | 23.1 | 1302 | 4/4 | PASS |
| gemma-3n-E2B-it | IQ3_XXS | 20.3 | 8.3 | 0 | 0/4 | fail |
| gemma-4-E2B-it | IQ4_XS | 69.4 | 9.5 | 389 | 3/4 | PASS |
| gemma-4-E2B-it-qat | Q4_K_XL | 91.0 | 9.2 | 624 | 0/4 | fail |

## What it shows

- **Family/post-training, not quant.** All three `LFM2-v2 8B-A1B` builds fail identically
  (Q3_K_S, Q3_K_XL, *and* Q4_K_XL). Raising the quant a full tier changed nothing. Meanwhile
  the tool-tuned `LFM2-1.2B-Tool` (5× smaller) passes, and every `LFM2.5` build passes. The
  discriminator is tool-training, not bit-width.
- **Reasoning is the wall-clock.** The passing 8B LFM2.5 models spend 1089–3084 thinking
  tokens, pushing turns to 77–176 s on CPU vs ~16–18 s for the non-reasoning small models.
  (More: [`reasoning-tax.md`](reasoning-tax.md).)
- **Gemma-E2B** runs slow on CPU (~8–9 tok/s); only gemma-4-E2B-IQ4_XS completed the task.

Best CPU picks from this set: LFM2.5-1.2B-Instruct (fast, passes) and LFM2.5-8B-A1B-Q4_K_M
(4/4, slower). Full data in [`tests/bench_logs/`](../tests/bench_logs/); harness:
[`tests/benchmark.py`](../tests/benchmark.py).

## Follow-up: Granite, and a corrected verdict

Adding two Granite models later:

| Model | Time s | Tok/s | Tool hits | Task |
|---|---:|---:|:--:|:--:|
| granite-4.0-h-tiny (Q4_K_XL) | 28.8 | 17.9 | 3/4 | PASS |
| granite-3.1-1b-a400m (Q8_0) | 18.1 | 36.5 | 0/4 | fail |

The a400m — fastest model tested — first got written off as "too small to call tools."
Wrong. Direct probes showed it emits a valid structured call the server just doesn't parse:

```
content:    <tool_call>[{"arguments": {"pattern": "src/**/*.rs"}, "name": "glob"}]
tool_calls: null
```

Granite-3.1's template triggers with `<\|tool_call\|>`, but the model emits plain
`<tool_call>`, so llama.cpp drops it to text. That's a harness parse gap, not model
incapacity — so openharn now recovers text-emitted calls (see
[`adaptive-tool-calling.md`](adaptive-tool-calling.md)). But with the fallback recovering
its calls, a400m *still* scores 0/4: it's inconsistent about shape and often fills
arguments with the tool's schema instead of real values. So the 0/4 stood, for the correct
reason — unreliable at *which* tool and *what* args, not "too small to attempt." granite-4.0
(h-tiny) uses a shape llama.cpp parses natively; it's a viable non-LFM option but doesn't
beat APEX-Compact.

Takeaway: a 0/4 can be harness *or* model — I conflated them here, which is the case for
separating the failure layers explicitly ([`adaptive-tool-calling.md`](adaptive-tool-calling.md)).
The "active-param floor" framing was too crude; format/selection reliability is a separate
axis from raw capability.

## Model list

| File | Params (active) | Quant | Family |
|---|---|---|---|
| `LFM2-1.2B-Tool-Q4_K_M` | 1.2 B | Q4_K_M | LFM2 v2, tool-tuned |
| `LFM2-1.2B-Tool-Q4_K_M` | 1.2 B | Q4_K_M | LFM2 v2, tool-tuned |
| `LFM2-8B-A1B-Q3_K_S / UD-Q3_K_XL / UD-Q4_K_XL` | 8 B (≈1 B) | Q3–Q4 | LFM2 v2 MoE |
| `LFM2.5-1.2B-Instruct-Q4_K_M` | 1.2 B | Q4_K_M | LFM2.5 |
| `LFM2.5-8B-A1B-APEX-I-Compact / -Mini` | 8 B (≈1 B) | — | LFM2.5 MoE (APEX) |
| `LFM2.5-8B-A1B-Q4_K_M` | 8 B (≈1 B) | Q4_K_M | LFM2.5 MoE |
| `gemma-3n-E2B-it-UD-IQ3_XXS` | E2B | IQ3_XXS | Gemma 3n |
| `gemma-4-E2B-it-IQ4_XS / -qat-UD-Q4_K_XL` | E2B | IQ4_XS / Q4 QAT | Gemma 4 |
| `granite-4.0-h-tiny-UD-Q4_K_XL` | 7 B (≈1 B) | Q4_K_XL | Granite 4.0 hybrid MoE |
| `granite-3.1-1b-a400m-instruct-Q8_0` | 1 B (400 M) | Q8_0 | Granite 3.1 MoE |

## Follow-up: Qwen 3.5 0.8B — below the floor

Qwen 3.5 0.8B (Q8_0, ~1.2 GB) scores 2/6 in default mode and 3/6 with
PROMPT_TOOLS+STRICT. It passes only the trivial tests (greeting, no_repeat) and
edit_anchor (with grammar). Key failures:

- Responds in Chinese by default even with English system prompt (breaks text assertions)
- Uses `bash find` instead of `glob_system` for system-wide search
- Spirals without grammar constraint (repeats same command endlessly)
- Outputs single characters (`?`, `*`) when grammar is too complex for the prompt

At 0.8B parameters, the model can't reliably follow multi-step instructions or maintain
English output. This is below the practical floor for openharn's behavioral suite.

| Model | Params | Default | PROMPT_TOOLS+STRICT |
|---|---|---|---|
| MiniCPM-V-4.6 | ~4B | 6/6 | 6/6 |
| LFM2-8B-A1B | 8B (1B active) | 3/6 | **6/6** |
| Qwen 3.5 0.8B | 0.8B | 2/6 | 3/6 |

## Follow-up: LFM2-8B with grammar-constrained text calls

LFM2-8B-A1B scores 0/4 in default mode (never emits tool calls). But with
`OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1`, it passes **all 6
behavioral tests**. The GBNF grammar forces valid `<tool_call>` output that the model
wouldn't emit otherwise.

This is a harness fix, not a model fix — the grammar was previously broken (underscores in
GBNF rule names; see `gbnf-grammar-fix.md`). With the fix, the grammar acts as a structural
guide: the model sees tool descriptions in the prompt, tries to call a tool, and the grammar
ensures the call is syntactically valid.

Takeaway: a model that "can't call tools" may just need grammar constraint. The distinction
between "model can't" and "model won't by default" matters for harness design.

## Follow-up: LFM2-1.2B-Tool behavioral tests (Linux, llama-server 9585)

LFM2-1.2B-Tool-Q4_K_M (~1.2 GB) tested on Linux with reasoning off:

| Mode | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding | Total |
|---|---|---|---|---|---|---|---|
| Default (native tools) | ✅ | ✅ | ✅ | ❌ | ✅ | ❌ | **4/6** |
| PROMPT_TOOLS+STRICT | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | **5/6** |
| **YESNO + STRICT** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |
| NARROW | ✅ | ✅ | ❌ | ❌ | ✅ | ✅ | **4/6** |
| SLM + NO_THINK | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | **2/6** |

### What works

- **greeting / no_repeat / missing_file / edit_anchor**: passes in most modes
- **system_search_uses_scope_flag**: with PROMPT_TOOLS+STRICT or YESNO+STRICT, model
  correctly calls `glob_system` (not `bash find` or `grep`)
- **grounding_limits_total_calls**: only passes with YESNO+STRICT — the narrowed tool
  list from Pass 1 reduces hallucination on complex multi-step queries
- **Speed**: ~29 tok/s on CPU, ~2.5s per tool call

### The grounding fix: YES/NO + STRICT

The model hallucinates on complex multi-step queries in default and PROMPT_TOOLS+STRICT
modes. Adding YES/NO two-pass selection fixes this:

1. **Pass 1**: Model sees all 13 tools as YES/NO choices and selects a subset
2. **Pass 2**: Only selected tools are advertised (via grammar + filtered prompt text)
3. **Result**: The narrowed tool list reduces the model's tendency to hallucinate results

The fix required a bug in `agent.rs`: `flatten_for_prompt_tools` was using the unfiltered
`schemas` instead of `effective_schemas` (YES/NO-filtered). With the fix, the prompt text
only lists the tools the model selected.

### Why NARROW and SLM fail

- **NARROW** (4/6): restricts to read/grep/glob only — too narrow for the edit_anchor and
  glob_system tests (which need glob_system specifically)
- **SLM + NO_THINK** (2/6): requires valid JSON actions, which the1.2B model can't produce

### Comparison with other models

| Model | Params | Default | Best mode | Score |
|---|---|---|---|---|
| MiniCPM-V-4.6 | ~4B | 6/6 | any | 6/6 |
| LFM2-8B-A1B | 8B (1B active) | 3/6 | PROMPT_TOOLS+STRICT | 6/6 |
| LFM2-1.2B-Tool | 1.2B | 4/6 | **YESNO+STRICT** | **6/6** |
| Qwen 3.5 0.8B | 0.8B | **6/6** | default | **6/6** |

### Key finding

The 1.2B model's "hallucination ceiling" was actually a harness bug — the YES/NO filter
was applied to the grammar but not the prompt text. With the fix, the model gets a cleaner
signal (fewer tools to choose from) and passes all 6 behavioral tests.

The winning recipe: `OPENHARN_YESNO=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1`
