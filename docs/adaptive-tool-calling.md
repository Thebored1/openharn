# Making uncooperative models call tools: three failure modes, three fixes

*Tool-calling is the one capability a coding agent can't do without, and it breaks in at
least three structurally different ways. This is how [openharn](../README.md) recovers
from each — and where it still can't.*

---

## Abstract

For a small local model, emitting a **dispatchable tool call** is the gate: everything
else the harness does is moot if the model never produces a call the agent can execute.
Across a benchmark of a dozen local models we found tool-calling fails in three distinct
ways, at three different layers of the stack: the **model** emits an unparseable format,
the **runtime** fails to parse a format that is actually valid, or the **server** has no
tool API at all. openharn now handles the second and third structurally —
`parse_text_tool_calls` recovers a valid tool call the runtime left as text, and
`OPENHARN_PROMPT_TOOLS` mode drives a server with no tool support by describing tools in
the prompt and flattening the conversation to plain roles. The first failure (a truly
unstructured reply, e.g. a Markdown shell fence) remains unsolved and we explain why. The
throughline is openharn's thesis: **the harness's job is to meet the model where it is.**

---

## 1. Three ways tool-calling breaks

Driving many local models through the same agent loop, the failures sort cleanly by
*where* they happen:

| Layer | Failure | Example | Fixable in the harness? |
|---|---|---|---|
| **Model** | emits no structured call — free text | LFM2-v2 writes ```` ```bash\nglob src/*.rs``` ```` | No (nothing structured to recover) |
| **Runtime** | model emits a *valid* call, server won't parse it | Granite-3.1 emits `<tool_call>[{…}]`; llama.cpp expects `<|tool_call|>` → `tool_calls: null` | **Yes** — recover the text |
| **Server** | no tool API at all | bitnet.cpp fork → `500: Unsupported param: tools` | **Yes** — don't use the API |

The distinction matters because the fixes are different, and conflating them (e.g.
"just use a better model") misdiagnoses two of the three.

---

## 2. Layer 0: native tool calls (the happy path)

openharn advertises its ten tools via OpenAI function-calling schemas and consumes the
`tool_calls` array the server returns. For a well-behaved model on a modern
llama-server (LFM2.5, tool-tuned builds), this Just Works and nothing below is needed.
The point of the next two layers is that "modern, well-behaved" is exactly what you
don't get on weak hardware with small models.

---

## 3. Layer 1: recovering a tool call the runtime dropped

Some models emit a **perfectly valid structured call** that the runtime nonetheless
leaves in the content. The canonical case is Granite-3.1: its chat template tells the
model to trigger with the special token `<|tool_call|>`, but the model emits plain
`<tool_call>`, and llama.cpp's parser — watching for the former — drops the latter to
text:

```
content:    <tool_call>[{"arguments": {"pattern": "src/**/*.rs"}, "name": "glob"}]
tool_calls: null
```

This is not a model failure — it's a one-regex-away parse gap. openharn closes it with
`parse_text_tool_calls` (in [`src/agent.rs`](../src/agent.rs)): when the native parse
yields nothing, it scans the content for a `<tool_call>` / `<|tool_call|>` marker
followed by JSON, tolerating both shapes models actually emit —

- a **list**: `[{"name": …, "arguments": {…}}]`
- a **single object**, optionally schema-wrapped: `{"function": {"name": …, "parameters": {…}}}`

— and synthesizes OpenAI-format `tool_calls`. It fires **only** when the native parse
found nothing, so a normal answer is never misread as a call. The recovered call is then
dispatched exactly like a native one, and the raw `<tool_call>` text is suppressed from
the display.

**What this rescued:** Granite-3.1-1b-a400m went from "0/4, dismissed as too small to
call tools" to "dispatches correctly" — a correction that reframed a *model* verdict as
a *harness* gap. (It still failed the task for a real reason — see §5.)

---

## 4. Layer 2: prompt-tools mode — a server with no tool API

Some servers can't do tool-calling at all. bitnet.cpp's server is an old llama.cpp fork
that returns `500: Unsupported param: tools` the instant openharn sends its tool list.
No amount of parsing helps if the request itself is rejected.

`OPENHARN_PROMPT_TOOLS=1` handles this by moving tools out of the API and into the
conversation. The key design choice: **openharn's internal representation is
unchanged** — it still records assistant `tool_calls` and `tool`-role results
internally. Only the *wire format* is transformed, per request, by
`flatten_for_prompt_tools`:

- The **system message** gains a rendered description of every tool plus the exact call
  format to emit (`<tool_call>[{"name": …, "arguments": {…}}]` — the same shape Layer 1
  recovers).
- An assistant turn that made **tool_calls** is rewritten to that call as **text**.
- A **tool-role result** becomes a plain **user** message (`Tool result:\n…`).
- The `tools` field is omitted entirely.

The result is a conversation of only `system`/`user`/`assistant` text messages — which
*any* server accepts — while the model's text tool-call comes back and is recovered by
the same Layer 1 parser. Internally, openharn's loop (circuit breaker, read-before-edit,
context-fit) never knows the difference.

**What this rescued:** openharn drove tools end-to-end against a server that has no tool
API whatsoever. The mechanism is model-agnostic; it works for any limited or old
endpoint, not just bitnet.cpp.

---

## 5. What the harness can't fix

Two honest limits:

- **Layer-0 model failures are out of reach.** LFM2-v2-8B-A1B emits a Markdown
  ```` ```bash ```` fence — `glob src/*.rs` as prose. There is no structured payload to
  recover: the tool name is guessable but the arguments aren't reliably mappable, and
  building a heuristic shell-fence parser trades correctness for coverage. We left it
  unsolved and said so, rather than ship a parser that guesses.
- **Recovery ≠ competence.** Both Granite-3.1-a400m (Layer 1) and BitNet-2B
  (Layer 2) *dispatch* once the harness stops failing them — and then pick the wrong
  tool, put arguments in the wrong field, or loop. The harness can make a model's calls
  *reach* the tools; it can't make a poorly-tool-trained model *choose* well. That's a
  model property, and naming it correctly matters as much as the fix.

---

## 6. Why this is the whole point

Every layer here is the openharn thesis restated: **the harness matters more than the
model.** A capable model on a sloppy harness looks broken; a limited model on an adaptive
one gets as far as its actual capability allows — no further, but no less. The value of
Layers 1 and 2 isn't that they made any *specific* model great (they didn't). It's that
they moved the failure boundary from "the harness gave up" to "the model genuinely can't"
— which is the only place a harness has any business drawing the line.

---

### Appendix — the knobs

| Mechanism | Where | Trigger |
|---|---|---|
| Native tool calls | default | server returns `tool_calls` |
| Text-call recovery | `parse_text_tool_calls` (always on) | native parse empty + content has a `<tool_call>` payload |
| Prompt-tools mode | `flatten_for_prompt_tools` | `OPENHARN_PROMPT_TOOLS=1` |

All three are unit-tested in [`src/agent.rs`](../src/agent.rs); the benchmark harness
([`tests/benchmark.py`](../tests/benchmark.py)) mirrors the recovery parser so scores
reflect openharn's real behavior.
