# Adapting openharn to your model, server, and use case

How to bend openharn without reading the Rust. If you can set an environment variable and
edit a text file, you can do everything here.

openharn defaults assume a capable-ish model on a modern `llama-server`. This guide covers
when your model or server isn't that — a weak model that fumbles tool calls, or an old
server with no tool-calling — and how to trade generality for reliability on purpose.

## Reliability vs. generality

Every knob below moves one thing: how much freedom the model has.

- **More freedom** (defaults): all thirteen tools, native tool-calling, the model decides. Good
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
| `OPENHARN_MAX_CALLS` | per-turn tool-call limit; excess calls are **truncated** (not executed) and a grounding message tells the model to make fewer calls next turn (default 1) | model makes too many calls per response |
| `OPENHARN_TOTAL_MAX` | total calls across all turns before tools are removed (default 5) | model never stops calling tools |
| `OPENHARN_NO_THINK` | suppress a reasoning model's thinking (faster on CPU) | LFM2.5 is too slow |
| `OPENHARN_SHOW_THINKING` | stream the raw chain-of-thought instead of the meter | debugging what the model thought |
| `OPENHARN_FRIENDLY_RESULTS` | arm intent detection: classify the user turn as `CHAT` or `TOOL` *before* the tool loop; a `CHAT` turn skips tools and answers directly | model fires tools on greetings / small talk ("hello" → `todowrite`) |
| `OPENHARN_STRICT_ABSTAIN` | (strict only) forbid free-form prose in the grammar: the model may emit ONLY a schema-valid tool-call array or the literal `NO_TOOL` | model "helpfully" answers/computes the request in prose instead of calling the tool |
| `OPENHARN_FC_PROXY` | in `--serve`, when a request carries `tools`, do ONE constrained tool-call generation and return the `tool_calls` (no agent loop). Lets an external function-calling client/benchmark drive openharn's tool-call layer | benchmarking or proxying the harness's tool-call reliability (see BFCL v4, `notes/bfcl-v4.md`) |
| `OPENHARN_FC_GATE` | (FC-proxy, strict) two-pass: a YES/NO relevance pre-pass decides call-vs-abstain, then a call is FORCED when a tool applies | separate the *should-I-call* judgment from the *emit-a-valid-call* mechanics — forces calls on relevant inputs without over-calling on irrelevant ones |
| `OPENHARN_TOOL_CHOICE` | (FC-proxy, native path) forwarded as `tool_choice`; `required` makes the server grammar-force a well-formed call in the model's OWN native format (llama.cpp derives the grammar from the model's chat template) | a model whose native FC works but *degrades* — e.g. mangles its own call syntax under heavy quantization. Forces a call, so pair with the gate on abstention workloads |
| `OPENHARN_TEMPLATE_KWARGS` | raw JSON forwarded as `chat_template_kwargs` into the model's chat template; canonical use `'{"enable_thinking":false}'` (no-op on templates without the switch) | a thinking model burns its budget reasoning — especially under `TOOL_CHOICE=required`, where mid-think truncation returns nothing |
| `OPENHARN_NATIVE_TEMPLATE` | (FC-proxy, experimental) render via the server's `/apply-template` (native tool presentation), complete any open think tag unconstrained, then grammar-force openharn's `<tool_call>[…]` array; falls back to prompt-tools if the endpoint is absent | native FC absent, OR prompt-tools *flattening* is hiding a multi-call model's ability (BFCL: it lifts LFM2-Q2 `parallel` 17→52% by restoring the native tool presentation — see `notes/bfcl-v4.md`). Grammar-from-token-0 taxes single calls, so pair with `PLAN_FIRST` |
| `OPENHARN_PLAN_FIRST` | (native-template, non-thinking models) inject an explicit UNconstrained planning step — "list every separate tool call, one per line" — before the constrained emission. The two-pass decouple that pays back the constraint tax and makes the model *commit* to N calls | a model whose template has no think tag but drops calls / under-decomposes under a grammar (BFCL: `parallel` 52→68%, recovers the single-call tax). Usually paired with `DEDUP_CALLS` |
| `OPENHARN_DEDUP_CALLS` | drop exact-duplicate tool calls (same name + same argument string) before returning | a model repeats a call under the forced array grammar (common with `PLAN_FIRST`) and the extra copy is scored wrong. Unsafe for the rare task that legitimately needs the identical call twice — hence opt-in |

