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

### Prior art that this study lands on top of

Scoping the literature *after* the measurements turned out to explain most of what we hit —
including both dead ends. Worth recording honestly: we re-derived known results.

**Grammar-constrained decoding (GCD)** — Geng et al., *"Grammar-Constrained Decoding for
Structured NLP Tasks without Finetuning"* (EMNLP 2023, arXiv:2305.13971). Establishes the
core openharn move: a formal grammar masks every token that can't lead to a valid output,
so structure is *guaranteed* rather than prompted-for, with **no finetuning**. Everything
`OPENHARN_STRICT_TOOLS` does is an instance of this. Their "without finetuning" framing is
the thesis openharn is built on.

**The format tax** — Park et al., *"Grammar-Aligned Decoding"* (NeurIPS 2024,
arXiv:2405.21047). The critical caveat: GCD **distorts the model's distribution** — output
is grammatical, but its likelihood is no longer proportional to what the model would
actually have said. This is *exactly* our dead end #1 below: transplanting openharn's JSON
array grammar onto MiniCPM (which multi-calls fluently in its native XML) produced valid
JSON that dropped the second call — 27.5% vs 47.5% raw. The grammar didn't make the model
worse at the task; it made it worse at *being itself*. Related work on the
reasoning-vs-format tension explains dead end #2 (grammar × thinking → empty), and the
fix that line converges on — **reason/draft freely first, then switch constrained decoding
on** — is precisely what `OPENHARN_NATIVE_TEMPLATE`'s think-phase and llama.cpp's lazy
grammar triggers (`tool_choice=required`) do. We rebuilt a known pattern the expensive way.

**LLMCompiler** — Kim et al., *"An LLM Compiler for Parallel Function Calling"* (ICML 2024,
arXiv:2312.04511). A **Function Calling Planner** decomposes a query into a **DAG of tasks
with explicit inter-dependencies**, a Task Fetching Unit dispatches, an Executor runs them
in parallel: up to 3.7× latency, 6.7× cost, ~9% accuracy over ReAct. This is the answer to
our single biggest remaining error class (dropped sub-tasks, 8/11 residual failures). The
DAG matters: dependencies are first-class, which is what a naive "split on *and*" misses —
see `parallel_multiple_27`, where task 2's `principal=5000` is only stated in task 1.

**TinyAgent** — Erdogan et al., *"TinyAgent: Function Calling at the Edge"* (EMNLP 2024
Demo, arXiv:2409.00608). LLMCompiler on **small models, llama.cpp, 4-bit quantization,
on-device** — our exact stack. Their planning success rates are the number to know:

| Model | Off-the-shelf | Fine-tuned |
|---|---|---|
| TinyLlama-1.1B | **12.71%** | 78.89% |
| Wizard-2-7B | **41.25%** | 83.09% |
| GPT-3.5 / GPT-4-Turbo | 65.04 / 79.08% | — |

and their diagnosis of off-the-shelf small models: *"not able to output the correct plans …
errors ranged from **using the wrong set of functions, hallucinated names, wrong
dependencies, and inconsistent syntax**."* Note that **two of those four failure modes are
exactly what a GBNF grammar structurally eliminates** — openharn's `t-{tool}` rules make a
hallucinated name unrepresentable, and the grammar forces syntax (we measured a single `ws`
quantifier fix moving the harness 44→57%). TinyAgent's answer was to **fine-tune**
(12.71→78.89%); the paper mentions no constrained decoding at all. That gap — *how much of
a fine-tuning gain a grammar buys for free* — is the open question this repo is positioned
to ask. Their **ToolRAG** (a DeBERTa multi-label classifier cutting the prompt 2762→1397
tokens, ~2×) is also the honest answer to prompt-size cost on CPU, which our 1400-token
MiniCPM tool prompt makes concrete.

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

### The variance collapse (the result I'd actually call novel)

Look down the run columns, not just the means. Every config on this box wobbles run-to-run
— CPU float reduction across 4 parallel slots isn't bit-deterministic, so at temp 0.001 a
hair's-width logit difference flips a token. Except one:

