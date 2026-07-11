# Notes: making uncooperative models call tools

Start with the mistake, because it's why these notes exist. In the benchmark,
`granite-3.1-1b-a400m` scored 0/4 on tool use and I wrote it off: too small to call tools.
That was wrong. The model was emitting a *valid* call the server silently dropped — a
harness bug I'd blamed on the model.

That misdiagnosis is the point. Tool-calling fails in a few structurally different places,
and from the outside several of them look identical ("the model didn't call the tool"). If
you don't separate them, you attribute a harness bug to the model — which is exactly what I
did. Once you do separate them, two of the three turn out to be the harness's problem to
fix.

## Three places it breaks

| Where | What happens | Example (observed) | Whose fault |
|---|---|---|---|
| Model | emits no structured call, just prose | LFM2-v2 writes ```` ```bash\nglob src/*.rs``` ```` | model |
| Runtime | model emits a *valid* call the server won't parse | Granite-3.1 emits `<tool_call>[…]`; llama.cpp watches for `<\|tool_call\|>` → `tool_calls: null` | harness |
| Server | no tool API at all | bitnet.cpp → `500: Unsupported param: tools` | harness |

The Granite case is the middle row. I read it as the top row. Both present as "no tool
call happened," so the distinction isn't self-evident — which is the whole reason it's
worth writing down. A framework that separates failure modes earns its keep precisely when
someone (me) conflates two of them in practice.

## Recovering a call the runtime dropped

Granite-3.1 emits a correct call; llama.cpp doesn't parse it, because the model writes
`<tool_call>` while the template's trigger is `<\|tool_call\|>`. The call lands in content
instead of `tool_calls`:

```
content:    <tool_call>[{"arguments": {"pattern": "src/**/*.rs"}, "name": "glob"}]
tool_calls: null
```

`parse_text_tool_calls` (in [`src/agent.rs`](../src/agent.rs)) catches this when the native
parse is empty: find a `<tool_call>`/`<|tool_call|>` marker + JSON (a list, or a single
`{function:{…}}` object), synthesize the `tool_calls`. It only runs when the server returned
nothing, so a normal answer isn't misread as a call. Yes, the fix is a regex — that it's
this simple is what makes misattributing it to the model embarrassing, not less real.

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
or malform JSON. Check it's applied — restrict to `glob` only, ask BitNet to *search* (a
`grep` job); it's forced to a valid `glob` call and can't emit the `grep` it would reach
for:

```
OPENHARN_TOOLS=glob OPENHARN_STRICT_TOOLS=1  →  · glob {"path":".","pattern":"*.rust"}
```

## What clearing a layer reveals

Here's the part that makes the layers more than filing. Once the harness stopped dropping
a400m's calls, its *actual* problem surfaced — and it wasn't the runtime. It picks the
wrong tool, fills arguments with the tool's schema instead of values, and loops. You
couldn't see that until layer 1 was fixed; the harness bug was hiding a real model failure.
Same with BitNet through prompt-tools:

```
· grep {"include":"Config","path":"/src/config","scope":"system"}   (×3, then stopped)
```

`include` isn't the search field (`pattern` is), the path is invented, `scope:system`
dodges the project check — valid keys, wrong choices. And even grammar-forced,
`*.rust` above should be `*.rs`: format fixed, judgment not.

So the fixes are real but bounded. On the tested weak models they move the outcome from
"call vanishes / 500s" to "call dispatches, then the model fails for a real reason." That's
a genuine improvement — it would let a model with good judgment and sloppy formatting
succeed — but on these models it isn't end-to-end. Peeling layers 1–3 exposes the model
layer underneath, which no harness crosses. Which families actually clear it on CPU:
[`small-model-tool-calling.md`](small-model-tool-calling.md).

All three mechanisms are unit-tested in [`src/agent.rs`](../src/agent.rs); reproduce with
`OPENHARN_PROMPT_TOOLS=1` / `OPENHARN_STRICT_TOOLS=1` / `OPENHARN_NARROW=1` (see
[`adapting-openharn.md`](adapting-openharn.md)).
