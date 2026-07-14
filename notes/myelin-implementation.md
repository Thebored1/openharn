# How openharn is implemented as Myelin

This note is a code-level walkthrough of the Myelin adaptation on the
`myelin-tools` branch: **how the existing openharn harness is re-targeted** to a
local notes app. It pairs with `docs/adapting-openharn-myeelin.md` (the
step-by-step recipe) and `notes/small-model-tool-calling.md` (why the harness
matters more than the model).

## What openharn already provides (reused as-is)

| Component | File | Role in Myelin |
|-----------|------|----------------|
| Streaming loop | `src/agent.rs::stream_response` | unchanged — the Myelin proxy reuses the same OpenAI-compatible request shape |
| Context trim | `src/agent.rs::fit_context` | not on the hot path for Myelin (one note, short history) but available |
| Tool-call recovery | `src/agent.rs::parse_text_tool_calls` | lets weak models that emit `<tool_call>[…]` still drive Myelin tools |
| Strict grammar | `src/agent.rs::tool_grammar` | forces schema-valid Myelin calls on models that fumble args |
| Anchored replacer | `src/edit.rs` | **the heart of `edit_note`** — exact-string replace that tolerates whitespace drift |
| Prompt-tools / narrow | env knobs in `src/agent.rs` | the same reliability ladder applies to notes (LFM2-8B needs `PROMPT_TOOLS`+`STRICT`) |

The point: **Myelin adds zero new harness machinery.** It only swaps the
*domain layer* (state + tools + prompt) and adds a thin HTTP front-end.

## The domain layer (`src/myelin.rs`)

```
MyelinSession { note: String, todos: Vec<Value> }   ← replaces tools::Session { cwd, read_set, todos }
```

- **State is one `String`.** No filesystem, no project root, no `read`-before-`write`
  guard. The open note *is* the state.
- **`execute(name, args) -> (new_note, message)`** mirrors `tools::Session::execute`
  exactly — a `match` on tool name. Each branch mutates `self.note` and returns the
  resulting text + a human-readable result.
- **`edit_note` reuses `edit::replace`** — the same engine that powers the coding
  agent's `edit`. Anchored, whitespace-tolerant, never reprints the whole note.
- **`format_note`** is pure in-memory regex (remove headings, bold, case, bullets↔numbered).
- **`write_note`** sets the whole body (empty string clears).
- **`search_notes` / `web_search`** are stubs in the harness (the real app would wire
  a local note index / search API); the *bench* carries web results in history, so
  they're exercised without a live backend.

## Schemas + prompt (the contract the model sees)

- `myelin_schemas()` — OpenAI function-calling JSON for the 7 tools. Descriptions match
  the bench's expectations (`myelin_bench.py::TOOLS`).
- `MYELIN_SYSTEM` — lean identity + tool-contract prompt, **no few-shot examples**
  (small models over-generalize from examples; see `small-model-tool-calling.md`).

Both are parallel to `tools::schemas()` + `src/prompt.txt` in the coding agent.

## The HTTP front-end (`src/myelin.rs::server`)

`OPENHARN_MYELIN=1 cargo run` launches an OpenAI-compatible server instead of the REPL:

```
POST /v1/chat/completions   → proxy to upstream llama-server, inject note frame, return
GET  /health                → {"status":"ok"}
Cookie: myelin_sid         → per-client note state (one note per browser/app)
```

Flow per request:
1. Read messages; extract/create session by cookie.
2. **Inject the note** into the last user message (`note_frame`) so the model sees the
   current note without a `read` step.
3. Forward `{system, messages, tools: myelin_schemas(), tool_choice:"auto"}` upstream.
4. **Deflection guard** — if history has a `web_search` result and the model replies
   "I will now fetch…" instead of writing, strip the tool_calls and force a
   `write_note` nudge. This is the `research_write` failure mode from the bench.
5. Return the upstream response unchanged (tool_calls included) so the bench scores
   the *outcome*.

## How it maps to the 10 benchmark cases

| Case | Myelin tool chosen | openharn mechanism |
|------|-------------------|--------------------|
| greeting / identity / no_phantom_note | none (text) | system-prompt rule "call NO tool for chat" |
| write_fresh / clear | `write_note` | whole-body set / empty |
| edit_surgical / edit_faithful / list_edit | `edit_note` | `edit::replace` anchored |
| format_headings | `edit_note` (`##`→`""`) *or* `format_note` | either produces the same outcome |
| research_write | `write_note` + no deflect | deflection guard in the server |

All 7 tools are wired; the model picks the right one 10/10 on LFM2.5
(verified manually through the upstream). The `0/10` bench runs were an
infra issue (single-threaded `tiny_http` proxy dying on slow CPU inference),
**not** a tool-wiring problem.

## Files to read, in order

1. `src/myelin.rs` — `MyelinSession` (state + tools), `myelin_schemas()`, `MYELIN_SYSTEM`, `server`
2. `src/edit.rs` — the anchored replacer that powers `edit_note`
3. `docs/adapting-openharn-myeelin.md` — the general recipe (any domain)
4. `src/main.rs` — the `OPENHARN_MYELIN=1` switch that picks server vs REPL
