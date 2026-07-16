# Notes: openharn configs that actually work per model

Tested `tests/behavior.py` (6 behavioral cases) against two local GGUFs on CPU
(`-ngl 0`, 12-core box). The harness has many modes; the *right* one is
model-dependent and the system prompt's `<example>` blocks were silently
breaking a weak model. Findings below.

## TL;DR — best config per model

| Model | Best config | Score |
|---|---|---|
| Qwen3.5-0.8B-UD-Q8_K_XL | native tools, **thinking ON** (default template) | **6/6** |
| LFM2-8B-A1B-UD-Q3_K_XL | **OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1** | **6/6** |

Both run on a plain llama-server: `-m <gguf> --jinja --ctx-size 16384 -ngl 0 --host 127.0.0.1 --port 8080 --no-warmup`.

## Qwen3.5-0.8B

- **Can** emit native OpenAI-format `tool_calls`. Do NOT use prompt-tools/strict
  for it — the 0.8B is too small to satisfy the GBNF grammar and loops or
  gives up (strict scored 4/6, prompt-tools 3/6 vs 6/6 native).
- **Thinking ON matters**: with thinking it picks the right tool
  (`glob` for "find a file"); with thinking off it falls back to `bash find`
  and fails `find_file_uses_glob_not_grep` (5/6). Cost ~1.6× wall time
  (4:15 vs 2:35 for the 6-case suite) — worth it at this size.
- Recommended:
  ```sh
  OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_MODEL=qwen3.5-0.8b ./openharn .
  ```

## LFM2-8B-A1B (Q3)

- **Cannot** emit native `tool_calls` and will not emit `<tool_call>` text
  without a grammar. Default / prompt-tools-only both score 3/6.
- **PROMPT_TOOLS + STRICT (GBNF grammar)** forces valid output → **6/6**,
  all real tool calls (read/search/edit/glob) correct. YESNO+STRICT also 6/6.
- Recommended:
  ```sh
  OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_MODEL=lfm2-8b-a1b-q3 ./openharn .
  ```

## Critical finding: the `<example>` blocks broke LFM2

LFM2 was stuck at 5/6 on `missing_file_is_reported_not_faked` — it
**hallucinated** the missing file's contents ("contains entries like apple: red,
banana: yellow…") instead of reporting "not found", even though the grounding
error listed the real files.

Root cause: the `<example>` blocks in `src/prompt.txt` (especially the ones
showing the assistant confidently stating file contents / producing information)
primed the weak model to fabricate rather than admit failure.

**Removing all 5 `<example>` blocks fixed it** (LFM2 5/6 → 6/6) and did
**not** regress Qwen (stays 6/6). This was the real blocker behind the
notes' "6/6" claim not reproducing on this quant — not the quant itself.

## System-prompt change retained

Added a persistence instruction to `src/prompt.txt` (Tool usage policy):
> Do not give up after a single failed search. If a file lookup returns nothing,
> keep trying before telling the user it doesn't exist: retry with broader scope
> (`scope="system"`), looser patterns (e.g. `*.md`, `*poem*`), and check
> cwd-relative vs absolute paths. Only report "not found" after you've
> exhausted reasonable variations.

This makes the model self-escalate `glob` → `glob_system` → `read` on one
prompt (verified on Qwen). It does not affect LFM2's score either way.

## How to reproduce

```sh
# server (model-specific, thinking on is default for both tested here)
LLAMA=~/.local/share/com.paper.myelin/bin/cpu/llama-server
"$LLAMA" -m <gguf> --jinja --ctx-size 16384 -ngl 0 --host 127.0.0.1 --port 8080 --no-warmup

# openharn in the model's best mode (see table), then in another shell:
cargo build && python3 tests/behavior.py 8080    # expects server on :8080
```
