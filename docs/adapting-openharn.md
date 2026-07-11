# Adapting openharn to your model, server, and use case

How to bend openharn without reading the Rust. If you can set an environment variable and
edit a text file, you can do everything here.

openharn defaults assume a capable-ish model on a modern `llama-server`. This guide covers
when your model or server isn't that — a weak model that fumbles tool calls, or an old
server with no tool-calling — and how to trade generality for reliability on purpose.

## Reliability vs. generality

Every knob below moves one thing: how much freedom the model has.

- **More freedom** (defaults): all ten tools, native tool-calling, the model decides. Good
  with a capable model, fragile with a weak one.
- **Less freedom** (strict / narrow): fewer tools, a grammar that forces valid calls, more
  grounding. A weak model can't malform a call or wander, at the cost of doing fewer kinds
  of task.

A model that can *only* search and read files, and *cannot* emit a broken call, is a useful
"explain this codebase" agent even if it could never be trusted to edit. On weak hardware,
narrowing is often the right call, not a compromise.

## Environment variables

No rebuild needed. Set these before launching openharn.

| Variable | What it does | Reach for it when… |
|---|---|---|
| `OPENHARN_BASE_URL` | endpoint (default `http://127.0.0.1:8080/v1`) | server is elsewhere / cloud |
| `OPENHARN_MODEL` | model name in the request (default `local`) | server needs a specific name |
| `OPENHARN_API_KEY` | bearer token | cloud provider |
| `OPENHARN_TOOLS` | comma list, e.g. `read,grep,glob` — restrict the set | smaller, safer tool surface |
| `OPENHARN_NARROW` | preset: `read,grep,glob` **+ strict + prompt-tools** | most reliable narrow agent for a weak model |
| `OPENHARN_STRICT_TOOLS` | grammar-force every reply to a schema-valid call or plain text | your model malforms calls |
| `OPENHARN_PROMPT_TOOLS` | describe tools in the prompt, omit the `tools` field | server has no tool API (bitnet.cpp, old forks) |
| `OPENHARN_NO_THINK` | suppress a reasoning model's thinking (faster on CPU) | LFM2.5 is too slow |
| `OPENHARN_SHOW_THINKING` | stream the raw chain-of-thought instead of the meter | debugging what the model thought |

`NARROW` implies `STRICT_TOOLS`; `STRICT_TOOLS` implies `PROMPT_TOOLS` (a grammar can't
combine with the native `tools` field). `NO_THINK` is ignored under strict (its prefill
breaks the grammar, and weak models don't reason anyway).

## Recipes

Server 500s on `tools` / no tool-calling:
```sh
OPENHARN_PROMPT_TOOLS=1 cargo run -- .
```

Model mangles arguments (wrong field names, bad JSON):
```sh
OPENHARN_STRICT_TOOLS=1 cargo run -- .
```

Most reliable agent a weak model can drive:
```sh
OPENHARN_NARROW=1 cargo run -- .          # read,grep,glob, grammar-locked, grounded
```

Custom tool set:
```sh
OPENHARN_TOOLS=read,grep,glob,edit OPENHARN_STRICT_TOOLS=1 cargo run -- .
```

Reasoning model too slow on CPU:
```sh
OPENHARN_NO_THINK=1 cargo run -- .
```

## Changing openharn itself

Each of these is a one-spot change.

**System prompt** — edit `src/prompt.txt` (plain text, compiled in). Rebuild with
`cargo build`.

**Add / remove / reword a tool** — two spots in `src/tools.rs`:
1. `schemas()` — the tool list the model sees (name, description, parameters). This also
   drives the prompt-tools description and the strict grammar automatically.
2. `Session::execute()` — the `match` that runs each tool by name. Add a
   `"mytool" => self.mytool(args),` arm and write the function (takes JSON `args`, returns a
   `String`).

Grounding, grammar, and prompt-tools rendering all derive from `schemas()`, so a new tool
is picked up everywhere.

**Stricter / looser** — in `src/agent.rs`: the grammar is `tool_grammar()` (constrains tool
name, argument keys, value types); the prompt-tools description is `tool_prompt()`; the
circuit breaker is the `repeats >= 3` check; context budget is `HISTORY_BUDGET`; result caps
are `TOOL_RESULT_CAP`.

**Grounding messages** — in `src/tools.rs`: `ground_missing()` (bad `read`) and
`ground_missing_path()` (bad `glob`/`grep` path) build the "that doesn't exist; here's what
does" replies.

## How the modes stack

```
native tools        →  capable model + modern server            (default)
+ text recovery     →  model emits a call the server won't parse (automatic)
+ prompt-tools      →  server has no tool API                    (OPENHARN_PROMPT_TOOLS)
+ strict grammar    →  model malforms calls                      (OPENHARN_STRICT_TOOLS)
+ narrow tool set   →  weak model, narrow reliable job           (OPENHARN_NARROW / OPENHARN_TOOLS)
```

Each layer is independent and opt-in — add only what your case needs.

## What adapting won't fix

The harness can make a call *reach* the tool reliably; it can't make a poorly-tool-trained
model *choose* well. If a model puts the search term in the wrong (but valid) field, points
at a plausible-but-wrong path, or loops on failure, that's model judgment — strictness and
grounding raise the floor and the circuit breaker stops the spiral, but they can't supply
competence the model lacks. Which families clear that bar on CPU:
[`small-model-tool-calling.md`](small-model-tool-calling.md).
