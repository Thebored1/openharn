# BFCL v4: does the openharn harness move a weak model's function-calling score?

**Model:** `LFM2-8B-A1B-UD-Q2_K_XL` (a 2-bit, 1B-active MoE), CPU-only (`-ngl 0`), 12
threads, `llama-server` build b9611/9947, temperature 0.001.
**Benchmark:** Berkeley Function Calling Leaderboard **v4** (Patil et al., ICML 2025) —
its official datasets + AST checker, via `bfcl-eval`.
**Subset:** first 40 entries of each of 5 single-turn categories = **200 entries**
(full v4 is impractical on CPU; the subset is fixed and reproducible — see
[`tests/bfcl/`](../tests/bfcl/)). `--partial-eval`, so these are subset scores, not
official leaderboard numbers.

## The question, not a leaderboard entry

openharn's thesis is *the harness matters more than the model*. BFCL v4 scores exactly
what the harness touches — tool-call reliability — so it is a clean test: hold the model
and `llama-server` fixed, and change only the layer in front of it.

Three conditions, same dataset, same AST checker:

- **A — raw native FC**: BFCL → `llama-server` directly (the model's own tool-calling).
- **B1 — harness, prompt-tools + strict**: BFCL → `openharn --serve` (FC-proxy) →
  `llama-server`. openharn describes the tools in the prompt and grammar-forces a
  schema-valid `<tool_call>[{…}]` **array**.
- **B2 — harness, native + recovery**: same proxy, but pass tools natively and only add
  openharn's text-call recovery. (Measured ≈ raw — llama.cpp parses this model's native
  calls fine, so recovery rarely fires — so it isn't broken out as a separate column.)

The FC-proxy (`OPENHARN_FC_PROXY=1`) runs exactly ONE constrained generation per request
and returns the `tool_calls` — no agent loop — so it measures the tool-call layer in
isolation. See `src/serve.rs` and `agent::fc_proxy_once`.

## Results (200-entry subset, `--partial-eval`)

| Category | A: raw native FC | B1: prompt-tools+strict | C: harness+gate (ws bug) | **D: C + ws fix** | D − A |
|---|---|---|---|---|---|
| simple_python (40) | 32 (80.0%) | 14 (35.0%) | 24 (60.0%) | 30 (75.0%) | −5.0 |
| multiple (40) | 33 (82.5%) | 4 (10.0%) | 18 (45.0%) | 23 (57.5%) | −25.0 |
| parallel (40) | 0 (0.0%) | 2 (5.0%) | 1 (2.5%) | 9 (22.5%) | **+22.5** |
| parallel_multiple (40) | 0 (0.0%) | 2 (5.0%) | 10 (25.0%) | 17 (42.5%) | **+42.5** |
| irrelevance (40) | 30 (75.0%) | 37 (92.5%) | 35 (87.5%) | 35 (87.5%) | +12.5 |
| **OVERALL** | **95/200 (47.5%)** | 59/200 (29.5%) | 88/200 (44.0%) | **114/200 (57.0%)** | **+9.5** |

**C/D** = the harness config: prompt-tools + strict grammar + abstention sentinel +
relevance gate (`OPENHARN_FC_PROXY=1 OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1
OPENHARN_STRICT_ABSTAIN=1 OPENHARN_FC_GATE=1`). **D** additionally has the whitespace-bound
grammar fix (below).

> **The whitespace bug (C→D).** The strict grammar used `ws ::= [ \t\n\r]*` (unbounded
> whitespace between tokens). On this weak model that backfires: after emitting a *valid*
> first call object it spews whitespace to `max_tokens` and never closes the `]`, so the
> array is unterminated and the parser recovers **nothing** — a correct call silently
> discarded. Bounding to `ws ::= [ \t\n\r]?` forces a `,`/`]`. This alone moved the harness
> from 44% to 57% (`parallel` 2.5→22.5, `parallel_multiple` 25→42.5). The bug was also
> *stochastic*, which is why it drove run-to-run variance.

> **Run-to-run noise.** At temperature 0.001 with 4 parallel `llama-server` slots on CPU,
> generation is not bit-deterministic. Two **D** runs landed at **57.0%** and **53.0%**
> (parallel especially swings, being a small noisy category); the ws fix reduced the
> variance (by killing the runaway) but didn't remove it. Treat D as **~53–57%**.

### The honest headline

**After fixing the whitespace bug, the harness beats raw native FC by ~5–9 points**
(D: 53–57% vs A: 47.5%). The win comes entirely from the two categories native FC
*structurally cannot* do — `parallel` and `parallel_multiple` (native returns a single
call; the harness's JSON **array** expresses N) — plus better abstention on `irrelevance`.
It is *not* free: forcing calls through the text-grammar still fills arguments a bit less
accurately than native FC, so `simple`/`multiple` sit slightly below raw.

This is openharn's thesis landing as stated: **a good harness makes a small model do
things its native tool-calling can't.** The caveat is honest — the harness's edge is on
multi-call and abstention; for plain single calls, this model's native FC is already good,
so the gains there are small or negative. (Note the earlier "net flat" conclusion was
mostly an artifact of the whitespace bug silently eating valid calls — worth remembering
how much a single grammar `*` vs `?` moved the whole result.)

### The path there (each step a model-agnostic change)

| Config | Overall | What moved |
|---|---|---|
| raw native FC | 47.5% | baseline |
| prompt-tools + strict (B1) | 29.5% | model escapes into **prose** (`decoder_failed`) instead of calling |
| + abstention sentinel | (mini) simple 35→100% | grammar forbids prose → `call` or `NO_TOOL`; but over-calls on irrelevance |
| + relevance gate (C) | 44–46.5% | YES/NO pre-pass restores abstention (irrelevance 87.5%) |
| + bounded-whitespace grammar (D) | **53–57%** | stops the runaway that dropped valid calls; parallel 2.5→22.5, parallel_multiple 25→42.5 |

(The intermediate steps were tuned on an 8/cat mini-set; the 8-entry categories are noisy,
which is why the mini-set was noisy. Real numbers, not the rosy small-sample ones.)

## Why the raw baseline fails (from the result files)

1. **Parallel = 0%.** llama.cpp native FC returns a *single* tool call, but
   `parallel`/`parallel_multiple` need N. e.g. `parallel_0` ("play Taylor Swift for 20m
   and Maroon 5 for 15m") — raw emits one `spotify_play`; the checker reports
   `wrong_count`. This is a *format* ceiling, not model judgment.
2. **Prose instead of a call.** On many `simple`/`multiple`/`parallel` entries the model
   *solves the task in text* (e.g. `parallel_1` returns a LaTeX EMF derivation) instead
   of calling the function → `decoder_failed` ("'str' object has no attribute 'keys'").
3. **Irrelevance over-calling.** 10/40 call a tool when no function fits (judgment; the
   harness can only nudge, not fix).

## What the harness changes (and what it can't)

Three new, model-agnostic tool-call knobs (all derived from the request's tools):

1. **Multi-call array** (always on in prompt-tools). openharn's format is a JSON array, so
   several calls fit in one reply. On `parallel_0` the harness returns BOTH
   `spotify_play(Taylor Swift, 20)` and `spotify_play(Maroon 5, 15)` — the exact ground
   truth — where native FC returned one. This is most of the `parallel_multiple` 0→~42% gain (once the whitespace fix stops the array being dropped).
2. **Abstention grammar** (`OPENHARN_STRICT_ABSTAIN`): `root ::= call | "NO_TOOL"` — the
   model may not emit free prose, only a valid call array or a literal abstention. This
   removes the "solve it in prose instead of calling" failure.
3. **Relevance gate** (`OPENHARN_FC_GATE`): a grammar-locked YES/NO pre-pass decides
   call-vs-abstain, then a call is *forced* when a tool applies. Separating judgment from
   mechanics keeps abstention (irrelevance) high while forcing calls on relevant inputs.

**What it can't fix — model judgment (consistent with openharn's own docs):**

- **Argument accuracy.** Forcing a call through the text-grammar fills arguments *less*
  accurately than the model's native FC (wrong enum/`missing_required`/`value_error`),
  which is why `simple`/`multiple` regress under the harness.
- **Parallel decomposition.** The array *lets* the model emit N calls, but on harder
  entries it still emits one (`wrong_count`) — it doesn't recognise that N actions are
  needed.
- **Wrong tool choice** among several (`multiple`), and **gate false-negatives** (it
  says "no tool" on some genuinely-relevant single-tool cases and abstains).

The harness raises the floor on *mechanics*; it cannot supply *judgment*.

## Model-agnostic harness fixes made in this pass

All derive from `schemas()`/the request's tools, so they help any model on any schema:

- **Grammar accepted only integers for `number` params** → now emits a proper decimal
  `number` rule (BFCL has float arguments). (`agent.rs::value_rule_for`)
- **Grammar rule names broke on non-alphanumeric tool names** (BFCL uses dotted names
  like `math.factorial`) → sanitize any non-alphanumeric to `-`. (`agent.rs::tool_grammar`)
- **Malformed JSON leaked via the free-`text` branch** (an unterminated `[{…}` no parser
  can recover) → plain text may no longer start with `[`/`{`/`<`, forcing JSON-looking
  output through the closed, schema-valid `call` branch; the `<tool_call>` marker is now
  optional so a bare `[{…}]` array is accepted too. (`agent.rs::tool_grammar`)
- **Unbounded whitespace let weak models run away** (`ws ::= [ \t\n\r]*`): after a valid
  first call the model emitted whitespace to `max_tokens` and never closed the `]`, so the
  array was unterminated and the call silently lost -> now `ws ::= [ \t\n\r]?`. Biggest
  single lever here (44->57%). (`agent.rs` GRAMMAR_TAIL)
- **BFCL registration:** `underscore_to_dot=True` — BFCL sanitizes dotted function names
  to underscores for the OpenAI FC schema, so the checker must map them back (without it,
  correct calls score as `wrong_func_name`). (`tests/bfcl/register_models.py`)

New harness features added for this (see `src/serve.rs`, `src/agent.rs`):
`OPENHARN_FC_PROXY` (single constrained tool-call over a request's own schemas),
`OPENHARN_STRICT_ABSTAIN` (call-or-`NO_TOOL` grammar), `OPENHARN_FC_GATE` (YES/NO
relevance pre-pass). Documented in [`docs/adapting-openharn.md`](../docs/adapting-openharn.md).

## Takeaway

- **Best config on this subset: the openharn harness (gate + bounded-ws grammar), ~53–57% vs raw 47.5%.** The win is entirely on multi-call categories native FC can't do.
- **The harness earns its keep on native FC's blind spots:** multi-call/parallel (`parallel_multiple` 0→~42%
  on `parallel_multiple`) and abstention gating. Reach for prompt-tools + strict + abstain
  + gate when native FC is weak/absent (the original openharn premise) or specifically for
  those categories.
- **The harness wins by fixing what native FC structurally can't** (parallel: 0→~20%, parallel_multiple: 0→~42%). For plain single calls native FC is already strong, so simple/multiple stay slightly below raw — the net still favours the harness on this subset.
- A cheap CPU model at 2-bit clears ~47% of a BFCL v4 single-turn subset at all — most of
  the remaining gap is *judgment* (decomposition, tool choice, abstention), which no
  harness supplies.

## Reproduce

See [`tests/bfcl/README.md`](../tests/bfcl/README.md).

## Hardware note (CPU vs GPU on this box)

The RTX 2050 (4 GB) was tested per instruction: full Vulkan offload of this 1B-active MoE
runs ~22 tok/s vs ~27 tok/s on 12 CPU threads, and leaves ~15 MiB VRAM headroom (OOM-risk
on longer prompts). CPU is both faster and safer here — generation is memory-bandwidth-
bound with ~1B active params, so the weak laptop GPU loses. All BFCL runs are CPU.
