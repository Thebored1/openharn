# openharn on BFCL v4

Reproducible setup for benchmarking a small model on the **Berkeley Function Calling
Leaderboard v4** (Patil et al., ICML 2025) *through openharn*, so the harness's effect
on tool-call reliability can be measured against the raw model on the same dataset and
the same official AST checker.

> Scope note (matches openharn's thesis): the point is not a leaderboard number, it is
> **does the harness move the score, model-agnostically, on weak/CPU hardware.** Runs
> are CPU-first (`llama-server -ngl 0`); see [`notes/bfcl-v4.md`](../../notes/bfcl-v4.md)
> for results and the GPU-vs-CPU measurement on this box.

## What connects to what

```
BFCL (bfcl generate/evaluate)                        [OpenAI FC client + AST checker]
   │  OPENAI_BASE_URL
   ├──────────────► llama-server :8080   (A: raw native FC — baseline)
   └──────────────► openharn --serve :8090 ──► llama-server :8080
                    OPENHARN_FC_PROXY=1        (B: harness — one constrained
                                                tool-call generation, no agent loop)
```

`openharn --serve` gains an **FC-proxy mode** (`OPENHARN_FC_PROXY=1`): when a request
carries `tools`, it runs exactly ONE constrained tool-call generation (openharn's
prompt-tools + strict grammar, or native tools) and returns the `tool_calls` directly —
no agent loop, no execution. That exposes only openharn's tool-call *reliability* layer,
which is what BFCL single-turn categories score. See `src/serve.rs` / `agent::fc_proxy_once`.

Harness sub-modes (env on the `--serve` process):
- **B1 prompt-tools + strict** — `OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1`
- **B2 native + recovery** — `OPENHARN_FC_PROXY=1` only (native `tools`, plus text-call recovery)

## Prerequisites

```sh
python -m venv .venv && . .venv/Scripts/activate     # or your venv
pip install bfcl-eval soundfile                       # soundfile: BFCL optional-dep fix
python tests/bfcl/register_models.py                  # add the two FC models (idempotent)

export BFCL_PROJECT_ROOT=/path/to/scratch             # result/, score/, id file live here
export PYTHONUTF8=1 PYTHONIOENCODING=utf-8             # BFCL prints emoji; Windows cp1252 crashes without this
export OPENAI_API_KEY=dummy                            # handler requires a key even for local
```

## Run

```sh
# fixed subset (full v4 is too slow on CPU)
python tests/bfcl/subset.py --n 40 \
  --categories simple_python multiple parallel parallel_multiple irrelevance

# A: baseline — point BFCL at llama-server directly
export OPENAI_BASE_URL=http://127.0.0.1:8080/v1
bfcl generate --model openharn-lfm2-raw --run-ids --num-threads 4 --temperature 0.001 -o
bfcl evaluate --model openharn-lfm2-raw --partial-eval

# D: the winning harness config — start openharn FC-proxy with abstain + gate + ws-fix,
#    point BFCL at it (this is condition D in notes/bfcl-v4.md, ~53-57%)
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=8090 \
OPENHARN_FC_PROXY=1 OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 \
OPENHARN_STRICT_ABSTAIN=1 OPENHARN_FC_GATE=1 OPENHARN_MAX_TOKENS=512 ./target/debug/openharn . &
export OPENAI_BASE_URL=http://127.0.0.1:8090/v1
bfcl generate --model openharn-lfm2-harness --run-ids --num-threads 4 --temperature 0.001 -o
bfcl evaluate --model openharn-lfm2-harness --partial-eval

# failure breakdown for either model
python tests/bfcl/analyze.py openharn-lfm2-harness
```

`--partial-eval` scores only the subset present in the result files (not the official
full-category number). `underscore_to_dot=True` in the registration is required so
BFCL's dotted function names (sanitized to underscores for the OpenAI FC schema) are
mapped back during checking.

## Quant-degraded native FC (e.g. MiniCPM-V Q4_0): native + required + no-think

For a model whose native FC *works but degrades under quantization* (mangles its own call
syntax), don't use prompt-tools — force the model's OWN format via the server and switch
thinking off (see the quant-rescue section in [`notes/bfcl-v4.md`](../../notes/bfcl-v4.md);
MiniCPM-V-4.6 Q4_0: 47.5% → 72.5% on `parallel_multiple`):

```sh
# server with enough per-slot context for tool prompts + generation:
llama-server -m MiniCPM-V-4_6-Q4_0.gguf --jinja --ctx-size 16384 --parallel 4 -ngl 0

OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=8090 \
OPENHARN_FC_PROXY=1 OPENHARN_TOOL_CHOICE=required \
OPENHARN_TEMPLATE_KWARGS='{"enable_thinking":false}' \
OPENHARN_MAX_TOKENS=1024 ./target/debug/openharn . &
```

`required` forces a call (llama.cpp grammar in the model's native format) — pair it with
the gate on abstention workloads. `enable_thinking:false` is a no-op on templates without
the switch.

## The AST winner: native presentation + plan-first + dedup (LFM2-Q2, ~72% AST)

The best AST-subset config found (45% → ~72%, replicated; full write-up + per-category table
and the three papers it's grounded in are in [`notes/bfcl-v4.md`](../../notes/bfcl-v4.md), section
"The wall moved"). Restores the model's native tool presentation (undoing prompt-tools
flattening), adds an unconstrained planning step before the constrained emission (paying back
the constraint tax and committing the model to N calls), and drops duplicate calls:

```sh
llama-server -m LFM2-8B-A1B-UD-Q2_K_XL.gguf --jinja --ctx-size 16384 --parallel 4 -ngl 0

OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=8090 \
OPENHARN_FC_PROXY=1 OPENHARN_NATIVE_TEMPLATE=1 OPENHARN_PLAN_FIRST=1 OPENHARN_DEDUP_CALLS=1 \
OPENHARN_MAX_TOKENS=512 ./target/debug/openharn . &
```

`run_arm.sh <name> <port> "<extra env>" <bfcl_root> <id_file>` runs one arm end-to-end
(serve → generate → evaluate → per-category accuracy + transport-failure count). The four
arms behind the table:

```sh
bash tests/bfcl/run_arm.sh D  8090 "OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 OPENHARN_STRICT_ABSTAIN=1 OPENHARN_FC_GATE=1" "$TEMP/bfcl_D" "$IDFILE"
bash tests/bfcl/run_arm.sh H1 8091 "OPENHARN_NATIVE_TEMPLATE=1" "$TEMP/bfcl_H1" "$IDFILE"
bash tests/bfcl/run_arm.sh H2 8092 "OPENHARN_NATIVE_TEMPLATE=1 OPENHARN_PLAN_FIRST=1" "$TEMP/bfcl_H2" "$IDFILE"
bash tests/bfcl/run_arm.sh H2d 8093 "OPENHARN_NATIVE_TEMPLATE=1 OPENHARN_PLAN_FIRST=1 OPENHARN_DEDUP_CALLS=1" "$TEMP/bfcl_H2d" "$IDFILE"
```

Note: `NATIVE_TEMPLATE` does not run the relevance gate (it forces a call), so it's for the
AST categories (all need a call), not `irrelevance`. Transport retry (3 attempts) is always on
and matters here — native-template makes 2–3 requests/entry and llama-server's accept queue
flakes under `--num-threads 4`; without retry the score is depressed by dropped connections.

### Thinking models: add `OPENHARN_PLAN_ALWAYS`

`PLAN_FIRST` is skipped when the template opens a `<think>` tag (the model already reasons), so
a *thinking* model uses its native think — which may under-call on composition. `PLAN_ALWAYS`
runs the enumeration step *after* the native think too. Cross-model example (MiniCPM-V-4.6 Q4_0
on GPU/CUDA, same 160-entry subset): winning config = 60.0% AST (strong singles, weak
composition — 22/26 parallel misses were under-calls); adding `PLAN_ALWAYS` cut under-calls to
14/18 and lifted AST to ~65.6% mean (two runs 68.75 / 62.5 — noisier, since think+plan is two
free-generation phases). Full table + caveats in [`notes/bfcl-v4.md`](../../notes/bfcl-v4.md)
("Does it transfer?").

```sh
# thinking model (GPU): winning config + PLAN_ALWAYS
llama-server -m MiniCPM-V-4_6-Q4_0.gguf --jinja --ctx-size 16384 --parallel 4 -ngl 99   # CUDA build
bash tests/bfcl/run_arm.sh MCPM 8095 \
  "OPENHARN_NATIVE_TEMPLATE=1 OPENHARN_PLAN_FIRST=1 OPENHARN_DEDUP_CALLS=1 OPENHARN_PLAN_ALWAYS=1" \
  "$TEMP/bfcl_mcpm" "$IDFILE"
```

## Decomposition probe (`decompose_probe.py`)

Probes the biggest residual failure class (dropped sub-tasks) without touching openharn —
asks the model only "how many separate tool calls does this need?" and scores against
`len(ground_truth)`. Needs a `llama-server` on :8080 with `--jinja`:

```sh
python tests/bfcl/decompose_probe.py "$SITE_PACKAGES/bfcl_eval"          # all 4 variants
python tests/bfcl/decompose_probe.py "$SITE_PACKAGES/bfcl_eval" two-pass  # just the winner
```

The variant comparison is the point (full analysis in [`notes/bfcl-v4.md`](../../notes/bfcl-v4.md)):
grammar-from-token-0 collapses to the prior (0%), free reasoning is 95% precise but half
unparseable (50%), and **draft-then-constrain is 40/40 parsed at 85%**. Both baselines it
must clear are printed automatically — including a constant "2", which scores 82.5% on this
category and beats the model's own implicit count.

## Agentic / multi-turn (`multi_turn_*`, `memory_*`, `web_search_*`)

These run through the same FC-proxy, but **drop `OPENHARN_FC_GATE`** — the relevance gate
is a single-turn irrelevance tool and abstains (`NO_TOOL`) on every agentic turn, scoring 0
by never acting. With the gate off the harness acts (e.g. `multi_turn_base_0` turn 0 emits
`mv final_report.pdf → temp`), but multi-turn is a **model-capability wall** for a 2-bit
1B-active model (0/5 on `multi_turn_base`) — see [`notes/bfcl-v4.md`](../../notes/bfcl-v4.md).

```sh
# note: NO OPENHARN_FC_GATE for agentic; --num-threads 1 (turns are stateful)
OPENHARN_BASE_URL=http://127.0.0.1:8080/v1 OPENHARN_SERVE=1 OPENHARN_SERVE_PORT=8090 \
OPENHARN_FC_PROXY=1 OPENHARN_PROMPT_TOOLS=1 OPENHARN_STRICT_TOOLS=1 \
OPENHARN_STRICT_ABSTAIN=1 OPENHARN_MAX_TOKENS=512 ./target/debug/openharn . &
bfcl generate --model openharn-lfm2-harness --test-category multi_turn_base --run-ids --num-threads 1 -o
bfcl evaluate --model openharn-lfm2-harness --test-category multi_turn_base --partial-eval
```

## Hurdles worth knowing (cost real time)

- `pip install bfcl-eval` then `import bfcl_eval` → `ModuleNotFoundError: soundfile`
  (a hard import in `model_config.py`). Fix: `pip install soundfile`.
- `bfcl evaluate` prints a 🦍 emoji → `UnicodeEncodeError` on Windows cp1252. Fix:
  `PYTHONUTF8=1 PYTHONIOENCODING=utf-8`.
- `bfcl evaluate` needs `OPENAI_API_KEY` set (dummy is fine) even for offline AST scoring —
  it builds the handler/client first.
- Stock `openharn --serve` returns final **text** only (no `tool_calls`, `usage: null`) —
  BFCL's FC handler needs both. That's why `OPENHARN_FC_PROXY` exists.
- `--run-ids` runs only the ids in `test_case_ids_to_generate.json` and ignores
  `--test-category`; `--partial-eval` is required to score a subset.
