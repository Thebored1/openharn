# How to implement openharn (design)

A from-scratch design for a tiny, local-first coding agent that makes **small**
language models (0.8B–8B) usable on **CPU**. This is the intent behind the
actual code in `src/`; read it before modifying the harness or adapting it to
another domain (see `docs/adapting-openharn.md`).

## Design thesis

**The harness matters more than the model.** A small model in a sloppy harness
looks broken; the same model in a good one punches above its weight. So we spend
our complexity budget on *structure*, not on the model:

- ground the model in real environment state,
- anchor edits so it changes a span, not a file,
- trim context to fit the window,
- recover from the model/server's weaknesses instead of trusting them.

Everything is deliberate and visible — no agent framework, ~1,500 lines of Rust.

## Core loop (the only mandatory piece)

```
history = [system]
loop:
    fit_context(history)              # trim to budget, keep system + whole turns
    wire = flatten_for_prompt_tools(history) if prompt_tools else history
    resp = POST /chat/completions { messages: wire, tools?, grammar? }
    (text, tool_calls) = stream_response(resp)
    if no tool_calls: print text; return
    for tc in tool_calls:
        result = session.execute(tc.name, tc.args)
        history.push({role: tool, content: cap_result(result)})
```

That's it. Everything else is a reliability layer around this loop.

## The four files

| File | Responsibility | Why separate |
|------|----------------|--------------|
| `main.rs` | REPL, env parsing, branch to server mode | entry point only |
| `agent.rs` | the loop: streaming, tool dispatch, context-fit, recovery, grammar, limits | all the *behavior* |
| `tools.rs` | the 10 tools + per-session `read`/`todo` state | model-facing surface |
| `edit.rs` | anchored replacer cascade | the one piece of real algorithm |

## Tool surface (what the model sees)

10 tools, chosen to cover real coding without inviting sprawl:

`read, write, edit, multiedit, glob, glob_system, grep, grep_system, bash,
webfetch, todowrite, todoread` (12 in code; README says 10 core + 2 system-search).

Each tool is **two declarations in one place** (`tools.rs`):
1. `schemas()` — the JSON the model sees (name, description, parameters). This
   single source drives the native `tools` field, the prompt-tools description,
   *and* the strict GBNF grammar automatically.
2. `Session::execute()` — the `match` that runs it (takes JSON args, returns a
   `String`).

Add a tool = add a schema entry + a match arm. No other wiring.

## Grounding (why the model stops hallucinating)

- **Read-before-edit**: `edit`/`write` require a prior `read` of that path. A
  missing read lists the files that *actually* exist (`ground_missing`), so the
  model corrects in one step instead of inventing filenames.
- **Scope honesty**: `glob`/`grep` stay in the project; `glob_system`/`grep_system`
  search the whole machine (roots resolved by the harness, never by the model).
  A search that finds nothing states its true scope and offers the system tool.
  The name↔content swap is the most common wrong-tool mistake: `glob`/`glob_system`
  are for finding a file **by name**, `grep`/`grep_system` for searching file
  **contents**. Wording in `prompt.txt` + the `glob`/`grep` schemas enforces this, and
  `tests/behavior.py` locks it with `find_file_uses_glob_not_grep` (name→glob) and
  `grep_for_content_not_glob` (content→grep).
- **Cap + trim**: a single tool result is capped (`TOOL_RESULT_CAP`); the whole
  conversation is trimmed to `HISTORY_BUDGET` (system message always kept, oldest
  whole turns dropped first so a tool result is never orphaned from its call).

## Reliability ladder (opt-in, additive)

Each layer is independent — turn on only what your model/server needs:

```
native tools        → capable model + modern server            (default)
+ text recovery     → model emits a call the server won't parse (automatic)
+ prompt-tools      → server has no tool API                    (OPENHARN_PROMPT_TOOLS)
+ strict grammar    → model malforms calls                      (OPENHARN_STRICT_TOOLS)
+ narrow tool set   → weak model, narrow reliable job           (OPENHARN_NARROW / OPENHARN_TOOLS)
```

- **Text recovery**: if the native parse yields no `tool_calls` but the text looks
  like `<tool_call>[…]` / `<|tool_call|>`, recover and dispatch it.
- **Prompt-tools**: omit the `tools` field, describe tools in the prompt, flatten
  history to plain roles.
- **Strict grammar**: a GBNF grammar forces every reply into a schema-valid call or
  plain text — a weak model cannot invent a field or malform JSON.
- **Narrow**: restrict to `read,grep,glob` + strict + prompt-tools for a maximal-
  reliability read-only agent.
- **Circuit breaker**: exact-repeat calls halt after 3 identical invocations;
  per-turn (`OPENHARN_MAX_CALLS`) and total (`OPENHARN_TOTAL_MAX`) limits inject a
  grounding message instead of letting the model spiral.

## The anchored edit engine (`edit.rs`)

The one real algorithm. `edit::replace(old, find, replace, replace_all)`:

- rejects when `find` is absent,
- rejects when `find` is ambiguous (appears >1× without `replace_all`),
- tolerates whitespace / indentation / escaped-newline drift between the model's
  recalled text and the file,
- replaces the *span*, never reprints the file.

This is what stops a small model from "reprinting the whole file and truncating."
It is a Rust port of opencode's replacer (MIT).

## Configuration philosophy

All behavior is env-var driven — **no rebuild to change mode**:

`OPENHARN_BASE_URL`, `OPENHARN_MODEL`, `OPENHARN_API_KEY`, `OPENHARN_TOOLS`,
`OPENHARN_NARROW`, `OPENHARN_STRICT_TOOLS`, `OPENHARN_PROMPT_TOOLS`,
`OPENHARN_MAX_CALLS`, `OPENHARN_TOTAL_MAX`, `OPENHARN_NO_THINK`,
`OPENHARN_SHOW_THINKING`, `OPENHARN_FRIENDLY_RESULTS`.

See `docs/adapting-openharn.md` for the full table and recipes.

## CPU-first constraints

Defaults assume a modest CPU-only machine (`llama-server -ngl 0`):

- keep the system prompt lean (fits in an 8k context with the conversation),
- prefer fewer/anchored tool calls over large rewrites,
- `OPENHARN_NO_THINK=1` for reasoning models (3–6× faster turns; see
  `notes/reasoning-tax.md`),
- if a change only helps on a big GPU, it's out of scope.

## Adaptation sketch (any domain)

Swap the **domain layer** only — state, tools, prompt — and keep the loop:

1. Define `YourSession` (persistent state the agent needs).
2. Implement `execute(name, args)` → mutate state, return `(state, message)`.
3. Write `your_schemas()` (OpenAI function-calling JSON; descriptions matter).
4. Write `YOUR_SYSTEM` (identity + tool contract + "no tool for chat" rule).
5. Wire it: REPL (`tools::Session` → `YourSession`) or HTTP server (copy
   the serve module).

## What the harness can't fix

It makes a call *reach* the tool reliably and stops spirals; it cannot supply
competence the model lacks. If a model puts the search term in the wrong (but
valid) field, points at a plausible-but-wrong path, or loops on failure, that's
model judgment — strictness and grounding raise the floor, but they can't make a
poorly-tool-trained model *choose* well. Which families clear that bar on CPU:
`notes/small-model-tool-calling.md`.
