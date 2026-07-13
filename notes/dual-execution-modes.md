# Notes: execution modes in openharn

openharn runs two completely different agent loops behind one binary, plus a third
"grammar-constrained" variant of the default loop. The distinction is important because
the failure modes, model requirements, and debugging surfaces don't overlap.

## 1. Default ReAct loop (`OPENHARN_SLM=0` or unset)

This is the original loop: it keeps the **full conversation history** and sends it to the
model every turn. The model sees system prompt + user + all prior assistant + tool turns.
It calls tools via the server's native `tools`/`tool_choice` API (or GBNF-constrained
text calls when `OPENHARN_STRICT_TOOLS=1`).

| Property | Value |
|---|---|
| Context per turn | ~16 KB (trimmed with `fit_context`, keeps system + recent turns) |
| Tool schema | 13 tools (read, write, edit, glob, grep, glob_system, grep_system, bash, multiedit, webfetch, todowrite, todoread, python) — all advertised at once |
| Circuit breaker | `MAX_CALLS` per turn (default 1), `TOTAL_MAX` across run (default 5), repeat hard-stop at 3, grounding messages when truncated |
| Retry | Whole-request retry on connection blip; no per-step retry |
| Model requirement | Must emit native tool calls **or** the `<tool_call>[{"name":...}]` text format (recovered by `parse_text_tool_calls`) |
| Debugging | Full transcript in `history`; `OPENHARN_SHOW_THINKING=1` prints reasoning meter |

**When it works:** model family has tool post-training (LFM2.5, Granite 4, Qwen 2.5 3B+,
Llama 3.1 8B+). With `OPENHARN_NO_THINK=1` on CPU it's ~15–20 tok/s and passes the
behavioral suite (MiniCPM-V-4.6: 6/6).

**When it fails:** model doesn't emit calls at all (LFM2-v2 8B-A1B → 0/4) or emits
malformed calls the parser can't recover. The loop then falls back to text answers and
the task stalls.

---

## 2. SLM harness (`OPENHARN_SLM=1`)

Direct port of the five harness requirements from the slm-agents paper (arXiv:2604.25850,
Lin et al.). The model **never sees conversation history**. It receives one compact JSON
observation per turn and must emit exactly one JSON action.

### Observation schema

```json
{
  "goal": "user's original request",
  "step": 2,
  "steps_left": 8,
  "valid_actions": ["SEARCH", "READ"],
  "searches": [{"pattern": "test.txt", "glob": "**/*", "hits": 1}],
  "files": ["test.txt"],
  "reads": [{"f": "test.txt", "o": 0, "n": 1}],
  "last_result": {"type": "read", "f": "test.txt", "o": 0, "n": 1, "content_preview": "hello world"},
  "feedback": "optional: only present when previous step failed validation"
}
```

### Action space (constrained by `valid_actions`)

| Action | When valid |
|---|---|
| `SEARCH` | always |
| `READ` | at least one file discovered |
| `ANSWER` | at least one `READ` executed |
| `ESCALATE` | always |

### Per-turn loop (with localized retry)

```
for step in 0..max_steps:
  for retry in 0..max_retries_per_step:
    obs = state.build_observation()
    action = model({"system": PROMPT, "user": obs})
    pre = validate_action(action, state)      # pre-execution
    if not pre.ok: record_failure; continue
    if action.is_terminal(): handle; return
    output, is_error = execute(action)         # grep_tool / read_tool
    post = verify_step_result(action, output)
    if not post.ok: record_failure; continue
    fold_result(action, output, state)         # updates searches/reads/files
    state.clear_feedback()
    break
  if all retries exhausted: stop with "step failed"
  step += 1
```

### Key differences from default loop

| Aspect | Default | SLM |
|---|---|---|
| Context size | ~16 KB history | ~2 KB JSON obs |
| Tools visible | All 13 at once | 1–4 per `valid_actions` |
| State location | Implicit in `history` + `Session.read` | Explicit `SlmState` struct |
| Verification | Post-hoc grounding messages | **Pre + post** per step |
| Retry scope | Whole request | **Single failed step** with `feedback` in next obs |
| Model requirement | Native tool calling | **Valid JSON only** — works on models that can't do native calls |
| Token cost/turn | Higher (full history + schemas) | Lower (tiny obs, no schema) |
| Speed on CPU | ~15–20 tok/s (MiniCPM) | ~20–25 tok/s (less prompt) |

### Why it exists

