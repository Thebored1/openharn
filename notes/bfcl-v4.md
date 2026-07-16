# BFCL v4: does the openharn harness move a weak model's function-calling score?

Lab notes from putting `LFM2-8B-A1B-UD-Q2_K_XL` (2-bit, ~1B-active MoE) through the
**Berkeley Function Calling Leaderboard v4** — the whole arc, not just the final table:
the setup, the hurdles, the wrong turns, the bug that flipped the result, and where the
model (not the harness) is the wall. Everything here is on CPU (`-ngl 0`), 12 threads,
`llama-server` build b9611/9947, temperature 0.001.

---

## Related work

**BFCL** — Patil, Mao, Cheng, Roosta, Ji, Zou, Adala, Kumar, Yan, et al.,
*"The Berkeley Function Calling Leaderboard (BFCL): From Tool Use to Agentic Evaluation of
Large Language Models"*, **ICML 2025 (PMLR v267)**; OpenReview `2GmDdhBdDk`; leaderboard at
gorilla.cs.berkeley.edu/leaderboard.html; code + data in `github.com/ShishirPatil/gorilla`
(`berkeley-function-call-leaderboard`, pip `bfcl-eval`). BFCL's contribution is a
**scalable AST evaluation**: a model's emitted call is decoded and checked against a
curated *possible-answer* set — function name, presence of every required parameter, and
each argument's value/type (incl. enums, nesting, case-insensitive matches) — without
executing it, so it scales to thousands of functions. A subset also does executable
checking; the v4 additions are **agentic** (multi-turn state, memory, web search, format
sensitivity). I used the official datasets + AST checker via `bfcl-eval`, on a fixed subset
(below) — these are **subset** numbers, not leaderboard entries.

This slots under the repo's existing frame — Belcak et al., *"Small Language Models are the
Future of Agentic AI"* (NVIDIA, arXiv:2506.02153v2), whose V1 (SLMs are sufficiently
powerful *with a good harness*) is exactly what BFCL v4 lets us falsify with a real
function-calling benchmark rather than openharn's own behavioral suite
([`small-model-tool-calling.md`](small-model-tool-calling.md)).

I read the paper/OpenReview + the Gorilla repo source (handlers, `model_config.py`,
`TEST_CATEGORIES.md`) as the primary sources; not blog posts.

---

## The question

openharn's thesis is *the harness matters more than the model*. BFCL scores exactly what
the harness touches — tool-call reliability — so it's a clean isolation: hold the model and
`llama-server` fixed, change only the layer in front, read the delta.

**Conditions** (same dataset, same AST checker):

