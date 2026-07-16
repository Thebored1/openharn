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

| Category | A: raw native FC | B1: prompt-tools+strict | C: harness+gate | C − A |
|---|---|---|---|---|
| simple_python (40) | 32 (80.0%) | 14 (35.0%) | 24 (60.0%) | −20.0 |
| multiple (40) | 33 (82.5%) | 4 (10.0%) | 18 (45.0%) | −37.5 |
| parallel (40) | 0 (0.0%) | 2 (5.0%) | 1 (2.5%) | +2.5 |
| parallel_multiple (40) | 0 (0.0%) | 2 (5.0%) | 10 (25.0%) | +25.0 |
| irrelevance (40) | 30 (75.0%) | 37 (92.5%) | 35 (87.5%) | +12.5 |
| **OVERALL** | **95/200 (47.5%)** | 59/200 (29.5%) | **88/200 (44.0%)** | **−3.5** |

**C** = the best harness config: prompt-tools + strict grammar + abstention sentinel +
relevance gate (`OPENHARN_FC_PROXY=1 OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1
OPENHARN_STRICT_ABSTAIN=1 OPENHARN_FC_GATE=1`).

> **Run-to-run noise.** At temperature 0.001 with 4 parallel `llama-server` slots on CPU,
> generation is not bit-deterministic: two gate runs landed at **44.0%** and **46.5%**
> overall (per-category swings up to ~10 points on the 40-entry categories). So **C and A
> are within noise** — treat the aggregate as a tie, and read the per-category deltas as
> directional, not exact.

### The honest headline

On this model, **no single harness config beats raw native FC overall** (44–46.5% vs
47.5% — a tie within noise).
The harness *redistributes* errors rather than net-reducing them: it **fixes the two
categories native FC scores 0% on** (`parallel_multiple` 0→25%, `parallel` 0→~5%) and
**improves abstention** (`irrelevance` 75→87.5%), but forcing calls through openharn's
text-grammar produces **less accurate arguments than the model's native tool-calling**, so
`simple`/`multiple` regress and the aggregate lands flat.

This *refines* openharn's thesis honestly. "The harness matters more than the model" is
strongest when native tool-calling is **broken or absent** (the original premise — bitnet.cpp,
old forks, models that emit descriptive text). On current `llama.cpp`, this Q2 model's
native FC already works, so the harness's value narrows to what native FC *cannot* do —
express multiple calls in one turn, and gate abstention — while it should otherwise defer
to native FC for argument accuracy.

### The path there (each step a model-agnostic change)

| Config | Overall | What moved |
|---|---|---|
| raw native FC | 47.5% | baseline |
| prompt-tools + strict (B1) | 29.5% | model escapes into **prose** (`decoder_failed`) instead of calling |
| + abstention sentinel | (mini) simple 35→100% | grammar forbids prose → `call` or `NO_TOOL`; but over-calls on irrelevance |
| + relevance gate (C) | 44–46.5% | YES/NO pre-pass restores abstention (irrelevance 87.5%); parallel_multiple fixed |

(The intermediate steps were tuned on an 8/cat mini-set; the 8-entry categories are noisy,
which is why the mini-set favoured the gate by +5 but the full 200 lands flat. Real numbers,
not the rosy small-sample ones.)

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
   truth — where native FC returned one. This is the whole `parallel_multiple` 0→25% gain.
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
- **BFCL registration:** `underscore_to_dot=True` — BFCL sanitizes dotted function names
  to underscores for the OpenAI FC schema, so the checker must map them back (without it,
  correct calls score as `wrong_func_name`). (`tests/bfcl/register_models.py`)

New harness features added for this (see `src/serve.rs`, `src/agent.rs`):
`OPENHARN_FC_PROXY` (single constrained tool-call over a request's own schemas),
`OPENHARN_STRICT_ABSTAIN` (call-or-`NO_TOOL` grammar), `OPENHARN_FC_GATE` (YES/NO
relevance pre-pass). Documented in [`docs/adapting-openharn.md`](../docs/adapting-openharn.md).

## Takeaway

- **Best single config for this model on this subset: raw native FC.** When native
  tool-calling works, defer to it — the harness's forced-call grammar costs argument
  accuracy.
- **The harness earns its keep on native FC's blind spots:** multi-call/parallel (`parallel_multiple` 0→25%
  on `parallel_multiple`) and abstention gating. Reach for prompt-tools + strict + abstain
  + gate when native FC is weak/absent (the original openharn premise) or specifically for
  those categories.
- **Aggregate score is a wash here (44–46.5% vs 47.5%, within run-to-run noise)** because the subset weights parallel
  (native's 0%) and simple/multiple (native's strength) equally; the right config depends
  on the actual task mix.
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
