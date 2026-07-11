# Notes: making uncooperative models call tools

If a small model never produces a tool call the harness can execute, nothing else in the
agent matters. Testing a dozen local models on CPU, tool-calling broke in three different
places — two of which the harness can work around, one it can't. These are notes on what
openharn does about each, and where it stops helping.

## Three places it breaks

| Where | What happens | Example (observed) | Workaround? |
|---|---|---|---|
| Model | emits no structured call, just prose | LFM2-v2 writes ```` ```bash\nglob src/*.rs``` ```` | no |
| Runtime | model emits a *valid* call the server won't parse | Granite-3.1 emits `<tool_call>[…]`; llama.cpp watches for `<\|tool_call\|>` → `tool_calls: null` | yes |
| Server | no tool API at all | bitnet.cpp → `500: Unsupported param: tools` | yes |

## Recovering a call the runtime dropped

Granite-3.1 emits a correct call; llama.cpp doesn't parse it, because the model writes
`<tool_call>` while the template's trigger is `<\|tool_call\|>`. The call lands in content
instead of `tool_calls`:

```
content:    <tool_call>[{"arguments": {"pattern": "src/**/*.rs"}, "name": "glob"}]
tool_calls: null
```

`parse_text_tool_calls` (in [`src/agent.rs`](../src/agent.rs)) catches this when the
native parse is empty: find a `<tool_call>`/`<|tool_call|>` marker + JSON (a list, or a
single `{function:{…}}` object), synthesize the `tool_calls`. It only runs when the server
returned nothing, so a normal answer isn't misread as a call.

## Driving a server with no tool API

bitnet.cpp's server rejects the `tools` field. `OPENHARN_PROMPT_TOOLS=1` moves tools into
the prompt: describe them in the system message, omit `tools`, and (via
`flatten_for_prompt_tools`) rewrite the internal tool-call/tool-result history into plain
`system`/`user`/`assistant` messages any server accepts. The model's text call comes back
and the recovery above picks it up. openharn's loop is unchanged.

## Forcing the format with a grammar

`OPENHARN_STRICT_TOOLS=1` attaches a GBNF grammar (generated from the tool schemas by
`tool_grammar`) that constrains the reply to a schema-valid call or plain text: valid tool
names, only known argument keys, typed/enum values. A weak model then can't invent a field
or malform JSON.

Check that it's actually applied — restrict to `glob` only, ask BitNet to *search* (a
`grep` job). It's forced to a valid `glob` call; it can't emit the `grep` it would reach
for:

```
OPENHARN_TOOLS=glob OPENHARN_STRICT_TOOLS=1  →  · glob {"path":".","pattern":"*.rust"}
```

## What this buys, and what it doesn't

These three workarounds make a call *reach* the tool. They don't make a bad model *choose
well*. Against a self-built bitnet.cpp server (BitNet-b1.58-2B-4T, i2_s, CPU):

- Prompt-tools works — BitNet dispatches a call where it previously got a `500`.
- Its judgment doesn't. Asked to find `Config`, it looped the same broken call until the
  circuit breaker stopped it:
  ```
  · grep {"include":"Config","path":"/src/config","scope":"system"}   (×3, then stopped)
  ```
  `include` isn't the search field (`pattern` is), the path is invented, `scope:system`
  dodges the project check. Valid keys, wrong choices.
- Even with the grammar, note `*.rust` above (should be `*.rs`) — format fixed, judgment
  not.

So on the tested weak models (Granite-3.1-1b-a400m, BitNet-2B) the result is: calls go
from "vanish / 500" to "dispatch, then the model fails the task for a real reason." That's
a real harness improvement — it would let a model with good judgment and sloppy formatting
succeed — but on these models it isn't an end-to-end fix. Strictness kills the
"can't-format" failures; nothing here kills "can't-decide," which is a model property.

Which model families clear that bar on CPU:
[`small-model-tool-calling.md`](small-model-tool-calling.md). All three mechanisms are
unit-tested in [`src/agent.rs`](../src/agent.rs); reproduce with `OPENHARN_PROMPT_TOOLS=1`
/ `OPENHARN_STRICT_TOOLS=1` / `OPENHARN_NARROW=1` (see
[`adapting-openharn.md`](adapting-openharn.md)).
