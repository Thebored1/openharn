# Notes: LFM2-8B with Code-as-Action (python tool) + YES/NO tool selection

## Summary

| Mode | LFM2-8B-A1B | MiniCPM-V-4.6 |
|---|---|---|
| Default (all 12 tools) | ❌ Too slow / no tool calls | ✅ 6/6 PASS |
| YES/NO (restricted tools) | ✅ **Works with python tool** | ✅ Works |

## Key Finding

**LFM2-8B-A1B CAN call tools when the action space is restricted to 1 tool.**

In YES/NO mode (`OPENHARN_YESNO=1`):
1. **Pass 1**: Model sees 12 tools as YES/NO → selects `["python"]`
2. **Pass 2**: Only python tool advertised → model writes Python code
3. **Execution**: Code runs, returns result

Output:
```
[yesno] selected: ["python"]
·· python {"code":"2 + 2"}
Result: 4
```

In default mode (all 12 tools):
- Prompt too large (~3.5k tokens just for schemas)
- Model takes 29s just for prompt processing
- Never emits tool calls

## Why This Works

| Factor | Default Mode | YES/NO Mode |
|---|---|---|
| Tools in prompt | 12 full schemas | 12 YES/NO questions (Pass 1) + 1 tool schema (Pass 2) |
| Prompt size | ~3.5k tokens | ~200 tokens (Pass 1) + ~300 tokens (Pass 2) |
| Decision complexity | Pick 1 of 12 + fill args | YES/NO per tool → fill args for 1 |
| Model capability needed | Tool-calling + arg filling | Basic reasoning + code writing |

## LFM2-8B Capability Profile

| Capability | Status |
|---|---|
| Native tool calling (OpenAI format) | ❌ Never emits `tool_calls` |
| Text-format tool calls (`<tool_call>[...]`) | ❌ Never emits |
| JSON action format (SLM harness) | ❌ Invalid JSON |
| Python code in markdown block | ✅ Works when python is only tool |
| Code-as-Action (CodeAct) | ✅ **Works** |

## Practical Recommendation

For LFM2-8B on CPU:
- **Use `OPENHARN_YESNO=1` + `OPENHARN_NO_THINK=1`**
- Model effectively becomes a "Python code writer" agent
- Fast (~15s/turn vs timeout in default mode)
- Only works for tasks expressible as Python code

## Commands

```bash
# YES/NO + Code-as-Action (works on LFM2-8B)
OPENHARN_YESNO=1 OPENHARN_NO_THINK=1 ./openharn /workspace

# Default mode (only works on MiniCPM-V-4.6, Qwen 2.5 3B+, etc.)
OPENHARN_NO_THINK=1 ./openharn /workspace
```