| Config | Runs | Spread |
|---|---|---|
| MiniCPM Q8 raw (thinking, free-form) | 85.0 / 85.0 / 77.5 | 7.5 |
| MiniCPM Q4 raw (thinking, free-form) | 47.5 / 50.0 / 45.0 | 5.0 |
| Q4 + `required`, **thinking on** | 27.5 / 40.0 / 45.0 | **17.5** |
| Q4 + native-template + array grammar | 22.5 / 30.0 / 30.0 | 7.5 |
| LFM2-Q2 harness D (200-entry) | 57.0 / 53.0 | 4.0 |
| **Q4 + `required` + `enable_thinking:false`** | **72.5 / 72.5 / 72.5** | **0.0** |

Three identical runs, 40 entries each, to the entry. The mechanism is mundane once you see
it: **run-to-run variance scales with the number of unconstrained tokens.** Free-form
generation is a noise amplifier — one divergent token cascades through a 1,000-token think
block and changes the answer. Strip the thinking (`enable_thinking:false`) and mask the
output space (`required`), and there are almost no free choices left: the grammar leaves so
few legal tokens that float noise can't change the argmax. Determinism falls out of
constraint.

The pathological row is the third: `required` **with** thinking is the *worst* variance
(17.5) — worse than no harness at all. Because it's bimodal, not noisy: the think block
either finishes in budget (→ a good forced call) or truncates (→ empty). A coin flip
between 45% and 27.5%. Constraining the *output* while leaving a long unconstrained
*prefix* is the worst of both worlds.

Practical upshot for a CPU-first project: the harness doesn't only raise the mean, **it
buys reproducibility** — which is the thing this whole study kept lacking (the 8/cat mini-set
lied, spot-checks lied, two "identical" gate runs differed by 2.5 points). This is a hard
measurement of the claim already floated in [`reasoning-tax.md`](reasoning-tax.md) — that
structured-output constraints reduce variance and token waste — and it comes with a bonus:
the winning config is also **~4× faster** (no think block to generate).

Caveat: n=3 runs, one model, one category. "Zero variance" means *these three runs agreed*,
not that the config is provably deterministic.

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

### What's left — the wall

After the rescue, 11/40 fail. Sorted by what could move them:

| Residual | Count | Movable by a harness? |
|---|---|---|
| **Dropped a sub-task** (decomposition) | 8 | No — a counting micro-pass is too unstable to condition on (57.5–85% on prompt wording); injecting it costs −20 pts (below) |
| Argument precision (`"Los Angeles"` vs `"Los Angeles, CA"`) | 2 | Partly; enums where the schema declares them, nothing where it's free-form |
| Format leak into an arg string (`pm_22`) | 1 | Yes — tolerant native-format parse |

So the ceiling is **decomposition**. TinyAgent prices what *training* buys there:
off-the-shelf small models plan at **12.71%** (1.1B) / **41.25%** (7B); fine-tuning gets
78–83%. A grammar deletes two of their four named failure modes (hallucinated names,
inconsistent syntax) — but can it make a model *notice there are two clauses in the
sentence*? I wrote "no" here, then measured it — got "yes" on one run, wrote that up,
and it failed to replicate. The measurement, and the correction, are below.

### Probing the wall: can a micro-pass count sub-tasks?

Setup: for each of the 40 `parallel_multiple` entries, ask the model *only* "how many
separate tool calls does this need?" and compare to `len(ground_truth)`. Two baselines
matter, and the second one is brutal:

- **the model's implicit count** (what it does today in the winning config): **32/40 = 80%**
- **a constant "2"**: the ground-truth distribution is **33 twos, 3 threes, 4 fours** — no
  entry needs 1 — so answering "2" blindly scores **33/40 = 82.5%**, already beating the
  model. Any decomposer has to clear a dumb constant.

| Decomposer variant | Exact | needs≥3 | Parsed | Note |
|---|---|---|---|---|
| always-2 (constant) | **82.5%** | **0/7** | — | trivial baseline |
| model's implicit | 80.0% | — | — | current behaviour |
| grammar `[1-9]` @ token 0 | **0.0%** | 0/7 | 40/40 | answered "1" ×40 |
| free reasoning, no grammar | 50.0% | 4/7 | **21/40** | 95% precise *when it answers* |
| two-pass, wording A | 85.0% | 5/7 | 40/40 | **did not reproduce — see below** |
| two-pass, wording B | **57.5%** | 5/7 | 40/40 | same procedure, one sentence changed |

