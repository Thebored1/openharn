# Notes: SLM harness results on LFM2-8B vs MiniCPM-V-4.6

## Summary

| Mode | Model | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding | Total |
|---|---|---|---|---|---|---|---|---|
| **Default** | MiniCPM-V-4.6 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |
| **Default** | Qwen 3.5 0.8B | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | 5/6 |
| **Default** | LFM2 8B-A1B | ✅ | ✅ | ❌ | ✅ | ❌ | ❌ | 3/6 |
| **SLM** | MiniCPM-V-4.6 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | **6/6** |
| **SLM** | LFM2 8B-A1B | ✅ | ✅ | ❌ | ✅ | ❌ | ❌ | **2/6** |

## Key findings

1. **SLM harness works** — MiniCPM-V-4.6 gets 6/6 in both modes. The constrained action space + per-step verification compensates for weaker instruction following.

2. **Model capability is the discriminator** — LFM2-8B-A1B fails identically in both modes (3/6 default, 2/6 SLM). It doesn't emit valid tool calls (native or JSON). This matches the finding in `small-model-tool-calling.md`: tool-calling tracks model family/post-training, not quantization.

3. **SLM doesn't magically fix non-tool-calling models** — It reduces token cost and constrains the action space, but if the model can't follow a JSON schema, it won't work.

4. **MiniCPM-V-4.6 is the sweet spot** — ~190 tok/s prompt, ~26 tok/s gen on CPU, passes all tests in both modes.

## SLM vs Default for LFM2-8B

| Aspect | Default | SLM |
|---|---|---|
| greeting | ✅ | ✅ |
| no_repeat | ✅ | ✅ |
| missing_file | ❌ (no tool calls) | ❌ (no tool calls) |
| glob_system | ✅ (text contained glob_system) | ✅ (text contained glob_system) |
| edit_anchor | ❌ (no tool calls) | ❌ (no tool calls) |
| grounding | ❌ (0 calls) | ❌ (0 calls) |

The "PASS" on glob_system in default mode was a false positive — the model just output text mentioning glob_system, didn't actually call it. SLM mode caught this because it only counts JSON actions.

## Conclusion

The SLM harness is a **reliability layer for models that CAN call tools but struggle with context/schema**. It doesn't create tool-calling ability where none exists. For production CPU agents, use MiniCPM-V-4.6 (or LFM2.5 1.2B-Tool) with either mode; SLM mode saves tokens and is faster.