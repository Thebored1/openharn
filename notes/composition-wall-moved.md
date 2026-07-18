# Notes: the composition wall I called immovable, and why it moved

For weeks the story in [`bfcl-v4.md`](bfcl-v4.md) ended the same way: a 2-bit LFM2-8B-A1B can
call one tool fine, can't compose two, and no amount of harness cleverness moves that — grammar,
gates, MiniCheck, decomposition, best-of-N all bottomed out at the same floor, so it had to be a
weights problem. I wrote "only weights do (TinyAgent)" and meant it.

Then the goal got reset to *get ≥60% AST, and at every decision go read how other people do it
instead of pulling it out of your ass.* So I did the reading first for once. The wall moved on
the first serious attempt: parallel calls went 17.5 → 72.5%, the AST subset 45 → ~72%, same 2-bit
model, same CPU box, replicated across two runs. The "immovable wall" was half me measuring my own
harness suppressing the model.

This is the write-up of how that happened, because the mistake is the interesting part.

## What I had wrong

I'd been measuring composition through the `prompt-tools` path — openharn flattens the tool list
into a text description in the system prompt and drops the native `tools` field. On that path
LFM2's `parallel` score was ~0–17%. I read that as "the model can't emit two calls." What it
actually was: I'd thrown away the tool presentation the model was *trained* on. LFM2's template
wraps tools in `<|tool_list_start|>…<|tool_list_end|>` special tokens; flattening replaces that
with prose. The capability was sitting behind a format I'd deleted.

I only saw it because three papers each told me a different piece, and none of them is mine:

## Related work

- **The constraint tax.** Li, Zhang, Lv, *"The Constraint Tax in Open-Weight LLMs"*
  (arXiv:2606.25605) reproduce exactly what I'd been eating and name the mechanism: when a JSON /
  grammar constraint is compiled into a token mask, tool-call tokens become *unreachable* during
  decoding, and the model's action selection degrades even though it can call fine unconstrained —
  they call it Tool Suppression. Their fix is a two-pass decouple: reason free, *then* constrain.
  That's why my strict configs always sat *below* raw native FC on the single-call categories.
  I'd noticed the tax and shrugged; they explain it and say how to pay it back.
- **Adapt the schema to the model, not the model to the schema.** Lee et al., ACL 2026,
  *"Don't Adapt Small Language Models for Tools; Adapt Tool Schemas to the Models"*
  (arXiv:2510.07248, "PA-Tool"): small models fail tool use when the tool *presentation* mismatches
  what they saw in pretraining — they hallucinate plausible-but-absent names, pick wrong tools. My
  flattening is that mismatch, dressed up as a harness feature.
- **Plan before you emit.** LLMCompiler (Kim et al., ICML 2024, arXiv:2312.04511) and guided
  structured templates (Dang et al., EMNLP 2025, arXiv:2509.18076): make the model enumerate the
  calls first. I checked the code — openharn's call grammar already permits an N-object array
  (`call ::= "[" obj ("," obj)* "]"`); parallel was never grammar-capped. So the model doesn't need
  permission to emit two calls, it needs to *commit* to two before the grammar clamps down.

Three inference-time, model-agnostic levers. Finetuning stays off the table — that's TinyAgent's
answer, not openharn's.

## What I changed

All opt-in, all model-agnostic, the coding-agent default untouched:

- **`OPENHARN_NATIVE_TEMPLATE`** already existed but had only ever run on the MiniCPM quant-rescue,
  never on LFM2's AST subset. It renders the model's own template via `/apply-template` (native
  presentation back), and grammar-forces the call. Running it *is* the PA-Tool experiment.
- **`OPENHARN_PLAN_FIRST`** (new): LFM2's template has no think tag — the assistant turn opens
  straight at the answer (checked via `/apply-template`), so there's no native reason-first phase
  to exploit. So inject one: an unconstrained "list every separate tool call, one per line" step,
  stop before any JSON, append it, *then* run the constrained call. The two-pass decouple for a
  non-thinking model. The primer names no model; the think tag, when present, is read from the
  template.
