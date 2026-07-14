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

---

## Related work

This three-layer taxonomy maps to the **NLT** paper (Johnson et al., arXiv:2510.14453) which identifies *task interference* and *format constraints* as dual bottlenecks: Layer 1 = model doesn't emit the format (NLT's solution: natural language YES/NO); Layer 2 = runtime drops valid format (our fix: `parse_text_tool_calls`); Layer 3 = server lacks tool API (our fix: `PROMPT_TOOLS=1`). The **slm-agents** paper (Ranjan & Talluri, 2026; GitHub: IshaanAyaan/slm-agents) makes the same distinction in its custom harness: the harness owns **form** (schema validation, state, retry) while the model owns **judgment** (which tool, what args). Our Layer 2/3 fixes are harness-side form guarantees; Layer 1 is where judgment lives.

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

But be honest about what kind of thing this table is. The bottom two rows are clean and
mechanical — facts about *software*, decidable without the model in the room. Granite's
mismatch is a literal token string (`<tool_call>` vs `<\|tool_call\|>`); the server either
accepts a `tools` field or 500s. You can point at the exact byte. The top row is not like
that. "The model emits prose, not a call" is a joint product of the model's training
distribution *and* what the harness presented to it — not a stratum sitting underneath the
other two. The proof is later in this doc: put a grammar on it and the same LFM2-v2 that
"won't emit the format" emits it. That's a harness lever reaching *into* model behavior, so
the harness/model boundary isn't a clean seam there.

So treat this as a **diagnostic heuristic, not an architecture**: check these places, in
this order, before blaming the model. Its value is catching the Granite-style
misattribution, not describing three real tiers of the world. Two of the rows decompose
cleanly; the third is a behavioral regime the harness can partly reach into but not fully
own.

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

So the fixes are real but bounded, and the boundary is worth stating exactly. It's not a
layer seam — it cuts *through* the model row. The harness can guarantee **form**: recover a
dropped call, describe tools when there's no API, grammar-constrain to a schema-valid call.
It cannot guarantee **judgment**: which tool, what arguments, whether to stop. Even
grammar-forced, the weak models here pick `glob` when the job needs `grep`, put the schema
where a value goes, and emit `*.rust` for `*.rs`. The harness reaches into the behavioral
regime far enough to fix shape; it can't cross into choosing well.

On the tested weak models the fixes move the outcome from "call vanishes / 500s" to "call
dispatches, then the model fails for a real reason." That's a genuine improvement — it would
let a model with good judgment and sloppy formatting succeed — but on these models it isn't
end-to-end, because their failure is on the judgment side of that line. Which families
actually clear it on CPU: [`small-model-tool-calling.md`](small-model-tool-calling.md).

All three mechanisms are unit-tested in [`src/agent.rs`](../src/agent.rs); reproduce with
`OPENHARN_PROMPT_TOOLS=1` / `OPENHARN_STRICT_TOOLS=1` / `OPENHARN_NARROW=1` (see
[`adapting-openharn.md`](../docs/adapting-openharn.md)).

## glob vs grep: a name/content confusion worth guarding

The name↔content swap is the most common *wrong-tool* mistake small models make, in
both directions:

- **name search → `grep`/`grep_system`** (should be `glob`/`glob_system`): "find a file
  called X" triggers a content search. Guarded by `find_file_uses_glob_not_grep` in
  [`tests/behavior.py`](../tests/behavior.py).
- **content search → `glob`/`glob_system`** (should be `grep`/`grep_system`): "search files
  for the string X" triggers a name search. Guarded by `grep_for_content_not_glob`.

The `OPENHARN_STRICT_TOOLS` grammar (above) *reduces* both — a weak model forced to a
valid call still reaches for `glob` on a `grep` job, but at least emits a parseable call
instead of text. The real fix is prompt/schema wording: `src/prompt.txt` now says
"find a file by NAME → glob; grep ONLY for CONTENTS", and the `glob`/`grep` schemas say
"PREFER glob over grep" / "Use grep ONLY to search file CONTENTS". Both behavioral cases
pass on LFM2.5 with that wording; they are regression locks, not one-off checks.