Three things are real here, and one was a mirage.

**Real 1 — grammar-at-token-0 scored 0/40**, answering "1" to everything. Not the model
failing: the constraint collapsing the output to the **prior**. Grammar-Aligned Decoding's
distribution distortion (arXiv:2405.21047) reproduced in one line of GBNF — the same
mistake as dead end #1. *My probe violated the design rule this very note derives.*

**Real 2 — unconstrained, it's 95% precise but only answers 21/40 times.** The other 19 are
unparseable. So *that* failure was **form**, not semantics.

**Real 3 — draft-then-constrain fixes the form problem completely**: 21/40 → **40/40 parsed**,
every time, both wordings. Constraining only the final digit after free reasoning does
recover parseability without collapsing the answer.

**The mirage — the accuracy.** The first two-pass run scored 85.0% and I wrote it up as
"the wall moved." It does not reproduce. The only difference between the runs is one
sentence of the instruction:

| Instruction tail | Predicted distribution | Exact |
|---|---|---|
| "Think it through, then give the final count." | `{1:3, 2:30, 3:4, 4:3}` | 85.0% |
| "…then end with the final count as: `COUNT=<digit>`" | `{1:1, 2:18, 3:4, 4:15, 6:1, 7:1}` | **57.5%** |

Ground truth is `{2:33, 3:3, 4:4}`. Wording B predicts **4 calls for 15–20 of 40 entries**
when only 4 entries need 4. A 27-point swing from a cosmetic prompt change means the pass
is not *counting* — it is pattern-matching the instruction. `tests/bfcl/decompose_probe.py`
ships wording B (the reproducible 57.5%), not the lucky 85%.

**And wiring it in actively hurts.** Feeding the predicted count to the generation as
"emit exactly N calls" (`OPENHARN_TOOL_CHOICE=required` + no-think), A/B on the same
pipeline:

| Arm | Accuracy |
|---|---|
| control (no count injected) | 57.5% |
| **+ decomposer count injected** | **37.5%** |

