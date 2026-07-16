# Notes: LFM2 execution modes — full results

## Test Results: LFM2-8B-A1B

| Mode | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding | Total |
|---|---|---|---|---|---|---|---|
| Default (all 12 tools) | ✅ | ✅ | ❌ | ✅* | ❌ | ❌ | 3/6 |
| YES/NO + CodeAct | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | 2/6 |
| **PROMPT_TOOLS + STRICT_TOOLS** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |
| SLM harness | ✅ | ✅ | ❌ | ✅* | ❌ | ❌ | 2/6 |

* = false positive (model mentioned glob_system in text, didn't call it)

## Test Results: LFM2-1.2B-Tool

| Mode | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding | Total |
|---|---|---|---|---|---|---|---|
| Default (native tools) | ✅ | ✅ | ✅ | ❌ | ✅ | ❌ | 4/6 |
| PROMPT_TOOLS+STRICT | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | 5/6 |
| **YESNO + STRICT** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |
| NARROW | ✅ | ✅ | ❌ | ❌ | ✅ | ✅ | 4/6 |
| SLM + NO_THINK | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | 2/6 |

## What works: PROMPT_TOOLS + STRICT_TOOLS

```bash
OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 ./openharn /workspace
```

This is the winning combo for LFM2-8B. Three things happen:

1. **PROMPT_TOOLS**: Tools described in the system prompt text (model ignores native
   `tools` API)
2. **STRICT_TOOLS**: GBNF grammar forces output to either a schema-valid `<tool_call>[...]`
   or plain text — model physically cannot invent field names or malform calls
3. **NO_THINK**: Skip reasoning (faster on CPU)

### Why it works (vs default mode)

| Factor | Default | PROMPT_TOOLS + STRICT |
|---|---|---|
| Tool format | Native `tools` API (model ignores it) | Text descriptions + grammar constraint |
| Grammar | Never worked (broken rule names — see `gbnf-grammar-fix.md`) | Fixed: dashed rule names, text escape hatch |
| Model output | Descriptive text about what it would do | Forced into valid `<tool_call>` or honest text answer |

### Example session

```
> read the file banana_xyz.txt
  · read {"limit":100,"offset":0,"path":"banana_xyz.txt"}
[1 calls (1 total). Feeding grounding back and letting model answer.]
The file banana_xyz.txt does not exist in the current project directory.
```

## Why YES/NO + CodeAct only gets 2/6 (LFM2-8B)

The YES/NO pass correctly selects tools (e.g. `["read"]`), but Pass 2 uses the native
`tools` API which LFM2-8B ignores — it outputs descriptive text instead of `<tool_call>`.
The model CAN write valid tool calls when grammar-constrained, but without grammar it
defaults to prose.

## Why YES/NO + STRICT gets 6/6 (LFM2-1.2B-Tool)

The 1.2B tool-tuned model CAN use native tools (4/6 default), but hallucinates on complex
multi-step queries. Adding YES/NO narrows the tool list per turn:

1. **Pass 1**: Model sees all 13 tools as YES/NO choices → selects a subset
2. **Pass 2**: Only selected tools are advertised (grammar + filtered prompt text)
3. **Result**: Fewer tools = less confusion = no hallucination

This required a bug fix: `flatten_for_prompt_tools` was using unfiltered `schemas`
instead of `effective_schemas`. With the fix, the prompt text only lists selected tools.

```bash
# LFM2-1.2B-Tool: 6/6 with YESNO+STRICT
OPENHARN_YESNO=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 ./openharn /workspace
```

## Practical recommendation

| Model | Best mode | Recipe |
|---|---|---|
| LFM2-8B-A1B | PROMPT_TOOLS+STRICT | `OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1` |
| LFM2-1.2B-Tool | YESNO+STRICT | `OPENHARN_YESNO=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1` |