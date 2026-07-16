# Notes: LFM2-8B with Code-as-Action (python tool) + YES/NO tool selection

## Summary

| Mode | LFM2-8B-A1B | MiniCPM-V-4.6 |
|---|---|---|
| Default (all 13 tools) | ❌ Too slow / no tool calls | ✅ 6/6 PASS |
| YES/NO (restricted tools) | ✅ Works with python tool only (2/6) | ✅ Works |
| **PROMPT_TOOLS + STRICT_TOOLS** | ✅ **6/6 PASS** | ✅ Works |

## Key Finding

**LFM2-8B-A1B gets 6/6 with PROMPT_TOOLS + STRICT_TOOLS** — not just python tasks.

The grammar-constrained text mode forces valid `<tool_call>` output via GBNF, which the
model wouldn't emit otherwise. This is strictly better than YES/NO + CodeAct for general
use.

## YES/NO + CodeAct (python-only)

In YES/NO mode (`OPENHARN_YESNO=1`):
1. **Pass 1**: Model sees 12 tools as YES/NO → selects `["python"]`
2. **Pass 2**: Only python tool advertised → model writes Python code
3. **Execution**: Code runs, returns result

This works but only for tasks expressible as Python code (2/6 behavioral tests).

## PROMPT_TOOLS + STRICT_TOOLS (general)

```bash
OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 ./openharn /workspace
```

Tools described in prompt text + GBNF grammar forces valid `<tool_call>` or plain text.
Passes all 6 behavioral tests — read, search, edit, glob, all work.

## LFM2-8B Capability Profile

| Capability | Status |
|---|---|
| Native tool calling (OpenAI format) | ❌ Never emits `tool_calls` |
| Text-format tool calls (`<tool_call>[...]`) | ❌ Never emits without grammar |
| GBNF grammar-constrained calls | ✅ **Works — 6/6** |
| JSON action format (SLM harness) | ❌ Invalid JSON |
| Python code in markdown block | ✅ Works when python is only tool |
| Code-as-Action (CodeAct) | ✅ Works (YES/NO mode) |

## Practical Recommendation

For LFM2-8B on CPU, use **PROMPT_TOOLS + STRICT_TOOLS** (not YES/NO):

```bash
# Best for LFM2-8B — general coding agent (6/6)
OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 ./openharn /workspace

# YES/NO + CodeAct — only for explicit python tasks (2/6)
OPENHARN_YESNO=1 OPENHARN_NO_THINK=1 ./openharn /workspace

# Default mode — only works on MiniCPM-V-4.6, Qwen 2.5 3B+, etc.
OPENHARN_NO_THINK=1 ./openharn /workspace
```