- **A — raw native FC**: BFCL → `llama-server` directly (the model's own tool-calling). The
  no-harness control.
- **B1 — harness, prompt-tools + strict**: BFCL → `openharn --serve` (FC-proxy) →
  `llama-server`. openharn describes the tools in the prompt and grammar-forces a
  schema-valid `<tool_call>[{…}]` **array**. (This is the tuned LFM2 config from
  `configs/LFM2-8B-A1B-UD-Q2_K_XL.conf`.)
- **B2 — harness, native + recovery**: FC-proxy but pass tools natively, only add text-call
  recovery. Measured ≈ A (llama.cpp parses this model's native calls fine, so recovery
  rarely fires) → not broken out as its own column.
- **C — harness, +abstain +gate**: B1 plus the abstention grammar and the relevance gate.
- **D — C + the whitespace fix**: the config that actually wins.

---

## Setup & procedure (and the hurdles, because they cost real time)

BFCL is built to score a model *by name* through its own handler registry; to point it at
either the raw model or openharn on the *same* data, I registered two OpenAI-compatible FC
models and chose the endpoint at run time with `OPENAI_BASE_URL`. Requests flow
**BFCL → (A) `llama-server:8080` | (D) `openharn --serve:8090` → `llama-server:8080`**.

Wiring, in order, with the potholes:

1. **`pip install bfcl-eval`** — then `import bfcl_eval` dies with
   `ModuleNotFoundError: No module named 'soundfile'`: the model registry imports a Qwen
   handler that transitively imports `soundfile`. Fix: `pip install soundfile`. (Nothing to
   do with our model; just a hard import in `model_config.py`.)
2. **openharn couldn't be driven by BFCL as-is.** Stock `--serve` runs the *agent loop* per
   request and returns only the final assistant **text** — `content`, no `tool_calls`, and
   `"usage": null`. BFCL's FC handler reads `choices[0].message.tool_calls` and
   `usage.prompt_tokens`, so it would both find no call *and* crash on the null usage. →
   Added **FC-proxy mode** (`OPENHARN_FC_PROXY=1`, `src/serve.rs` + `agent::fc_proxy_once`):
   when a request carries `tools`, do ONE constrained generation and return the `tool_calls`
   + a real `usage` block. No agent loop — so it measures the tool-call layer in isolation.
3. **Model registration** (`tests/bfcl/register_models.py`, patches the installed
   `model_config.py`): two `ModelConfig` entries, `OpenAICompletionsHandler`, `is_fc_model=True`.
4. **`underscore_to_dot=True` — the subtle one.** First run of `simple_python` scored 50%,
   all failures `wrong_func_name`: the model emitted `math_factorial` but the checker
   wanted `math.factorial`. BFCL sanitizes dotted names to underscores for the *OpenAI FC
   schema* (OpenAI function names can't contain `.`), and the checker only maps them back
   when the model is registered `underscore_to_dot=True`. Flipping it: 50% → **100%** on the
   same outputs. So a chunk of "failures" was a registration artifact, not the model.
5. **Windows console (cp1252) crash.** `bfcl evaluate` prints a 🦍 emoji and dies with
   `UnicodeEncodeError: 'charmap' codec can't encode '\U0001f98d'`. Fix: `PYTHONUTF8=1
   PYTHONIOENCODING=utf-8`.
6. **`OPENAI_API_KEY` required even for offline scoring** — `bfcl evaluate` instantiates the
   handler (which builds an OpenAI client) before doing AST checks. Set a dummy key.
7. **Subsetting.** Full v4 is impractical on this box, so I fixed a reproducible slice: the
   first N per category via `test_case_ids_to_generate.json` + `--run-ids`, and scored with
   `--partial-eval`. `--run-ids` runs *only* the listed ids (and ignores `--test-category`);
   `--partial-eval` scores only the entries present in the result files (and warns loudly
   that it's not a leaderboard number). `BFCL_PROJECT_ROOT` redirects `result/`, `score/`,
   the id file, and `.env` to a scratch dir.

**Subset:** first 40 of each of 5 single-turn categories = **200 entries**
(`simple_python`, `multiple`, `parallel`, `parallel_multiple`, `irrelevance`). Iteration was
done on an 8/cat = 40-entry mini-set (which turned out to be misleading — see below). All of
this is scripted in [`tests/bfcl/`](../tests/bfcl/) (`register_models.py`, `subset.py`,
`analyze.py`, `README.md`).

**Categories, and why these.** BFCL v4's AST-scored single-turn set — `simple_{python,java,
javascript}`, `multiple` (pick one of several), `parallel` (N calls at once),
`parallel_multiple` (several tools *and* N calls), `irrelevance` (abstain — no tool fits),
and their `live_*` user-contributed variants — is exactly the tool-call layer openharn
touches. The **agentic** set (`multi_turn_*`, `memory_*`, `web_search_*`,
`format_sensitivity`) needs BFCL's stateful execution environment; probed separately below.

---

## Hardware: GPU vs CPU on this box (tested per instruction)

The RTX 2050 laptop GPU (4 GB, ~3.4 GB free) *can* full-offload this 3.1 GB model via the
**winget** Vulkan `llama-server` build (`ggml-vulkan.dll`; the Downloads zip is CPU-only) —
devices show as `Vulkan0` (Intel iGPU) / `Vulkan1` (RTX 2050). But measured head-to-head on
a 200-token generation:

| Setup | gen tok/s | prompt tok/s | VRAM headroom |
|---|---:|---:|---|
| CPU (`-ngl 0`, 12 threads) | **26.6** | 44.3 | n/a |
| RTX 2050 full offload (`--device Vulkan1 -ngl 99`) | 22.4 | 53.0 | **~15 MiB** |

For a ~1B-active MoE, generation is memory-bandwidth-bound with tiny compute, so the weak
laptop GPU is *slower* on decode and leaves no room for KV/compute (OOM-tight on longer
prompts). "As much as the system can" resolves to **CPU** here — faster and safer, and it
matches the CPU-first thesis. All BFCL runs are CPU. (Details: [[gpu-vs-cpu-lfm2-a1b]] in
memory; consistent with the OOM note in [`small-model-tool-calling.md`](small-model-tool-calling.md).)

---

## Results (200-entry subset, `--partial-eval`)

| Category | A: raw native FC | B1: prompt-tools+strict | C: +abstain+gate (ws bug) | **D: C + ws fix** | D − A |
|---|---|---|---|---|---|
| simple_python (40) | 32 (80.0%) | 14 (35.0%) | 24 (60.0%) | 30 (75.0%) | −5.0 |
| multiple (40) | 33 (82.5%) | 4 (10.0%) | 18 (45.0%) | 23 (57.5%) | −25.0 |
| parallel (40) | 0 (0.0%) | 2 (5.0%) | 1 (2.5%) | 9 (22.5%) | **+22.5** |
| parallel_multiple (40) | 0 (0.0%) | 2 (5.0%) | 10 (25.0%) | 17 (42.5%) | **+42.5** |
| irrelevance (40) | 30 (75.0%) | 37 (92.5%) | 35 (87.5%) | 35 (87.5%) | +12.5 |
| **OVERALL** | **95/200 (47.5%)** | 59/200 (29.5%) | 88/200 (44.0%) | **114/200 (57.0%)** | **+9.5** |

**D config:** `OPENHARN_FC_PROXY=1 OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1
OPENHARN_STRICT_ABSTAIN=1 OPENHARN_FC_GATE=1`, on a binary with the bounded-whitespace grammar.

### Headline

**After fixing a grammar bug, the harness beats raw native FC by ~5–9 points** (D: ~53–57%
vs A: 47.5%). The win is *entirely* on the two categories native FC **structurally cannot**
do — `parallel`/`parallel_multiple`, where native returns a single call and openharn's JSON
**array** expresses N — plus better abstention on `irrelevance`. It isn't free: forcing
calls through the text-grammar fills arguments a bit less accurately than the model's native
tool-calling, so `simple`/`multiple` sit below raw. That's the thesis landing as stated: *a
good harness makes a small model do things its native tool-calling can't* — with an honest
caveat that for plain single calls, this model's native FC is already good.

### Run-to-run noise (don't over-read small deltas)

Temperature 0.001 with 4 parallel `llama-server` slots on CPU is **not** bit-deterministic.
Two **D** runs landed at **57.0%** and **53.0%** (parallel swings most, being a 40-entry
category where each item is 2.5 points). The whitespace fix *reduced* variance (it killed
the stochastic runaway) but didn't remove it. Treat D as **~53–57%**, and read per-category
deltas as directional. The earlier "C = 44.0% / 46.5%" spread was the same effect, larger.

---

## The path (each step a model-agnostic change — and the two dead ends)

| Config | Overall | What moved / what I learned |
|---|---|---|
| raw native FC (A) | 47.5% | baseline. Surprise: native FC *works* on this Q2 model (see reconciliation) |
| prompt-tools + strict (B1) | **29.5%** | **dead end #1** — the tuned config *loses*: the model escapes into **prose** instead of calling (`decoder_failed`) |
| + abstention sentinel | mini: simple 35→100% | grammar `call \| "NO_TOOL"` forbids prose → forces a call or a literal abstention; but now over-calls on irrelevance |
| + relevance gate (C) | mini **52.5%** → full **44%** | **dead end #2** — the YES/NO gate looked like a +5 win on the noisy 8/cat mini-set, then landed *flat* on the full 200 |
| + bounded-whitespace grammar (D) | **53–57%** | the real fix: stops the runaway that silently dropped valid calls; `parallel` 2.5→22.5, `parallel_multiple` 25→42.5 |

Two things I got wrong along the way and had to walk back:

- **B1 (the recommended config) is the *worst* on BFCL.** `configs/LFM2-8B-A1B-UD-Q2_K_XL.conf`
  = `PROMPT_TOOLS=1 STRICT_TOOLS=1`, tuned to `4/4` — but by `tune_model.py` on openharn's
  *own coding tools + behavioral tests*, not BFCL. On BFCL that exact config scores 29.5%,
  below raw's 47.5%, because openharn's `<tool_call>` text protocol competes with the
  model's native FC training and the model just answers in prose. "Best for the coding
  agent" ≠ "best for function-calling."
- **The mini-set lied.** I tuned abstain→gate on 8 entries/category; the gate showed +5 there
  and I nearly shipped that story. On the full 200 it was flat. 8-entry categories are ±12.5
  points *per item* — useless for ranking close configs. Lesson re-learned.

---

## The whitespace bug (how it was found, because it's the whole story)

The gate result (C) sat at ~44%, and `parallel`/`simple` had a lot of `null` model outputs.
Tracing them:

1. `null` results split two ways in the raw files: `"NO_TOOL"` (the gate abstained) and `""`
   (empty). The `NO_TOOL` ones are gate false-negatives. The `""` ones were the mystery.
2. Replaying `parallel_0` through the gate serve: the gate said **YES** (correct), but the
   proxy returned **0 tool_calls** with content that *looked* like a valid call. Dumping the
   full (untruncated) content:

   ```
   [{"name": "spotify_play", "arguments": {"artist": "Taylor Swift", "duration": 20}}  \n  \n   \n  \n … (whitespace to max_tokens, no closing "]")
   ```

   The model emits a **valid first call object**, then instead of `,` or `]` it spews
   whitespace forever and never closes the array. `parse_text_tool_calls` needs a closing
   `]`, finds none, recovers **nothing** — a correct call silently discarded.
3. Root cause: `GRAMMAR_TAIL` had `ws ::= [ \t\n\r]*` — **unbounded** inter-token whitespace.
   The weak model, unsure how to continue after the first object, loops on the cheap `ws`
   production. Fix: `ws ::= [ \t\n\r]?` (at most one), which forces the next token to be `,`
   or `]`.
4. Verified live before/after on the exact failing cases: `parallel_0` 0 → **2 correct
   calls**; `simple_python_0` `""` → a clean `calculate_triangle_area` call.

This single `*`→`?` moved the whole harness from **44% to 57%** (`parallel` +20,
`parallel_multiple` +17.5, `simple` +15, `multiple` +12.5). It was *stochastic* (it fired in
some runs, not others), which is also why the numbers wobbled run-to-run. The lesson is
uncomfortable and worth keeping: a permissive grammar quantifier can silently eat correct
output on a weak model, and it looks exactly like "the model is bad."

---

## Failure taxonomy (harness config D, real examples)

Four root causes, only two of which the harness can touch:

| # | Cause | Harness-induced? | Fixable? |
|---|---|---|---|
| A | **Runaway whitespace** → valid call lost | yes (grammar) | ✅ fixed (above) |
| B | **Gate false-negative** → abstains (`NO_TOOL`) on a relevant request | yes (gate judgment) | partially |
| C | **Argument value/type error** → forced call fills args wrong | partly (grammar can't supply values) | partially |
| D | **Model judgment** → wrong tool, duplicate calls, no decomposition | no | no |

- `simple_python_8` [**C**] — *"area of a circle radius 10"* → `{radius:10, units:"units"}`;
  enum wanted `["meters",""]`. The model echoed the word "units" from the prompt.
- `simple_python_13` [**C**] — *"area under curve x²"* → `interval:[1,3]`; expected
  `[[1.0,3.0]]` (floats, nested). Ints where the checker wants floats.
- `multiple_2` [**B**] — *"capital of Brazil"* → `NO_TOOL`. Gate wrongly abstained.
- `multiple_3` [**A→D**] — *"Euclidean distance A(3,4) B(1,2)"* → was runaway-lost; post-fix
  it emits the call but **duplicates** it → `wrong_count`.
- `parallel_0` [**A**] — *"play Taylor Swift 20m and Maroon 5 15m"* → the runaway case;
  post-fix emits both calls correctly.
- `parallel_4` [**D**] — *"BMI for 6ft and 5.6ft"* → sent `height:211.2` (a bogus unit
  conversion) instead of `6.0`.
- `parallel_multiple_0` [**D**] — *"sum of multiples + product of primes"* → only the first
  call; didn't decompose into 2.
- `parallel_multiple_20` [**D**] — *"median, variance, mode"* → emitted `median` twice + `mode`,
  **missed `variance`**.
- `irrelevance_11` [**B/D**] — *"closest integer to 30"* → called `get_closest_prime`; should
  have abstained. The tool name looked plausibly relevant.

The harness raises the floor on **mechanics** (get a well-formed, in-schema call out); it
cannot supply **judgment** (which tool, what values, how many calls, when to abstain). That's
the same line openharn's own docs draw, now measured on an external benchmark.

---

## Agentic tools (multi_turn / memory / web_search)

Probed the agentic path — `multi_turn_base` (4-turn scenarios; BFCL instantiates stateful
backends `GorillaFileSystem` + `TwitterAPI`, injects their methods as tools, executes the
model's calls against live state, and checks the resulting state).

- **The plumbing works.** BFCL drives the multi-turn loop through openharn's FC-proxy
  end-to-end; no crashes; all 4 turns of each scenario ran.
- **Hurdle → finding: the relevance gate must be OFF for agentic.** With `FC_GATE=1`, every
  turn came back `["NO_TOOL"]` — the harness abstained the entire scenario (0%, "Failed to
  decode the model response" each turn). The gate is a *single-turn irrelevance* tool; in an
  agentic loop the tool list is large (all methods of the involved classes), the context is
  long, and the weak model's YES/NO judgment collapses to NO. Dropping `FC_GATE`, the harness
  *acts*: turn 0 correctly `mv final_report.pdf → temp`. So the winning single-turn config is
  the *wrong* config for agentic — gate **on** for single-turn, **off** for multi-turn.
- **Over-calling.** Without the gate the multi-call array + weak model over-emit duplicates
  (`search_tweets` ×N, `mv` ×N) where most turns want one action.
- **Score: 0/5 on `multi_turn_base` (gate off).** This is a **model ceiling**, not a harness
  one: even acting correctly on turn 0, a 2-bit 1B-active model can't sustain multi-step
  planning. Consistent with BFCL multi-turn being where frontier models drop to ~40–50% and
  small models to ~0–5%. No harness knob supplies multi-step planning; the levers that remain
  (a per-turn call cap; wiring openharn's real agent loop with grounding/circuit-breaker)
  would tidy the over-calling but won't move ~0% on this model — they'd only pay off on a
  stronger one.

I did **not** add agentic-specific code: the finding there is "disable the gate + it's a
model ceiling," not a code change.

---

## Reconciliation with the earlier notes

[`small-model-tool-calling.md`](small-model-tool-calling.md) concluded LFM2-8B-A1B *"never
calls a tool"* natively (0/4, defaults to a Markdown fence) and only works with
`PROMPT_TOOLS+STRICT`. Here the **raw native FC** condition scores **47.5%** on BFCL and
emits clean `tool_calls`. That's not a contradiction to wave away — it's two real differences:

1. **Newer `llama.cpp`** (b9611/9947 here vs 9585 there): native tool parsing / the LFM2
   jinja template improved, so the model now samples its native call format under BFCL's FC
   request shape.
2. **Different task shape.** The old note drove openharn's *coding tools* through the agent
   loop; BFCL sends a single FC request with `tools` and scores one response. LFM2's native
   FC training fires on the latter.

So the honest update: on *current* llama.cpp, this Q2 model's native FC works, and openharn's
prompt-tools protocol — which *helped* on the old stack — now *competes* with it and loses on
single calls. The harness's remaining, real edge is the stuff native FC can't express
(multi-call, gated abstention) plus the grammar-hardening fixes. Same as the a400m episode in
the old note: a "0" can be harness *or* model, and here a big chunk of an apparent harness
win/loss was actually a grammar quantifier.

---

## Model-agnostic changes made (all derive from the request's tools/schemas)

- **Bounded whitespace** (`ws *`→`?`) — biggest lever (44→57%); stops the runaway that
  silently dropped valid calls. (`agent.rs` GRAMMAR_TAIL)
- **`number` params allow decimals** — were pinned to the integer rule; BFCL has float args.
  (`agent.rs::value_rule_for`)
- **Grammar rule-name sanitization** — any non-alphanumeric → `-`, so dotted names like
  `math.factorial` yield valid GBNF rule names. (`agent.rs::tool_grammar`)
- **Text branch can't start with `[`/`{`/`<`** — forces JSON-looking output through the
  closed `call` branch instead of leaking an unterminated array as unrecoverable text;
  `<tool_call>` marker made optional so a bare `[{…}]` array is accepted too.
  (`agent.rs::tool_grammar`)
- **FC-proxy** (`OPENHARN_FC_PROXY`), **abstention grammar** (`OPENHARN_STRICT_ABSTAIN`,
  `call | "NO_TOOL"`), **relevance gate** (`OPENHARN_FC_GATE`, YES/NO pre-pass). New serve
  features (`src/serve.rs`, `src/agent.rs`); flags in
  [`docs/adapting-openharn.md`](../docs/adapting-openharn.md).
- **BFCL registration:** `underscore_to_dot=True` (checker maps sanitized dotted names back).
  (`tests/bfcl/register_models.py`)

---

## Takeaway

- **Best config on this subset: the openharn harness (D, ~53–57%) beats raw native FC
  (47.5%)** — but only after the whitespace fix, and only because of the multi-call
  categories native FC can't do (`parallel_multiple` 0→~42%, `parallel` 0→~20%) and better
  abstention.
- **The tuned coding-agent config (B1) is the wrong tool for BFCL** (29.5%) — it makes the
  model answer in prose. Config is task-specific.
- **A cheap 2-bit model clears ~47–57% of a single-turn subset**; the rest is *judgment*
  (decomposition, tool choice, argument values, abstention), which no harness supplies.
- **Multi-turn is a model wall (0%)**; the harness plumbs through it but can't plan for the
  model. Disable the gate for agentic use.
- **Process lesson**: a permissive grammar `*` and an 8-entry mini-set each nearly wrote a
  wrong conclusion. Real numbers, full subset, and trace the `null`s.

---

## Follow-up: cross-model — the config is model-specific, and the wrong one is catastrophic

Same `parallel_multiple` ×40 subset, same CPU box, raw vs the LFM2-winning harness config
(D: prompt-tools + strict + abstain + gate):

| Model | Raw native FC | Harness (D) | What happens |
|---|---|---|---|
| LFM2-8B-A1B Q2_K_XL | 0.0% | ~42.5% | weak native → harness rescues |
| LFM2-8B-A1B Q4_K_M | 0.0% | 22.5% | weak native → harness rescues |
| MiniCPM-V-4.6 Q8_0 | 72.5–85% | 0.0% | strong native → **harness breaks it** |
| Qwen3.5-0.8B Q8 | 70.0% | 0.0% | strong native → **harness breaks it** |
| LFM2.5-8B-APEX-Compact | — | — | reasoning tax: 4k+ think tokens, never reaches a call on CPU |

Both "breaks it" cases returned **40/40 empty**, by two different mechanisms: MiniCPM-V
emits nothing on a text-only (prompt-tools-flattened) prompt, and Qwen/MiniCPM are
**thinking models** whose templates open a `<think>` block that the strict grammar forbids
— the model literally cannot emit its first token. (This also retroactively explains the
gate returning empty on those models.) `PROMPT_TOOLS`/`STRICT` are a crutch for models that
*can't* do native FC; applied to one that can, they're not merely suboptimal — they zero it.

Side result: **Qwen3.5-0.8B hits 70% raw** on parallel_multiple — far above the old
"below the floor" verdict from the behavioral suite ([`small-model-tool-calling.md`](small-model-tool-calling.md));
that verdict was about openharn's coding-agent tasks, not FC capability.

Quant check on the good model (raw native FC, 3 runs each): **Q8_0 = 77.5/85/85 (~82.5%)**
vs **Q4_0 = 40/42.5/47.5 (~43%)** — a ~40-point cliff, distributions non-overlapping. Note
`Q4_0` is the *legacy uniform* quant; this is a much bigger gap than the LFM2 UD-Q2-vs-Q4_K_M
pair, where two *smart* quants measured the same within noise.

## Follow-up: rescuing MiniCPM-Q4_0 with the harness (the quant-degradation experiment)

Question: Q4_0 halves MiniCPM's tool-calling (82.5→43%). Can the harness recover it,
model-agnostically? Failure diff (Q8-passed ∩ Q4-failed, 18 entries) showed quantization
broke two things — **format motor control** (9× `decoder_failed`: Q4 mangles its own
`<tool_call><function=…>` XML syntax) and **decomposition** (11× `wrong_count`: one call
where N needed). Judgment was mostly intact.

The path there — two instructive dead ends before the fix (all ×3 runs, 16k-ctx server):

| Config | Runs | Mean | Broken leg |
|---|---|---|---|
| raw `tool_choice=auto` (thinking) | 45/50/47.5 | 47.5% | mangles native XML on ~25% of entries |
| **dead end 1:** custom native-template + openharn JSON grammar | 22.5/30/30 | 27.5% | format fixed, but the *foreign* array format suppresses the model's multi-call habit — it closes the array after one item; a decomposition system-nudge did nothing |
| **dead end 2:** `tool_choice=required` (thinking) | 27.5/40/45 | 37.5% | native grammar fixes format AND keeps multi-call, but the forced grammar × think phase kills entries that reason long → 13–23/40 **empty** |
| **fix:** `required` + `enable_thinking:false` | **72.5/72.5/72.5** | **72.5%** | none — format forced, multi-call intact, no think-budget deaths |

**Result: 47.5% → 72.5%, recovering ~71% of the quant gap (Q8 raw ≈ 82.5%), with zero
run-to-run variance and ~4× faster generation.** The residual 27.5% is genuine judgment
(e.g. one `get_rectangle_property(property="length, width")` call where ground truth wants
two calls).

Mechanics worth recording:

- **llama-server rejects `tools` + custom `grammar` in one request** ("Cannot use custom
  grammar constraints with tools") — you cannot bolt openharn's GBNF onto a native FC call.
  `tool_choice=required` is the sanctioned route: the server grammar-forces the model's
  OWN template-derived format. Same GBNF idea, right target format.
- **The think-tag discovery:** `/apply-template` shows MiniCPM-V-4.6's template ends the
  assistant turn with an open `<think>\n` — it's a thinking model. Any grammar applied from
  token 0 (strict mode, the YES/NO gate, `required`) collides with that. The generic
  escape hatches: complete the think block unconstrained first (the `OPENHARN_NATIVE_TEMPLATE`
  machinery detects the open tag from the template's own rendering), or switch thinking off
  via the template's own switch (`chat_template_kwargs: {"enable_thinking": false}` — no-op
  where unsupported).
- **New knobs** (all model-agnostic; `docs/adapting-openharn.md`): `OPENHARN_TOOL_CHOICE`
  (forward `required`), `OPENHARN_TEMPLATE_KWARGS` (raw `chat_template_kwargs` passthrough),
  `OPENHARN_NATIVE_TEMPLATE` (apply-template + think-phase + array grammar; kept as the
  fallback for servers/models where native FC is absent — with its multi-call caveat
  documented).
- **Slot-context trap:** `--ctx-size 8192` with 4 auto-slots = 2048/slot; MiniCPM's ~1400-token
  tool prompt + thinking overflows it and silently truncates mid-think (returns nothing under
  `required`). The 16k/`--parallel 4` re-baseline also lifted *raw* Q4 from ~43 to 47.5%.
- **Spot checks lie, again:** `required` fixed 3 of 4 hand-picked entries, then lost to raw
  on the full 40 (37.5 vs 47.5) until no-think landed. Full-subset × 3 runs or it didn't happen.

The refined per-model decision tree, one experiment later:

```
native FC absent/broken   → prompt-tools + strict (+abstain/gate)     [LFM2-Q2: 0 → ~42%]
native FC works, degraded → native + tool_choice=required (+no-think) [MiniCPM-Q4: 47.5 → 72.5%]
native FC works, healthy  → leave it alone (harness only for gaps)    [MiniCPM-Q8, Qwen-0.8B]
reasoning tax dominates   → enable_thinking:false first, then decide  [APEX-Compact]
```

## Reproduce

[`tests/bfcl/README.md`](../tests/bfcl/README.md) — exact `bfcl generate/evaluate` commands,
the two-model registration, the subset builder, and the failure analyzer.
