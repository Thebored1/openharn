# Notes: Qwen 3.5 0.8B behavioral test results

## Test Results

| Mode | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding | Total |
|---|---|---|---|---|---|---|---|
| Default (native tools) | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | **2/6** |
| PROMPT_TOOLS + STRICT | ✅ | ✅ | ❌* | ❌ | ✅ | ❌ | **3/6** |

*Model correctly called `read` with valid args but answered in Chinese ("抱歉，我还没有找到这个文件") — test checks English phrases only.

## Setup

- Model: `Qwen3.5-0.8B-UD-Q8_K_XL.gguf` (~1.2 GB, Q8_0 quant)
- Server: `llama-server --chat-template chatml --ctx-size 8192`
- ~150 tok/s prompt, ~16 tok/s generation on CPU

## What works

- **greeting / no_repeat**: trivial tests, passes in all modes
- **edit_anchor**: with PROMPT_TOOLS+STRICT, the model correctly calls `read` then describes the edit in text (3/6 mode)

## What fails and why

### missing_file (honest=False, faked=False)

The model correctly calls `read` with the right path, gets the "not found" grounding
error, but then answers in Chinese: "抱歉，我还没有找到这个文件" (Sorry, I haven't found
this file yet). The behavioral test only checks English phrases ("not found", "doesn't
exist", etc.), so it reports honest=False.

This is a **test limitation**, not a harness or model failure. The model behaved correctly.

### system_search_uses_scope_flag

Two failure modes depending on the mode:

- **Default mode**: Model outputs `<think>...</think>` then just the filename
  as text — no tool call at all.
- **PROMPT_TOOLS+STRICT**: Model calls `bash` with `find / -name "zzz_nope_openharn.html"`
  instead of `glob_system`. It also calls `grep_system` (which is correct), but the test
  specifically checks for `glob_system`.

The model doesn't understand the distinction between `bash find` and `glob_system`. It
treats them as interchangeable, which is reasonable but doesn't pass the test.

### grounding_limits_total_calls

In default mode: model outputs `<think>` tags then gives up (0 calls).

In PROMPT_TOOLS+STRICT mode: model outputs a single `?` or `*` character — the grammar
constrains it but the model can't generate a valid `<tool_call>` starting with `<`.

Without grammar (PROMPT_TOOLS only): model spirals — repeats the same `awk` command
endlessly. The spiral guard fires but the model keeps going.

## Key findings

1. **Chinese output** — Qwen 3.5 0.8B responds in Chinese by default even with an English
   system prompt. This breaks any test that checks for English text patterns.

2. **Too small for complex instructions** — at 0.8B parameters, the model can't reliably
   follow multi-step instructions ("search the entire system for X and tell me Y"). It
   defaults to the simplest possible action.

3. **Grammar works but model can't comply** — with STRICT, the grammar forces valid
   `<tool_call>` output on the first turn. But on subsequent turns or complex prompts,
   the model outputs single characters (`?`, `*`) that don't match the grammar.

4. **Uses `bash find` instead of `glob_system`** — the model knows about `find` from
   pre-training but doesn't internalize that `glob_system` is the dedicated tool for
   system-wide search.

5. **Spirals without grammar** — in PROMPT_TOOLS mode (no grammar constraint), the model
   repeats the same command endlessly. The circuit breaker fires at 3 repeats but the
   model doesn't learn.

## Comparison with other models

| Model | Params | Default | PROMPT_TOOLS+STRICT | Notes |
|---|---|---|---|---|
| MiniCPM-V-4.6 | ~4B | 6/6 | 6/6 | Best overall |
| LFM2-8B-A1B | 8B (1B active) | 3/6 | **6/6** | Needs grammar constraint |
| Qwen 3.5 0.8B | 0.8B | 2/6 | 3/6 | Too small, Chinese output |
| LFM2.5-1.2B-Instruct | 1.2B | — | — | Tool-tuned, fast, worth testing |

## Conclusion

**Qwen 3.5 0.8B is too small for a general coding agent.** It passes trivial tests
(greeting, no_repeat) and can make valid tool calls when grammar-constrained, but fails
anything requiring judgment, multi-step reasoning, or English output. At 0.8B parameters,
it's below the practical floor for openharn's behavioral suite.

For CPU agents, use:
- **MiniCPM-V-4.6** (6/6 all modes) — best overall
- **LFM2-8B-A1B** (6/6 with PROMPT_TOOLS+STRICT) — needs grammar, but works
- **LFM2.5-1.2B-Instruct** — tool-tuned, fast, worth testing next