The benchmark in [`small-model-tool-calling.md`](small-model-tool-calling.md) showed
LFM2-v2 8B-A1B scoring 0/4 on the default loop — it never emitted a native call. But
the same model **can** emit valid JSON actions when the action space is tiny and the
observation is structured. The SLM harness trades conversation flexibility for
reliability on weak models.

---

## 3. Grammar-constrained text mode (`OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1`)

A variant of the default loop where tools are described in the system prompt text (not
the native `tools` API) and a GBNF grammar forces every reply to be either a valid
`<tool_call>[{"name":...,"arguments":{...}}]` or plain text. This is the winning
configuration for models that ignore native tool APIs.

| Property | Value |
|---|---|
| Tool format | Text descriptions in system prompt + GBNF grammar constraint |
| Grammar | `root ::= call \| text` — model can answer in text OR call a tool |
| Rule names | Dashed-lowercase (e.g. `t-glob-system`, `kv-read`) — see `gbnf-grammar-fix.md` |
| Model requirement | Must follow text prompt instructions; grammar prevents malformed calls |

**When it works:** model ignores native `tools` API but can follow text instructions
(LFM2-8B-A1B → **6/6** with `NO_THINK`).

**When it fails:** model can't follow text instructions at all, or grammar is too complex
for the context window.

### Quick start

```bash
# Best for LFM2-8B on CPU (6/6 behavioral tests)
OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 \
  ./target/debug/openharn /workspace
```

### Why it works for LFM2-8B

LFM2-8B ignores the native `tools` API — it outputs descriptive text about what it
would do instead of emitting `<tool_call>`. But when the grammar forces valid `<tool_call>`
output, the model complies. The grammar acts as a structural guide: the model sees the
tool descriptions in the prompt, tries to call a tool, and the grammar ensures the call
is syntactically valid.

---

## Quick start

```bash
# Default (ReAct) — best for strong models
./target/debug/openharn /workspace

# Grammar-constrained text — best for models that ignore native tools
OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 \
  ./target/debug/openharn /workspace

# YES/NO + STRICT — best for small models that hallucinate (1.2B–3B)
OPENHARN_YESNO=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 \
  ./target/debug/openharn /workspace

# SLM mode — best for <3B models or models without native tool calling
OPENHARN_SLM=1 \
OPENHARN_SLM_MAX_STEPS=10 \
OPENHARN_SLM_MAX_RETRIES=2 \
OPENHARN_SLM_OBS_BUDGET=2000 \
OPENHARN_NO_THINK=1 \
./target/debug/openharn /workspace
```

---

## Behavioral test results

| Mode | Model | greeting | no_repeat | missing_file | glob_system | edit_anchor | grounding |
|---|---|---|---|---|---|---|---|
| Default | MiniCPM-V-4.6 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Default | Qwen 3.5 0.8B | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ |
| Default | LFM2 8B-A1B | ✅ | ✅ | ❌ | ✅ | ❌ | ❌ |
| Default | LFM2-1.2B-Tool | ✅ | ✅ | ✅ | ❌ | ✅ | ❌ |
| **PROMPT_TOOLS+STRICT** | **LFM2 8B-A1B** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **PROMPT_TOOLS+STRICT** | **LFM2-1.2B-Tool** | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| **YESNO+STRICT** | **LFM2-1.2B-Tool** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **SLM** | MiniCPM-V-4.6 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **SLM** | LFM2-1.2B-Tool | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ |

The grammar-constrained text mode is the only configuration where LFM2-8B passes all
six tests. The GBNF grammar was previously broken (see `gbnf-grammar-fix.md`); with the
fix, the grammar forces valid tool calls that the model wouldn't emit otherwise.

For LFM2-1.2B-Tool, adding YES/NO two-pass selection narrows the tool list per turn,
reducing hallucination on complex queries (6/6 vs 5/6 with PROMPT_TOOLS+STRICT alone).

---

## Files

- `src/agent.rs` — default loop (`run`) + SLM entry (`run_slm_mode`) + grammar (`tool_grammar`)
- `src/slm_harness/state.rs` — `SlmState` + observation builder
- `src/slm_harness/actions.rs` — `SlmAction` enum + JSON parser
- `src/slm_harness/verifier.rs` — `validate_action` + `verify_step_result`
- `src/slm_harness/executor.rs` — `execute_action` + `fold_result`
- `src/slm_harness/mod.rs` — main SLM loop (`run_slm`)