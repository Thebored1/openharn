# Making uncooperative models call tools

Tool-calling is the one thing a coding agent can't skip: if the model never produces a
call the harness can execute, nothing else matters. Testing a dozen small local models,
it breaks in three structurally different places — and two of them are the harness's
fault, not the model's. This documents what openharn does about each, and (bluntly) where
it can't help.

## Three places it breaks

| Where | What happens | Example (observed) | Harness can fix? |
|---|---|---|---|
| **Model** | emits no structured call, just prose | LFM2-v2 writes ```` ```bash\nglob src/*.rs``` ```` | no |
| **Runtime** | model emits a *valid* call the server won't parse | Granite-3.1 emits `<tool_call>[…]`; llama.cpp watches for `<\|tool_call\|>` → `tool_calls: null` | **yes** |
| **Server** | no tool API at all | bitnet.cpp → `500: Unsupported param: tools` | **yes** |

## Fix 1 — recover a call the runtime dropped

Granite-3.1 emits a correct call; llama.cpp just doesn't parse it, because the model
writes `<tool_call>` while the template's trigger is `<\|tool_call\|>`. The call lands in
the content instead of `tool_calls`:

```
content:    <tool_call>[{"arguments": {"pattern": "src/**/*.rs"}, "name": "glob"}]
tool_calls: null
```

`parse_text_tool_calls` (in [`src/agent.rs`](../src/agent.rs)) catches this when the
native parse is empty: it finds a `<tool_call>`/`<|tool_call|>` marker + JSON (list or a
single `{function:{…}}` object) and synthesizes the `tool_calls`. It only fires when the
server returned nothing, so a normal answer is never misread.

## Fix 2 — drive a server with no tool API

bitnet.cpp's server rejects the `tools` field outright. `OPENHARN_PROMPT_TOOLS=1` moves
tools out of the API: it describes them in the system prompt, omits `tools`, and — via
`flatten_for_prompt_tools` — rewrites the internal tool-call/tool-result history into
plain `system`/`user`/`assistant` messages any server accepts. The model's text call comes
back and Fix 1 recovers it. openharn's own loop never knows the difference.

## What this actually buys you (and what it doesn't)

Be precise, because the distinction is the whole point: these two fixes make a call
**reach the tool**. They do not make a bad model **choose well**.

Concretely, against a self-built bitnet.cpp server (BitNet-b1.58-2B-4T, i2_s, CPU):

- Prompt-tools works — BitNet dispatches a real tool call where before it got a `500`.
- But its *judgment* is unusable. Asked to find `Config`, it looped the same broken call
  until the circuit breaker stopped it:
  ```
  · grep {"include":"Config","path":"/src/config","scope":"system"}   (×3, then stopped)
  ```
  `include` isn't the search field (`pattern` is), the path is invented, and `scope:system`
  dodges the project check. All valid *keys*; all wrong *choices*.

So the honest summary: the tested weak models (Granite-3.1-1b-a400m, BitNet-2B) go from
"call vanishes / 500s" to "call dispatches, then the model fails the task for a real
reason." That's a genuine harness improvement — it would let a model with **good judgment
and sloppy formatting** succeed — but it is not, on these models, an end-to-end fix. It
isn't dressed up as one.

## Fix 3 — force the *format* with a grammar

For the "sloppy formatting" case there's a third lever: `OPENHARN_STRICT_TOOLS=1` attaches
a GBNF grammar (generated from the tool schemas by `tool_grammar`) that constrains the
reply to a schema-valid call or plain text — valid tool names, only known argument keys,
typed/enum values. A weak model then *cannot* invent a field or malform JSON.

Evidence it's actually applied: restrict to `glob` only and ask BitNet to *search* (a
`grep` job). It is forced to a valid `glob` call — it cannot emit the `grep` it would
otherwise reach for:

```
OPENHARN_TOOLS=glob OPENHARN_STRICT_TOOLS=1  →  · glob {"path":".","pattern":"*.rust"}
```

Note `*.rust` (should be `*.rs`) — the grammar fixed the *format*, not the *judgment*. That
is exactly the boundary: strictness kills the "can't format" failures; nothing here kills
"can't decide."

## The line

openharn's bet is that the harness matters more than the model — meet the model where it
is. These three fixes move the failure boundary from "the harness gave up" (the call
vanished, the server 500'd, the JSON was malformed) to "the model genuinely can't choose
the right tool." That second boundary is a real property of the model, and no harness
crosses it. Which model families do clear it on CPU:
[`small-model-tool-calling.md`](small-model-tool-calling.md).

---

Reproduce: `OPENHARN_PROMPT_TOOLS=1` / `OPENHARN_STRICT_TOOLS=1` / `OPENHARN_NARROW=1`
against your endpoint (see [`adapting-openharn.md`](adapting-openharn.md)); all three
mechanisms are unit-tested in [`src/agent.rs`](../src/agent.rs).
