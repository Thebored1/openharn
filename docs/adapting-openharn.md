# Adapting openharn to your model, your server, and your use case

*A plain-English guide to bending openharn without reading the Rust. If you know how to
set an environment variable and edit a text file, you can do everything here.*

openharn has one job: let a **small** model drive real coding tools on a **CPU**. Out of
the box it assumes a capable-ish model on a modern `llama-server`. This guide is for when
your model or your server *isn't* that — a weak model that fumbles tool calls, or an old
server that doesn't support tool-calling at all — and for deliberately trading generality
for reliability.

---

## 1. The dial you're actually turning: reliability vs. generality

Every knob below moves openharn along one axis: **how much freedom you give the model.**

- **More freedom** (defaults): all ten tools, native tool-calling, the model decides
  everything. Great with a capable model; fragile with a weak one.
- **Less freedom** (strict / narrow modes): fewer tools, a grammar that *forces* valid
  calls, heavy grounding. A weak model can't malform a call or wander off — at the cost
  of doing fewer kinds of task.

The insight worth internalizing: **you can make openharn strict enough that even a bad
model becomes usable — for a narrow job.** A dumb model that can *only* search and read
files, and *cannot* emit a broken call, is a useful "explain this codebase" agent even if
it could never be trusted to edit. Narrowing isn't a compromise; for weak hardware it's
often the right design.

---

## 2. Everything you can set with an environment variable

No rebuild needed. Set these before launching openharn.

| Variable | What it does | Reach for it when… |
|---|---|---|
| `OPENHARN_BASE_URL` | the OpenAI-compatible endpoint (default `http://127.0.0.1:8080/v1`) | your server is elsewhere / a cloud provider |
| `OPENHARN_MODEL` | model name sent in the request (default `local`) | your server needs a specific name |
| `OPENHARN_API_KEY` | bearer token | using a cloud provider |
| `OPENHARN_TOOLS` | comma list, e.g. `read,grep,glob` — restrict to a subset | you want a smaller, safer tool surface |
| `OPENHARN_NARROW` | preset: read-only navigation (`read,grep,glob`) **+ strict + prompt-tools** | you want the most reliable, narrow agent for a weak model |
| `OPENHARN_STRICT_TOOLS` | grammar-force every reply to be a *schema-valid* tool call or plain text | your model malforms calls (wrong field names, broken JSON) |
| `OPENHARN_PROMPT_TOOLS` | describe tools in the prompt, omit the `tools` field | your server has **no** tool API (old llama.cpp fork, bitnet.cpp) |
| `OPENHARN_NO_THINK` | suppress a reasoning model's thinking (much faster on CPU) | a hybrid-thinking model (LFM2.5) is too slow |
| `OPENHARN_SHOW_THINKING` | stream the raw chain-of-thought instead of the collapsed meter | you're debugging what the model thought |

Notes: `NARROW` implies `STRICT_TOOLS`; `STRICT_TOOLS` implies `PROMPT_TOOLS` (a grammar
can't be combined with the native `tools` field). `NO_THINK` is ignored under strict
(the reasoning prefill would break the grammar, and weak models don't reason anyway).

---

## 3. Recipes

**"My server 500s on `tools` / doesn't support tool-calling."**
```sh
OPENHARN_PROMPT_TOOLS=1 cargo run -- .
```
openharn describes the tools in the system prompt and recovers the model's text tool-call.

**"My model calls tools but mangles the arguments (wrong field names, bad JSON)."**
```sh
OPENHARN_STRICT_TOOLS=1 cargo run -- .
```
A grammar forces every call to match the tool schema: only real field names, correct
types, valid JSON. (It can't fix *bad judgment* — a wrong value in a valid field — but it
eliminates malformed calls entirely.)

**"I want the most reliable agent a weak model can drive."**
```sh
OPENHARN_NARROW=1 cargo run -- .
```
Read-only navigation (`read`, `grep`, `glob`), grammar-locked, grounded. It can explore
and explain a codebase but can't mutate anything or wander.

**"Restrict it to a custom set of tools."**
```sh
OPENHARN_TOOLS=read,grep,glob,edit OPENHARN_STRICT_TOOLS=1 cargo run -- .
```

**"My reasoning model is too slow on CPU."**
```sh
OPENHARN_NO_THINK=1 cargo run -- .
```

---

## 4. Changing openharn itself (small, surgical edits)

You don't need to understand the whole codebase — each of these is a one-spot change.

### Change the system prompt (the model's personality / rules)
Edit **`src/prompt.txt`**. It's plain text, compiled in at build time. Rebuild with
`cargo build`.

### Add, remove, or reword a tool
Two spots in **`src/tools.rs`**:
1. **`schemas()`** — the list of tools advertised to the model (name, description,
   parameters). Add or edit an entry here to change what the model *sees*. This also
   drives the prompt-tools description and the strict grammar automatically.
2. **`Session::execute()`** — the `match` that runs each tool by name. Add a
   `"mytool" => self.mytool(args),` arm and write the function. Tools take a JSON `args`
   and return a `String` result.

That's it — the grounding, the grammar, and the prompt-tools rendering all derive from
`schemas()`, so a new tool is picked up everywhere.

### Make it stricter or looser
In **`src/agent.rs`**:
- The **grammar** is generated by `tool_grammar()` — it constrains tool name, argument
  keys, and value types from `schemas()`. To hard-require certain arguments or forbid a
  field, tighten the per-tool rule there.
- The **prompt-tools description** is `tool_prompt()` — reword the instructions the model
  sees in prompt-tools mode.
- The **circuit breaker** (how many identical calls before it stops) is the `repeats >= 3`
  check; the **context budget** is `HISTORY_BUDGET`; per-result caps are `TOOL_RESULT_CAP`.

### Change the grounding messages
In **`src/tools.rs`**: `ground_missing()` (a bad `read`) and `ground_missing_path()` (a
bad `glob`/`grep` path) build the "that doesn't exist; here's what does" replies. Edit the
wording or extend the same pattern to other tools.

---

## 5. How the modes fit together (one mental model)

Everything is layered so you only add what you need:

```
native tools        →  capable model + modern server            (default)
+ text recovery     →  model emits a call the server won't parse (automatic)
+ prompt-tools      →  server has no tool API                    (OPENHARN_PROMPT_TOOLS)
+ strict grammar    →  model malforms calls                      (OPENHARN_STRICT_TOOLS)
+ narrow tool set   →  weak model, narrow reliable job           (OPENHARN_NARROW / OPENHARN_TOOLS)
```

Each layer is independent and opt-in. The defaults are for the best case; the layers are
how you meet a worse case where it is. That progression *is* openharn — the model gets as
far as it can, and the harness is what decides where "as far as it can" lands.

---

## 6. What no amount of adapting will fix

Honesty, because it saves you time: the harness can make a model's calls *reach* the
tools reliably; it cannot make a poorly-tool-trained model *choose* well. If a model puts
the search term in the wrong (but valid) field, points at a plausible-but-wrong path, or
loops on failure, that's model judgment — strictness and grounding raise the floor and the
circuit breaker stops the spiral, but they can't supply competence the checkpoint lacks.
See [`small-model-tool-calling.md`](small-model-tool-calling.md) for which model families
actually clear that bar on CPU.
