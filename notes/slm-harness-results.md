# Notes: SLM harness results on LFM2-8B vs MiniCPM-V-4.6

## Summary

| Mode | Model | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding | Total |
|---|---|---|---|---|---|---|---|---|
| **Default** | MiniCPM-V-4.6 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |
| **Default** | Qwen 3.5 0.8B | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | 5/6 |
| **Default** | LFM2 8B-A1B | ✅ | ✅ | ❌ | ✅ | ❌ | ❌ | 3/6 |
| **SLM** | MiniCPM-V-4.6 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |
| **SLM** | LFM2 8B-A1B | ✅ | ✅ | ❌ | ✅ | ❌ | ❌ | **2/6** |
| **PROMPT_TOOLS + STRICT** | LFM2 8B-A1B | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |

## Key findings

1. **SLM harness works** — MiniCPM-V-4.6 gets 6/6 in both modes. The constrained action
   space + per-step verification compensates for weaker instruction following.

2. **Model capability is the discriminator** — LFM2-8B-A1B fails in default (3/6) and SLM
   (2/6) because it doesn't emit valid tool calls natively or as JSON.

3. **PROMPT_TOOLS + STRICT_TOOLS is the fix for LFM2-8B** — The grammar was previously
   broken (see `gbnf-grammar-fix.md`). With the fix, LFM2-8B gets 6/6 because the GBNF
   grammar forces valid `<tool_call>` output. The model CAN call tools when constrained;
   it just doesn't by default.

4. **MiniCPM-V-4.6 is the sweet spot** — ~190 tok/s prompt, ~26 tok/s gen on CPU, passes
   all tests in all modes.

## SLM vs Default vs PROMPT_TOOLS+STRICT for LFM2-8B

| Aspect | Default | SLM | PROMPT_TOOLS+STRICT |
|---|---|---|---|
| greeting | ✅ | ✅ | ✅ |
| no_repeat | ✅ | ✅ | ✅ |
| missing_file | ❌ | ❌ | ✅ |
| glob_system | ✅ (false positive) | ✅ (false positive) | ✅ (real call) |
| edit_anchor | ❌ | ❌ | ✅ |
| grounding | ❌ | ❌ | ✅ |

The "PASS" on glob_system in default/SLM was a false positive — the model output text
mentioning glob_system, didn't actually call it. PROMPT_TOOLS+STRICT makes a real call
because the grammar forces valid `<tool_call>` output.

## Conclusion

The SLM harness is a **reliability layer for models that CAN call tools but struggle with
context/schema**. It doesn't create tool-calling ability where none exists. For LFM2-8B,
the fix is PROMPT_TOOLS+STRICT (grammar-constrained text tool calls), not SLM. For
production CPU agents, use MiniCPM-V-4.6 with either mode.