- **`OPENHARN_DEDUP_CALLS`** (new): plan-first makes the model *repeat* calls (more below), which
  the checker scores `wrong_count`. Drop exact-duplicate calls.
- **Transport retry** (always on): 3 attempts on a dropped llama-server connection instead of
  silently returning no call. Not cosmetic — native-template makes 2–3 requests per entry and the
  server's accept queue flakes under load; one run logged 64 recovered flakes. Without it, the
  baseline was being depressed by my own dropped connections, not the model.

## What happened

160-entry AST subset (simple/multiple/parallel/parallel_multiple, 40 each), LFM2-8B-A1B UD-Q2_K_XL,
CPU `-ngl 0`, all arms on one binary and one server session:

| | D (flatten+gate) | native | native+plan | native+plan+dedup |
|---|---|---|---|---|
| simple_python | 70.0 | 55.0 | 60.0 | 87.5 / 80.0 |
| multiple | 55.0 | 45.0 | 52.5 | 62.5 / 60.0 |
| parallel | 17.5 | 52.5 | 67.5 | 75.0 / 70.0 |
| parallel_multiple | 37.5 | 55.0 | 57.5 | 70.0 / 70.0 |
| **AST** | **45.0%** | **51.9%** | **59.4%** | **73.75 / 70.0%** |

The winning column is two independent full runs, shown `A / B`, because earlier in this same
project I shipped an 85% number that evaporated on the next run. This one holds: 73.75 and 70.0,
mean 71.9%, both clearing 60 by ten-plus points.

Read left to right and it's a clean decomposition of *why*:

- **native presentation alone** is the unlock — parallel 17.5 → 52.5, parallel_multiple 37.5 → 55.
  That's the PA-Tool effect measured on my own box: the capability was latent, flattening was
  hiding it. But it *taxes* the single-call categories (simple 70 → 55) — grammar from token 0
  with no reasoning buffer, exactly the constraint tax.
- **plan-first** pays the tax back (singles recover) *and* the explicit commit-to-N lifts parallel
  again, to 67.5. 59.4% — one entry short of target, which is almost funny.
- **dedup** clears it and then some, +12 to 14 points, because plan-first's repeats were costing
  entries across every category.

## The duplicate thing

Here's plan-first on `parallel_0` ("play Taylor Swift 20m and Maroon 5 15m"), verbatim from
LFM2-Q2:

```
1. Play Taylor Swift for 20 minutes -> call spotify_play with artist="Taylor Swift" duration=20.
2. Play Maroon 5 for 15 minutes -> call spotify_play with artist="Maroon 5" duration=15.
3. No additional tools are needed; each call is explicit and distinct.
4. Execute both calls.
spotify_play: {"artist": "Taylor Swift", "duration": 20}
spotify_play: {"artist": "Maroon 5", "duration": 15}
```

The model *can* enumerate two distinct calls — it says so ("each call is explicit and distinct").
What it couldn't do was produce them when the grammar clamped from token 0, or when flattening hid
the format. Give it the buffer and it plans them cleanly. Notice the tail, though: it starts
re-emitting the calls, and the constrained pass that follows sometimes repeats one — that's the
duplicate source. dedup mops it up. I checked the passing entries by hand: parallel_0/1/2/3 pass
with genuinely distinct arguments, not because dedup collapsed a lucky repeat.

## What's still walled

Every win here is *independent* parallel calls. Dependent composition — one call's output feeds
another's argument, like `parallel_multiple_27` where the second call's `principal=5000` is only
stated in the first — didn't move. Plan-first enumerates it fine; the model still can't route the
shared value. Same failure as the agentic `cd` then `mv` sequences. That one is a weights problem,
not a presentation one, and it's the honest remaining wall.

So the corrected statement is smaller and more useful than "only finetuning moves it": before you
reach for weights, check that your harness isn't hiding capability the model already has. Mine was.

Full arm-by-arm detail, caveats (multiple never fully recovers; dedup has one known false-positive
entry), and reproduce commands are in [`bfcl-v4.md`](bfcl-v4.md), section "The wall moved."