−20 points. That answers the open question: forcing a count does **not** make the model find
the missing sub-task — a confidently-wrong N makes it **fabricate filler calls**. The
model's own implicit decomposition (80% counting) is better than an unreliable external
count, and a wrong count is worse than no count. (Caveat: this control scores 57.5% rather
than the 72.5% headline because the A/B harness re-implements the tool conversion more
crudely than BFCL's `convert_to_tool`; both arms share that pipeline, so the −20 **delta**
is valid, the absolute isn't.)

The one durable signal: `needs≥3` = **5/7 in both wordings**, where the constant scores 0/7
by construction. It can detect "this needs more than two" — it just can't calibrate *how
many*, and over-fires on the twos.

### So the wall stands — but the reason is sharper

Not "the model can't decompose." Measured:

> **The model can sometimes count, but not stably enough to condition on.** A pass whose
> answer swings 27 points on cosmetic prompt wording isn't a measurement, it's a coin flip
> with a prior. And because a wrong count *forces* fabrication, an unreliable planner is
> worse than none: the harness can't safely delegate to a judgment it can't trust.

So the wall from the earlier section holds, with one crack and one correction:

- **The crack is real**: relevance factored out (the gate: 75→87.5%) — a judgment *can*
  become a mechanic when the question is binary and the model's answer is stable.
- **The correction**: counting did *not* factor out. Its form problem is solvable
  (40/40 parsed); its *calibration* is not. Stability, not parseability, is the bar — and
  nothing in the harness supplies stability.

That is also the honest reading of TinyAgent's 12.71% → 78.89%: what fine-tuning buys is not
the ability to emit a plan, it's a **reliable** one. Grammar deletes two of their four
failure modes (hallucinated names, inconsistent syntax). It cannot buy the other two
(wrong function set, wrong dependencies), and this probe is a direct measurement of that
boundary.

**Meta-lesson, stated plainly:** every result in this study that was not replicated turned
out to be wrong — the 8/cat mini-set (+5 → flat), the spot-check of `required` (3/4 → lost
on 40), "only judgment misses left" (→ a format leak), and now the decomposer (85% → 57.5%).
Four for four. One run is a hypothesis.

The refined per-model decision tree, one experiment later:

```
native FC absent/broken   → prompt-tools + strict (+abstain/gate)     [LFM2-Q2: 0 → ~42%]
native FC works, degraded → native + tool_choice=required (+no-think) [MiniCPM-Q4: 47.5 → 72.5%]
native FC works, healthy  → leave it alone (harness only for gaps)    [MiniCPM-Q8, Qwen-0.8B]
reasoning tax dominates   → enable_thinking:false first, then decide  [APEX-Compact]
```

## The grounded gate: a clean negative

The residual is dropped sub-tasks. The idea: don't ask the model to *assert* a plan is
complete (that failed — the counter was a coin flip). Start from its implicit plan (~80%
count, which is the good part), then run a completeness *gate* — "is every part of the
request now covered?" — and if not, generate the missing call and loop. Count emerges from
grounded binary checks. This is Least-to-Most (Zhou et al., arXiv:2205.10625) + factored
isolation (Radhakrishnan et al., arXiv:2307.11768) + our own YES/NO gate, which was the one
micro-pass that ever worked (relevance, 75→87.5%).

The whole thing lives or dies on the gate. So I tested the gate in isolation as a
completeness classifier: 40 fixed base plans from one implicit run, labelled COMPLETE (passes
BFCL) or INCOMPLETE (fails) — 17 vs 23. A good gate flags the 23. Two checkers.

**Deviations, on purpose — this is hypothesis-checking, not the benchmark.** GPU, not CPU:
six full-offload MiniCPM-Q4 instances on the RTX 2050 (586 MiB each, ports 8080–8085,
~111 tok/s), work distributed across them. `cache_prompt=false` on every request — no KV
reuse, each check clean. Subset of 40, not the full category. None of this is thesis-legal;
it's here to move fast on a hypothesis.

| Checker | Acc | INCOMPLETE caught (of 23) | false alarms (of 17) | shape |
|---|---:|---:|---:|---|
| own Q4, grammar-locked YES/NO | 57.5% | 6 (26%) | 0 | rubber stamp — says "complete" 39/40 |
| MiniCheck-FT5 (770M) | 55.0% | 21 (91%) | 16 | NO-machine — rejects everything |

Neither works, and they fail as mirror images. The Q4 checking itself just says yes —
exactly the self-assessment weakness that sent me to MiniCheck in the first place. MiniCheck
(Tang et al., EMNLP 2024) is a dedicated fact-checker at GPT-4 accuracy for 400× less; it
flips the failure but not the outcome. Its recall is a threshold artifact, not signal — the
raw support-probabilities don't separate the classes:

```
MiniCheck support-prob   COMPLETE   mean 0.138   [0.020, 0.824]
                         INCOMPLETE mean 0.121   [0.009, 0.591]
                         AUC = 0.563          (0.5 = chance)
```

AUC 0.563 is chance. (And it took two framing tries: doc=request/claim=calls gave 26%;
flipping to doc=actions/claim=request gave the 91%. My first framing was wrong, but even the
right one is chance under the probabilities.) MiniCheck is trained for *factual* grounding —
is claim C supported by document D — which synthesises a document to back one claim.
Completeness is the other direction: check that *many* clauses of the claim each have an
action. `interest_coverage(XYZ,3)` doesn't "textually support" the imperative "find the
interest coverage ratio" the way its training defines support. Wrong relation, not a
tunable threshold.

**The wall, finally in the right place.** It's not that the model can't decompose — it does,
~80%, *inside generation*, where it holds the tools and is committed to acting. The wall is
that **there is no cheap reliable verifier of plan-completeness** — not the model on itself
(chance), not a SOTA fact-checker (chance). "Is this plan done?" is irreducibly semantic and
nothing supplies it. That is why LLMCompiler/TinyAgent buy reliability by *training the
planner*, not by bolting on a checker — there is no verifier shortcut. TinyAgent's
12.71→78.89% is that price, and it's paid in weights.

The contrast that makes the point: openharn's *existing* grounding works because it's
**literal**. Read-before-edit doesn't ask a model "does this file exist?" — the filesystem
returns the real list. Plan-completeness has no literal signal, so it collapses to judgment,
and judgment is what the small model doesn't have. The only grounding that gives a
reliable gate is **execution** — run the calls, read back whether the request's targets are
now obtained — which is a literal signal, and which openharn already has two forms of: the
`slm_harness` executor+verifier, and BFCL's own multi-turn state check. So I ran it: MiniCPM
Q4 on `multi_turn_base` (GorillaFileSystem/TwitterAPI, GPU), **0/3**. And the execution gate
did its job *perfectly* — the model executed real calls (`mkdir temp`, `mv final_report.pdf
temp`, then invented `find`/`grep` steps), the filesystem state genuinely changed, and the
state check correctly scored it 0 because the plan was wrong. That's the whole point in one
run: the execution verifier is reliable *because it's literal* — it reads real state, not a
model's opinion — which is exactly why the semantic gates (own-Q4, MiniCheck) sit at chance
and this one doesn't. But a reliable gate doesn't rescue the model: with verification solid,
the bottleneck is entirely **generation** — the 4-bit model plans the wrong calls. Every
road ends in the same place: the model, not the harness. A verifier tells you the plan is
wrong; it can't write a right one.

## Decomposition done right: plan → parse → per-item (fixes 8, breaks 7)

The counter failed because it asked the model to *assert a number*. This asks it to *plan*
and lets the harness count the plan — Least-to-Most (Zhou et al., arXiv:2205.10625) /
decomposed prompting. One unconstrained think-on generation: "break this into a numbered
list of the separate tool calls needed; don't write the calls, just list what each does."
Regex the list. Run one focused native+`required` generation per item, each given the FULL
request as context ("shared values live in the request" — mitigates cross-item deps). If the
list won't parse, fall back to the plain implicit call.

Same 40, on GPU (6× MiniCPM-Q4 instances; GPU Q4_0 is less precise than CPU, so this is
within-backend only — don't compare to the CPU 72.5%):

| | implicit | decompose |
|---|---|---|
| planning parsed | — | **38/40** |
| count-accuracy | 21/40 | **29/40** |
| count dist | `{1:17, 2:21}` (under-calls) | `{2:28, 3:4, 4:4}` ≈ gt |
| **BFCL AST** | 45.0% | 47.5% |

The list parses (38/40 vs 21/40 for the `COUNT=` format) and the count jumps (+20 points,
17 under-calls → 4). But BFCL moves +2.5 — one entry — and that flat number is a lie: it's a
**15/40 churn**. Decompose **fixed 8, broke 7**.

- **Fixed (8):** the dropped-sub-task/wrong_count cases — incl. `pm_27` (transfer $5000 *then*
  interest at 3%): implicit dropped the interest call; decompose emitted it AND pulled
  `principal:5000.0` from the request context. The dependency mitigation worked, on the
  hardest case. Decomposition does exactly what it's for.
- **Broke (7):** two mechanisms. The plan **over-splits** — `pm_4`, gt=2, the list had 4
  items → a duplicate `derivative` call. And each extra per-item generation is **new failure
  surface** — `pm_9`, one call came back mangled (`hotel_book({"location":"Boston\n</>\n<parameter=nights>…` —
  the Q4_0 XML leak, in a per-item gen) and flight_book got dropped.

So the mechanism is real and works on its target; **applied to everything it's a wash,
because you can't tell which cases need it.** That's the completeness-gate again — help the
~8 that need decomposing, leave the ~7 that don't, and the router is the checker we already
measured at chance. The fix exists; the routing doesn't.

Where it goes: decompose fails *differently* from implicit (fixes 8 implicit misses). So
best-of-{implicit, decompose} with a **reliable selector** keeps both — up to 19 vs ~12
alone. The selector needs a literal signal (execution), which only exists on agentic tasks.
Which is the next section.

## Verifier-as-selector: pass@6 = 0

The one honest use of the reliable (execution) verifier is as a *selector*, not a checker:
generate N diverse candidates, execute each, keep the one that satisfies. A verifier can't
write a right plan but it can pick one. The bet: if the model is right 1-in-N, best-of-N +
the verifier recovers it. So the gating question is the model's 1-in-N rate, and that's cheap
to measure — the 6 GPU instances are 6 parallel attempts for free.

multi_turn_base ×3, 6 attempts each at temp 0.7 (diversity — the opposite of the determinism
config; best-of-N wants spread). BFCL's state check is the selector.

```
per-attempt accuracy:  0% 0% 0% 0% 0% 0%
pass@1 = 0/3     pass@6 = 0/3     correct candidates: 0/18
```

Zero. The model never produces a correct multi-turn trajectory in 6 diverse tries, so there
is nothing to select. best-of-N with a perfect verifier is still 0 when all N are wrong.
Matches this morning's LFM2-Q2 (0/5) — these small models can't do multi-turn agentic, full
stop, and no amount of harness selection conjures a right answer that isn't in the
distribution.

### Per-turn selection: the wall, finally at the root (`tests/bfcl/per_turn_selection.py`)

Trajectory pass@N demands the whole scenario at once. Per-*turn* selection asks less: at each
turn generate N candidates, execute each against the correct prefix state, and an ORACLE
selector (BFCL's own `state_checker` vs the ground-truth turn state) keeps any candidate that
matches. It's a ceiling (the selector uses labels), but it answers the real question — is the
model's *per-turn* 1-in-N rate high enough to assemble a trajectory. Built it on BFCL's
executor + state check, committing GT history between turns; N candidates fan out across the 6
GPU instances.

```
N=6  : per-turn hit 8/13 (62%)   trajectory 0/3
N=18 : per-turn hit 8/13 (62%)   trajectory 0/3   — IDENTICAL. 3x the candidates, 0 more turns.
```

Two hard facts. **(1) The model does 62% of turns fine** — massively more than the 0%
trajectory score suggests; selection recovers every one of them (winner usually candidate 0).
**(2) The missed turns are rate-ZERO, not rare** — tripling N to 18 recovers exactly none.
And they're the *same* turns every time. So trajectory recovery is 0/3 not because chaining is
unlucky, but because every entry contains ≥1 turn the model simply cannot produce.

What those turns are is the whole investigation in one table:

| | ground-truth calls | |
|---|---|---|
| **MISS** (0/18) | `cd; mkdir; mv` · `cd; mv` · `cd; touch` · `cp; cd; mv` | dependent multi-call: navigate **then mutate** |
| **HIT** | single calls · `cd; grep` (navigate then **read**) | single, or read-after-navigate |

Every rate-0 turn is a **stateful, order-dependent multi-call sequence** — each call depends on
the prior call's effect (`cd` moves the dir, then `mv`/`mkdir` act relative to it). The model
can't chain those. This is the **same wall as `parallel_multiple`**: there it *dropped* a call
from a parallel set; here it can't *sequence* dependent calls. Both are **multi-call
composition**, and that is the capability the model lacks. Single calls the harness fixes
(form). Composition it cannot supply, because it isn't in the model's distribution — which is
why grammar, the gate, MiniCheck, decomposition, and best-of-N all bottomed out at the same
floor. Per-turn selection is just the instrument that finally isolated it: it extracts 100% of
what the model can do and 0% of what it can't, and compute doesn't move the second number.

That's the terminal finding. The harness owns *form*, cleanly and with real wins. The model
owns *composition*, and nothing in the harness — no verifier, no selector, no N — puts it there.
Only weights do (TinyAgent, 12.71→78.89%).

## The wall moved — I was wrong that it was terminal

Everything above this line called multi-call composition a model-capability wall that "nothing
in the harness puts there, only weights do." That was too strong, and this section walks back
the overconfident half of it with numbers that replicate. The composition capability **was in
LFM2-Q2's weights the whole time** — the harness config I was measuring (`prompt-tools`, which
*flattens* the tool list into a text blob) was itself **suppressing** it. Give the model its
native tool presentation back, add a cheap reasoning buffer, and strip the duplicate calls, and
parallel goes 17.5 → 72.5 and the AST subset goes **45 → ~72%**. On the same 2-bit model, same
CPU box.

I only found this because the goal was reset to "get ≥60% AST, and research every decision
instead of pulling it out of your ass." So I did the reading first. Three papers, all
inference-time and model-agnostic (finetuning is off the table — that's TinyAgent's answer, not
ours):

- **Tool Suppression / the "constraint tax"** — Li, Zhang, Lv, *arXiv:2606.25605*. Reproduces
  exactly what I'd been eating: when a JSON/grammar constraint is compiled to a token mask,
  tool-call tokens go *unreachable* and the model's action selection degrades under the
  constraint even though it can call fine unconstrained. Their fix is a **two-pass decouple** —
  reason free, *then* constrain. This is why `simple`/`multiple` sat *below* raw in every
  strict config: grammar-from-token-0.
- **Adapt schemas to the model, not the model to schemas** — Lee et al., ACL 2026,
  *arXiv:2510.07248* (PA-Tool). Small models fail tool use when the tool *presentation*
  mismatches what they saw in pretraining. Our `prompt-tools` flattening is precisely that
  mismatch — it throws away the `<|tool_list_start|>…<|tool_list_end|>` framing LFM2 was
  trained on.
- **Plan-then-emit** — LLMCompiler (Kim et al., ICML 2024) and guided-structured templates
  (Dang et al., EMNLP 2025, *arXiv:2509.18076*): make the model enumerate the calls first. The
  call grammar *already* permits an N-object array (`call ::= "[" obj ("," obj)* "]"` — checked
  the code, parallel was never grammar-capped), so a plan is all that's needed to make the model
  *commit* to N calls instead of dropping to 1.

### What I changed (model-agnostic, all opt-in)

- **`OPENHARN_NATIVE_TEMPLATE`** already existed (renders the model's own template via
  `/apply-template`, reasons if the template opens a think tag, then grammar-forces the call).
  It had never been run on LFM2-Q2's AST subset — only on the MiniCPM quant-rescue. Running it
  *is* the "native presentation" arm.
- **`OPENHARN_PLAN_FIRST`** (new) — for a model whose template has **no** think tag (LFM2's
  assistant turn opens straight at the answer, checked via `/apply-template`), inject an
  explicit unconstrained planning step: "list EVERY separate tool call, one per line," stop
  before any JSON, append it, *then* run the constrained call. This is the two-pass decouple for
  a non-thinking model. The primer names no model; the tag is read from the template.
- **`OPENHARN_DEDUP_CALLS`** (new) — drop exact-duplicate calls (same name + same argument
  string). Plan-first makes LFM2 *repeat* calls under the forced array grammar (it lists the
  plan, then re-emits — see the trace below), which the checker scores `wrong_count`.
- **Transport retry** (always on) — 3 attempts on a dropped/refused connection instead of
  silently returning no call. Not cosmetic: native-template makes 2–3 requests/entry and
  llama-server's accept queue flakes under `--num-threads 4` — one run logged **64 recovered
  flakes**. Without retry the baseline was being depressed by the harness's own dropped
  connections, not the model.

### Results — LFM2-8B-A1B UD-Q2_K_XL, 160-entry AST subset, CPU (`-ngl 0`)

All arms on one binary, one llama-server session, `temp 0.001`, `--num-threads 4`.

| Category | D (prompt-tools+gate) | H1 native-template | H2 +plan-first | **H2 +plan +dedup** |
|---|---|---|---|---|
| simple_python (40) | 70.0 | 55.0 | 60.0 | **87.5 / 80.0** |
| multiple (40) | 55.0 | 45.0 | 52.5 | **62.5 / 60.0** |
| parallel (40) | 17.5 | 52.5 | 67.5 | **75.0 / 70.0** |
| parallel_multiple (40) | 37.5 | 55.0 | 57.5 | **70.0 / 70.0** |
| **AST total** | **45.0%** | **51.9%** | **59.4%** | **73.75% / 70.0%** |

The winning arm is shown as **run A / run B** — two independent full runs, because I got burned
earlier in this same file by an 85% number that didn't replicate. It replicates: 73.75 and 70.0,
mean **71.9% AST**, both clearing 60 by 10+ points. Variance is ±4, the same temp-0.001 CPU
non-determinism documented up top.

Read the columns left to right — it's a clean decomposition of *why* it works:

- **native presentation alone (H1)** is the big composition unlock: parallel **17.5 → 52.5**,
  parallel_multiple **37.5 → 55**. That's the PA-Tool effect measured — the capability was
  latent, flattening was hiding it. But it *taxes the single-call categories* (simple 70 → 55,
  multiple 55 → 45): grammar-from-token-0 with no reasoning buffer, exactly Li et al.'s tax.
- **plan-first (H2)** pays the tax back and pushes further: it gives the non-thinking model the
  reasoning buffer, so singles recover (55 → 60, 45 → 52.5) *and* the explicit N-commitment
  lifts parallel again (52.5 → **67.5**). 59.4% — one entry short of target.
- **dedup** clears it and then some. Plan-first's cost is that LFM2 repeats calls; removing the
  repeats fixes `wrong_count` across the board (+11 on simple alone in run A). This is the last
  +12–14 points.

### The mechanism, concretely

Plan-first output for `parallel_0` ("play Taylor Swift 20m and Maroon 5 15m"), verbatim from
LFM2-Q2:

```
1. Play Taylor Swift for 20 minutes → call `spotify_play` with artist="Taylor Swift" and duration=20.
2. Play Maroon 5 for 15 minutes → call `spotify_play` with artist="Maroon 5" and duration=15.
3. No additional tools are needed; each call is explicit and distinct.
4. Execute both calls.
​```
spotify_play: {"artist": "Taylor Swift", "duration": 20}
spotify_play: {"artist": "Maroon 5", "duration": 15}
​```
```

That's the whole thing. The model *can* enumerate two distinct calls — it says so explicitly
("each call is explicit and distinct"). What it couldn't do was emit them when the grammar
clamped from token 0 (H1's tax) or when flattening hid the native format (D's parallel 0–17%).
Note the tail: it starts re-emitting the calls, which is the duplicate source dedup mops up. The
grammar pass that follows produces the clean `[{…},{…}]` array; the checker sees two correct
distinct calls (verified in the result files: parallel_0/1/2/3 all pass with genuinely distinct
args, not dedup artifacts).

### Caveats — read these before believing the headline

- **`multiple` never fully recovers** (62.5/60 vs ~82.5 raw native FC). The constraint tax on
  single-tool-choice-among-many is only *partly* paid back by plan-first. If you only care about
  single calls, raw native FC still wins there — the harness's edge is composition.
- **dedup has one known false positive**: BFCL contains exactly one entry (`parallel_158`) whose
  correct answer *is* the same call twice with identical args. dedup would break it. It is not in
  this subset (checked), and dedup is **opt-in** and off by default, so the coding agent is
  unaffected — but the flag is unsafe for the rare genuine identical-repeat call.
- **Deviations from the CPU-first thesis: none this run** (that's the point — it's `-ngl 0`,
  same box, same quant as the 45% baseline). The only non-defaults are `--num-threads 4` + the
  transport retry, both documented above; retry is what makes the 4-thread number trustworthy.
- **This does not resurrect the *agentic* multi_turn wall.** Those failures are *dependent,
  order-sensitive* sequences (`cd` then `mv`), a different and harder thing than a *parallel* set
  of independent calls. plan-first was not tested there and I would not expect it to clear that —
  the per-turn rate-0 finding above stands until measured otherwise.

### What this does to the thesis

"Harness owns form, weights own composition" was half right. Better: **the harness can suppress
or unlock latent composition, and the default prompt-tools path was suppressing it.** The wins
are still form-shaped underneath — native presentation (don't destroy the format), a plan buffer
(don't constrain before the model has committed), and dedup (clean the emission). None of it is
new capability injected into the model; it's *stopping the harness from hiding capability the
weights already had.* That is a materially more useful statement than "only finetuning moves it,"
and it's the one the numbers support. `parallel_multiple_27`-style **dependent** composition
(shared value produced by one call, needed by another) is still unmoved — that one really is a
weights problem, and it's the honest remaining wall.

Winning config (BFCL AST, ≥60% target cleared at ~72%):

```sh
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=8090 \
OPENHARN_FC_PROXY=1 OPENHARN_NATIVE_TEMPLATE=1 OPENHARN_PLAN_FIRST=1 OPENHARN_DEDUP_CALLS=1 \
OPENHARN_MAX_TOKENS=512 ./target/debug/openharn .
```

## Reproduce

[`tests/bfcl/README.md`](../tests/bfcl/README.md) — exact `bfcl generate/evaluate` commands,
the two-model registration, the subset builder, the failure analyzer, and
[`run_arm.sh`](../tests/bfcl/run_arm.sh) (one arm end-to-end, used for the table above).
