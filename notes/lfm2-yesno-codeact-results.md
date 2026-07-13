# Notes: LFM2-8B-A1B execution modes — full results

## Test Results

| Mode | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding | Total |
|---|---|---|---|---|---|---|---|
| Default (all 12 tools) | ✅ | ✅ | ❌ | ✅* | ❌ | ❌ | 3/6 |
| YES/NO + CodeAct | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | 2/6 |
| **PROMPT_TOOLS + STRICT_TOOLS** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |
| SLM harness | ✅ | ✅ | ❌ | ✅* | ❌ | ❌ | 2/6 |

* = false positive (model mentioned glob_system in text, didn't call it)

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

## Why YES/NO + CodeAct only gets 2/6

The YES/NO pass correctly selects tools (e.g. `["read"]`), but Pass 2 uses the native
`tools` API which LFM2-8B ignores — it outputs descriptive text instead of `<tool_call>`.
The model CAN write valid tool calls when grammar-constrained, but without grammar it
defaults to prose.

## Practical recommendation

For LFM2-8B on CPU, use **PROMPT_TOOLS + STRICT_TOOLS** (not YES/NO):

```bash
# Best for LFM2-8B (6/6)
OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 ./openharn /workspace

# YES/NO + CodeAct only works for explicit python tasks (2/6)
OPENHARN_YESNO=1 OPENHARN_NO_THINK=1 ./openharn /workspace
```