`NARROW` implies `STRICT_TOOLS`; `STRICT_TOOLS` implies `PROMPT_TOOLS` (a grammar can't
combine with the native `tools` field). `NO_THINK` is ignored under strict (its prefill
breaks the grammar, and weak models don't reason anyway). `FRIENDLY_RESULTS`
**requires** `PROMPT_TOOLS` — intent detection only arms when both are set
(`agent.rs`: `friendly_mode = cfg.friendly_results && prompt_tools`). Enabling it
switches the whole run to text-tool mode, so it fixes greetings at the cost of tool
reliability on models that prefer native calls (e.g. MiniCPM-V drops 6/6 → 4/6).
For a capable model, prefer the system-prompt greeting rule + thinking ON over this flag.

## Recipes

Server 500s on `tools` / no tool-calling:
```sh
OPENHARN_PROMPT_TOOLS=1 cargo run -- .
```

Model mangles arguments (wrong field names, bad JSON):
```sh
OPENHARN_STRICT_TOOLS=1 cargo run -- .
```

**Model ignores native tool API** (e.g. LFM2-8B outputs descriptive text instead of
`<tool_call>`):
```sh
OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 cargo run -- .
```
This is the winning combo for LFM2-8B on CPU (6/6 behavioral tests). The GBNF grammar
forces valid `<tool_call>` output that the model wouldn't emit otherwise.

**Small model hallucinates tool results** (e.g. LFM2-1.2B generates fake file sizes
instead of calling tools):
```sh
OPENHARN_YESNO=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_NO_THINK=1 cargo run -- .
```
YES/NO narrows the tool list per turn, reducing hallucination on complex queries.
(6/6 behavioral tests for LFM2-1.2B-Tool on CPU.)

Most reliable agent a weak model can drive:
```sh
OPENHARN_NARROW=1 cargo run -- .          # read,grep,glob, grammar-locked, grounded
```

**Model fires tools on a greeting / small talk** ("hello" → `todowrite`):
two paths — pick by model:
- *Capable model (native tools work):* add a system-prompt rule ("don't call
  tools for casual conversation; reply in plain text") + keep **thinking ON**.
  MiniCPM-V-4.6 reaches 6/6 this way. (The `hello` → `todowrite`
  behavior is the over-eager-tool bug the `greeting_uses_no_tools` test guards.)
- *Deterministic guard:* `OPENHARN_FRIENDLY_RESULTS=1` classifies the
  turn as `CHAT` and skips tools — but it needs `PROMPT_TOOLS` too and
  switches to text-tool mode, which weakens tool tasks on some models.
  ```sh
  OPENHARN_FRIENDLY_RESULTS=1 OPENHARN_PROMPT_TOOLS=1 cargo run -- .
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
   *System-wide search tools are dedicated `glob_system` and `grep_system` (the `scope`
   parameter was removed).*
2. `Session::execute()` — the `match` that runs each tool by name. Add a
   `"mytool" => self.mytool(args),` arm and write the function (takes JSON `args`, returns a
   `String`).

Grounding, grammar, and prompt-tools rendering all derive from `schemas()`, so a new tool
is picked up everywhere.

**Stricter / looser** — in `src/agent.rs`: the grammar is `tool_grammar()` (constrains tool
name, argument keys, value types); the prompt-tools description is `tool_prompt()`; the
circuit breaker limits per-turn calls (`max_calls`, default 1) and total calls (`total_max`,
default 5) before injecting a grounding message; exact-repeat calls halt after 3 identical
invocations; context budget is `HISTORY_BUDGET`; result caps are `TOOL_RESULT_CAP`.

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
[`small-model-tool-calling.md`](../notes/small-model-tool-calling.md).

### Multi-call composition: which part the harness moves, which it doesn't

There are two kinds of "more than one call," and they are **not** the same wall. Measured on
BFCL v4 (see [`../notes/bfcl-v4.md`](../notes/bfcl-v4.md)):

- **Single call** — the model is fine on its own. A 2-bit LFM2-8B-A1B scores ~80% on
  `simple`/`multiple` *raw*. Grammar-from-token-0 actually *taxes* single calls (the
  "constraint tax," Li et al. arXiv:2606.25605) — so `NATIVE_TEMPLATE` alone drops them; pair
  it with `PLAN_FIRST` to pay the tax back.
- **Independent parallel calls** (`parallel`, `parallel_multiple`) — the harness **does** move
  this, further than an earlier version of this doc claimed. The capability is *latent in the
  weights*; the default `prompt-tools` path was **suppressing** it by flattening away the native
  tool presentation. Restore the native presentation (`NATIVE_TEMPLATE`), add a plan buffer
  (`PLAN_FIRST`), strip duplicate emissions (`DEDUP_CALLS`), and LFM2-Q2 goes `parallel`
  **17.5 → 72.5%**, the whole AST subset **45 → ~72%** — replicated, same 2-bit model, same CPU.
  The wins are still *form-shaped*: don't hide the format, don't constrain before the model has
  committed, clean the emission. No new capability is injected — the harness just stops hiding
  what the weights already had.
- **Dependent calls in sequence** (agentic `cd; mkdir; mv`, and cross-call shared values like a
  `principal=5000` stated once but needed by a second call) — **still not fixed.** This is the
  real weights wall. Feeding the model oracle-correct history before each turn (a perfect
  external memory) still yields a 0% hit rate on dependent multi-call turns, so an external
  scratchpad/memory does not rescue it either: the missing piece is *authoring the dependency*,
  not storing it.

What moves the dependent wall: better weights — higher bit-depth (MiniCPM does multi-call ~82%
at Q8 vs ~47% at Q4; composition is the first thing quantization eats) or task-specific
fine-tuning (TinyAgent, 12.7 → 78.9%). But before spending there, make sure your harness isn't
suppressing latent parallel ability the way the flattening path was — that was free.
